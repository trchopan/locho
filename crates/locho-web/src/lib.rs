use iroh::{endpoint::presets, Endpoint, EndpointAddr, EndpointId, RelayUrl};
use locho_core::{
    capability,
    protocol::MAX_BODY_LEN,
    session::{PeerSession, SessionError},
};
use n0_future::time::{timeout, Duration};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WebClient {
    session: Arc<PeerSession>,
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
) -> Result<WebClient, JsValue> {
    let endpoint = Endpoint::builder(presets::N0)
        .bind()
        .await
        .map_err(|_| JsValue::from_str("endpointBind"))?;
    let id: EndpointId = host_id
        .parse()
        .map_err(|_| JsValue::from_str("invalidHostId"))?;
    let mut target = EndpointAddr::new(id);
    if let Some(relay) = peer_relay_url {
        let relay: RelayUrl = relay
            .parse()
            .map_err(|_| JsValue::from_str("invalidPeerRelay"))?;
        target = target.with_relay_url(relay);
    }
    let (session, driver) = PeerSession::start(endpoint, target);
    let session = Arc::new(session);
    wasm_bindgen_futures::spawn_local(driver.run());
    match timeout(
        Duration::from_secs(15),
        session.wait_connected(Duration::from_secs(15)),
    )
    .await
    {
        Ok(Ok(())) => Ok(WebClient { session }),
        Ok(Err(error)) => {
            let _ = session.close().await;
            Err(map_error(error))
        }
        Err(_) => {
            let _ = session.close().await;
            Err(JsValue::from_str("startupTimeout"))
        }
    }
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
    pub async fn health(&self, capability: String, authority: String) -> Result<String, JsValue> {
        let capability =
            capability::parse(&capability).map_err(|_| JsValue::from_str("invalidCapability"))?;
        let cancel = CancellationToken::new();
        let mut stream = self
            .session
            .open_tcp(&capability, Duration::from_secs(15), &cancel)
            .await
            .map_err(map_error)?;
        let request =
            format!("GET /health HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
        timeout(Duration::from_secs(15), async {
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
                if response.len() + count > MAX_BODY_LEN {
                    return Err(JsValue::from_str("responseLimit"));
                }
                response.extend_from_slice(&buffer[..count]);
            }
            String::from_utf8(response).map_err(|_| JsValue::from_str("invalidResponse"))
        })
        .await
        .map_err(|_| JsValue::from_str("timeout"))?
    }

    pub async fn close(&self) -> Result<(), JsValue> {
        self.session.close().await.map_err(map_error)
    }
}
