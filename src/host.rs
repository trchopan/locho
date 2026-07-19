use crate::{
    auth,
    config::{Config, ServiceType},
    http_utils,
    protocol::*,
};
use anyhow::{bail, Context, Result};
use bytes::Bytes;
use futures_util::StreamExt;
use iroh::Endpoint;
use reqwest::Client;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::sync::Semaphore;
use tokio::time::timeout;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info};
use url::Url;

pub async fn run(config_path: PathBuf) -> Result<()> {
    let config = Config::load(&config_path)?;
    let _state_lock = crate::state::acquire_state_lock()?;
    let host_secret_key = crate::state::load_or_create_host_secret_key()?;
    let endpoint = Endpoint::builder()
        .discovery_n0()
        .alpns(vec![ALPN.to_vec()])
        .secret_key(host_secret_key)
        .bind()
        .await?;
    let mut persisted_state = crate::state::load_or_create_host_state(endpoint.node_id())?;
    let active_names = config
        .services
        .iter()
        .map(|service| service.name.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut secrets = persisted_state.service_secrets.clone();
    secrets.retain(|name, _| active_names.contains(name.as_str()));
    for service in &config.services {
        secrets
            .entry(service.name.clone())
            .or_insert_with(crate::auth::generate_secret);
    }
    persisted_state.service_secrets = secrets.clone();
    crate::state::save_host_state(&persisted_state)?;
    let services = Arc::new(HostServices {
        config,
        secrets,
        tcp_connections: Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS)),
    });
    info!(config = %config_path.display(), services = services.config.services.len(), "host started");
    println!("locho host started\n\nAttach from another machine with:");
    for service in &services.config.services {
        let secret = services.secrets.get(&service.name).unwrap();
        println!(
            "\nlocho attach {} {} {}",
            endpoint.node_id(),
            service.name,
            secret
        );
    }

    while let Some(incoming) = endpoint.accept().await {
        let services = Arc::clone(&services);
        tokio::spawn(async move {
            match incoming.accept() {
                Ok(connecting) => match connecting.await {
                    Ok(connection) => {
                        while let Ok((send, recv)) = connection.accept_bi().await {
                            let services = Arc::clone(&services);
                            tokio::spawn(async move {
                                if let Err(error) = handle_stream(send, recv, services).await {
                                    error!(%error, "tunnel stream failed");
                                }
                            });
                        }
                    }
                    Err(error) => error!(%error, "tunnel connection failed"),
                },
                Err(error) => error!(%error, "invalid incoming connection"),
            }
        });
    }
    Ok(())
}

struct HostServices {
    config: Config,
    secrets: std::collections::HashMap<String, String>,
    tcp_connections: Arc<Semaphore>,
}

