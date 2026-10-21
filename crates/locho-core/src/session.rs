//! Owned peer sessions. The caller must poll [`SessionDriver::run`] on its
//! persistent executor; this module does not spawn tasks or create a runtime.

use crate::{auth, capability::Capability, protocol::*, service::ServiceType};
use futures_util::StreamExt;
use iroh::{endpoint::Connection, Endpoint, EndpointAddr};
use n0_future::time::{sleep, timeout};
use std::{fmt, sync::Arc, time::Duration};
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Starting,
    Connecting,
    Connected,
    Reconnecting,
    Failed,
    Closing,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportPath {
    Unknown,
    Direct,
    Relay,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorStage {
    Connect,
    Acquire,
    OpenStream,
    Handshake,
    Shutdown,
}

/// Sanitized categories; underlying transport text and credential input are not
/// exposed. A service status is the Locho handshake, not an application HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    Closed,
    Cancelled,
    Timeout(ErrorStage),
    Transport(ErrorStage),
    Protocol,
    ServiceRejected(u16),
    UnsupportedService,
    ResourceLimit,
    DriverStopped,
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("peer session is closed"),
            Self::Cancelled => f.write_str("operation cancelled"),
            Self::Timeout(stage) => write!(f, "peer operation timed out ({stage:?})"),
            Self::Transport(stage) => write!(f, "peer transport failed ({stage:?})"),
            Self::Protocol => f.write_str("invalid service handshake response"),
            Self::ServiceRejected(status) => {
                write!(f, "TCP attachment rejected with status {status}")
            }
            Self::UnsupportedService => f.write_str("TCP stream requires a TCP capability"),
            Self::ResourceLimit => f.write_str("peer stream limit reached"),
            Self::DriverStopped => f.write_str("peer session driver stopped unexpectedly"),
        }
    }
}
impl std::error::Error for SessionError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStatus {
    pub revision: u64,
    pub state: SessionState,
    pub path: TransportPath,
    /// Validated transport addresses, suitable for the native CLI's path display.
    pub path_description: String,
    pub last_error: Option<SessionError>,
    pub terminal_error: Option<SessionError>,
}

impl Default for SessionStatus {
    fn default() -> Self {
        Self {
            revision: 0,
            state: SessionState::Starting,
            path: TransportPath::Unknown,
            path_description: "none".into(),
            last_error: None,
            terminal_error: None,
        }
    }
}

#[derive(Clone)]
struct ActiveConnection {
    generation: u64,
    connection: Connection,
}
type Connections = watch::Sender<Option<ActiveConnection>>;

struct Owner {
    shutdown: CancellationToken,
    status: watch::Receiver<SessionStatus>,
    connections: Connections,
    streams: Arc<Semaphore>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// Clones share one owner. Dropping the last handle requests shutdown; status
/// receivers and leases do not keep the session owner alive.
#[derive(Clone)]
pub struct PeerSession {
    owner: Arc<Owner>,
}

/// Owns the endpoint exclusively. Do not pass an endpoint used by another session
/// or host: session shutdown closes it. Dropping the driver publishes terminal state.
pub struct SessionDriver {
    endpoint: Endpoint,
    target: EndpointAddr,
    connections: Connections,
    status: watch::Sender<SessionStatus>,
    shutdown: CancellationToken,
    completed: bool,
}

/// A generation-scoped lease. Invalidating an old lease cannot cancel a newer
/// connection. Native HTTP adapters may use the underlying connection directly.
#[derive(Clone)]
pub struct ConnectionLease {
    connection: Connection,
    generation: u64,
    connections: Connections,
    shutdown: CancellationToken,
    streams: Arc<Semaphore>,
}

impl PeerSession {
    /// Returns immediately without probing the service. Caller owns endpoint bind
    /// timeout/cleanup before this call, then must drive `driver.run()` persistently.
    pub fn start(endpoint: Endpoint, target: EndpointAddr) -> (Self, SessionDriver) {
        let (connections, _) = watch::channel(None);
        let (status, receiver) = watch::channel(SessionStatus::default());
        let shutdown = CancellationToken::new();
        let owner = Arc::new(Owner {
            shutdown: shutdown.clone(),
            status: receiver,
            connections: connections.clone(),
            streams: Arc::new(Semaphore::new(512)),
        });
        (
            Self { owner },
            SessionDriver {
                endpoint,
                target,
                connections,
                status,
                shutdown,
                completed: false,
            },
        )
    }

