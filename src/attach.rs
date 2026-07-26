use crate::{auth, http_utils, protocol::*};
use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use futures_core::Stream;
use futures_util::StreamExt;
use http::{Response, StatusCode};
use http_body_util::{BodyExt, StreamBody};
use hyper::{
    body::{Body, Frame, Incoming},
    server::conn::http1,
    service::service_fn,
    Request,
};
use hyper_util::rt::TokioIo;
use iroh::NodeAddr;
use iroh::{
    endpoint::{Connection, ConnectionType},
    Endpoint, NodeId,
};
use std::net::SocketAddr;
use std::{convert::Infallible, fmt, io::Write, pin::Pin};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Semaphore};
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

type HttpStream = Pin<Box<dyn Stream<Item = Result<Frame<Bytes>, anyhow::Error>> + Send>>;
type HttpResponse = Response<StreamBody<HttpStream>>;

#[derive(Clone)]
struct ActiveConnection {
    generation: u64,
    connection: Connection,
}

type ConnectionReceiver = watch::Receiver<Option<ActiveConnection>>;

#[derive(Clone)]
struct ConnectionState {
    receiver: ConnectionReceiver,
    sender: watch::Sender<Option<ActiveConnection>>,
}

#[derive(Clone)]
struct ConnectionLease {
    connection: Connection,
    generation: u64,
    sender: watch::Sender<Option<ActiveConnection>>,
}

struct HttpBodyLeaseGuard {
    lease: Option<ConnectionLease>,
}

#[derive(Debug)]
struct TunnelUnavailable;

impl fmt::Display for TunnelUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("tunnel unavailable while reconnecting")
    }
}

impl std::error::Error for TunnelUnavailable {}

#[derive(Debug)]
enum TransportMonitorExit {
    PathLost,
    Ended,
}

impl ConnectionLease {
    fn invalidate(&self) {
        let _ = self.sender.send_if_modified(|current| {
            if current
                .as_ref()
                .is_some_and(|connection| connection.generation == self.generation)
            {
                *current = None;
                true
            } else {
                false
            }
        });
    }
}

impl Drop for HttpBodyLeaseGuard {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            lease.invalidate();
        }
    }
}

impl HttpBodyLeaseGuard {
    fn invalidate(&self) {
        if let Some(lease) = &self.lease {
            lease.invalidate();
        }
    }

    fn complete(&mut self) {
        self.lease = None;
    }
}

pub async fn run(
    host_id: String,
    service: String,
    secret: String,
    direct_address: Option<SocketAddr>,
    tcp: bool,
    listen: SocketAddr,
) -> Result<()> {
    if service.is_empty() {
        return Err(anyhow!("service name cannot be empty"));
    }
    let node_id: NodeId = host_id.parse().context("invalid host ID")?;
    #[cfg(feature = "integration-test")]
    let direct_address = match direct_address {
        Some(address) => Some(address),
        None => std::env::var_os("LOCHO_TEST_DIRECT_ADDR")
            .map(|address| address.to_string_lossy().parse())
            .transpose()
            .context("invalid LOCHO_TEST_DIRECT_ADDR")?,
    };
    let endpoint = Endpoint::builder().discovery_n0().bind().await?;
    if let Some(address) = direct_address {
        endpoint.add_node_addr(NodeAddr::new(node_id).with_direct_addresses([address]))?;
    }
    let listener = TcpListener::bind(listen).await?;
    let (connection_sender, connection_receiver) = watch::channel(None);
    let connection_state = ConnectionState {
        receiver: connection_receiver,
        sender: connection_sender.clone(),
    };
    let supervisor_receiver = connection_state.receiver.clone();
    let shutdown = CancellationToken::new();
    let supervisor = tokio::spawn(connection_supervisor(
        endpoint.clone(),
        node_id,
        connection_sender,
        supervisor_receiver,
        shutdown.clone(),
    ));
    let result = if tcp {
        run_tcp_listener(
            listener,
            connection_state,
            service,
            secret,
            shutdown.clone(),
        )
        .await
    } else {
        println!(
            "locho attached\n\nService: {}\nLocal proxy:\nhttp://{}\n\nTry:\ncurl http://{}/",
            service, listen, listen
        );
        std::io::stdout().flush()?;
        info!(%listen, "local proxy listening");
        run_http_listener(
            listener,
            connection_state,
            service,
            secret,
            shutdown.clone(),
        )
        .await
    };
    shutdown.cancel();
    let _ = supervisor.await;
    endpoint.close().await;
    result
}

