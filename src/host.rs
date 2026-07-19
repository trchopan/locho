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
use tracing::{error, info, warn};
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
    let services = Arc::new(HostServices { config, secrets });
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
    let req = match read_json_head::<LochoRequestHead, _>(&mut reader, MAX_HEAD_LEN).await {
        Ok(req) => req,
        Err(error) => {
            error!(%error, "malformed request header");
            let response = LochoResponseHead {
                version: PROTOCOL_VERSION,
                status: 400,
                headers: vec![],
                body_len: Some(0),
            };
            write_json_head(&mut writer, &response).await?;
            write_body(&mut writer, &[]).await?;
            return Ok(());
        }
    };
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
        None => {
            warn!(service = %req.service, "unknown service requested");
            return write_error(&mut writer, 404).await;
        }
    };
    let secret = match services.secrets.get(&service.name) {
        Some(secret) => secret,
        None => return write_error(&mut writer, 403).await,
    };
    if auth::verify_secret_proof(secret, &req.secret_proof).is_err() {
        warn!("auth failure");
        let response = LochoResponseHead {
            version: PROTOCOL_VERSION,
            status: 403,
            headers: vec![],
            body_len: Some(0),
        };
        write_json_head(&mut writer, &response).await?;
        write_body(&mut writer, &[]).await?;
        return Ok(());
    }
    info!(method = %req.method, path = %req.path_and_query, "authenticated stream accepted");
    let body = match read_body_with_limit(&mut reader, req.body_len, MAX_BODY_LEN).await {
        Ok(body) => body,
        Err(error) => {
            let status = if error.to_string().contains("exceeds limit") {
                413
            } else {
                400
            };
            error!(%error, "invalid request body");
            let response = LochoResponseHead {
                version: PROTOCOL_VERSION,
                status,
                headers: vec![],
                body_len: Some(0),
            };
            write_json_head(&mut writer, &response).await?;
            write_body(&mut writer, &[]).await?;
            return Ok(());
        }
    };
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
    use tokio::io::duplex;

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
        })
    }

    async fn request_response(request: LochoRequestHead) -> LochoResponseHead {
        let (client, server) = duplex(4096);
        let (mut client_reader, mut client_writer) = tokio::io::split(client);
        let (server_reader, server_writer) = tokio::io::split(server);
        let task = tokio::spawn(handle_stream(server_writer, server_reader, services()));
        write_json_head(&mut client_writer, &request).await.unwrap();
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
}