    pub fn snapshot(&self) -> SessionStatus {
        self.owner.status.borrow().clone()
    }

    /// Watch-style receiver, initialized to the latest snapshot. Updates coalesce;
    /// inspect the snapshot before waiting for `changed()` (including closed state).
    pub fn subscribe(&self) -> watch::Receiver<SessionStatus> {
        self.owner.status.clone()
    }

    pub async fn wait_connected(&self, deadline: Duration) -> Result<(), SessionError> {
        self.acquire(deadline).await.map(|_| ())
    }

    pub async fn acquire(&self, deadline: Duration) -> Result<ConnectionLease, SessionError> {
        let mut receiver = self.owner.connections.subscribe();
        let wait = async {
            loop {
                if self.owner.shutdown.is_cancelled() {
                    return Err(SessionError::Closed);
                }
                if let Some(active) = receiver.borrow_and_update().clone() {
                    return Ok(ConnectionLease {
                        connection: active.connection,
                        generation: active.generation,
                        connections: self.owner.connections.clone(),
                        shutdown: self.owner.shutdown.clone(),
                        streams: self.owner.streams.clone(),
                    });
                }
                receiver.changed().await.map_err(|_| SessionError::Closed)?;
            }
        };
        tokio::select! {
            biased;
            _ = self.owner.shutdown.cancelled() => Err(SessionError::Closed),
            result = timeout(deadline, wait) => result.map_err(|_| SessionError::Timeout(ErrorStage::Acquire))?,
        }
    }

    /// Acquisition plus the entire service handshake share the caller's deadline.
    pub async fn open_tcp(
        &self,
        capability: &Capability,
        deadline: Duration,
        cancel: &CancellationToken,
    ) -> Result<ServiceStream, SessionError> {
        if capability.service_type != ServiceType::Tcp {
            return Err(SessionError::UnsupportedService);
        }
        tokio::select! {
            biased;
            _ = self.owner.shutdown.cancelled() => Err(SessionError::Closed),
            _ = cancel.cancelled() => Err(SessionError::Cancelled),
            result = timeout(deadline, async {
                self.acquire(deadline).await?.open_tcp(capability, deadline, cancel).await
            }) => result.map_err(|_| SessionError::Timeout(ErrorStage::Handshake))?,
        }
    }

    /// Concurrent callers await the same terminal snapshot. Driver must be running
    /// for endpoint cleanup; `run` bounds cleanup with `SHUTDOWN_TIMEOUT`.
    pub async fn close(&self) -> Result<(), SessionError> {
        timeout(
            SHUTDOWN_TIMEOUT + Duration::from_secs(1),
            self.close_inner(),
        )
        .await
        .map_err(|_| SessionError::Timeout(ErrorStage::Shutdown))?
    }

    async fn close_inner(&self) -> Result<(), SessionError> {
        self.owner.shutdown.cancel();
        let mut status = self.subscribe();
        loop {
            let snapshot = status.borrow_and_update().clone();
            if snapshot.state == SessionState::Closed {
                return snapshot.terminal_error.map_or(Ok(()), Err);
            }
            status
                .changed()
                .await
                .map_err(|_| SessionError::DriverStopped)?;
        }
    }
}

impl ConnectionLease {
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn invalidate(&self) {
        self.connections.send_if_modified(|current| {
            if current
                .as_ref()
                .is_some_and(|active| active.generation == self.generation)
            {
                *current = None;
                true
            } else {
                false
            }
        });
    }

    pub async fn open_tcp(
        &self,
        capability: &Capability,
        deadline: Duration,
        cancel: &CancellationToken,
    ) -> Result<ServiceStream, SessionError> {
        if capability.service_type != ServiceType::Tcp {
            return Err(SessionError::UnsupportedService);
        }
        let permit = self
            .streams
            .clone()
            .try_acquire_owned()
            .map_err(|_| SessionError::ResourceLimit)?;
        let operation = async {
            let (send, recv) = self
                .connection
                .open_bi()
                .await
                .map_err(|_| SessionError::Transport(ErrorStage::OpenStream))?;
            let mut stream = ServiceStream {
                send,
                recv,
                send_finished: false,
                _permit: permit,
            };
            authorize_tcp(&mut stream.send, &mut stream.recv, capability).await?;
            Ok(stream)
        };
        tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => Err(SessionError::Closed),
            _ = cancel.cancelled() => Err(SessionError::Cancelled),
            result = timeout(deadline, operation) => {
                let result = result.map_err(|_| SessionError::Timeout(ErrorStage::Handshake))?;
                // STOP_SENDING during a handshake is a stream-local failure,
                // not evidence that unrelated services need reconnecting.
                if result.is_err() && self.connection.close_reason().is_some() {
                    self.invalidate();
                }
                result
            }
        }
    }
}