async fn handle_stream<W, R>(
    mut writer: W,
    mut reader: R,
    services: Arc<HostServices>,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin + Send + 'static,
{
    let head = match read_json_head::<StreamRequestHead, _>(&mut reader, MAX_HEAD_LEN).await {
        Ok(head) => head,
        Err(error) => {
            error!(%error, "malformed request header");
            write_error(&mut writer, 400).await?;
            return Ok(());
        }
    };
    match head {
        StreamRequestHead::Http(req) => handle_http_stream(writer, reader, req, services).await,
        StreamRequestHead::Tcp(req) => handle_tcp_stream(writer, reader, req, services).await,
    }
}

async fn handle_http_stream<W, R>(
    mut writer: W,
    reader: R,
    req: LochoRequestHead,
    services: Arc<HostServices>,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin + Send + 'static,
{
    let req = match validate_http_request(req, &services) {
        Ok(body) => body,
        Err(status) => return write_error(&mut writer, status).await,
    };
    info!(method = %req.method, path = %req.path_and_query, "authenticated stream accepted");
    let service = services
        .config
        .services
        .iter()
        .find(|service| service.name == req.service)
        .expect("validated HTTP service must exist");
    let upstream = match (&service.service_type, &service.upstream) {
        (ServiceType::Http, Some(upstream)) => upstream.clone(),
        (ServiceType::Tcp, _) => return write_error(&mut writer, 501).await,
        _ => return write_error(&mut writer, 500).await,
    };
    let response = match forward_to_upstream(upstream, req, reader, &mut writer).await {
        Ok(value) => value,
        Err(error) => {
            error!(%error, "upstream request failed");
            let status = error
                .downcast_ref::<reqwest::Error>()
                .filter(|error| error.is_timeout())
                .map(|_| 504)
                .unwrap_or(502);
            let status = if error.to_string().contains("body exceeds limit") {
                413
            } else {
                status
            };
            return write_error(&mut writer, status).await;
        }
    };
    write_json_head(&mut writer, &response).await
}

fn validate_http_request(
    req: LochoRequestHead,
    services: &HostServices,
) -> Result<LochoRequestHead, u16> {
    if req.version != PROTOCOL_VERSION {
        return Err(400);
    }
    let service = services
        .config
        .services
        .iter()
        .find(|service| service.name == req.service)
        .ok_or(404u16)?;
    if !matches!(service.service_type, ServiceType::Http) {
        return Err(400);
    }
    let secret = services.secrets.get(&service.name).ok_or(403u16)?;
    auth::verify_secret_proof(secret, &req.secret_proof).map_err(|_| 403u16)?;
    Ok(req)
}

async fn handle_tcp_stream<W, R>(
    mut writer: W,
    reader: R,
    req: TcpRequestHead,
    services: Arc<HostServices>,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    if req.version != PROTOCOL_VERSION {
        return write_error(&mut writer, 400).await;
    }
    let service = match services
        .config
        .services
        .iter()
        .find(|service| service.name == req.service)
    {
        Some(service) => service,
        None => return write_error(&mut writer, 404).await,
    };
    let secret = services
        .secrets
        .get(&service.name)
        .ok_or_else(|| anyhow::anyhow!("missing service secret"))?;
    if auth::verify_secret_proof(secret, &req.secret_proof).is_err() {
        return write_error(&mut writer, 403).await;
    }
    let endpoint = match (&service.service_type, service.endpoint) {
        (ServiceType::Tcp, Some(endpoint)) => endpoint,
        _ => return write_error(&mut writer, 400).await,
    };
    let _permit = match services.tcp_connections.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return write_error(&mut writer, 429).await,
    };
    let upstream = match timeout(TCP_CONNECT_TIMEOUT, TcpStream::connect(endpoint)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            error!(service = %service.name, %endpoint, %error, "TCP upstream unavailable");
            return write_error(&mut writer, 502).await;
        }
        Err(_) => {
            error!(service = %service.name, %endpoint, "TCP upstream connection timed out");
            return write_error(&mut writer, 504).await;
        }
    };
    write_json_head(
        &mut writer,
        &LochoResponseHead {
            version: PROTOCOL_VERSION,
            status: 200,
            headers: vec![],
            body_len: Some(0),
        },
    )
    .await?;
    write_body(&mut writer, &[]).await?;
    let tunnel = tokio::io::join(reader, writer);
    relay_with_idle_timeout(tunnel, upstream).await?;
    Ok(())
}

async fn write_error<W: AsyncWrite + Unpin>(writer: &mut W, status: u16) -> Result<()> {
    write_json_head(
        writer,
        &LochoResponseHead {
            version: PROTOCOL_VERSION,
            status,
            headers: vec![],
            body_len: Some(0),
        },
    )
    .await?;
    write_body(writer, &[]).await
}

