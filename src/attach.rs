use crate::{attach_config, auth, http_utils, protocol::*};
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
use iroh::{
    endpoint::{presets, Connection},
    Endpoint, EndpointAddr, EndpointId,
};
use std::net::SocketAddr;
use std::{convert::Infallible, fmt, io::Write, pin::Pin};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Semaphore};
use tokio::task::JoinSet;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

const MAX_TOTAL_CONNECTIONS: usize = 512;

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
struct TransportFailure(anyhow::Error);

impl fmt::Display for TransportFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "transport failure: {}", self.0)
    }
}

impl std::error::Error for TransportFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

#[derive(Debug)]
struct ServiceRejected {
    status: u16,
}

impl fmt::Display for ServiceRejected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "TCP attachment rejected with status {}",
            self.status
        )
    }
}

impl std::error::Error for ServiceRejected {}

#[derive(Debug)]
enum TransportMonitorExit {
    PathLost,
    Ended,
}

fn transport_failure(error: anyhow::Error) -> anyhow::Error {
    anyhow::Error::new(TransportFailure(error))
}

fn is_transport_failure(error: &anyhow::Error) -> bool {
    error.downcast_ref::<TransportFailure>().is_some()
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
    capability: String,
    direct_address: Option<SocketAddr>,
    listen: SocketAddr,
) -> Result<()> {
    let capability = crate::capability::parse(&capability)?;
    run_attachments(
        host_id,
        vec![attach_config::AttachmentConfig { capability, listen }],
        direct_address,
    )
    .await
}

pub async fn run_config(
    config_path: std::path::PathBuf,
    direct_address: Option<SocketAddr>,
) -> Result<()> {
    let config = attach_config::AttachConfig::load(&config_path)?;
    let direct_address = direct_address.or(config.direct_address);
    let host_id = config.host_id.clone();
    run_attachments(host_id, config.attachments()?, direct_address).await
}