/// Authorized service I/O. Drop cancels unfinished I/O and releases capacity.
/// A gracefully shut down send half retains its buffered data for delivery.
/// Application code must not replay interrupted requests. This stream is neither
/// an HTTP response nor a proof of backend user authentication.
pub struct ServiceStream {
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
    send_finished: bool,
    _permit: OwnedSemaphorePermit,
}

impl tokio::io::AsyncRead for ServiceStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.recv).poll_read(cx, buf)
    }
}
impl tokio::io::AsyncWrite for ServiceStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        tokio::io::AsyncWrite::poll_write(std::pin::Pin::new(&mut self.send), cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        tokio::io::AsyncWrite::poll_flush(std::pin::Pin::new(&mut self.send), cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let result = tokio::io::AsyncWrite::poll_shutdown(std::pin::Pin::new(&mut self.send), cx);
        if matches!(result, std::task::Poll::Ready(Ok(()))) {
            self.send_finished = true;
        }
        result
    }
}
impl Drop for ServiceStream {
    fn drop(&mut self) {
        if !self.send_finished {
            let _ = self.send.reset(0u32.into());
        }
        let _ = self.recv.stop(0u32.into());
    }
}

async fn authorize_tcp<W: tokio::io::AsyncWrite + Unpin, R: tokio::io::AsyncRead + Unpin>(
    writer: &mut W,
    reader: &mut R,
    capability: &Capability,
) -> Result<(), SessionError> {
    write_json_head(
        writer,
        &StreamRequestHead::Tcp(TcpRequestHead {
            version: PROTOCOL_VERSION,
            service: capability.service.clone(),
            secret_proof: auth::secret_proof(&capability.secret),
        }),
    )
    .await
    .map_err(|_| SessionError::Transport(ErrorStage::Handshake))?;
    let response: LochoResponseHead = read_json_head(reader, MAX_HEAD_LEN)
        .await
        .map_err(|_| SessionError::Protocol)?;
    if response.version != PROTOCOL_VERSION || !(100..=599).contains(&response.status) {
        return Err(SessionError::Protocol);
    }
    if response.status != 200 {
        return Err(SessionError::ServiceRejected(response.status));
    }
    // Protocol v3 success has no body/headers: reject framing that could turn
    // handshake bytes into application data or hold the operation open.
    if response.body_len != Some(0) || !response.headers.is_empty() {
        return Err(SessionError::Protocol);
    }
    Ok(())
}

impl SessionDriver {
    fn update(&self, state: SessionState, error: Option<SessionError>) {
        self.status.send_modify(|status| {
            status.revision += 1;
            status.state = state;
            if error.is_some() || state == SessionState::Connected {
                status.last_error = error;
            }
            if state != SessionState::Connected {
                status.path = TransportPath::Unknown;
                status.path_description = "none".into();
            }
        });
    }

    fn path(&self, connection: &Connection) {
        let paths = connection.paths();
        let direct = paths.iter().any(|p| p.is_ip());
        let relay = paths.iter().any(|p| p.is_relay());
        let description = format_transport_paths(paths.iter().map(|p| {
            (
                p.remote_addr().to_string(),
                p.is_ip(),
                p.is_relay(),
                p.is_selected(),
            )
        }));
        self.status.send_if_modified(|status| {
            if status.path_description == description {
                return false;
            }
            status.revision += 1;
            status.path = match (direct, relay) {
                (true, true) => TransportPath::Mixed,
                (true, false) => TransportPath::Direct,
                (false, true) => TransportPath::Relay,
                _ => TransportPath::Unknown,
            };
            status.path_description = description;
            true
        });
    }

