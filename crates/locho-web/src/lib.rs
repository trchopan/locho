mod addressing;

use http::uri::Authority;
use iroh::{
    address_lookup::PkarrPublisher, endpoint::presets, Endpoint, EndpointAddr, EndpointId, RelayUrl,
};
use locho_core::{
    capability,
    session::{PeerSession, SessionError},
};
use n0_future::time::{timeout, Duration};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use wasm_bindgen::prelude::*;

const MAX_HEALTH_RESPONSE: usize = 8 * 1024 * 1024;
const MAX_HEALTH_HEADERS: usize = 64 * 1024;

#[wasm_bindgen]
#[derive(Default)]
pub struct WebCancellation {
    token: CancellationToken,
}

#[wasm_bindgen]
impl WebCancellation {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.token.cancel();
    }
}

#[wasm_bindgen]
pub struct WebClient {
    session: Arc<PeerSession>,
    shutdown: CancellationToken,
}

fn map_error(error: SessionError) -> JsValue {
    JsValue::from_str(match error {
        SessionError::Closed => "closed",
        SessionError::Cancelled => "cancelled",
        SessionError::Timeout(_) => "timeout",
        SessionError::Transport(_) => "transport",
        SessionError::Protocol => "protocol",
        SessionError::ServiceRejected(_) => "serviceRejected",
        SessionError::UnsupportedService => "unsupportedService",
        SessionError::ResourceLimit => "resourceLimit",
        SessionError::DriverStopped => "driverStopped",
    })
}

/// Connects to the host identity through iroh's browser-compatible transport.
/// `peer_relay_url` is an optional host address hint, not client relay policy.
#[wasm_bindgen]
pub async fn connect(
    host_id: String,
    peer_relay_url: Option<String>,
    cancellation: &WebCancellation,
) -> Result<WebClient, JsValue> {
    if cancellation.token.is_cancelled() {
        return Err(JsValue::from_str("cancelled"));
    }
    let id: EndpointId = host_id
        .parse()
        .map_err(|_| JsValue::from_str("invalidHostId"))?;
    let peer_relay = peer_relay_url
        .map(|relay| {
            relay
                .parse::<RelayUrl>()
                .map(addressing::relay_url)
                .map_err(|_| JsValue::from_str("invalidPeerRelay"))
        })
        .transpose()?;
    let builder = Endpoint::builder(presets::N0)
        .clear_address_lookup()
        .address_lookup(PkarrPublisher::n0_dns())
        .address_lookup(addressing::BrowserLookup);
    let endpoint = tokio::select! {
        biased;
        _ = cancellation.token.cancelled() => return Err(JsValue::from_str("cancelled")),
        result = timeout(Duration::from_secs(15), builder.bind()) => {
            result.map_err(|_| JsValue::from_str("startupTimeout"))?
                .map_err(|_| JsValue::from_str("endpointBind"))?
        }
    };
    let mut target = EndpointAddr::new(id);
    if let Some(relay) = peer_relay {
        target = target.with_relay_url(relay);
    }
    let (session, driver) = PeerSession::start(endpoint, target);
    let session = Arc::new(session);
    wasm_bindgen_futures::spawn_local(driver.run());
    let connected = tokio::select! {
        biased;
        _ = cancellation.token.cancelled() => Err(JsValue::from_str("cancelled")),
        result = timeout(
        Duration::from_secs(15),
        session.wait_connected(Duration::from_secs(15)),
        ) => match result {
            Ok(result) => result.map_err(map_error),
            Err(_) => Err(JsValue::from_str("startupTimeout")),
        },
    };
    if let Err(error) = connected {
        let _ = session.close().await;
        return Err(error);
    }
    Ok(WebClient {
        session,
        shutdown: CancellationToken::new(),
    })
}

#[wasm_bindgen]
impl WebClient {
    pub fn status(&self) -> String {
        let status = self.session.snapshot();
        format!(
            "{{\"revision\":{},\"state\":\"{:?}\",\"path\":\"{:?}\"}}",
            status.revision, status.state, status.path
        )
        .to_lowercase()
    }

