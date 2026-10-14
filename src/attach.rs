use crate::{attach_config, auth, http_utils, protocol::*, relay};
use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use futures_core::Stream;
use http::{Response, StatusCode};
use http_body_util::{BodyExt, StreamBody};
use hyper::{
    body::{Body, Frame, Incoming},
    server::conn::http1,
    service::service_fn,
    Request,
};
use hyper_util::rt::TokioIo;
use iroh::{EndpointAddr, EndpointId};
pub(crate) use locho_core::session::format_transport_paths;
use locho_core::session::{ConnectionLease, PeerSession, SessionState};
use std::net::SocketAddr;
use std::{convert::Infallible, fmt, io::Write, pin::Pin};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

const MAX_TOTAL_CONNECTIONS: usize = 512;

type HttpStream = Pin<Box<dyn Stream<Item = Result<Frame<Bytes>, anyhow::Error>> + Send>>;
type HttpResponse = Response<StreamBody<HttpStream>>;

type ConnectionState = PeerSession;

struct HttpBodyLeaseGuard {
    lease: Option<ConnectionLease>,
}

#[derive(Debug)]
struct HttpResponseTimeout;

impl fmt::Display for HttpResponseTimeout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HTTP response timed out")
    }
}

impl std::error::Error for HttpResponseTimeout {}

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

fn transport_failure(error: anyhow::Error) -> anyhow::Error {
    anyhow::Error::new(TransportFailure(error))
}

