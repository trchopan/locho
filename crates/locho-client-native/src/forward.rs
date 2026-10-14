//! Reusable native forwarding for SDK loopback and CLI multi-service listeners.
//! Callers own listener policy, peer session, shutdown and logging configuration.

use locho_core::{
    capability::Capability,
    protocol::*,
    session::{PeerSession, SessionError},
};
use std::sync::Arc;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    task::JoinSet,
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Keeps the listener bound across peer recovery. On shutdown stop accepting,
/// cancel handshakes, drain at most ten seconds, then abort/join remaining tasks.
pub async fn serve_tcp(
    listener: TcpListener,
    session: PeerSession,
    capability: Capability,
    total_connections: Arc<Semaphore>,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    let connections = Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS));
    let mut tasks = JoinSet::new();
    let result = loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break Ok(()),
            result = listener.accept() => {
                let (stream, _) = match result { Ok(value) => value, Err(error) => break Err(error) };
                let permit = match connections.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => { error!(service = %capability.service, "TCP connection limit reached"); drop(stream); continue; }
                };
                let total = match total_connections.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => { error!(service = %capability.service, "global connection limit reached"); drop(stream); continue; }
                };
                let session = session.clone();
                let capability = capability.clone();
                let shutdown = shutdown.clone();
                tasks.spawn(async move {
                    let (_permit, _total) = (permit, total);
                    forward_connection(stream, session, capability, shutdown).await;
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
        warn!("shutdown deadline reached; aborting local TCP connections");
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
    result
}

async fn forward_connection(
    local: TcpStream,
    session: PeerSession,
    capability: Capability,
    shutdown: CancellationToken,
) {
    info!(service = %capability.service, "local TCP connection");
    let lease = tokio::select! {
        biased;
        _ = shutdown.cancelled() => return,
        result = session.acquire(ATTACH_RECONNECT_TIMEOUT) => match result {
            Ok(lease) => lease,
            Err(error) => { error!(service = %capability.service, %error, "TCP tunnel unavailable"); return; }
        },
    };
    let remote = match lease
        .open_tcp(&capability, HANDSHAKE_TIMEOUT, &shutdown)
        .await
    {
        Ok(stream) => stream,
        Err(error) => {
            if !matches!(error, SessionError::Closed | SessionError::Cancelled) {
                error!(service = %capability.service, %error, "TCP tunnel handshake failed");
            }
            return;
        }
    };
    if let Err(error) = relay_with_idle_timeout(local, remote).await {
        if error.downcast_ref::<TunnelIdleTimeout>().is_none() {
            lease.invalidate();
            // Do not forward arbitrary I/O error text into SDK diagnostics.
            error!(service = %capability.service, "TCP tunnel forwarding failed");
        }
    }
}