    /// Proof request only: returns raw HTTP/1.1 response bytes as UTF-8 text.
    /// The complete Fetch-like API and streaming bodies are later milestones.
    pub async fn health(
        &self,
        capability: String,
        authority: String,
        cancellation: &WebCancellation,
    ) -> Result<String, JsValue> {
        if self.shutdown.is_cancelled() {
            return Err(JsValue::from_str("closed"));
        }
        if cancellation.token.is_cancelled() {
            return Err(JsValue::from_str("cancelled"));
        }
        let authority: Authority = authority
            .parse()
            .map_err(|_| JsValue::from_str("invalidAuthority"))?;
        if authority.as_str().contains(['\r', '\n']) {
            return Err(JsValue::from_str("invalidAuthority"));
        }
        let capability =
            capability::parse(&capability).map_err(|_| JsValue::from_str("invalidCapability"))?;
        let operation = timeout(Duration::from_secs(15), async {
            let mut stream = self
                .session
                .open_tcp(&capability, Duration::from_secs(15), &cancellation.token)
                .await
                .map_err(map_error)?;
            let request =
                format!("GET /health HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
            stream
                .write_all(request.as_bytes())
                .await
                .map_err(|_| JsValue::from_str("transport"))?;
            let mut response = Vec::new();
            let mut buffer = [0u8; 16 * 1024];
            loop {
                let count = stream
                    .read(&mut buffer)
                    .await
                    .map_err(|_| JsValue::from_str("transport"))?;
                if count == 0 {
                    break;
                }
                if response.len() + count > MAX_HEALTH_RESPONSE {
                    return Err(JsValue::from_str("responseLimit"));
                }
                response.extend_from_slice(&buffer[..count]);
            }
            validate_health_response(&response).map_err(JsValue::from_str)?;
            String::from_utf8(response).map_err(|_| JsValue::from_str("invalidResponse"))
        });
        tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => Err(JsValue::from_str("closed")),
            _ = cancellation.token.cancelled() => Err(JsValue::from_str("cancelled")),
            result = operation => result.map_err(|_| JsValue::from_str("timeout"))?,
        }
    }

    pub async fn close(&self) -> Result<(), JsValue> {
        self.shutdown.cancel();
        self.session.close().await.map_err(map_error)
    }
}

fn validate_health_response(response: &[u8]) -> Result<(), &'static str> {
    let separator = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("invalidResponse")?;
    if separator > MAX_HEALTH_HEADERS {
        return Err("responseLimit");
    }
    let head = std::str::from_utf8(&response[..separator]).map_err(|_| "invalidResponse")?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or("invalidResponse")?;
    let mut status = status_line.split(' ');
    if status.next() != Some("HTTP/1.1")
        || !status
            .next()
            .and_then(|value| value.parse::<u16>().ok())
            .is_some_and(|value| (100..=599).contains(&value))
    {
        return Err("invalidResponse");
    }
    let mut content_length = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err("invalidResponse");
        };
        if name.is_empty() || value.contains(['\r', '\n']) {
            return Err("invalidResponse");
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err("unsupportedFraming");
        }
        if name.eq_ignore_ascii_case("content-length") {
            let value = value
                .trim()
                .parse::<usize>()
                .map_err(|_| "invalidResponse")?;
            if content_length.is_some_and(|previous| previous != value) {
                return Err("invalidResponse");
            }
            content_length = Some(value);
        }
    }
    let body = &response[separator + 4..];
    if content_length.is_some_and(|length| length != body.len()) {
        return Err("truncatedResponse");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_bounded_content_length_health_response() {
        validate_health_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").unwrap();
    }

    #[test]
    fn rejects_ambiguous_or_truncated_health_framing() {
        for response in [
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n".as_slice(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nok".as_slice(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nok".as_slice(),
        ] {
            assert!(validate_health_response(response).is_err());
        }
    }

    #[test]
    fn rejects_oversized_headers() {
        let mut response = b"HTTP/1.1 200 OK\r\nX-Header: ".to_vec();
        response.resize(MAX_HEALTH_HEADERS + 1, b'a');
        response.extend_from_slice(b"\r\n\r\n");
        assert_eq!(
            validate_health_response(&response).unwrap_err(),
            "responseLimit"
        );
    }
}
