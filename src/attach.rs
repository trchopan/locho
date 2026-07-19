use crate::{auth, http_utils, protocol::*};
use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use http::{Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, server::conn::http1, service::service_fn, Request};
use hyper_util::rt::TokioIo;
use iroh::{endpoint::Connection, Endpoint, NodeId};
use std::convert::Infallible;
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::time::timeout;
use tracing::{error, info};

type HttpResponse = Response<Full<Bytes>>;

pub async fn run(
    host_id: String,
    service: String,
    secret: String,
    tcp: bool,
    listen: SocketAddr,
) -> Result<()> {
    if service.is_empty() {
        return Err(anyhow!("service name cannot be empty"));
    }
    let node_id: NodeId = host_id.parse().context("invalid host ID")?;
    let endpoint = Endpoint::builder().discovery_n0().bind().await?;
    let connection = endpoint
        .connect(node_id, ALPN)
        .await
        .context("connect to host")?;
    let listener = TcpListener::bind(listen).await?;
    if tcp {
        return run_tcp_listener(listener, connection, service, secret).await;
    }
    println!(
        "locho attached\n\nService: {}\nLocal proxy:\nhttp://{}\n\nTry:\ncurl http://{}/",
        service, listen, listen
    );
    info!(%listen, "local proxy listening");
    loop {
        let (stream, peer) = listener.accept().await?;
        let connection = connection.clone();
        let service_name = service.clone();
        let secret = secret.clone();
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let connection = connection.clone();
                let service = service_name.clone();
                let secret = secret.clone();
                async move {
                    Ok::<_, Infallible>(handle_request(request, connection, service, secret).await)
                }
            });
            if let Err(error) = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
            {
                error!(%error, ?peer, "local connection failed");
            }
        });
    }
}

async fn run_tcp_listener(
    listener: TcpListener,
    connection: Connection,
    service: String,
    secret: String,
) -> Result<()> {
    let tcp_connections = std::sync::Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS));
    println!(
        "locho attached\n\nService: {}\nLocal TCP listener: {}",
        service,
        listener.local_addr()?
    );
    loop {
        let (stream, peer) = listener.accept().await?;
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
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(error) = handle_tcp_connection(stream, connection, service, secret).await {
                error!(%error, ?peer, "local TCP connection failed");
            }
        });
    }
}

async fn handle_tcp_connection(
    local: TcpStream,
    connection: Connection,
    service: String,
    secret: String,
) -> Result<()> {
    let (mut writer, mut reader) = connection.open_bi().await?;
    write_json_head(
        &mut writer,
        &StreamRequestHead::Tcp(TcpRequestHead {
            version: PROTOCOL_VERSION,
            service,
            secret_proof: auth::secret_proof(&secret),
        }),
    )
    .await?;
    let response: LochoResponseHead = timeout(
        TCP_CONNECT_TIMEOUT,
        read_json_head(&mut reader, MAX_HEAD_LEN),
    )
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
    connection: Connection,
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
    let body = match request.into_body().collect().await {
        Ok(collected) => {
            let bytes = collected.to_bytes();
            if bytes.len() > MAX_BODY_LEN {
                return error_response(StatusCode::PAYLOAD_TOO_LARGE);
            }
            bytes
        }
        Err(_) => return error_response(StatusCode::BAD_REQUEST),
    };
    match tunnel_request(connection, service, secret, method, path, headers, body).await {
        Ok(response) => response,
        Err(error) => {
            error!(%error, "tunnel request failed");
            if error.to_string().contains("403") {
                error_response(StatusCode::FORBIDDEN)
            } else if error.to_string().contains("501") {
                error_response(StatusCode::NOT_IMPLEMENTED)
            } else {
                error_response(StatusCode::BAD_GATEWAY)
            }
        }
    }
}

async fn tunnel_request(
    connection: Connection,
    service: String,
    secret: String,
    method: http::Method,
    path: String,
    headers: Vec<(String, String)>,
    body: Bytes,
) -> Result<HttpResponse> {
    let (mut writer, mut reader) = connection.open_bi().await?;
    let head = LochoRequestHead {
        version: PROTOCOL_VERSION,
        service,
        secret_proof: auth::secret_proof(&secret),
        method: method.to_string(),
        path_and_query: path,
        headers,
        body_len: Some(body.len() as u64),
    };
    write_json_head(&mut writer, &StreamRequestHead::Http(head)).await?;
    write_body(&mut writer, &body).await?;
    let response: LochoResponseHead = read_json_head(&mut reader, MAX_HEAD_LEN).await?;
    if response.version != PROTOCOL_VERSION {
        return Err(anyhow!(
            "unsupported tunnel response version {}",
            response.version
        ));
    }
    let body = read_body_with_limit(&mut reader, response.body_len, MAX_BODY_LEN).await?;
    let status =
        StatusCode::from_u16(response.status).map_err(|_| anyhow!("invalid response status"))?;
    info!(status = %status, "local response");
    let mut output = Response::builder().status(status).body(Full::new(body))?;
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
        .body(Full::new(Bytes::new()))
        .unwrap()
}
