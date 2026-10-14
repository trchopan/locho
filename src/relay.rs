use anyhow::{bail, Context, Result};
use iroh::{
    endpoint::{presets, Builder},
    Endpoint,
};
pub(crate) use locho_core::relay::RelaySettings;
use locho_core::relay::{validate_token, RelayOptions};
use serde::Deserialize;
use std::path::Path;

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

fn parse(contents: &str) -> Result<RelaySettings> {
    // TOML errors can include source excerpts containing credentials.
    let file: RelayFile = toml::from_str(contents)
        .map_err(|_| anyhow::anyhow!("failed to parse relay configuration"))?;
    let options = file
        .relays
        .into_iter()
        .enumerate()
        .map(|(index, entry)| {
            let auth_token = entry
                .token_env
                .as_deref()
                .map(|name| read_token(name, index + 1))
                .transpose()?;
            Ok(RelayOptions {
                url: entry.url,
                auth_token,
                quic_address_discovery: entry.quic_address_discovery,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(RelaySettings::new(file.include_n0_relays, options)?)
}

pub(crate) fn load(path: Option<&Path>) -> Result<Option<RelaySettings>> {
    path.map(|path| {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read relay configuration {}", path.display()))?;
        parse(&contents)
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
    if validate_token(&token).is_err() {
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
        let settings = parse(
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
        let settings = parse(
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
        let settings = parse(
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
        let error = parse("include_n0_relays = false").unwrap_err();
        assert!(error.to_string().contains("at least one relay"));
    }

    #[test]
    fn rejects_non_base_relay_urls() {
        for value in [
            "http://relay.example.org",
            "https://relay.example.org/relay",
            "https://relay.example.org?token=x",
        ] {
            let error = parse(&format!("[[relays]]\nurl = \"{value}\"")).unwrap_err();
            assert!(!error.to_string().is_empty());
        }
    }

    #[test]
    fn malformed_configuration_does_not_echo_source_credentials() {
        let error = parse("[[relays]]\nurl = \"https://private-secret@relay.example.org\"\nunknown = \"private-secret\"").unwrap_err();
        assert!(!format!("{error:#} {error:?}").contains("private-secret"));
    }
}