async fn run_attachments(
    host_id: String,
    attachments: Vec<attach_config::AttachmentConfig>,
    direct_address: Option<SocketAddr>,
) -> Result<()> {
    let node_id: EndpointId = host_id.parse().context("invalid host ID")?;
    #[cfg(feature = "integration-test")]
    let direct_address = match direct_address {
        Some(address) => Some(address),
        None => std::env::var_os("LOCHO_TEST_DIRECT_ADDR")
            .map(|address| address.to_string_lossy().parse())
            .transpose()
            .context("invalid LOCHO_TEST_DIRECT_ADDR")?,
    };
    let endpoint = Endpoint::builder(presets::N0).bind().await?;
    let endpoint_addr = direct_address
        .map(|address| EndpointAddr::new(node_id).with_ip_addr(address))
        .unwrap_or_else(|| EndpointAddr::new(node_id));
    let mut listeners = Vec::with_capacity(attachments.len());
    for attachment in &attachments {
        listeners.push((
            TcpListener::bind(attachment.listen)
                .await
                .with_context(|| {
                    format!(
                        "bind listener for service {:?}",
                        attachment.capability.service
                    )
                })?,
            attachment.capability.clone(),
        ));
    }
    let listeners = listeners
        .into_iter()
        .map(|(listener, capability)| {
            let listen = listener.local_addr()?;
            Ok((listener, capability, listen))
        })
        .collect::<Result<Vec<_>>>()?;
    for (_, capability, listen) in &listeners {
        if matches!(capability.service_type, crate::config::ServiceType::Tcp) {
            println!(
                "locho attached\n\nService: {}\nLocal TCP listener: {}",
                capability.service, listen
            );
        } else {
            println!(
                "locho attached\n\nService: {}\nLocal proxy:\nhttp://{}\n\nTry:\ncurl http://{}/",
                capability.service, listen, listen
            );
        }
        info!(service = %capability.service, %listen, "local listener ready");
    }
    std::io::stdout().flush()?;
    let (connection_sender, connection_receiver) = watch::channel(None);
    let connection_state = ConnectionState {
        receiver: connection_receiver,
        sender: connection_sender.clone(),
    };
    let supervisor_receiver = connection_state.receiver.clone();
    let shutdown = CancellationToken::new();
    let supervisor = tokio::spawn(connection_supervisor(
        endpoint.clone(),
        endpoint_addr,
        connection_sender,
        supervisor_receiver,
        shutdown.clone(),
    ));
    let total_connections = std::sync::Arc::new(Semaphore::new(MAX_TOTAL_CONNECTIONS));
    let mut listener_tasks = JoinSet::new();
    for (listener, capability, _listen) in listeners {
        let connection = connection_state.clone();
        let service = capability.service.clone();
        let secret = capability.secret.clone();
        let total_connections = total_connections.clone();
        let shutdown = shutdown.clone();
        let tcp = matches!(capability.service_type, crate::config::ServiceType::Tcp);
        if tcp {
            listener_tasks.spawn(async move {
                run_tcp_listener(
                    listener,
                    connection,
                    service,
                    secret,
                    total_connections,
                    shutdown,
                )
                .await
            });
        } else {
            listener_tasks.spawn(async move {
                run_http_listener(
                    listener,
                    connection,
                    service,
                    secret,
                    total_connections,
                    shutdown,
                )
                .await
            });
        }
    }
    let result = loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                if signal.is_ok() {
                    warn!("shutdown requested");
                }
                break Ok(());
            }
            completed = listener_tasks.join_next(), if !listener_tasks.is_empty() => {
                match completed {
                    Some(Ok(Ok(()))) => {
                        if listener_tasks.is_empty() {
                            break Ok(());
                        }
                    }
                    Some(Ok(Err(error))) => break Err(error),
                    Some(Err(error)) => break Err(error.into()),
                    None => break Ok(()),
                }
            }
        }
    };
    shutdown.cancel();
    if timeout(SHUTDOWN_TIMEOUT, async {
        while listener_tasks.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        warn!("shutdown deadline reached; aborting attachment listeners");
        listener_tasks.abort_all();
        while listener_tasks.join_next().await.is_some() {}
    }
    let _ = supervisor.await;
    endpoint.close().await;
    result
}