pub async fn forward_to_upstream<R, W>(
    upstream: Url,
    req: LochoRequestHead,
    mut reader: R,
    writer: &mut W,
) -> Result<LochoResponseHead>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let url = http_utils::join_upstream_url(&upstream, &req.path_and_query)?;
    let method =
        reqwest::Method::from_bytes(req.method.as_bytes()).context("invalid request method")?;
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
    let mut request = client.request(method, url);
    if let Some(body_len) = req.body_len {
        request = request.header(reqwest::header::CONTENT_LENGTH, body_len);
    }
    for (name, value) in req.headers {
        if let (Ok(name), Ok(value)) = (
            name.parse::<reqwest::header::HeaderName>(),
            value.parse::<reqwest::header::HeaderValue>(),
        ) {
            if !http_utils::is_hop_by_hop_header(&name) {
                request = request.header(name, value);
            }
        }
    }
    let (body_sender, body_receiver) = mpsc::channel::<Result<Bytes, reqwest::Error>>(8);
    let body_len = req.body_len;
    let body_task = tokio::spawn(async move {
        let mut total = 0usize;
        if let Some(len) = body_len {
            if len > MAX_BODY_LEN as u64 {
                bail!("request body exceeds limit")
            }
            let mut remaining = len;
            let mut buffer = vec![0u8; BODY_CHUNK_LEN];
            while remaining > 0 {
                let count = remaining.min(buffer.len() as u64) as usize;
                reader.read_exact(&mut buffer[..count]).await?;
                body_sender
                    .send(Ok(Bytes::copy_from_slice(&buffer[..count])))
                    .await
                    .context("send request body")?;
                remaining -= count as u64;
            }
        } else {
            while let Some(chunk) = read_body_chunk(&mut reader).await? {
                total += chunk.len();
                if total > MAX_BODY_LEN {
                    bail!("request body exceeds limit")
                }
                body_sender
                    .send(Ok(chunk))
                    .await
                    .context("send request body")?;
            }
        }
        Ok::<_, anyhow::Error>(())
    });
    let response = match request
        .body(reqwest::Body::wrap_stream(ReceiverStream::new(
            body_receiver,
        )))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            body_task.abort();
            let _ = body_task.await;
            return Err(error.into());
        }
    };
    body_task.await.context("request body task failed")??;
    let status = response.status().as_u16();
    let headers = http_utils::headers_to_pairs(response.headers());
    let body_len = response.content_length();
    if body_len.is_some_and(|len| len > MAX_BODY_LEN as u64) {
        bail!("upstream response exceeds limit")
    }
    let head = LochoResponseHead {
        version: PROTOCOL_VERSION,
        status,
        headers,
        body_len,
    };
    write_json_head(writer, &head).await?;
    let mut response_body = response.bytes_stream();
    let mut total = 0usize;
    while let Some(chunk) = response_body.next().await {
        let chunk = chunk?;
        total += chunk.len();
        if total > MAX_BODY_LEN {
            bail!("upstream response exceeds limit")
        }
        if body_len.is_some() {
            write_body(writer, &chunk).await?;
        } else {
            write_body_chunk(writer, &chunk).await?;
        }
    }
    if body_len.is_none() {
        write_body_end(writer).await?;
    }
    info!(status, "upstream response");
    Ok(head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ServiceConfig, ServiceType};
    use http_body_util::BodyExt;
    use std::collections::HashMap;
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn services() -> Arc<HostServices> {
        Arc::new(HostServices {
            config: Config {
                services: vec![ServiceConfig {
                    name: "api".into(),
                    service_type: ServiceType::Http,
                    upstream: Some(Url::parse("https://example.com").unwrap()),
                    endpoint: None,
                }],
            },
            secrets: HashMap::from([("api".into(), "correct".into())]),
            tcp_connections: Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS)),
        })
    }

    fn tcp_services(endpoint: std::net::SocketAddr) -> Arc<HostServices> {
        Arc::new(HostServices {
            config: Config {
                services: vec![ServiceConfig {
                    name: "database".into(),
                    service_type: ServiceType::Tcp,
                    upstream: None,
                    endpoint: Some(endpoint),
                }],
            },
            secrets: HashMap::from([("database".into(), "correct".into())]),
            tcp_connections: Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS)),
        })
    }

    async fn request_response(request: LochoRequestHead) -> LochoResponseHead {
        let (client, server) = duplex(4096);
        let (mut client_reader, mut client_writer) = tokio::io::split(client);
        let (server_reader, server_writer) = tokio::io::split(server);
        let task = tokio::spawn(handle_stream(server_writer, server_reader, services()));
        write_json_head(&mut client_writer, &StreamRequestHead::Http(request))
            .await
            .unwrap();
        write_body(&mut client_writer, &[]).await.unwrap();
        let response = read_json_head(&mut client_reader, MAX_HEAD_LEN)
            .await
            .unwrap();
        task.await.unwrap().unwrap();
        response
    }

    fn request(service: &str, proof: &str) -> LochoRequestHead {
        LochoRequestHead {
            version: PROTOCOL_VERSION,
            service: service.into(),
            secret_proof: proof.into(),
            method: "GET".into(),
            path_and_query: "/".into(),
            headers: vec![],
            body_len: Some(0),
        }
    }

    #[tokio::test]
    async fn capability_is_scoped_to_service() {
        let response = request_response(request("api", &auth::secret_proof("wrong"))).await;
        assert_eq!(response.status, 403);
    }

    #[tokio::test]
    async fn unknown_service_is_rejected() {
        let response = request_response(request("missing", &auth::secret_proof("correct"))).await;
        assert_eq!(response.status, 404);
    }

    #[tokio::test]
    async fn unsupported_request_version_is_rejected() {
        let mut request = request("api", &auth::secret_proof("correct"));
        request.version = PROTOCOL_VERSION + 1;
        let response = request_response(request).await;
        assert_eq!(response.status, 400);
    }

    #[tokio::test]
    async fn tcp_mode_rejects_an_http_service() {
        let (client, server) = duplex(4096);
        let (mut client_reader, mut client_writer) = tokio::io::split(client);
        let (server_reader, server_writer) = tokio::io::split(server);
        let task = tokio::spawn(handle_stream(server_writer, server_reader, services()));
        write_json_head(
            &mut client_writer,
            &StreamRequestHead::Tcp(TcpRequestHead {
                version: PROTOCOL_VERSION,
                service: "api".into(),
                secret_proof: auth::secret_proof("correct"),
            }),
        )
        .await
        .unwrap();
        let response: LochoResponseHead = read_json_head(&mut client_reader, MAX_HEAD_LEN)
            .await
            .unwrap();
        assert_eq!(response.status, 400);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn tcp_forwards_data_bidirectionally() {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        let upstream = tokio::spawn(async move {
            let (mut stream, _) = upstream_listener.accept().await.unwrap();
            let mut request = [0u8; 4];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(&request, b"ping");
            stream.write_all(b"pong").await.unwrap();
            stream.shutdown().await.unwrap();
        });

        let (client, server) = duplex(4096);
        let (mut client_reader, mut client_writer) = tokio::io::split(client);
        let (server_reader, server_writer) = tokio::io::split(server);
        let task = tokio::spawn(handle_stream(
            server_writer,
            server_reader,
            tcp_services(upstream_addr),
        ));
        write_json_head(
            &mut client_writer,
            &StreamRequestHead::Tcp(TcpRequestHead {
                version: PROTOCOL_VERSION,
                service: "database".into(),
                secret_proof: auth::secret_proof("correct"),
            }),
        )
        .await
        .unwrap();
        let response: LochoResponseHead = read_json_head(&mut client_reader, MAX_HEAD_LEN)
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        read_body_with_limit(&mut client_reader, response.body_len, MAX_BODY_LEN)
            .await
            .unwrap();
        client_writer.write_all(b"ping").await.unwrap();
        client_writer.shutdown().await.unwrap();
        let mut reply = [0u8; 4];
        client_reader.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"pong");
        task.await.unwrap().unwrap();
        upstream.await.unwrap();
    }

    #[tokio::test]
    async fn tcp_reports_unavailable_upstream() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        drop(listener);
        let (client, server) = duplex(4096);
        let (mut client_reader, mut client_writer) = tokio::io::split(client);
        let (server_reader, server_writer) = tokio::io::split(server);
        let task = tokio::spawn(handle_stream(
            server_writer,
            server_reader,
            tcp_services(endpoint),
        ));
        write_json_head(
            &mut client_writer,
            &StreamRequestHead::Tcp(TcpRequestHead {
                version: PROTOCOL_VERSION,
                service: "database".into(),
                secret_proof: auth::secret_proof("correct"),
            }),
        )
        .await
        .unwrap();
        let response: LochoResponseHead = read_json_head(&mut client_reader, MAX_HEAD_LEN)
            .await
            .unwrap();
        assert_eq!(response.status, 502);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn http_forwarding_preserves_length_and_streams_response_chunks() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let expected_response = Bytes::from(vec![b'x'; BODY_CHUNK_LEN * 2 + 3]);
        let expected_response_for_server = expected_response.clone();
        let upstream = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let service = hyper::service::service_fn(
                move |request: hyper::Request<hyper::body::Incoming>| {
                    let expected_response = expected_response_for_server.clone();
                    async move {
                        assert_eq!(request.headers().get("content-length").unwrap(), "12");
                        assert_eq!(
                            request.into_body().collect().await.unwrap().to_bytes(),
                            "request-body"
                        );
                        Ok::<_, std::convert::Infallible>(hyper::Response::new(
                            http_body_util::Full::new(expected_response),
                        ))
                    }
                },
            );
            hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                .await
                .unwrap();
        });

        let request = LochoRequestHead {
            version: PROTOCOL_VERSION,
            service: "api".into(),
            secret_proof: auth::secret_proof("correct"),
            method: "POST".into(),
            path_and_query: "/echo".into(),
            headers: vec![],
            body_len: Some(12),
        };
        let (mut request_writer, request_reader) = duplex(4096);
        write_body(&mut request_writer, b"request-body")
            .await
            .unwrap();
        let (mut response_reader, mut response_writer) = duplex(BODY_CHUNK_LEN * 3);
        let head = forward_to_upstream(
            Url::parse(&format!("http://{address}")).unwrap(),
            request,
            request_reader,
            &mut response_writer,
        )
        .await
        .unwrap();
        drop(request_writer);
        assert_eq!(head.status, 200);
        assert_eq!(head.body_len, Some(expected_response.len() as u64));
        let received_head: LochoResponseHead = read_json_head(&mut response_reader, MAX_HEAD_LEN)
            .await
            .unwrap();
        assert_eq!(received_head.status, head.status);
        assert_eq!(received_head.body_len, head.body_len);
        let mut received = vec![0u8; expected_response.len()];
        response_reader.read_exact(&mut received).await.unwrap();
        assert_eq!(received, expected_response);
        upstream.await.unwrap();
    }
}
