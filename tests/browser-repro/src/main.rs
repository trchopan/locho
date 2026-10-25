//! Loopback-only relay fixture reusing the production host implementation.
//! Not a release binary and not a substitute for testing the published CLI.
#![allow(dead_code, unused_imports)] // Reused production modules also expose CLI-only helpers.
#[path = "../../../src/auth.rs"]
mod auth;
#[path = "../../../src/config.rs"]
mod config;
#[path = "../../../src/host.rs"]
mod host;
#[path = "../../../src/http_utils.rs"]
mod http_utils;
#[path = "../../../src/state.rs"]
mod state;
#[path = "../../../src/task.rs"]
mod task;
use locho_core::protocol;

mod relay {
    use anyhow::{bail, Result};
    use iroh::{endpoint::presets, Endpoint, RelayMode, RelayUrl};
    use std::{path::Path, sync::OnceLock};

    pub type RelaySettings = ();
    pub static LOCAL_RELAY: OnceLock<RelayUrl> = OnceLock::new();

    pub fn load(path: Option<&Path>) -> Result<Option<RelaySettings>> {
        if path.is_some() {
            bail!("fixture does not accept external relay configuration");
        }
        Ok(None)
    }

    pub fn endpoint_builder(_: Option<&RelaySettings>) -> iroh::endpoint::Builder {
        let config = iroh::RelayConfig::new(LOCAL_RELAY.get().unwrap().clone(), None);
        Endpoint::builder(presets::Minimal)
            .clear_ip_transports()
            .relay_mode(RelayMode::Custom([config].into_iter().collect()))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Only disposable protocol/transport traces. The launcher captures stderr
    // privately and scans it before retaining any diagnostics.
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "locho_browser_repro=debug,locho_core=debug".into()),
        )
        .init();
    let mut args = std::env::args().skip(1);
    let config = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("expected disposable host config"))?;
    let relay_file = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("expected relay manifest path"))?;
    let host_relay = args.next();
    let browser_relay = args.next();
    let layout = args.next();
    anyhow::ensure!(
        layout
            .as_deref()
            .is_none_or(|layout| matches!(layout, "same" | "split")),
        "invalid layout"
    );
    if args.next().is_some() {
        anyhow::bail!("unexpected argument");
    }
    let mut server_config = iroh_relay::server::ServerConfig::default();
    server_config.relay = Some(iroh_relay::server::RelayConfig::new(
        "127.0.0.1:0".parse::<std::net::SocketAddr>()?,
    ));
    let server = iroh_relay::server::Server::spawn(server_config).await?;
    let url: iroh::RelayUrl = format!("http://{}/", server.http_addr().unwrap()).parse()?;
    fn loopback(value: &str) -> anyhow::Result<iroh::RelayUrl> {
        let parsed = url::Url::parse(value)?;
        anyhow::ensure!(
            parsed.scheme() == "http"
                && matches!(
                    parsed.host_str(),
                    Some("127.0.0.1" | "localhost" | "localhost.")
                )
                && parsed.port().is_some_and(|port| port > 0)
                && parsed.path() == "/"
                && parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.query().is_none()
                && parsed.fragment().is_none(),
            "invalid loopback proxy"
        );
        Ok(value.parse()?)
    }
    let effective_host = host_relay
        .as_deref()
        .map(loopback)
        .transpose()?
        .unwrap_or_else(|| url.clone());
    let effective_browser = browser_relay
        .as_deref()
        .map(loopback)
        .transpose()?
        .unwrap_or_else(|| effective_host.clone());
    let second = if layout
        .as_deref()
        .map_or(effective_host != effective_browser, |layout| {
            layout == "split"
        }) {
        let mut config = iroh_relay::server::ServerConfig::default();
        config.relay = Some(iroh_relay::server::RelayConfig::new(
            "127.0.0.1:0".parse::<std::net::SocketAddr>()?,
        ));
        Some(iroh_relay::server::Server::spawn(config).await?)
    } else {
        None
    };
    let browser_backend = match second.as_ref() {
        Some(server) => format!("http://{}/", server.http_addr().unwrap()),
        None => url.to_string(),
    };
    relay::LOCAL_RELAY.set(effective_host.clone()).unwrap();
    std::fs::write(
        relay_file,
        serde_json::to_vec(
            &serde_json::json!({ "relayUrl": effective_host.as_str(), "homeRelayUrl": effective_browser.as_str(),
            "backendRelayUrl": url.as_str(), "browserBackendRelayUrl": browser_backend }),
        )?,
    )?;
    // host::run is the production implementation; only endpoint construction
    // above differs (one local relay, no public discovery/publisher).
    let result = host::run(config.into(), None, None).await;
    if let Some(server) = second {
        server.shutdown().await?;
    }
    server.shutdown().await?;
    result
}