fn is_transport_failure(error: &anyhow::Error) -> bool {
    error.downcast_ref::<TransportFailure>().is_some()
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

impl Drop for HttpBodyLeaseGuard {
    fn drop(&mut self) {
        self.invalidate();
    }
}

pub async fn run(
    host_id: String,
    capability: String,
    relay_config_path: Option<std::path::PathBuf>,
    direct_address: Option<SocketAddr>,
    listen: SocketAddr,
    http_timeout_secs: Option<u64>,
) -> Result<()> {
    let capability = crate::capability::parse(&capability)?;
    let http_timeout = match capability.service_type {
        crate::config::ServiceType::Http => attach_config::resolve_http_timeout(http_timeout_secs)?,
        crate::config::ServiceType::Tcp => {
            if http_timeout_secs.is_some() {
                anyhow::bail!("--http-timeout-secs is only supported for HTTP attachments")
            }
            HTTP_REQUEST_TIMEOUT
        }
    };
    run_attachments(
        host_id,
        vec![attach_config::AttachmentConfig {
            capability,
            listen,
            http_timeout,
        }],
        relay::load(relay_config_path.as_deref())?,
        direct_address,
    )
    .await
}

pub async fn run_config(
    config_path: std::path::PathBuf,
    relay_config_path: Option<std::path::PathBuf>,
    direct_address: Option<SocketAddr>,
) -> Result<()> {
    let config = attach_config::AttachConfig::load(&config_path)?;
    let direct_address = direct_address.or(config.direct_address);
    let host_id = config.host_id.clone();
    run_attachments(
        host_id,
        config.attachments()?,
        relay::load(relay_config_path.as_deref())?,
        direct_address,
    )
    .await
}

async fn run_attachments(
    host_id: String,
    attachments: Vec<attach_config::AttachmentConfig>,
    relay_settings: Option<relay::RelaySettings>,
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
    let endpoint = relay::endpoint_builder(relay_settings.as_ref())
        .bind()
        .await?;
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
            attachment.http_timeout,
        ));
    }
    let listeners = listeners
        .into_iter()
        .map(|(listener, capability, http_timeout)| {
            let listen = listener.local_addr()?;
            Ok((listener, capability, listen, http_timeout))
        })
        .collect::<Result<Vec<_>>>()?;
    for (_, capability, listen, _) in &listeners {
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
    let (connection_state, driver) = PeerSession::start(endpoint, endpoint_addr);
    let shutdown = CancellationToken::new();
    let mut supervisor = tokio::spawn(driver.run());
    let mut status = connection_state.subscribe();
    let reporter = tokio::spawn(async move {
        let mut last_path = "none".to_owned();
        let mut last_error = None;
        loop {
            let snapshot = status.borrow_and_update().clone();
            if snapshot.last_error != last_error {
                if let Some(error) = snapshot.last_error {
                    warn!(%error, "tunnel connection recovering");
                }
                last_error = snapshot.last_error;
            }
            if snapshot.path_description != last_path {
                info!(transport_path = %snapshot.path_description, "transport path changed");
                println!("transport path: {}", snapshot.path_description);
                last_path = snapshot.path_description;
            }
            if snapshot.state == SessionState::Closed || status.changed().await.is_err() {
                break;
            }
        }
    });
    let total_connections = std::sync::Arc::new(Semaphore::new(MAX_TOTAL_CONNECTIONS));
    let mut listener_tasks = JoinSet::new();
    for (listener, capability, _listen, http_timeout) in listeners {
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
                    http_timeout,
                    shutdown,
                )
                .await
            });
        }
    }
    let mut supervisor_finished = false;
    let result = loop {
        tokio::select! {
            completed = &mut supervisor => {
                supervisor_finished = true;
                break match completed {
                    Ok(()) => Err(anyhow!("peer session driver stopped unexpectedly")),
                    Err(error) => Err(error.into()),
                };
            }
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
    let close_result = connection_state.close().await;
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
    if !supervisor_finished {
        let _ = supervisor.await;
    }
    let _ = reporter.await;
    result.and(close_result.map_err(anyhow::Error::from))
}

async fn run_http_listener(
    listener: TcpListener,
    connection: ConnectionState,
    service: String,
    secret: String,
    total_connections: std::sync::Arc<Semaphore>,
    http_timeout: std::time::Duration,
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
                        error!(service = %service_name, ?peer, "HTTP connection limit reached");
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
                        let http_timeout = http_timeout;
                        async move {
                            Ok::<_, Infallible>(
                                handle_request(
                                    request,
                                    connection,
                                    service,
                                    secret,
                                    http_timeout,
                                )
                                .await,
                            )
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
    println!(
        "locho attached\n\nService: {}\nLocal TCP listener: {}",
        service,
        listener.local_addr()?
    );
    std::io::stdout().flush()?;
    let capability = crate::capability::Capability {
        service,
        service_type: crate::config::ServiceType::Tcp,
        secret,
    };
    locho_client_native::forward::serve_tcp(
        listener,
        connection,
        capability,
        total_connections,
        shutdown,
    )
    .await?;
    Ok(())
}

async fn acquire_connection(state: ConnectionState) -> Result<ConnectionLease> {
    state
        .acquire(ATTACH_RECONNECT_TIMEOUT)
        .await
        .map_err(|_| anyhow::Error::new(TunnelUnavailable))
}

struct HttpTunnelRequest {
    service: String,
    secret: String,
    http_timeout: std::time::Duration,
    method: http::Method,
    path: String,
    headers: Vec<(String, String)>,
    body: Incoming,
}

async fn handle_request(
    request: Request<Incoming>,
    connection: ConnectionState,
    service: String,
    secret: String,
    http_timeout: std::time::Duration,
) -> HttpResponse {
    let method = request.method().clone();
    let service_name = service.clone();
    let method_name = method.to_string();
    let path = request
        .uri()
        .path_and_query()
        .map(|v| v.as_str())
        .unwrap_or("/")
        .to_string();
    let path_name = path.clone();
    info!(
        service = %service_name,
        method = %method_name,
        path = %path,
        "local request"
    );
    if !http_utils::is_supported_method(&method) {
        return error_response(StatusCode::METHOD_NOT_ALLOWED);
    }
    let headers = http_utils::headers_to_pairs(request.headers());
    match tunnel_request(
        connection,
        HttpTunnelRequest {
            service,
            secret,
            http_timeout,
            method,
            path,
            headers,
            body: request.into_body(),
        },
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            error!(
                service = %service_name,
                method = %method_name,
                %error,
                path = %path_name,
                "tunnel request failed"
            );
            if error.downcast_ref::<TunnelUnavailable>().is_some() {
                error_response(StatusCode::SERVICE_UNAVAILABLE)
            } else if error.downcast_ref::<HttpResponseTimeout>().is_some() {
                error_response(StatusCode::GATEWAY_TIMEOUT)
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
    request: HttpTunnelRequest,
) -> Result<HttpResponse> {
    let lease = acquire_connection(connection).await?;
    let result = tunnel_request_on_lease(&lease, request).await;
    if result.as_ref().is_err_and(|error| {
        is_transport_failure(error) || error.downcast_ref::<HttpResponseTimeout>().is_some()
    }) {
        lease.invalidate();
    }
    result
}

async fn tunnel_request_on_lease(
    lease: &ConnectionLease,
    request: HttpTunnelRequest,
) -> Result<HttpResponse> {
    let HttpTunnelRequest {
        service,
        secret,
        http_timeout,
        method,
        path,
        headers,
        body,
    } = request;
    let service_name = service.clone();
    let method_name = method.to_string();
    let path_name = path.clone();
    let (mut writer, mut reader) = timeout(HANDSHAKE_TIMEOUT, lease.connection().open_bi())
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
    let response: LochoResponseHead =
        timeout(http_timeout, read_json_head(&mut reader, MAX_HEAD_LEN))
            .await
            .map_err(|_| anyhow::Error::new(HttpResponseTimeout))?
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
                    http_timeout,
                    reader.read_exact(&mut buffer[..count]),
                )
                    .await
                    .map_err(|_| anyhow::Error::new(HttpResponseTimeout))
                    .and_then(|result| result.context("HTTP response body read failed"));
                if read.is_err() {
                    body_guard.invalidate();
                }
                read?;
                yield Frame::data(Bytes::copy_from_slice(&buffer[..count]));
                remaining -= count as u64;
            }
        } else {
            loop {
                let chunk = timeout(
                    http_timeout,
                    read_body_chunk(&mut reader),
                )
                    .await
                    .map_err(|_| anyhow::Error::new(HttpResponseTimeout))
                    .and_then(|result| result.context("HTTP response body read failed"));
                if chunk.is_err() {
                    body_guard.invalidate();
                }
                let chunk = chunk?;
                let Some(chunk) = chunk else { break };
                yield Frame::data(chunk);
            }
        }
        body_guard.complete();
    }) as HttpStream;
    info!(
        service = %service_name,
        method = %method_name,
        status = %status,
        path = %path_name,
        "local response"
    );
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
    use locho_core::session::{next_backoff, reconnect_delay};

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

    #[test]
    fn response_timeout_has_gateway_status() {
        let response = error_response(StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    }
}
