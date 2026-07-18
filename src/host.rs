use crate::{auth, http_utils, protocol::*};
use anyhow::{bail, Context, Result};
use bytes::Bytes;
use iroh::Endpoint;
use reqwest::Client;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::{error, info, warn};
use url::Url;

pub async fn run(upstream: Url) -> Result<()> {
    if upstream.scheme() != "https" || upstream.host().is_none() {
        bail!("upstream must be an HTTPS URL with a host")
    }
    let host_secret_key = crate::state::load_or_create_host_secret_key()?;
    let endpoint = Endpoint::builder()
        .discovery_n0()
        .alpns(vec![ALPN.to_vec()])
        .secret_key(host_secret_key)
        .bind()
        .await?;
    let persisted_state = crate::state::load_or_create_host_state(endpoint.node_id())?;
    let secret = persisted_state.attach_secret;
    info!(upstream = %upstream, "host started");
    println!("locho host started\n\nUpstream:\n{}\n\nAttach from another machine with:\n\nlocho attach {} {}", upstream, endpoint.node_id(), secret);

    while let Some(incoming) = endpoint.accept().await {
        let upstream = upstream.clone();
        let secret = secret.clone();
        tokio::spawn(async move {
            match incoming.accept() {
                Ok(connecting) => match connecting.await {
                    Ok(connection) => {
                        while let Ok((send, recv)) = connection.accept_bi().await {
                            let upstream = upstream.clone();
                            let secret = secret.clone();
                            tokio::spawn(async move {
                                if let Err(error) =
                                    handle_stream(send, recv, upstream, secret).await
                                {
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

async fn handle_stream<W, R>(
    mut writer: W,
    mut reader: R,
    upstream: Url,
    secret: String,
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
                version: 1,
                status: 400,
                headers: vec![],
                body_len: Some(0),
            };
            write_json_head(&mut writer, &response).await?;
            write_body(&mut writer, &[]).await?;
            return Ok(());
        }
    };
    if auth::verify_secret_proof(&secret, &req.secret_proof).is_err() {
        warn!("auth failure");
        let response = LochoResponseHead {
            version: 1,
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
                version: 1,
                status,
                headers: vec![],
                body_len: Some(0),
            };
            write_json_head(&mut writer, &response).await?;
            write_body(&mut writer, &[]).await?;
            return Ok(());
        }
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
                    version: 1,
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
            version: 1,
            status,
            headers,
            body_len: Some(body.len() as u64),
        },
        body,
    ))
}