    pub async fn run(mut self) {
        self.supervise().await;
        self.update(SessionState::Closing, None);
        self.connections.send_replace(None);
        let cleanup = timeout(SHUTDOWN_TIMEOUT, self.endpoint.close()).await;
        self.status.send_modify(|status| {
            status.revision += 1;
            status.state = SessionState::Closed;
            if cleanup.is_err() {
                status.terminal_error = Some(SessionError::Timeout(ErrorStage::Shutdown));
            }
        });
        self.completed = true;
    }

    async fn supervise(&mut self) {
        let mut backoff = RECONNECT_INITIAL_BACKOFF;
        let mut recovering = false;
        let mut generation = 0;
        let mut invalidation = self.connections.subscribe();
        loop {
            if self.endpoint.is_closed() && !self.shutdown.is_cancelled() {
                self.update(SessionState::Failed, Some(SessionError::DriverStopped));
                self.status.send_modify(|status| {
                    status.revision += 1;
                    status.terminal_error = Some(SessionError::DriverStopped);
                });
                self.shutdown.cancel();
                break;
            }
            self.update(
                if recovering {
                    SessionState::Reconnecting
                } else {
                    SessionState::Connecting
                },
                None,
            );
            let attempt = tokio::select! {
                biased;
                _ = self.shutdown.cancelled() => break,
                result = timeout(HANDSHAKE_TIMEOUT, self.endpoint.connect(self.target.clone(), ALPN)) => result,
            };
            let connection = match attempt {
                Ok(Ok(connection)) => connection,
                failure => {
                    let error = if failure.is_err() {
                        SessionError::Timeout(ErrorStage::Connect)
                    } else {
                        SessionError::Transport(ErrorStage::Connect)
                    };
                    self.update(
                        if recovering {
                            SessionState::Reconnecting
                        } else {
                            SessionState::Connecting
                        },
                        Some(error),
                    );
                    if !self.retry(reconnect_delay(recovering, backoff)).await {
                        break;
                    }
                    if !recovering {
                        backoff = next_backoff(backoff);
                    }
                    continue;
                }
            };
            generation += 1;
            self.update(SessionState::Connected, None);
            self.path(&connection);
            self.connections.send_replace(Some(ActiveConnection {
                generation,
                connection: connection.clone(),
            }));
            invalidation.borrow_and_update();
            let mut paths = connection.paths_stream();
            let stable = sleep(RECONNECT_STABLE_DURATION);
            tokio::pin!(stable);
            let mut is_stable = false;
            loop {
                // A lease can invalidate between publication and the receiver's
                // borrow_and_update; inspect the value as well as notifications.
                if invalidation.borrow().is_none() {
                    break;
                }
                tokio::select! {
                    biased;
                    _ = self.shutdown.cancelled() => {
                        connection.close(0u32.into(), b"locho shutdown");
                        return;
                    }
                    _ = connection.closed() => break,
                    _ = &mut stable, if !is_stable => { backoff = RECONNECT_INITIAL_BACKOFF; is_stable = true; }
                    changed = invalidation.changed() => {
                        if changed.is_err() || invalidation.borrow().is_none() { break; }
                    }
                    update = paths.next() => {
                        let Some(_) = update else { break; };
                        // Path snapshots are advisory. Let QUIC recover from
                        // temporary path loss until it declares closure.
                        self.path(&connection);
                    }
                }
            }
            self.connections.send_replace(None);
            connection.close(0u32.into(), b"transport recovery");
            recovering = true;
            self.update(
                SessionState::Reconnecting,
                Some(SessionError::Transport(ErrorStage::Connect)),
            );
            if !self.retry(reconnect_delay(recovering, backoff)).await {
                break;
            }
        }
    }

    async fn retry(&self, delay: Duration) -> bool {
        tokio::select! { biased; _ = self.shutdown.cancelled() => false, _ = sleep(delay) => true }
    }
}

impl Drop for SessionDriver {
    fn drop(&mut self) {
        if !self.completed {
            self.shutdown.cancel();
            if let Some(active) = self.connections.send_replace(None) {
                active.connection.close(0u32.into(), b"driver stopped");
            }
            self.status.send_modify(|status| {
                status.revision += 1;
                status.state = SessionState::Closed;
                status.path = TransportPath::Unknown;
                status.path_description = "none".into();
                status.terminal_error = Some(SessionError::DriverStopped);
                status.last_error = Some(SessionError::DriverStopped);
            });
        }
    }
}