async fn connection_supervisor(
    endpoint: Endpoint,
    node_id: NodeId,
    sender: watch::Sender<Option<ActiveConnection>>,
    mut receiver: ConnectionReceiver,
    shutdown: CancellationToken,
) {
    let mut backoff = RECONNECT_INITIAL_BACKOFF;
    let mut generation = 0;
    'supervisor: loop {
        let connection = tokio::select! {
            _ = shutdown.cancelled() => break,
            result = timeout(HANDSHAKE_TIMEOUT, endpoint.connect(node_id, ALPN)) => {
                match result {
                    Ok(Ok(connection)) => connection,
                    Ok(Err(error)) => {
                        warn!(%error, "tunnel connection failed; retrying");
                        retry_delay(&shutdown, backoff).await;
                        backoff = next_backoff(backoff);
                        continue;
                    }
                    Err(_) => {
                        warn!("tunnel connection timed out; retrying");
                        retry_delay(&shutdown, backoff).await;
                        backoff = next_backoff(backoff);
                        continue;
                    }
                }
            }
        };

        generation += 1;
        let _ = sender.send(Some(ActiveConnection {
            generation,
            connection: connection.clone(),
        }));
        receiver.borrow_and_update();
        let mut monitor = spawn_transport_monitor(&endpoint, node_id, generation);
        let monitor_finished = async {
            if let Some(monitor) = monitor.as_mut() {
                monitor.await.unwrap_or(TransportMonitorExit::Ended)
            } else {
                std::future::pending::<TransportMonitorExit>().await
            }
        };
        tokio::pin!(monitor_finished);
        let stable = sleep(RECONNECT_STABLE_DURATION);
        tokio::pin!(stable);
        let mut connection_stable = false;
        loop {
            tokio::select! {
                _ = &mut stable, if !connection_stable => {
                    backoff = RECONNECT_INITIAL_BACKOFF;
                    connection_stable = true;
                }
                _ = connection.closed() => {
                    info!("tunnel connection closed; reconnecting");
                    let _ = sender.send(None);
                    break;
                }
                monitor_exit = &mut monitor_finished => {
                    info!(?monitor_exit, "tunnel transport monitor ended; reconnecting");
                    let _ = sender.send(None);
                    connection.close(0u32.into(), b"transport monitor ended");
                    break;
                }
                changed = receiver.changed() => {
                    if changed.is_ok() && receiver.borrow().is_none() {
                        info!("tunnel connection invalidated; reconnecting");
                        connection.close(0u32.into(), b"tunnel connection invalidated");
                        break;
                    }
                }
                _ = shutdown.cancelled() => {
                    connection.close(0u32.into(), b"locho shutdown");
                    if let Some(monitor) = monitor {
                        monitor.abort();
                        let _ = monitor.await;
                    }
                    break 'supervisor;
                }
            }
        }
        if let Some(monitor) = monitor {
            monitor.abort();
            let _ = monitor.await;
        }
        retry_delay(&shutdown, backoff).await;
        backoff = next_backoff(backoff);
    }
    let _ = sender.send(None);
}

async fn retry_delay(shutdown: &CancellationToken, delay: std::time::Duration) {
    tokio::select! {
        _ = sleep(delay) => {}
        _ = shutdown.cancelled() => {}
    }
}

fn next_backoff(current: std::time::Duration) -> std::time::Duration {
    current
        .checked_mul(2)
        .unwrap_or(RECONNECT_MAX_BACKOFF)
        .min(RECONNECT_MAX_BACKOFF)
}

fn spawn_transport_monitor(
    endpoint: &Endpoint,
    node_id: NodeId,
    generation: u64,
) -> Option<tokio::task::JoinHandle<TransportMonitorExit>> {
    let watcher = match endpoint.conn_type(node_id) {
        Ok(watcher) => watcher,
        Err(_) => {
            warn!("connected to host but transport path is not yet available");
            return None;
        }
    };
    let initial_path = watcher.get().ok();
    if let Some(connection_type) = &initial_path {
        info!(generation, transport_path = %connection_type, "transport path established");
        println!("transport path: {connection_type}");
    }
    Some(tokio::spawn(async move {
        let mut paths = watcher.stream();
        let mut last_path = initial_path;
        while let Some(connection_type) = paths.next().await {
            if !transport_path_changed(last_path.as_ref(), &connection_type) {
                continue;
            }
            info!(generation, transport_path = %connection_type, "transport path changed");
            println!("transport path: {connection_type}");
            if matches!(connection_type, ConnectionType::None) {
                return TransportMonitorExit::PathLost;
            }
            last_path = Some(connection_type);
        }
        TransportMonitorExit::Ended
    }))
}

