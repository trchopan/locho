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
#[cfg(feature = "integration-test")]
use iroh::NodeAddr;
use iroh::{endpoint::Connection, Endpoint, NodeId};
use std::net::SocketAddr;
use std::{convert::Infallible, pin::Pin};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

type HttpStream = Pin<Box<dyn Stream<Item = Result<Frame<Bytes>, anyhow::Error>> + Send>>;
type HttpResponse = Response<StreamBody<HttpStream>>;

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
    #[cfg(feature = "integration-test")]
    if let Some(address) = std::env::var_os("LOCHO_TEST_DIRECT_ADDR") {
        let address = address
            .to_string_lossy()
            .parse()
            .context("invalid LOCHO_TEST_DIRECT_ADDR")?;
        endpoint.add_node_addr(NodeAddr::new(node_id).with_direct_addresses([address]))?;
    }
    let connection = timeout(HANDSHAKE_TIMEOUT, endpoint.connect(node_id, ALPN))
        .await
        .context("connect to host timed out")?
        .context("connect to host")?;
    let transport_monitor = if let Ok(watcher) = endpoint.conn_type(node_id) {
        Some(tokio::spawn(async move {
            let mut paths = watcher.stream();
            while let Some(connection_type) = paths.next().await {
                info!(transport_path = %connection_type, "transport path changed");
                println!("transport path: {connection_type}");
            }
        }))
    } else {
        warn!("connected to host but transport path is not yet available");
        None
    };
    let listener = match TcpListener::bind(listen).await {
        Ok(listener) => listener,
        Err(error) => {
            endpoint.close().await;
            if let Some(monitor) = transport_monitor {
                monitor.abort();
                let _ = monitor.await;
            }
            return Err(error.into());
        }
    };
    let result = if tcp {
        run_tcp_listener(listener, connection, service, secret).await
    } else {
        println!(
            "locho attached\n\nService: {}\nLocal proxy:\nhttp://{}\n\nTry:\ncurl http://{}/",
            service, listen, listen
        );
        info!(%listen, "local proxy listening");
        run_http_listener(listener, connection, service, secret).await
    };
    endpoint.close().await;
    if let Some(monitor) = transport_monitor {
        monitor.abort();
        let _ = monitor.await;
    }
    result
}

async fn run_http_listener(
    listener: TcpListener,
    connection: Connection,
    service: String,
    secret: String,
) -> Result<()> {
    let shutdown = CancellationToken::new();
    let mut clients = JoinSet::new();
    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, peer) = result?;
                let connection = connection.clone();
                let service_name = service.clone();
                let secret = secret.clone();
                clients.spawn(async move {
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
            signal = tokio::signal::ctrl_c() => {
                if signal.is_ok() {
                    warn!("shutdown requested");
                    shutdown.cancel();
                }
                break;
            }
        }
    }
    shutdown.cancel();
    connection.close(0u32.into(), b"locho shutdown");
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
    let shutdown = CancellationToken::new();
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
                    if let Err(error) = handle_tcp_connection(stream, connection, service, secret).await {
                        error!(%error, ?peer, "local TCP connection failed");
                    }
                });
            }
            signal = tokio::signal::ctrl_c() => {
                if signal.is_ok() {
                    warn!("shutdown requested");
                    shutdown.cancel();
                }
                break;
            }
        }
    }
    shutdown.cancel();
    connection.close(0u32.into(), b"locho shutdown");
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
            if error.to_string().contains("403") {
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
    connection: Connection,
    service: String,
    secret: String,
    method: http::Method,
    path: String,
    headers: Vec<(String, String)>,
    body: Incoming,
) -> Result<HttpResponse> {
    let (mut writer, mut reader) = connection.open_bi().await?;
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
    let response: LochoResponseHead = read_json_head(&mut reader, MAX_HEAD_LEN).await?;
    if response.version != PROTOCOL_VERSION {
        return Err(anyhow!(
            "unsupported tunnel response version {}",
            response.version
        ));
    }
    let status =
        StatusCode::from_u16(response.status).map_err(|_| anyhow!("invalid response status"))?;
    let body_len = response.body_len;
    let stream = Box::pin(async_stream::try_stream! {
        if let Some(length) = body_len {
            let mut remaining = length;
            let mut buffer = vec![0u8; BODY_CHUNK_LEN];
            while remaining > 0 {
                let count = remaining.min(BODY_CHUNK_LEN as u64) as usize;
                reader.read_exact(&mut buffer[..count]).await?;
                yield Frame::data(Bytes::copy_from_slice(&buffer[..count]));
                remaining -= count as u64;
            }
        } else {
            while let Some(chunk) = read_body_chunk(&mut reader).await? {
                yield Frame::data(chunk);
            }
        }
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

fn error_response(status: StatusCode) -> HttpResponse {
    Response::builder()
        .status(status)
        .body(StreamBody::new(
            Box::pin(futures_util::stream::empty()) as HttpStream
        ))
        .unwrap()
}