pub fn next_backoff(current: Duration) -> Duration {
    current
        .checked_mul(2)
        .unwrap_or(RECONNECT_MAX_BACKOFF)
        .min(RECONNECT_MAX_BACKOFF)
}
pub fn reconnect_delay(recovering: bool, backoff: Duration) -> Duration {
    if recovering {
        RECONNECT_INITIAL_BACKOFF
    } else {
        backoff
    }
}

/// Native CLI-compatible detailed path presentation. SDKs can use the typed path.
pub fn format_transport_paths<I: IntoIterator<Item = (String, bool, bool, bool)>>(
    paths: I,
) -> String {
    let mut direct = Vec::new();
    let mut relay = Vec::new();
    let mut selected = None;
    for (address, is_direct, is_relay, is_selected) in paths {
        let address = address
            .strip_prefix("ip:")
            .or_else(|| address.strip_prefix("relay:"))
            .unwrap_or(&address)
            .to_owned();
        if is_direct {
            direct.push(address.clone());
        }
        if is_relay {
            relay.push(address.clone());
        }
        if is_selected {
            selected = Some(address);
        }
    }
    match (direct.is_empty(), relay.is_empty()) {
        (false, true) => format!("direct({})", selected.unwrap_or_else(|| direct.join(", "))),
        (true, false) => format!("relay({})", selected.unwrap_or_else(|| relay.join(", "))),
        (false, false) => format!(
            "mixed(direct: {}, relay: {})",
            direct.join(", "),
            relay.join(", ")
        ),
        _ => "none".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability;
    use iroh::{endpoint::presets, RelayMode};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn endpoint(server: bool) -> Endpoint {
        let mut builder = Endpoint::builder(presets::N0).relay_mode(RelayMode::Disabled);
        if server {
            builder = builder.alpns(vec![ALPN.to_vec()]);
        }
        builder.bind().await.unwrap()
    }

    fn address(endpoint: &Endpoint) -> EndpointAddr {
        let port = endpoint
            .bound_sockets()
            .into_iter()
            .find(|a| a.is_ipv4())
            .unwrap()
            .port();
        EndpointAddr::new(endpoint.id()).with_ip_addr(([127, 0, 0, 1], port).into())
    }

    async fn connected() -> (
        PeerSession,
        tokio::task::JoinHandle<()>,
        Endpoint,
        Connection,
    ) {
        let server = endpoint(true).await;
        connected_to(server).await
    }

    async fn connected_to(
        server: Endpoint,
    ) -> (
        PeerSession,
        tokio::task::JoinHandle<()>,
        Endpoint,
        Connection,
    ) {
        let (session, driver) = PeerSession::start(endpoint(false).await, address(&server));
        let task = tokio::spawn(driver.run());
        let connection = timeout(Duration::from_secs(5), async {
            server.accept().await.unwrap().await.unwrap()
        })
        .await
        .unwrap();
        session
            .wait_connected(Duration::from_secs(5))
            .await
            .unwrap();
        (session, task, server, connection)
    }

    async fn response(connection: Connection, status: u16, version: u8, body_len: Option<u64>) {
        let (mut send, mut recv) = connection.accept_bi().await.unwrap();
        let request: StreamRequestHead = read_json_head(&mut recv, MAX_HEAD_LEN).await.unwrap();
        assert!(matches!(request, StreamRequestHead::Tcp(_)));
        write_json_head(
            &mut send,
            &LochoResponseHead {
                version,
                status,
                headers: vec![],
                body_len,
            },
        )
        .await
        .unwrap();
        send.finish().unwrap();
        let _ = send.stopped().await;
    }

    #[tokio::test]
    async fn stopped_handshake_write_preserves_existing_stream() {
        let server = Endpoint::builder(presets::N0)
            .relay_mode(RelayMode::Disabled)
            .alpns(vec![ALPN.to_vec()])
            .transport_config(
                iroh::endpoint::QuicTransportConfig::builder()
                    .stream_receive_window(4u32.into())
                    .build(),
            )
            .bind()
            .await
            .unwrap();
        let (session, driver, server, connection) = connected_to(server).await;
        let capability = capability::parse("api:tcp:private-secret").unwrap();
        let peer = connection.clone();
        let serving = tokio::spawn(async move {
            let (mut send, mut recv) = peer.accept_bi().await.unwrap();
            let _: StreamRequestHead = read_json_head(&mut recv, MAX_HEAD_LEN).await.unwrap();
            write_json_head(
                &mut send,
                &LochoResponseHead {
                    version: PROTOCOL_VERSION,
                    status: 200,
                    headers: vec![],
                    body_len: Some(0),
                },
            )
            .await
            .unwrap();
            let (mut rejected_send, mut rejected_recv) = peer.accept_bi().await.unwrap();
            // The four-byte receive window blocks the handshake JSON write.
            rejected_recv.stop(1u32.into()).unwrap();
            rejected_send.reset(1u32.into()).unwrap();
            let mut ping = [0u8; 4];
            recv.read_exact(&mut ping).await.unwrap();
            assert_eq!(&ping, b"ping");
            send.write_all(b"pong").await.unwrap();
            send.finish().unwrap();
            send.stopped().await.unwrap();
        });
        let mut existing = session
            .open_tcp(
                &capability,
                Duration::from_secs(5),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let error = session
            .open_tcp(
                &capability,
                Duration::from_secs(5),
                &CancellationToken::new(),
            )
            .await
            .err();
        assert_eq!(error, Some(SessionError::Transport(ErrorStage::Handshake)));
        // Even before the driver gets scheduled, the active lease must remain.
        assert!(session.owner.connections.borrow().is_some());
        assert!(connection.close_reason().is_none());
        existing.write_all(b"ping").await.unwrap();
        let mut pong = [0u8; 4];
        existing.read_exact(&mut pong).await.unwrap();
        assert_eq!(&pong, b"pong");
        serving.await.unwrap();
        drop(existing);
        session.close().await.unwrap();
        driver.await.unwrap();
        server.close().await;
    }

    #[tokio::test]
    async fn graceful_shutdown_delivers_buffered_data_after_drop() {
        let (session, driver, server, connection) = connected().await;
        let capability = capability::parse("api:tcp:private-secret").unwrap();
        let payload = vec![42u8; 1024 * 1024];
        let expected = payload.clone();
        let (release, released) = tokio::sync::oneshot::channel();
        let serving = tokio::spawn(async move {
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            let _: StreamRequestHead = read_json_head(&mut recv, MAX_HEAD_LEN).await.unwrap();
            write_json_head(
                &mut send,
                &LochoResponseHead {
                    version: PROTOCOL_VERSION,
                    status: 200,
                    headers: vec![],
                    body_len: Some(0),
                },
            )
            .await
            .unwrap();
            send.finish().unwrap();
            released.await.unwrap();
            let received = recv.read_to_end(expected.len()).await.unwrap();
            assert_eq!(received, expected);
        });
        let mut stream = session
            .open_tcp(
                &capability,
                Duration::from_secs(5),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        // The remote half closes first, as it can during bidirectional relay.
        let mut byte = [0u8; 1];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
        stream.write_all(&payload).await.unwrap();
        stream.shutdown().await.unwrap();
        drop(stream);
        release.send(()).unwrap();
        timeout(Duration::from_secs(5), serving)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.owner.streams.available_permits(), 512);
        session.close().await.unwrap();
        driver.await.unwrap();
        server.close().await;
    }

    #[tokio::test]
    async fn service_rejection_does_not_invalidate_peer_and_stream_drop_releases_capacity() {
        let (session, driver, server, connection) = connected().await;
        let capability = capability::parse("api:tcp:private-secret").unwrap();
        let rejected = tokio::spawn(response(connection.clone(), 403, 3, Some(0)));
        let error = session
            .open_tcp(
                &capability,
                Duration::from_secs(5),
                &CancellationToken::new(),
            )
            .await
            .err()
            .unwrap();
        assert_eq!(error, SessionError::ServiceRejected(403));
        rejected.await.unwrap();
        assert_eq!(session.snapshot().state, SessionState::Connected);
        let accepted = tokio::spawn(async move {
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            let _: StreamRequestHead = read_json_head(&mut recv, MAX_HEAD_LEN).await.unwrap();
            write_json_head(
                &mut send,
                &LochoResponseHead {
                    version: 3,
                    status: 200,
                    headers: vec![],
                    body_len: Some(0),
                },
            )
            .await
            .unwrap();
            let mut request = [0; 3];
            recv.read_exact(&mut request).await.unwrap();
            assert_eq!(&request, b"GET");
            send.write_all(b"OK").await.unwrap();
            send.finish().unwrap();
            let _ = send.stopped().await;
        });
        let mut stream = session
            .open_tcp(
                &capability,
                Duration::from_secs(5),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        stream.write_all(b"GET").await.unwrap();
        let mut bytes = [0; 2];
        stream.read_exact(&mut bytes).await.unwrap();
        assert_eq!(&bytes, b"OK");
        drop(stream);
        accepted.await.unwrap();
        assert_eq!(session.owner.streams.available_permits(), 512);
        let (first, second) = tokio::join!(session.close(), session.close());
        first.unwrap();
        second.unwrap();
        session.close().await.unwrap();
        assert_eq!(session.snapshot().state, SessionState::Closed);
        assert_eq!(
            session.wait_connected(Duration::ZERO).await,
            Err(SessionError::Closed)
        );
        driver.await.unwrap();
        server.close().await;
    }

    #[tokio::test]
    async fn handshake_rejects_wrong_version_and_body_framing() {
        let (session, driver, server, connection) = connected().await;
        let capability = capability::parse("api:tcp:private-secret").unwrap();
        for (version, body_len) in [(2, Some(0)), (3, None), (3, Some(1))] {
            let reply = tokio::spawn(response(connection.clone(), 200, version, body_len));
            let result = session
                .open_tcp(
                    &capability,
                    Duration::from_secs(5),
                    &CancellationToken::new(),
                )
                .await;
            assert_eq!(result.err(), Some(SessionError::Protocol));
            reply.await.unwrap();
        }
        session.close().await.unwrap();
        driver.await.unwrap();
        server.close().await;
    }

    #[tokio::test]
    async fn stalled_handshake_timeout_cancellation_and_close_are_isolated() {
        let (session, driver, server, connection) = connected().await;
        let capability = capability::parse("api:tcp:private-secret").unwrap();
        let stalled = tokio::spawn(async move {
            let mut streams = Vec::new();
            for _ in 0..3 {
                streams.push(connection.accept_bi().await.unwrap());
            }
            std::future::pending::<()>().await;
        });
        let error = session
            .open_tcp(
                &capability,
                Duration::from_millis(50),
                &CancellationToken::new(),
            )
            .await
            .err();
        assert_eq!(error, Some(SessionError::Timeout(ErrorStage::Handshake)));
        let cancel = CancellationToken::new();
        let operation = session.open_tcp(&capability, Duration::from_secs(5), &cancel);
        let cancellation = async {
            sleep(Duration::from_millis(20)).await;
            cancel.cancel();
        };
        let (result, _) = tokio::join!(operation, cancellation);
        assert_eq!(result.err(), Some(SessionError::Cancelled));
        assert_eq!(session.snapshot().state, SessionState::Connected);
        let cancel = CancellationToken::new();
        let operation = session.open_tcp(&capability, Duration::from_secs(5), &cancel);
        let closing = async {
            sleep(Duration::from_millis(20)).await;
            session.close().await.unwrap();
        };
        let (result, _) = tokio::join!(operation, closing);
        assert_eq!(result.err(), Some(SessionError::Closed));
        assert_eq!(session.owner.streams.available_permits(), 512);
        stalled.abort();
        let _ = stalled.await;
        driver.await.unwrap();
        server.close().await;
    }

    #[tokio::test]
    async fn stale_lease_cannot_invalidate_recovered_connection() {
        let (session, driver, server, connection) = connected().await;
        let old = session.acquire(Duration::from_secs(1)).await.unwrap();
        connection.close(0u32.into(), b"test recovery");
        let replacement = timeout(Duration::from_secs(5), async {
            server.accept().await.unwrap().await.unwrap()
        })
        .await
        .unwrap();
        let fresh = session.acquire(Duration::from_secs(5)).await.unwrap();
        assert!(fresh.generation > old.generation);
        old.invalidate();
        assert_eq!(
            session
                .owner
                .connections
                .borrow()
                .as_ref()
                .unwrap()
                .generation,
            fresh.generation
        );
        session.close().await.unwrap();
        replacement.closed().await;
        driver.await.unwrap();
        server.close().await;
    }

    #[tokio::test]
    async fn driver_drop_retains_terminal_cause_for_late_subscribers() {
        let target = endpoint(true).await;
        let (session, driver) = PeerSession::start(endpoint(false).await, address(&target));
        assert_eq!(
            session.wait_connected(Duration::from_millis(5)).await,
            Err(SessionError::Timeout(ErrorStage::Acquire))
        );
        // A wait timeout leaves caller-owned resources running.
        assert_eq!(session.snapshot().state, SessionState::Starting);
        drop(driver);
        let status = session.subscribe().borrow().clone();
        assert_eq!(status.state, SessionState::Closed);
        assert_eq!(status.terminal_error, Some(SessionError::DriverStopped));
        assert_eq!(session.close().await, Err(SessionError::DriverStopped));
        target.close().await;
    }

    #[tokio::test]
    async fn dropping_owner_closes_driver_but_subscription_does_not() {
        let (session, driver, server, connection) = connected().await;
        let subscription = session.subscribe();
        drop(subscription);
        assert_eq!(session.snapshot().state, SessionState::Connected);
        let mut late = session.subscribe();
        drop(session);
        timeout(Duration::from_secs(5), driver)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(late.borrow_and_update().state, SessionState::Closed);
        connection.closed().await;
        server.close().await;
    }

    #[tokio::test]
    async fn close_interrupts_connection_attempt_and_backoff() {
        let target = endpoint(true).await;
        let address = address(&target);
        target.close().await;
        let (session, driver) = PeerSession::start(endpoint(false).await, address);
        let task = tokio::spawn(driver.run());
        sleep(Duration::from_millis(20)).await;
        timeout(Duration::from_secs(5), session.close())
            .await
            .unwrap()
            .unwrap();
        task.await.unwrap();
        assert_eq!(session.snapshot().state, SessionState::Closed);
    }

    #[tokio::test]
    async fn independent_sessions_status_revisions_and_limits() {
        let (first, first_driver, first_server, _first_connection) = connected().await;
        let (second, second_driver, second_server, _second_connection) = connected().await;
        let initial = second.snapshot();
        let permit = second
            .owner
            .streams
            .clone()
            .acquire_many_owned(512)
            .await
            .unwrap();
        let capability = capability::parse("api:tcp:private-secret").unwrap();
        assert_eq!(
            second
                .open_tcp(
                    &capability,
                    Duration::from_secs(1),
                    &CancellationToken::new()
                )
                .await
                .err(),
            Some(SessionError::ResourceLimit)
        );
        first.close().await.unwrap();
        first_driver.await.unwrap();
        first_server.close().await;
        assert_eq!(second.snapshot().state, SessionState::Connected);
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            second
                .open_tcp(&capability, Duration::from_secs(1), &cancel)
                .await
                .err(),
            Some(SessionError::Cancelled)
        );
        drop(permit);
        second.close().await.unwrap();
        second_driver.await.unwrap();
        second_server.close().await;
        let late = second.subscribe().borrow().clone();
        assert!(late.revision > initial.revision);
        assert_eq!(late.state, SessionState::Closed);
        assert_eq!(late.path, TransportPath::Unknown);
    }

    #[tokio::test]
    async fn handshake_deadline_includes_blocked_request_write() {
        let (mut writer, _blocked_reader) = tokio::io::duplex(1);
        let (_silent_writer, mut reader) = tokio::io::duplex(1);
        let capability = capability::parse("api:tcp:private-secret").unwrap();
        assert!(timeout(
            Duration::from_millis(10),
            authorize_tcp(&mut writer, &mut reader, &capability)
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn cancelled_backoff_returns_immediately_and_failed_driver_keeps_cause() {
        let target = endpoint(true).await;
        let client = endpoint(false).await;
        let (session, driver) = PeerSession::start(client.clone(), address(&target));
        driver.shutdown.cancel();
        assert!(!timeout(
            Duration::from_millis(100),
            driver.retry(RECONNECT_MAX_BACKOFF)
        )
        .await
        .unwrap());
        drop(driver);
        client.close().await;
        assert_eq!(
            session.snapshot().terminal_error,
            Some(SessionError::DriverStopped)
        );

        let client = endpoint(false).await;
        client.close().await;
        let (session, driver) = PeerSession::start(client, address(&target));
        driver.run().await;
        assert_eq!(session.close().await, Err(SessionError::DriverStopped));
        assert_eq!(
            session.snapshot().last_error,
            Some(SessionError::DriverStopped)
        );
        target.close().await;
    }
}