fn transport_path_changed(previous: Option<&ConnectionType>, current: &ConnectionType) -> bool {
    previous != Some(current)
}

async fn run_http_listener(
    listener: TcpListener,
    connection: ConnectionState,
    service: String,
    secret: String,
    shutdown: CancellationToken,
) -> Result<()> {
    let http_connections = std::sync::Arc::new(Semaphore::new(MAX_HTTP_CONNECTIONS));
    let mut clients = JoinSet::new();
    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, peer) = result?;
                let connection = connection.clone();
                let service_name = service.clone();
                let secret = secret.clone();
                let permit = match http_connections.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        drop(stream);
                        error!(?peer, "HTTP connection limit reached");
                        continue;
                    }
                };
                clients.spawn(async move {
                    let _permit = permit;
                    let service = service_fn(move |request| {
                        let connection = connection.clone();
                        let service = service_name.clone();
                        let secret = secret.clone();
                        async move {
                            Ok::<_, Infallible>(handle_request(request, connection, service, secret).await)
                        }
                    });
                    http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await
                    .map_err(anyhow::Error::from)
                });
            }
            signal = tokio::signal::ctrl_c() => {
                if signal.is_ok() {
                    warn!("shutdown requested");
                    shutdown.cancel();
                }
                break;
            }
            _ = shutdown.cancelled() => break,
            completed = clients.join_next(), if !clients.is_empty() => {
                if let Some(result) = completed {
                    crate::task::log_result(result, "HTTP client");
                }
                crate::task::reap_finished_results(&mut clients, "HTTP client");
            }
        }
    }
    shutdown.cancel();
    if timeout(SHUTDOWN_TIMEOUT, async {
        while clients.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        warn!("shutdown deadline reached; aborting active local connections");
        clients.abort_all();
    }
    Ok(())
}

async fn run_tcp_listener(
    listener: TcpListener,
    connection: ConnectionState,
    service: String,
    secret: String,
    shutdown: CancellationToken,
) -> Result<()> {
    let tcp_connections = std::sync::Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS));
    println!(
        "locho attached\n\nService: {}\nLocal TCP listener: {}",
        service,
        listener.local_addr()?
    );
    std::io::stdout().flush()?;
    let mut clients = JoinSet::new();
    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, peer) = result?;
                let connection = connection.clone();
                let service = service.clone();
                let secret = secret.clone();
                let permit = match tcp_connections.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        drop(stream);
                        error!(?peer, "TCP connection limit reached");
                        continue;
                    }
                };
                clients.spawn(async move {
                    let _permit = permit;
                    handle_tcp_connection(stream, connection, service, secret).await
                });
            }
            signal = tokio::signal::ctrl_c() => {
                if signal.is_ok() {
                    warn!("shutdown requested");
                    shutdown.cancel();
                }
                break;
            }
            _ = shutdown.cancelled() => break,
            completed = clients.join_next(), if !clients.is_empty() => {
                if let Some(result) = completed {
                    crate::task::log_result(result, "TCP client");
                }
                crate::task::reap_finished_results(&mut clients, "TCP client");
            }
        }
    }
    shutdown.cancel();
    if timeout(SHUTDOWN_TIMEOUT, async {
        while clients.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        warn!("shutdown deadline reached; aborting active local TCP connections");
        clients.abort_all();
    }
    Ok(())
}

async fn acquire_connection(state: ConnectionState) -> Result<ConnectionLease> {
    let active = match timeout(
        ATTACH_RECONNECT_TIMEOUT,
        wait_for_connection(state.receiver),
    )
    .await
    {
        Ok(active) => active?,
        Err(_) => return Err(anyhow::Error::new(TunnelUnavailable)),
    };
    Ok(ConnectionLease {
        connection: active.connection,
        generation: active.generation,
        sender: state.sender,
    })
}

async fn wait_for_connection(mut receiver: ConnectionReceiver) -> Result<ActiveConnection> {
    loop {
        if let Some(connection) = receiver.borrow().clone() {
            return Ok(connection);
        }
        receiver
            .changed()
            .await
            .context("connection supervisor stopped")?;
    }
}

async fn handle_tcp_connection(
    local: TcpStream,
    connection: ConnectionState,
    service: String,
    secret: String,
) -> Result<()> {
    let lease = acquire_connection(connection).await?;
    let result = handle_tcp_connection_on_lease(&lease, local, service, secret).await;
    if result.is_err() {
        lease.invalidate();
    }
    result
}