async fn connection_supervisor(
    endpoint: Endpoint,
    endpoint_addr: EndpointAddr,
    sender: watch::Sender<Option<ActiveConnection>>,
    mut receiver: ConnectionReceiver,
    shutdown: CancellationToken,
) {
    let mut backoff = RECONNECT_INITIAL_BACKOFF;
    let mut recovering_after_connection_loss = false;
    let mut generation = 0;
    'supervisor: loop {
        let connection = tokio::select! {
            _ = shutdown.cancelled() => break,
            result = timeout(HANDSHAKE_TIMEOUT, endpoint.connect(endpoint_addr.clone(), ALPN)) => {
                match result {
                    Ok(Ok(connection)) => connection,
                    Ok(Err(error)) => {
                        warn!(%error, "tunnel connection failed; retrying");
                        let delay = reconnect_delay(recovering_after_connection_loss, backoff);
                        retry_delay(&shutdown, delay).await;
                        if !recovering_after_connection_loss {
                            backoff = next_backoff(backoff);
                        }
                        continue;
                    }
                    Err(_) => {
                        warn!("tunnel connection timed out; retrying");
                        let delay = reconnect_delay(recovering_after_connection_loss, backoff);
                        retry_delay(&shutdown, delay).await;
                        if !recovering_after_connection_loss {
                            backoff = next_backoff(backoff);
                        }
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
        let mut monitor = spawn_transport_monitor(&connection, generation);
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
                    recovering_after_connection_loss = true;
                    break;
                }
                monitor_exit = &mut monitor_finished => {
                    info!(?monitor_exit, "tunnel transport monitor ended; reconnecting");
                    let _ = sender.send(None);
                    recovering_after_connection_loss = true;
                    connection.close(0u32.into(), b"transport monitor ended");
                    break;
                }
                changed = receiver.changed() => {
                    if changed.is_ok() && receiver.borrow().is_none() {
                        info!("tunnel connection invalidated; reconnecting");
                        recovering_after_connection_loss = true;
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
            if !monitor.is_finished() {
                monitor.abort();
                let _ = monitor.await;
            }
        }
        let delay = reconnect_delay(recovering_after_connection_loss, backoff);
        retry_delay(&shutdown, delay).await;
        if !recovering_after_connection_loss {
            backoff = next_backoff(backoff);
        }
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

fn reconnect_delay(
    recovering_after_connection_loss: bool,
    backoff: std::time::Duration,
) -> std::time::Duration {
    if recovering_after_connection_loss {
        RECONNECT_INITIAL_BACKOFF
    } else {
        backoff
    }
}

fn spawn_transport_monitor(
    connection: &Connection,
    generation: u64,
) -> Option<tokio::task::JoinHandle<TransportMonitorExit>> {
    let initial_path = transport_path(connection);
    if initial_path == "none" {
        warn!("connected to host but transport path is not yet available");
    } else {
        info!(generation, transport_path = %initial_path, "transport path established");
        println!("transport path: {initial_path}");
    }
    let connection = connection.clone();
    Some(tokio::spawn(async move {
        let mut paths = connection.paths_stream();
        let mut last_path = initial_path;
        while let Some(path_list) = paths.next().await {
            let connection_type = format_transport_paths(path_list.iter().map(|path| {
                (
                    path.remote_addr().to_string(),
                    path.is_ip(),
                    path.is_relay(),
                    path.is_selected(),
                )
            }));
            if !transport_path_changed(&last_path, &connection_type) {
                continue;
            }
            info!(generation, transport_path = %connection_type, "transport path changed");
            println!("transport path: {connection_type}");
            if connection_type == "none" {
                return TransportMonitorExit::PathLost;
            }
            last_path = connection_type;
        }
        TransportMonitorExit::Ended
    }))
}

fn transport_path(connection: &Connection) -> String {
    format_transport_paths(connection.paths().iter().map(|path| {
        (
            path.remote_addr().to_string(),
            path.is_ip(),
            path.is_relay(),
            path.is_selected(),
        )
    }))
}

pub(crate) fn format_transport_paths<I>(paths: I) -> String
where
    I: IntoIterator<Item = (String, bool, bool, bool)>,
{
    let mut direct = Vec::new();
    let mut relay = Vec::new();
    let mut selected = None;
    for (address, is_direct, is_relay, is_selected) in paths {
        let address = strip_transport_prefix(&address).to_owned();
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
        (true, true) => "none".into(),
    }
}

fn strip_transport_prefix(address: &str) -> &str {
    address
        .strip_prefix("ip:")
        .or_else(|| address.strip_prefix("relay:"))
        .unwrap_or(address)
}

fn transport_path_changed(previous: &str, current: &str) -> bool {
    previous != current
}

async fn run_http_listener(
    listener: TcpListener,
    connection: ConnectionState,
    service: String,
    secret: String,
    total_connections: std::sync::Arc<Semaphore>,
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
                let total_permit = match total_connections.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        drop(stream);
                        error!(service = %service_name, ?peer, "global connection limit reached");
                        continue;
                    }
                };
                clients.spawn(async move {
                    let _permit = permit;
                    let _total_permit = total_permit;
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
            _ = shutdown.cancelled() => break,
            completed = clients.join_next(), if !clients.is_empty() => {
                if let Some(result) = completed {
                    crate::task::log_result(result, "HTTP client");
                }
                crate::task::reap_finished_results(&mut clients, "HTTP client");
            }
        }
    }
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
    total_connections: std::sync::Arc<Semaphore>,
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
                let total_permit = match total_connections.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        drop(stream);
                        error!(service = %service, ?peer, "global connection limit reached");
                        continue;
                    }
                };
                clients.spawn(async move {
                    let _permit = permit;
                    let _total_permit = total_permit;
                    handle_tcp_connection(stream, connection, service, secret).await
                });
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
    if result.as_ref().is_err_and(is_transport_failure) {
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
    let (mut writer, mut reader) = lease
        .connection
        .open_bi()
        .await
        .map_err(|error| transport_failure(error.into()))?;
    write_json_head(
        &mut writer,
        &StreamRequestHead::Tcp(TcpRequestHead {
            version: PROTOCOL_VERSION,
            service,
            secret_proof: auth::secret_proof(&secret),
        }),
    )
    .await
    .map_err(transport_failure)?;
    let response: LochoResponseHead =
        timeout(HANDSHAKE_TIMEOUT, read_json_head(&mut reader, MAX_HEAD_LEN))
            .await
            .context("TCP attachment handshake timed out")?
            .map_err(transport_failure)?;
    if response.status != 200 {
        return Err(anyhow::Error::new(ServiceRejected {
            status: response.status,
        }));
    }
    read_body_with_limit(&mut reader, response.body_len, MAX_BODY_LEN)
        .await
        .map_err(transport_failure)?;
    let remote = tokio::io::join(reader, writer);
    let result = relay_with_idle_timeout(local, remote).await;
    if result.as_ref().is_err_and(|error| !is_idle_timeout(error)) {
        return result.map_err(transport_failure);
    }
    result?;
    Ok(())
}

fn is_idle_timeout(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<crate::protocol::TunnelIdleTimeout>()
        .is_some()
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
    if result.as_ref().is_err_and(is_transport_failure) {
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
        .context("HTTP tunnel stream open timed out")?
        .map_err(|error| transport_failure(error.into()))?;
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
    write_json_head(&mut writer, &StreamRequestHead::Http(head))
        .await
        .map_err(transport_failure)?;
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
                write_body(&mut writer, chunk)
                    .await
                    .map_err(transport_failure)?;
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
                write_body_chunk(&mut writer, chunk)
                    .await
                    .map_err(transport_failure)?;
            }
        }
        write_body_end(&mut writer)
            .await
            .map_err(transport_failure)?;
    }
    let response: LochoResponseHead = timeout(
        HTTP_ATTACHMENT_SAFETY_TIMEOUT,
        read_json_head(&mut reader, MAX_HEAD_LEN),
    )
    .await
    .context("HTTP attachment handshake timed out")?
    .map_err(transport_failure)?;
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
                let read = timeout(
                    http_response_body_timeout(),
                    reader.read_exact(&mut buffer[..count]),
                )
                    .await
                    .context("HTTP response body read timed out")?;
                read.inspect_err(|_| body_guard.invalidate())?;
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
    HTTP_ATTACHMENT_SAFETY_TIMEOUT
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

    #[test]
    fn transport_path_labels() {
        assert_eq!(
            format_transport_paths([("ip:127.0.0.1:1".into(), true, false, true)]),
            "direct(127.0.0.1:1)"
        );
        assert_eq!(
            format_transport_paths([("relay:relay.example".into(), false, true, true)]),
            "relay(relay.example)"
        );
        assert_eq!(
            format_transport_paths([
                ("ip:127.0.0.1:1".into(), true, false, true),
                ("relay:relay.example".into(), false, true, false),
            ]),
            "mixed(direct: 127.0.0.1:1, relay: relay.example)"
        );
        assert_eq!(format_transport_paths(std::iter::empty()), "none");
        assert!(!transport_path_changed("direct", "direct"));
        assert!(transport_path_changed("direct", "relay"));
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

    #[test]
    fn established_connection_loss_uses_initial_retry_delay() {
        assert_eq!(
            reconnect_delay(true, RECONNECT_MAX_BACKOFF),
            RECONNECT_INITIAL_BACKOFF
        );
        assert_eq!(
            reconnect_delay(false, RECONNECT_MAX_BACKOFF),
            RECONNECT_MAX_BACKOFF
        );
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
