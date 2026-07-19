use crate::{
    auth,
    config::{Config, ServiceType},
    http_utils,
    protocol::*,
};
use anyhow::{bail, Context, Result};
use bytes::Bytes;
use iroh::Endpoint;
use reqwest::Client;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio::time::timeout;
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
    R: AsyncRead + Unpin,
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
    mut reader: R,
    req: LochoRequestHead,
    services: Arc<HostServices>,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let req = match validate_http_request(req, &services) {
        Ok(body) => body,
        Err(status) => return write_error(&mut writer, status).await,
    };
    info!(method = %req.method, path = %req.path_and_query, "authenticated stream accepted");
    let body = match read_body_with_limit(&mut reader, req.body_len, MAX_BODY_LEN).await {
        Ok(req) => req,
        Err(error) => {
            let status = if error.to_string().contains("exceeds limit") {
                413
            } else {
                400
            };
            error!(%error, "invalid request body");
            return write_error(&mut writer, status).await;
        }
    };
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
    let (response, response_body) = match forward_to_upstream(upstream, req, body).await {
        Ok(value) => value,
        Err(error) => {
            error!(%error, "upstream request failed");
            let status = error
                .downcast_ref::<reqwest::Error>()
                .filter(|error| error.is_timeout())
                .map(|_| 504)
                .unwrap_or(502);
            (
                LochoResponseHead {
                    version: PROTOCOL_VERSION,
                    status,
                    headers: vec![],
                    body_len: Some(0),
                },
                Bytes::new(),
            )
        }
    };
    write_json_head(&mut writer, &response).await?;
    write_body(&mut writer, &response_body).await
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

pub async fn forward_to_upstream(
    upstream: Url,
    req: LochoRequestHead,
    body: Bytes,
) -> Result<(LochoResponseHead, Bytes)> {
    let url = http_utils::join_upstream_url(&upstream, &req.path_and_query)?;
    let method =
        reqwest::Method::from_bytes(req.method.as_bytes()).context("invalid request method")?;
    let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
    let mut request = client.request(method, url);
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
    let response = request.body(body).send().await?;
    let status = response.status().as_u16();
    let headers = http_utils::headers_to_pairs(response.headers());
    let body = response.bytes().await?;
    if body.len() > MAX_BODY_LEN {
        bail!("upstream response exceeds limit")
    }
    info!(status, "upstream response");
    Ok((
        LochoResponseHead {
            version: PROTOCOL_VERSION,
            status,
            headers,
            body_len: Some(body.len() as u64),
        },
        body,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ServiceConfig, ServiceType};
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
}