async fn handle_tcp_connection_on_lease(
    lease: &ConnectionLease,
    local: TcpStream,
    service: String,
    secret: String,
) -> Result<()> {
    let (mut writer, mut reader) = lease.connection.open_bi().await?;
    write_json_head(
        &mut writer,
        &StreamRequestHead::Tcp(TcpRequestHead {
            version: PROTOCOL_VERSION,
            service,
            secret_proof: auth::secret_proof(&secret),
        }),
    )
    .await?;
    let response: LochoResponseHead =
        timeout(HANDSHAKE_TIMEOUT, read_json_head(&mut reader, MAX_HEAD_LEN))
            .await
            .context("TCP attachment handshake timed out")??;
    if response.status != 200 {
        return Err(anyhow!(
            "TCP attachment rejected with status {}",
            response.status
        ));
    }
    read_body_with_limit(&mut reader, response.body_len, MAX_BODY_LEN).await?;
    let remote = tokio::io::join(reader, writer);
    relay_with_idle_timeout(local, remote).await?;
    Ok(())
}

async fn handle_request(
    request: Request<Incoming>,
    connection: ConnectionState,
    service: String,
    secret: String,
) -> HttpResponse {
    let method = request.method().clone();
    let path = request
        .uri()
        .path_and_query()
        .map(|v| v.as_str())
        .unwrap_or("/")
        .to_string();
    info!(%method, path = %path, "local request");
    if !http_utils::is_supported_method(&method) {
        return error_response(StatusCode::METHOD_NOT_ALLOWED);
    }
    let headers = http_utils::headers_to_pairs(request.headers());
    match tunnel_request(
        connection,
        service,
        secret,
        method,
        path,
        headers,
        request.into_body(),
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            error!(%error, "tunnel request failed");
            if error.downcast_ref::<TunnelUnavailable>().is_some() {
                error_response(StatusCode::SERVICE_UNAVAILABLE)
            } else if error.to_string().contains("403") {
                error_response(StatusCode::FORBIDDEN)
            } else if error.to_string().contains("501") {
                error_response(StatusCode::NOT_IMPLEMENTED)
            } else if error.to_string().contains("body exceeds limit") {
                error_response(StatusCode::PAYLOAD_TOO_LARGE)
            } else {
                error_response(StatusCode::BAD_GATEWAY)
            }
        }
    }
}

async fn tunnel_request(
    connection: ConnectionState,
    service: String,
    secret: String,
    method: http::Method,
    path: String,
    headers: Vec<(String, String)>,
    body: Incoming,
) -> Result<HttpResponse> {
    let lease = acquire_connection(connection).await?;
    let result =
        tunnel_request_on_lease(&lease, service, secret, method, path, headers, body).await;
    if result.is_err() {
        lease.invalidate();
    }
    result
}

