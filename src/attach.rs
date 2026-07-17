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
use tokio::net::TcpListener;
use tracing::{error, info};

type HttpResponse = Response<Full<Bytes>>;

pub async fn run(host_id: String, secret: String, listen: SocketAddr) -> Result<()> {
    let node_id: NodeId = host_id.parse().context("invalid host ID")?;
    let endpoint = Endpoint::builder().discovery_n0().bind().await?;
    let connection = endpoint
        .connect(node_id, ALPN)
        .await
        .context("connect to host")?;
    let listener = TcpListener::bind(listen).await?;
    println!(
        "locho attached\n\nLocal proxy:\nhttp://{}\n\nTry:\ncurl http://{}/",
        listen, listen
    );
    info!(%listen, "local proxy listening");
    loop {
        let (stream, peer) = listener.accept().await?;
        let connection = connection.clone();
        let secret = secret.clone();
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let connection = connection.clone();
                let secret = secret.clone();
                async move { Ok::<_, Infallible>(handle_request(request, connection, secret).await) }
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

async fn handle_request(
    request: Request<Incoming>,
    connection: Connection,
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
    match tunnel_request(connection, secret, method, path, headers, body).await {
        Ok(response) => response,
        Err(error) => {
            error!(%error, "tunnel request failed");
            error_response(if error.to_string().contains("403") {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::BAD_GATEWAY
            })
        }
    }
}

async fn tunnel_request(
    connection: Connection,
    secret: String,
    method: http::Method,
    path: String,
    headers: Vec<(String, String)>,
    body: Bytes,
) -> Result<HttpResponse> {
    let (mut writer, mut reader) = connection.open_bi().await?;
    let head = LochoRequestHead {
        version: 1,
        secret_proof: auth::secret_proof(&secret),
        method: method.to_string(),
        path_and_query: path,
        headers,
        body_len: Some(body.len() as u64),
    };
    write_json_head(&mut writer, &head).await?;
    write_body(&mut writer, &body).await?;
    let response: LochoResponseHead = read_json_head(&mut reader, MAX_HEAD_LEN).await?;
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
