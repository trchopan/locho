//! Developer-only loopback transport fixture. Not exported in normal SDK builds.
use super::*;
use iroh::address_lookup::{AddressLookup, EndpointInfo, Error, Item};
use n0_future::boxed::BoxStream;
use std::io::Write;

thread_local! {
    static CANONICAL_POLICY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Synchronous private control. Both variants still use the public JS connect
/// path; no wrapper, cached initialization await or cancellation path differs.
#[wasm_bindgen]
pub fn configure_repro_client_policy(canonical: bool) {
    CANONICAL_POLICY.set(canonical);
}

pub fn candidate_builder(builder: iroh::endpoint::Builder) -> iroh::endpoint::Builder {
    if CANONICAL_POLICY.get() {
        builder.relay_mode(addressing::canonical_default_relay_mode())
    } else {
        builder
    }
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = globalThis, js_name = __lochoReproTrace)]
    fn emit_trace(line: &str) -> Result<(), JsValue>;
}

#[derive(Default)]
struct TraceWriter {
    bytes: Vec<u8>,
    dropped: usize,
}
impl Write for TraceWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let count = bytes.len().min(8192usize.saturating_sub(self.bytes.len()));
        self.bytes.extend_from_slice(&bytes[..count]);
        self.dropped += bytes.len() - count;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Drop for TraceWriter {
    fn drop(&mut self) {
        let mut line = String::from_utf8_lossy(&self.bytes).into_owned();
        if self.dropped > 0 {
            line.push_str(&format!(" [trace_line_truncated bytes={}]", self.dropped));
        }
        let _ = emit_trace(&line);
    }
}

/// Install only when the private reproduction harness explicitly requests it.
#[wasm_bindgen]
pub fn initialize_repro_tracing(transport: bool) {
    let filter = if transport {
        "locho_core=debug,locho_web=debug,iroh=debug,iroh_relay=debug,noq=debug,noq_proto::connection=trace"
    } else {
        "locho_core=debug,locho_web=debug"
    };
    let _ = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(TraceWriter::default)
        .with_env_filter(filter)
        .try_init();
}

#[wasm_bindgen]
impl WebClient {
    /// Called by the private harness only AFTER an operation fails, before close.
    /// Aggregate counters are not packet/stream delivery acknowledgments.
    pub fn repro_stats(&self) -> String {
        let Some(stats) = self.session.diagnostic_connection_stats() else {
            return "null".into();
        };
        serde_json::json!({
            "datagramsTx": stats.udp_tx.datagrams, "datagramsRx": stats.udp_rx.datagrams,
            "bytesTx": stats.udp_tx.bytes, "bytesRx": stats.udp_rx.bytes,
            "streamFramesTx": stats.frame_tx.stream, "streamFramesRx": stats.frame_rx.stream,
            "acksTx": stats.frame_tx.acks, "acksRx": stats.frame_rx.acks,
            "pathAcksTx": stats.frame_tx.path_acks, "pathAcksRx": stats.frame_rx.path_acks,
            "pathAbandonTx": stats.frame_tx.path_abandon, "pathAbandonRx": stats.frame_rx.path_abandon,
            "resetsTx": stats.frame_tx.reset_stream, "resetsRx": stats.frame_rx.reset_stream,
            "stopSendingTx": stats.frame_tx.stop_sending, "stopSendingRx": stats.frame_rx.stop_sending,
            "sentPackets": stats.sent_packets, "lostPackets": stats.lost_packets,
            "lostBytes": stats.lost_bytes,
            "paths": self.session.diagnostic_connection_paths(),
        }).to_string()
    }
}

pub async fn trace_online(endpoint: &Endpoint) {
    if tracing::enabled!(target: "locho_web::repro", tracing::Level::DEBUG) {
        let online = n0_future::future::poll_once(endpoint.online())
            .await
            .is_some();
        tracing::debug!(stage = "connect_return", endpoint = %endpoint.id().fmt_short(), online, "Locho browser setup");
    }
}

#[derive(Clone, Debug)]
struct ControlledLookup {
    peer: EndpointId,
    relay: RelayUrl,
    delay_ms: u32,
}
impl AddressLookup for ControlledLookup {
    fn resolve(&self, peer: EndpointId) -> Option<BoxStream<Result<Item, Error>>> {
        if peer != self.peer {
            return None;
        }
        let lookup = self.clone();
        Some(Box::pin(n0_future::stream::once_future(async move {
            n0_future::time::sleep(Duration::from_millis(lookup.delay_ms.into())).await;
            Ok(Item::new(
                EndpointInfo::new(peer).with_relay_url(lookup.relay),
                "controlled-fixture",
                None,
            ))
        })))
    }
}

#[wasm_bindgen]
pub async fn connect_repro(
    host_id: String,
    relay_url: String,
    home_relay_url: String,
    stale_hint: bool,
    lookup_delay_ms: u32,
    canonical_home: bool,
    cancellation: &WebCancellation,
) -> Result<WebClient, JsValue> {
    let url = url::Url::parse(&relay_url).map_err(|_| JsValue::from_str("invalidReproRelay"))?;
    if url.scheme() != "http"
        || !matches!(
            url.host_str(),
            Some("127.0.0.1" | "localhost" | "localhost.")
        )
        || !url.port().is_some_and(|port| port > 0)
        || url.path() != "/"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || lookup_delay_ms > 10_000
    {
        return Err(JsValue::from_str("invalidReproRelay"));
    }
    let peer = host_id
        .parse()
        .map_err(|_| JsValue::from_str("invalidHostId"))?;
    let relay: RelayUrl = relay_url
        .parse()
        .map_err(|_| JsValue::from_str("invalidReproRelay"))?;
    let home =
        url::Url::parse(&home_relay_url).map_err(|_| JsValue::from_str("invalidReproRelay"))?;
    if home.scheme() != "http"
        || !matches!(
            home.host_str(),
            Some("127.0.0.1" | "localhost" | "localhost.")
        )
        || !home.port().is_some_and(|port| port > 0)
        || home.path() != "/"
        || !home.username().is_empty()
        || home.password().is_some()
        || home.query().is_some()
        || home.fragment().is_some()
    {
        return Err(JsValue::from_str("invalidReproRelay"));
    }
    let home: RelayUrl = home_relay_url
        .parse()
        .map_err(|_| JsValue::from_str("invalidReproRelay"))?;
    let home = if canonical_home {
        addressing::relay_url(home)
    } else {
        home
    };
    let relay_config = iroh::RelayConfig::new(home, None);
    let builder = Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Custom(
            [relay_config].into_iter().collect(),
        ))
        .address_lookup(ControlledLookup {
            peer,
            relay,
            delay_ms: lookup_delay_ms,
        });
    #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
    let builder = builder.clear_ip_transports();
    let hint = if stale_hint {
        "https://127.0.0.1:1/".into()
    } else {
        relay_url
    };
    super::connect_using_builder(host_id, vec![hint], cancellation, builder).await
}
