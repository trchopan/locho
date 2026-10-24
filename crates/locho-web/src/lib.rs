mod addressing;
mod http;

use ::http::uri::Authority;
use iroh::{
    address_lookup::PkarrPublisher, endpoint::presets, Endpoint, EndpointAddr, EndpointId, RelayUrl,
};
use locho_core::{
    capability,
    session::{PeerSession, SessionError},
};
use n0_future::time::{timeout, Duration};
use std::sync::Arc;
use std::{cell::Cell, rc::Rc};
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
    requests: Arc<tokio::sync::Semaphore>,
    observers: Arc<tokio::sync::Semaphore>,
}

#[wasm_bindgen]
pub struct WebStatusObserver {
    receiver: tokio::sync::watch::Receiver<locho_core::session::SessionStatus>,
    first: bool,
    finished: bool,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

fn status_json(status: &locho_core::session::SessionStatus) -> String {
    let code = |value: Option<SessionError>| {
        value.map(|error| match error {
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
    };
    serde_json::json!({
        "revision": status.revision,
        "state": format!("{:?}", status.state).to_lowercase(),
        "path": format!("{:?}", status.path).to_lowercase(),
        "connectionErrorCode": code(status.last_error),
        "errorCode": code(status.terminal_error),
    })
    .to_string()
}

#[wasm_bindgen]
impl WebStatusObserver {
    pub async fn next(
        &mut self,
        cancellation: &WebCancellation,
    ) -> Result<Option<String>, JsValue> {
        if cancellation.token.is_cancelled() {
            return Ok(None);
        }
        if self.first {
            self.first = false;
            let status = self.receiver.borrow_and_update();
            self.finished = status.state == locho_core::session::SessionState::Closed;
            return Ok(Some(status_json(&status)));
        }
        if self.finished {
            return Ok(None);
        }
        tokio::select! {
            biased;
            _ = cancellation.token.cancelled() => Ok(None),
            result = self.receiver.changed() => {
                if result.is_err() && self.receiver.borrow().state != locho_core::session::SessionState::Closed {
                    return Err(JsValue::from_str("driverStopped"));
                }
                let status = self.receiver.borrow_and_update();
                self.finished = status.state == locho_core::session::SessionState::Closed;
                Ok(Some(status_json(&status)))
            }
        }
    }
}

#[wasm_bindgen]
pub struct WebResponse {
    metadata: String,
    body: Vec<u8>,
}
#[wasm_bindgen]
impl WebResponse {
    pub fn metadata(&self) -> String {
        self.metadata.clone()
    }
    pub fn body(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.body)
    }
}

type BodyResult = Result<Option<Vec<u8>>, &'static str>;
// A pull channel keeps the authorized stream/permit inside a cancellation-aware
// owner task. Shutdown therefore drops paused/unread bodies without requiring JS
// to call next(), while no network body reads occur without a consumer pull.
#[wasm_bindgen]
pub struct WebStreamingResponse {
    metadata: String,
    pulls: tokio::sync::mpsc::Sender<tokio::sync::oneshot::Sender<BodyResult>>,
    cancellation: CancellationToken,
    done: Rc<Cell<bool>>,
    error: Rc<Cell<Option<&'static str>>>,
    finished: tokio::sync::watch::Receiver<bool>,
}

impl Drop for WebStreamingResponse {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

#[wasm_bindgen]
impl WebStreamingResponse {
    pub async fn finish(&self) {
        self.cancellation.cancel();
        let mut finished = self.finished.clone();
        while !*finished.borrow_and_update() {
            if finished.changed().await.is_err() {
                break;
            }
        }
    }
    pub fn metadata(&self) -> String {
        self.metadata.clone()
    }
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
    pub async fn next(&self) -> Result<Option<Vec<u8>>, JsValue> {
        if !self.done.get() {
            let (send, receive) = tokio::sync::oneshot::channel();
            if self.pulls.send(send).await.is_ok() {
                if let Ok(result) = receive.await {
                    return result.map_err(JsValue::from_str);
                }
            }
        }
        match self.error.get() {
            Some(code) => Err(JsValue::from_str(code)),
            None => Ok(None),
        }
    }
}

#[wasm_bindgen]
pub struct WebTicket {
    inner: locho_core::ticket::Ticket,
}

#[wasm_bindgen]
pub fn parse_ticket(value: String) -> Result<WebTicket, JsValue> {
    locho_core::ticket::parse(&value)
        .map(|inner| WebTicket { inner })
        .map_err(|_| JsValue::from_str("invalidTicket"))
}

#[wasm_bindgen]
impl WebTicket {
    pub async fn request_stream(
        &self,
        client: &WebClient,
        options: String,
        body: Vec<u8>,
        idle_timeout_ms: u32,
        cancellation: &WebCancellation,
    ) -> Result<WebStreamingResponse, JsValue> {
        if self.inner.capability().service_type != locho_core::service::ServiceType::Tcp {
            return Err(JsValue::from_str("unsupportedService"));
        }
        let cap = self.inner.capability();
        client
            .request_stream(
                capability::format(&cap.service, &cap.service_type, &cap.secret),
                options,
                body,
                idle_timeout_ms,
                cancellation,
            )
            .await
    }
    pub async fn request(
        &self,
        client: &WebClient,
        options: String,
        body: Vec<u8>,
        cancellation: &WebCancellation,
    ) -> Result<WebResponse, JsValue> {
        if self.inner.capability().service_type != locho_core::service::ServiceType::Tcp {
            return Err(JsValue::from_str("unsupportedService"));
        }
        let cap = self.inner.capability();
        client
            .request(
                capability::format(&cap.service, &cap.service_type, &cap.secret),
                options,
                body,
                cancellation,
            )
            .await
    }
    pub fn metadata(&self) -> String {
        serde_json::json!({
            "hostId": self.inner.host_id(),
            "service": self.inner.capability().service,
            "serviceType": self.inner.capability().service_type.as_str(),
            "peerRelayUrls": self.inner.peer_relay_urls(),
        })
        .to_string()
    }
    pub fn export(&self) -> Result<String, JsValue> {
        self.inner
            .encode()
            .map_err(|_| JsValue::from_str("invalidTicket"))
    }
    pub async fn connect(&self, cancellation: &WebCancellation) -> Result<WebClient, JsValue> {
        if self.inner.capability().service_type != locho_core::service::ServiceType::Tcp {
            return Err(JsValue::from_str("unsupportedService"));
        }
        connect_inner(
            self.inner.host_id().into(),
            self.inner.peer_relay_urls().to_vec(),
            cancellation,
        )
        .await
    }
    pub async fn health(
        &self,
        client: &WebClient,
        authority: String,
        cancellation: &WebCancellation,
    ) -> Result<String, JsValue> {
        let cap = self.inner.capability();
        client
            .health(
                capability::format(&cap.service, &cap.service_type, &cap.secret),
                authority,
                cancellation,
            )
            .await
    }
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
    connect_inner(host_id, peer_relay_url.into_iter().collect(), cancellation).await
}

async fn connect_inner(
    host_id: String,
    peer_relay_urls: Vec<String>,
    cancellation: &WebCancellation,
) -> Result<WebClient, JsValue> {
    if cancellation.token.is_cancelled() {
        return Err(JsValue::from_str("cancelled"));
    }
    let id: EndpointId = host_id
        .parse()
        .map_err(|_| JsValue::from_str("invalidHostId"))?;
    let peer_relays = peer_relay_urls
        .into_iter()
        .map(|relay| {
            relay
                .parse::<RelayUrl>()
                .map(addressing::relay_url)
                .map_err(|_| JsValue::from_str("invalidPeerRelay"))
        })
        .collect::<Result<Vec<_>, _>>()?;
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
    for relay in peer_relays {
        target = target.with_relay_url(relay);
    }
    let started = n0_future::time::Instant::now();
    let target = tokio::select! {
        biased;
        // Retain ownership for the normal session shutdown path below.
        _ = cancellation.token.cancelled() => target.clone(),
        target = addressing::initial_target(&endpoint, target.clone()) => target,
    };
    // Address refresh consumes the existing startup budget, not extra time.
    let remaining = Duration::from_secs(15).saturating_sub(started.elapsed());
    let (session, driver) = PeerSession::start(endpoint, target);
    let session = Arc::new(session);
    wasm_bindgen_futures::spawn_local(driver.run());
    let connected = tokio::select! {
        biased;
        _ = cancellation.token.cancelled() => Err(JsValue::from_str("cancelled")),
        result = timeout(
        remaining,
        session.wait_connected(remaining),
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
        requests: Arc::new(tokio::sync::Semaphore::new(16)),
        observers: Arc::new(tokio::sync::Semaphore::new(16)),
    })
}

#[wasm_bindgen]
impl WebClient {
    pub async fn request_stream(
        &self,
        capability: String,
        options: String,
        body: Vec<u8>,
        idle_timeout_ms: u32,
        cancellation: &WebCancellation,
    ) -> Result<WebStreamingResponse, JsValue> {
        if self.shutdown.is_cancelled() {
            return Err(JsValue::from_str("closed"));
        }
        if cancellation.token.is_cancelled() {
            return Err(JsValue::from_str("cancelled"));
        }
        if !(1..=300_000).contains(&idle_timeout_ms) {
            return Err(JsValue::from_str("invalidTimeout"));
        }
        if options.len() > 128 * 1024 {
            return Err(JsValue::from_str("requestLimit"));
        }
        let request: http::Request =
            serde_json::from_str(&options).map_err(|_| JsValue::from_str("invalidRequest"))?;
        let bytes = http::encode(&request, &body).map_err(JsValue::from_str)?;
        let cap =
            capability::parse(&capability).map_err(|_| JsValue::from_str("invalidCapability"))?;
        let permit = self
            .requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| JsValue::from_str("resourceLimit"))?;
        let duration = Duration::from_millis(request.timeout_ms.into());
        let operation = timeout(duration, async {
            let mut stream = self
                .session
                .open_tcp(&cap, duration, &cancellation.token)
                .await
                .map_err(map_error)?;
            stream
                .write_all(&bytes)
                .await
                .map_err(|_| JsValue::from_str("transport"))?;
            http::head(stream, &request.method)
                .await
                .map_err(JsValue::from_str)
        });
        let (response, mut body) = tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => return Err(JsValue::from_str("closed")),
            _ = cancellation.token.cancelled() => return Err(JsValue::from_str("cancelled")),
            result = operation => result.map_err(|_| JsValue::from_str("setupTimeout"))??,
        };
        let metadata =
            serde_json::to_string(&response).map_err(|_| JsValue::from_str("internal"))?;
        let (pulls, mut requests) =
            tokio::sync::mpsc::channel::<tokio::sync::oneshot::Sender<BodyResult>>(1);
        let cancel = cancellation.token.clone();
        let shutdown = self.shutdown.clone();
        let done = Rc::new(Cell::new(false));
        let error = Rc::new(Cell::new(None));
        let (finished, receiver) = tokio::sync::watch::channel(false);
        let result = WebStreamingResponse {
            metadata,
            pulls,
            cancellation: cancel.clone(),
            done: done.clone(),
            error: error.clone(),
            finished: receiver,
        };
        wasm_bindgen_futures::spawn_local(async move {
            let _permit = permit;
            let run = async {
                while !body.empty() {
                    let Some(reply) = requests.recv().await else {
                        return Ok(());
                    };
                    let next = body.next(Some(idle_timeout_ms), false).await;
                    let finished = !matches!(next, Ok(Some(_)));
                    let failure = next.as_ref().err().copied();
                    let _ = reply.send(next);
                    if let Some(code) = failure {
                        return Err(code);
                    }
                    if finished {
                        break;
                    }
                }
                Ok(())
            };
            let outcome = tokio::select! {
                biased;
                _ = shutdown.cancelled() => Err("closed"),
                _ = cancel.cancelled() => Err("cancelled"),
                result = run => result,
            };
            error.set(outcome.err());
            done.set(true);
            drop(body);
            drop(_permit);
            let _ = finished.send(true);
        });
        Ok(result)
    }
    pub fn observe_status(&self) -> Result<WebStatusObserver, JsValue> {
        let permit = self
            .observers
            .clone()
            .try_acquire_owned()
            .map_err(|_| JsValue::from_str("resourceLimit"))?;
        Ok(WebStatusObserver {
            receiver: self.session.subscribe(),
            first: true,
            finished: false,
            _permit: permit,
        })
    }
    pub async fn request(
        &self,
        capability: String,
        options: String,
        body: Vec<u8>,
        cancellation: &WebCancellation,
    ) -> Result<WebResponse, JsValue> {
        if self.shutdown.is_cancelled() {
            return Err(JsValue::from_str("closed"));
        }
        if cancellation.token.is_cancelled() {
            return Err(JsValue::from_str("cancelled"));
        }
        if options.len() > 128 * 1024 {
            return Err(JsValue::from_str("requestLimit"));
        }
        let request: http::Request =
            serde_json::from_str(&options).map_err(|_| JsValue::from_str("invalidRequest"))?;
        let bytes = http::encode(&request, &body).map_err(JsValue::from_str)?;
        let cap =
            capability::parse(&capability).map_err(|_| JsValue::from_str("invalidCapability"))?;
        let _permit = self
            .requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| JsValue::from_str("resourceLimit"))?;
        let duration = Duration::from_millis(request.timeout_ms.into());
        let operation = timeout(duration, async {
            let mut stream = self
                .session
                .open_tcp(&cap, duration, &cancellation.token)
                .await
                .map_err(map_error)?;
            stream
                .write_all(&bytes)
                .await
                .map_err(|_| JsValue::from_str("transport"))?;
            let response = http::read(&mut stream, &request.method)
                .await
                .map_err(JsValue::from_str)?;
            let metadata =
                serde_json::to_string(&response).map_err(|_| JsValue::from_str("internal"))?;
            Ok(WebResponse {
                metadata,
                body: response.body,
            })
        });
        tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => Err(JsValue::from_str("closed")),
            _ = cancellation.token.cancelled() => Err(JsValue::from_str("cancelled")),
            result = operation => result.map_err(|_| JsValue::from_str("timeout"))?,
        }
    }
    pub fn status(&self) -> String {
        status_json(&self.session.snapshot())
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

    #[tokio::test]
    async fn status_observer_cancels_without_status_change_and_releases_capacity() {
        let (sender, receiver) =
            tokio::sync::watch::channel(locho_core::session::SessionStatus::default());
        let permits = Arc::new(tokio::sync::Semaphore::new(1));
        let mut observer = WebStatusObserver {
            receiver,
            first: true,
            finished: false,
            _permit: permits.clone().try_acquire_owned().unwrap(),
        };
        let cancel = WebCancellation::new();
        let first = observer.next(&cancel).await.unwrap().unwrap();
        assert!(first.contains("\"state\":\"starting\""));
        let waiting = observer.next(&cancel);
        let cancel_work = async {
            tokio::task::yield_now().await;
            cancel.cancel();
        };
        let (result, _) = tokio::join!(waiting, cancel_work);
        assert!(result.unwrap().is_none());
        assert_eq!(permits.available_permits(), 0);
        drop(observer);
        assert_eq!(permits.available_permits(), 1);
        drop(sender);
    }

    #[tokio::test]
    async fn status_observer_coalesces_and_preserves_terminal_error() {
        use locho_core::session::{SessionState, SessionStatus};
        let (sender, receiver) = tokio::sync::watch::channel(SessionStatus::default());
        let permits = Arc::new(tokio::sync::Semaphore::new(1));
        let mut observer = WebStatusObserver {
            receiver,
            first: true,
            finished: false,
            _permit: permits.try_acquire_owned().unwrap(),
        };
        let cancel = WebCancellation::new();
        observer.next(&cancel).await.unwrap();
        sender.send_modify(|s| {
            s.revision = 1;
            s.state = SessionState::Closing;
        });
        sender.send_modify(|s| {
            s.revision = 2;
            s.state = SessionState::Closed;
            s.terminal_error = Some(SessionError::DriverStopped);
        });
        let value: serde_json::Value =
            serde_json::from_str(&observer.next(&cancel).await.unwrap().unwrap()).unwrap();
        assert_eq!(value["revision"], 2);
        assert_eq!(value["errorCode"], "driverStopped");
        assert!(observer.next(&cancel).await.unwrap().is_none());
    }

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
