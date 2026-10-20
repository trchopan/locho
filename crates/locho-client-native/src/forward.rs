//! Reusable native forwarding for SDK loopback and CLI multi-service listeners.
//! Callers own listener policy, peer session, shutdown and logging configuration.

use locho_core::{
    capability::Capability,
    protocol::*,
    session::{PeerSession, SessionError},
};
use std::sync::Arc;
use tokio::{net::TcpListener, sync::Semaphore, task::JoinSet, time::timeout};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

trait LocalStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T> LocalStream for T where T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}

type BoxedLocalStream = Box<dyn LocalStream>;
type AcceptFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = std::io::Result<(BoxedLocalStream, String)>> + Send + 'a>,
>;

trait LocalListener: Send {
    fn accept(&self) -> AcceptFuture<'_>;
}

impl LocalListener for TcpListener {
    fn accept(&self) -> AcceptFuture<'_> {
        Box::pin(async move {
            let (stream, peer) = self.accept().await?;
            Ok((Box::new(stream) as BoxedLocalStream, peer.to_string()))
        })
    }
}

#[cfg(unix)]
impl LocalListener for crate::unix::UnixListenerGuard {
    fn accept(&self) -> AcceptFuture<'_> {
        Box::pin(async move {
            let (stream, _) = self.accept().await?;
            Ok((Box::new(stream) as BoxedLocalStream, "unix".to_owned()))
        })
    }
}

/// Keeps the listener bound across peer recovery. On shutdown stop accepting,
/// cancel handshakes, drain at most ten seconds, then abort/join remaining tasks.
pub async fn serve_tcp(
    listener: TcpListener,
    session: PeerSession,
    capability: Capability,
    total_connections: Arc<Semaphore>,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    serve_tcp_with_idle_timeout(
        listener,
        session,
        capability,
        total_connections,
        shutdown,
        Some(TCP_IDLE_TIMEOUT),
    )
    .await
}

pub async fn serve_tcp_with_idle_timeout(
    listener: TcpListener,
    session: PeerSession,
    capability: Capability,
    total_connections: Arc<Semaphore>,
    shutdown: CancellationToken,
    idle_timeout: Option<std::time::Duration>,
) -> std::io::Result<()> {
    serve_listener(
        listener,
        session,
        capability,
        total_connections,
        shutdown,
        idle_timeout,
    )
    .await
}

#[cfg(unix)]
pub async fn serve_unix(
    listener: crate::unix::UnixListenerGuard,
    session: PeerSession,
    capability: Capability,
    total_connections: Arc<Semaphore>,
    shutdown: CancellationToken,
    idle_timeout: Option<std::time::Duration>,
) -> std::io::Result<()> {
    serve_listener(
        listener,
        session,
        capability,
        total_connections,
        shutdown,
        idle_timeout,
    )
    .await
}

async fn serve_listener<L: LocalListener + 'static>(
    listener: L,
    session: PeerSession,
    capability: Capability,
    total_connections: Arc<Semaphore>,
    shutdown: CancellationToken,
    idle_timeout: Option<std::time::Duration>,
) -> std::io::Result<()> {
    let connections = Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS));
    let mut tasks = JoinSet::new();
    let result = loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break Ok(()),
            result = listener.accept() => {
                let (stream, peer) = match result { Ok(value) => value, Err(error) => break Err(error) };
                let permit = match connections.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => { error!(service = %capability.service, peer = %peer, "stream connection limit reached"); drop(stream); continue; }
                };
                let total = match total_connections.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => { error!(service = %capability.service, peer = %peer, "global stream connection limit reached"); drop(stream); continue; }
                };
                let session = session.clone();
                let capability = capability.clone();
                let shutdown = shutdown.clone();
                tasks.spawn(async move {
                    let (_permit, _total) = (permit, total);
                    forward_connection(stream, session, capability, shutdown, idle_timeout).await;
                });
            }
            result = tasks.join_next(), if !tasks.is_empty() => {
                if result.is_some_and(|value| value.is_err()) {
                    error!("native TCP forwarding task failed");
                }
            }
        }
    };
    drop(listener);
    if timeout(SHUTDOWN_TIMEOUT, async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        warn!("shutdown deadline reached; aborting local stream connections");
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
    result
}

async fn forward_connection(
    local: BoxedLocalStream,
    session: PeerSession,
    capability: Capability,
    shutdown: CancellationToken,
    idle_timeout: Option<std::time::Duration>,
) {
    info!(service = %capability.service, "local stream connection");
    let lease = tokio::select! {
        biased;
        _ = shutdown.cancelled() => return,
        result = session.acquire(ATTACH_RECONNECT_TIMEOUT) => match result {
            Ok(lease) => lease,
            Err(error) => { error!(service = %capability.service, %error, "stream tunnel unavailable"); return; }
        },
    };
    let remote = match lease
        .open_tcp(&capability, HANDSHAKE_TIMEOUT, &shutdown)
        .await
    {
        Ok(stream) => stream,
        Err(error) => {
            if !matches!(error, SessionError::Closed | SessionError::Cancelled) {
                error!(service = %capability.service, %error, "stream tunnel handshake failed");
            }
            return;
        }
    };
    if let Err(error) = relay_with_idle_policy(local, remote, idle_timeout).await {
        if error.downcast_ref::<TunnelIdleTimeout>().is_none() {
            // A local reset or a remote per-stream rejection is not evidence
            // that the shared peer connection is broken. Its driver observes
            // connection-wide failures independently.
            if lease.connection().close_reason().is_some() {
                lease.invalidate();
            }
            // Do not forward arbitrary I/O error text into SDK diagnostics.
            error!(service = %capability.service, "stream tunnel forwarding failed");
        }
    }
}