async fn tunnel_request_on_lease(
    lease: &ConnectionLease,
    service: String,
    secret: String,
    method: http::Method,
    path: String,
    headers: Vec<(String, String)>,
    body: Incoming,
) -> Result<HttpResponse> {
    let (mut writer, mut reader) = timeout(HANDSHAKE_TIMEOUT, lease.connection.open_bi())
        .await
        .context("HTTP tunnel stream open timed out")??;
    let head = LochoRequestHead {
        version: PROTOCOL_VERSION,
        service,
        secret_proof: auth::secret_proof(&secret),
        method: method.to_string(),
        path_and_query: path,
        headers,
        body_len: body.size_hint().exact(),
    };
    let body_len = head.body_len;
    if body_len.is_some_and(|len| len > MAX_BODY_LEN as u64) {
        return Err(anyhow!("request body exceeds limit"));
    }
    write_json_head(&mut writer, &StreamRequestHead::Http(head)).await?;
    let mut body = body;
    if let Some(body_len) = body_len {
        let mut written = 0u64;
        while let Some(chunk) = body.frame().await {
            let frame = chunk?
                .into_data()
                .map_err(|_| anyhow!("request body contains trailers"))?;
            let frame_len = frame.len() as u64;
            if written + frame_len > body_len {
                return Err(anyhow!("request body exceeds declared length"));
            }
            for chunk in frame.chunks(BODY_CHUNK_LEN) {
                write_body(&mut writer, chunk).await?;
            }
            written += frame_len;
        }
        if written != body_len {
            return Err(anyhow!("request body length changed during upload"));
        }
    } else {
        let mut written = 0usize;
        while let Some(chunk) = body.frame().await {
            let frame = chunk?
                .into_data()
                .map_err(|_| anyhow!("request body contains trailers"))?;
            written += frame.len();
            if written > MAX_BODY_LEN {
                return Err(anyhow!("request body exceeds limit"));
            }
            for chunk in frame.chunks(BODY_CHUNK_LEN) {
                write_body_chunk(&mut writer, chunk).await?;
            }
        }
        write_body_end(&mut writer).await?;
    }
    let response: LochoResponseHead = timeout(
        HTTP_REQUEST_TIMEOUT + HANDSHAKE_TIMEOUT,
        read_json_head(&mut reader, MAX_HEAD_LEN),
    )
    .await
    .context("HTTP attachment handshake timed out")??;
    if response.version != PROTOCOL_VERSION {
        return Err(anyhow!(
            "unsupported tunnel response version {}",
            response.version
        ));
    }
    let status =
        StatusCode::from_u16(response.status).map_err(|_| anyhow!("invalid response status"))?;
    let body_len = response.body_len;
    let mut body_guard = HttpBodyLeaseGuard {
        lease: Some(lease.clone()),
    };
    let stream = Box::pin(async_stream::try_stream! {
        if let Some(length) = body_len {
            let mut remaining = length;
            let mut buffer = vec![0u8; BODY_CHUNK_LEN];
            while remaining > 0 {
                let count = remaining.min(BODY_CHUNK_LEN as u64) as usize;
                timeout(
                    http_response_body_timeout(),
                    reader.read_exact(&mut buffer[..count]),
                )
                    .await
                    .context("HTTP response body read timed out")?
                    .inspect_err(|_| body_guard.invalidate())?;
                yield Frame::data(Bytes::copy_from_slice(&buffer[..count]));
                remaining -= count as u64;
            }
        } else {
            loop {
                let chunk = timeout(http_response_body_timeout(), read_body_chunk(&mut reader))
                    .await
                    .context("HTTP response body read timed out")?
                    .inspect_err(|_| body_guard.invalidate())?;
                let Some(chunk) = chunk else { break };
                yield Frame::data(chunk);
            }
        }
        body_guard.complete();
    }) as HttpStream;
    info!(status = %status, "local response");
    let mut output = Response::builder()
        .status(status)
        .body(StreamBody::new(stream))?;
    for (name, value) in http_utils::pairs_to_headers(response.headers).iter() {
        if !http_utils::is_hop_by_hop_header(name) {
            output.headers_mut().append(name, value.clone());
        }
    }
    Ok(output)
}

fn http_response_body_timeout() -> std::time::Duration {
    #[cfg(feature = "integration-test")]
    if let Some(milliseconds) = std::env::var_os("LOCHO_TEST_HTTP_BODY_TIMEOUT_MS") {
        if let Ok(milliseconds) = milliseconds.to_string_lossy().parse::<u64>() {
            return std::time::Duration::from_millis(milliseconds);
        }
    }
    HTTP_REQUEST_TIMEOUT
}

fn error_response(status: StatusCode) -> HttpResponse {
    Response::builder()
        .status(status)
        .body(StreamBody::new(
            Box::pin(futures_util::stream::empty()) as HttpStream
        ))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn transport_path_changes_ignore_duplicate_states() {
        let direct = ConnectionType::Direct((IpAddr::V4(Ipv4Addr::LOCALHOST), 12345).into());
        assert!(!transport_path_changed(Some(&direct), &direct));
        assert!(transport_path_changed(None, &direct));

        let relay = ConnectionType::Relay("https://relay.example.com".parse().unwrap());
        let mixed = ConnectionType::Mixed(
            (IpAddr::V4(Ipv4Addr::LOCALHOST), 12345).into(),
            "https://relay.example.com".parse().unwrap(),
        );
        assert!(transport_path_changed(Some(&direct), &relay));
        assert!(transport_path_changed(Some(&relay), &mixed));
        assert_eq!(direct.to_string(), "direct(127.0.0.1:12345)");
        assert_eq!(relay.to_string(), "relay(https://relay.example.com./)");
        assert_eq!(
            mixed.to_string(),
            "mixed(udp: 127.0.0.1:12345, relay: https://relay.example.com./)"
        );
    }

    #[test]
    fn reconnect_backoff_is_capped() {
        let mut backoff = RECONNECT_INITIAL_BACKOFF;
        for expected in [500, 1_000, 2_000, 4_000] {
            backoff = next_backoff(backoff);
            assert_eq!(backoff, std::time::Duration::from_millis(expected));
        }
        assert_eq!(next_backoff(RECONNECT_MAX_BACKOFF), RECONNECT_MAX_BACKOFF);
    }

    #[tokio::test]
    async fn reconnect_backoff_can_be_cancelled() {
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            retry_delay(&shutdown, RECONNECT_MAX_BACKOFF),
        )
        .await
        .expect("cancelled retry should not wait for the full backoff");
    }
}
