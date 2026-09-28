use anyhow::{bail, Context, Result};
use http::HeaderValue;
use iroh::{
    endpoint::{default_relay_mode, presets, Builder},
    Endpoint, RelayConfig, RelayMap, RelayMode, RelayUrl,
};
use serde::Deserialize;
use std::{collections::HashSet, path::Path, sync::Arc};
use url::Url;

const DEFAULT_QUIC_ADDRESS_DISCOVERY: bool = true;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelayFile {
    #[serde(default)]
    include_n0_relays: bool,
    #[serde(default)]
    relays: Vec<RelayEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelayEntry {
    url: String,
    #[serde(default)]
    token_env: Option<String>,
    #[serde(default = "default_quic_address_discovery")]
    quic_address_discovery: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ConfiguredRelay {
    pub(crate) url: RelayUrl,
    pub(crate) quic_address_discovery: bool,
}

#[derive(Clone)]
pub(crate) struct RelaySettings {
    mode: RelayMode,
    relays: Vec<ConfiguredRelay>,
    include_n0_relays: bool,
}

impl std::fmt::Debug for RelaySettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RelaySettings")
            .field("relays", &self.relays)
            .field("include_n0_relays", &self.include_n0_relays)
            .finish_non_exhaustive()
    }
}

impl RelaySettings {
    fn parse(contents: &str) -> Result<Self> {
        let file: RelayFile =
            toml::from_str(contents).context("failed to parse relay configuration")?;
        let mut urls = HashSet::new();
        let custom_map = RelayMap::empty();
        let mut relays = Vec::with_capacity(file.relays.len());

        for (index, entry) in file.relays.into_iter().enumerate() {
            let url = parse_relay_url(&entry.url)
                .with_context(|| format!("invalid relay URL at relays[{}]", index + 1))?;
            if !urls.insert(url.clone()) {
                bail!("duplicate relay URL {:?}", url.as_str());
            }

            let token = entry
                .token_env
                .as_deref()
                .map(|name| read_token(name, index + 1))
                .transpose()?;
            let mut relay_config = if entry.quic_address_discovery {
                RelayConfig::from(url.clone())
            } else {
                RelayConfig::new(url.clone(), None)
            };
            if let Some(token) = token {
                relay_config = relay_config.with_auth_token(token);
            }
            custom_map.insert(url.clone(), Arc::new(relay_config));
            relays.push(ConfiguredRelay {
                url,
                quic_address_discovery: entry.quic_address_discovery,
            });
        }

        if !file.include_n0_relays && custom_map.is_empty() {
            bail!("relay configuration must define at least one relay when include_n0_relays is false")
        }

        let relay_map = if file.include_n0_relays {
            default_relay_mode().relay_map()
        } else {
            RelayMap::empty()
        };
        relay_map.extend(&custom_map);

        Ok(Self {
            mode: RelayMode::Custom(relay_map),
            relays,
            include_n0_relays: file.include_n0_relays,
        })
    }

    pub(crate) fn mode(&self) -> RelayMode {
        self.mode.clone()
    }

    pub(crate) fn report(&self) -> (&[ConfiguredRelay], bool) {
        (&self.relays, self.include_n0_relays)
    }
}

pub(crate) fn load(path: Option<&Path>) -> Result<Option<RelaySettings>> {
    path.map(|path| {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read relay configuration {}", path.display()))?;
        RelaySettings::parse(&contents)
            .with_context(|| format!("relay configuration check failed for {}", path.display()))
    })
    .transpose()
}

pub(crate) fn endpoint_builder(settings: Option<&RelaySettings>) -> Builder {
    let builder = Endpoint::builder(presets::N0);
    match settings {
        Some(settings) => builder.relay_mode(settings.mode()),
        None => builder,
    }
}

fn parse_relay_url(value: &str) -> Result<RelayUrl> {
    let url = Url::parse(value).context("relay URL is not valid")?;
    if url.scheme() != "https" {
        bail!("relay URL must use HTTPS")
    }
    if !url.has_host() {
        bail!("relay URL must include a host")
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("relay URL must not contain credentials")
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        bail!("relay URL must be a base URL without a path, query, or fragment")
    }
    Ok(RelayUrl::from(url))
}

fn read_token(name: &str, index: usize) -> Result<String> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        bail!(
            "relay {} token_env must be a non-empty environment variable name",
            index
        )
    }
    let token = std::env::var(name).with_context(|| {
        format!(
            "relay {} authentication token environment variable is not set",
            index
        )
    })?;
    if token.is_empty() || HeaderValue::from_str(&format!("Bearer {token}")).is_err() {
        bail!(
            "relay {} authentication token is not a valid bearer token",
            index
        )
    }
    Ok(token)
}

fn default_quic_address_discovery() -> bool {
    DEFAULT_QUIC_ADDRESS_DISCOVERY
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn parses_custom_only_relay_configuration() {
        let settings = RelaySettings::parse(
            r#"
                [[relays]]
                url = "https://relay.example.org"
                quic_address_discovery = false
            "#,
        )
        .unwrap();

        let (relays, include_n0_relays) = settings.report();
        assert!(!include_n0_relays);
        assert_eq!(relays.len(), 1);
        assert_eq!(relays[0].url.as_str(), "https://relay.example.org/");
        assert!(!relays[0].quic_address_discovery);
        assert_eq!(settings.mode().relay_map().len(), 1);
    }

    #[test]
    fn merges_custom_relays_with_n0_relays() {
        let settings = RelaySettings::parse(
            r#"
                include_n0_relays = true

                [[relays]]
                url = "https://relay.example.org"
            "#,
        )
        .unwrap();

        let (relays, include_n0_relays) = settings.report();
        assert!(include_n0_relays);
        assert_eq!(relays.len(), 1);
        let relay_map = settings.mode().relay_map();
        assert!(relay_map.contains(&relays[0].url));
        assert!(relay_map
            .urls::<Vec<_>>()
            .iter()
            .any(|url| url != &relays[0].url));
    }

    #[test]
    fn resolves_auth_token_without_leaking_it_in_errors() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("LOCHO_RELAY_TEST_TOKEN", "secret/token=with+base64");
        let settings = RelaySettings::parse(
            r#"
                [[relays]]
                url = "https://relay.example.org"
                token_env = "LOCHO_RELAY_TEST_TOKEN"
            "#,
        )
        .unwrap();
        std::env::remove_var("LOCHO_RELAY_TEST_TOKEN");

        let (relays, _) = settings.report();
        let relay = settings.mode().relay_map().get(&relays[0].url).unwrap();
        assert_eq!(
            relay.auth_token.as_deref(),
            Some("secret/token=with+base64")
        );
        assert!(!format!("{settings:?}").contains("secret/token=with+base64"));
    }

    #[test]
    fn rejects_empty_custom_configuration() {
        let error = RelaySettings::parse("include_n0_relays = false").unwrap_err();
        assert!(error.to_string().contains("at least one relay"));
    }

    #[test]
    fn rejects_non_base_relay_urls() {
        for value in [
            "http://relay.example.org",
            "https://relay.example.org/relay",
            "https://relay.example.org?token=x",
        ] {
            let error =
                RelaySettings::parse(&format!("[[relays]]\nurl = \"{value}\"")).unwrap_err();
            assert!(!error.to_string().is_empty());
        }
    }
}
