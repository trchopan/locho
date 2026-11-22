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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RelayFile {
    #[serde(default)]
    include_n0_relays: bool,
    #[serde(default)]
    relays: Vec<RelayEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RelayEntry {
    url: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    token_env: Option<String>,
    #[serde(default = "default_quic_address_discovery")]
    quic_address_discovery: bool,
}

fn parse(contents: &str) -> Result<(RelaySettings, bool)> {
    // TOML errors can include source excerpts containing credentials.
    let file: RelayFile = toml::from_str(contents)
        .map_err(|_| anyhow::anyhow!("failed to parse relay configuration"))?;
    let mut has_inline_token = false;
    let options = file
        .relays
        .into_iter()
        .enumerate()
        .map(|(index, entry)| {
            let relay_index = index + 1;
            let auth_token = match (entry.token, entry.token_env.as_deref()) {
                (Some(_), Some(_)) => {
                    bail!("relay {} cannot set both token and token_env", relay_index)
                }
                (Some(token), None) => {
                    has_inline_token = true;
                    Some(validate_inline_token(token, relay_index)?)
                }
                (None, Some(name)) => Some(read_token(name, relay_index)?),
                (None, None) => None,
            };
            Ok(RelayOptions {
                url: entry.url,
                auth_token,
                quic_address_discovery: entry.quic_address_discovery,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((
        RelaySettings::new(file.include_n0_relays, options)?,
        has_inline_token,
    ))
}

pub(crate) fn load(path: Option<&Path>) -> Result<Option<RelaySettings>> {
    path.map(|path| {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read relay configuration {}", path.display()))?;
        let (settings, has_inline_token) = parse(&contents)
            .with_context(|| format!("relay configuration check failed for {}", path.display()))?;
        if has_inline_token {
            require_private_permissions(path).with_context(|| {
                format!("relay configuration check failed for {}", path.display())
            })?;
        }
        Ok(settings)
    })
    .transpose()
}

fn require_private_permissions(_path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let metadata = std::fs::metadata(_path)
            .with_context(|| format!("failed to inspect {}", _path.display()))?;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            bail!(
                "relay configuration with inline token has overly broad permissions ({mode:04o}); require owner-only permissions (0600)"
            );
        }
    }
    Ok(())
}

fn validate_inline_token(token: String, index: usize) -> Result<String> {
    if validate_token(&token).is_err() {
        bail!(
            "relay {} authentication token is not a valid bearer token",
            index
        )
    }
    Ok(token)
}

pub(crate) fn endpoint_builder(settings: Option<&RelaySettings>) -> Builder {
    let mut builder = Endpoint::builder(presets::N0);
    if std::env::var("LOCHO_DNS_EAGER_FALLBACK").as_deref() == Ok("1") {
        // The outer iroh lookup deadline can expire before an unreachable
        // system DNS tier finishes. This opt-in races the existing fallback
        // providers; it does not change system DNS or the TLS trust policy.
        let resolver = crate::dns::resilient_resolver();
        tracing::info!("DNS eager fallback enabled for this process");
        builder = builder.dns_resolver(resolver);
    }
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

    #[tokio::test]
    async fn eager_dns_answers_before_unreachable_primary_deadline() {
        use iroh::dns::{DnsResolver, NameserverConfig};
        use tokio::{
            net::UdpSocket,
            time::{timeout, Duration},
        };
        let mut primaries = Vec::new();
        for _ in 0..4 {
            primaries.push(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        }
        let primary_configs: Vec<_> = primaries
            .iter()
            .map(|socket| {
                let address = socket.local_addr().unwrap();
                NameserverConfig::udp(address.ip()).with_port(address.port())
            })
            .collect();
        let fallback = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let fallback_addr = fallback.local_addr().unwrap();
        let responder = tokio::spawn(async move {
            let mut packet = [0u8; 512];
            let (size, peer) = fallback.recv_from(&mut packet).await.unwrap();
            let mut end = 12;
            while packet[end] != 0 {
                end += 1 + packet[end] as usize;
            }
            end += 5;
            assert!(end <= size);
            let mut response = packet[..end].to_vec();
            response[2..12].copy_from_slice(&[0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0]);
            response.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 127, 0, 0, 42]);
            fallback.send_to(&response, peer).await.unwrap();
        });
        let builder = DnsResolver::builder()
            .add_nameserver_configs(primary_configs.clone())
            .fallback_nameserver_configs([
                NameserverConfig::udp(fallback_addr.ip()).with_port(fallback_addr.port())
            ]);
        // Reproduce the original failure: an unreachable primary remains
        // pending beyond the outer deadline, preventing deferred fallback.
        let deferred = builder.clone().build();
        assert!(deferred
            .lookup_ipv4("hardening.test", Duration::from_millis(250))
            .await
            .is_err());
        // Merging tiers also fails: three dead servers occupy all query slots.
        let merged = builder
            .with_fallback_mode(iroh::dns::FallbackMode::Eager)
            .build();
        assert!(merged
            .lookup_ipv4("hardening.test", Duration::from_millis(500))
            .await
            .is_err());
        let system = DnsResolver::builder()
            .add_nameserver_configs(primary_configs)
            .disable_fallback();
        let public = DnsResolver::builder()
            .add_nameserver_config(
                NameserverConfig::udp(fallback_addr.ip()).with_port(fallback_addr.port()),
            )
            .disable_fallback();
        let resolver = crate::dns::racing_resolver(system, public);
        let result = timeout(
            Duration::from_secs(1),
            resolver.lookup_ipv4("hardening.test", Duration::from_secs(1)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            result.collect::<Vec<_>>(),
            vec![std::net::Ipv4Addr::new(127, 0, 0, 42)]
        );
        responder.await.unwrap();
    }

    #[test]
    fn parses_custom_only_relay_configuration() {
        let (settings, has_inline_token) = parse(
            r#"
                [[relays]]
                url = "https://relay.example.org"
                quic_address_discovery = false
            "#,
        )
        .unwrap();

        assert!(!has_inline_token);
        let (relays, include_n0_relays) = settings.report();
        assert!(!include_n0_relays);
        assert_eq!(relays.len(), 1);
        assert_eq!(relays[0].url.as_str(), "https://relay.example.org/");
        assert!(!relays[0].quic_address_discovery);
        assert_eq!(settings.mode().relay_map().len(), 1);
    }

    #[test]
    fn merges_custom_relays_with_n0_relays() {
        let (settings, has_inline_token) = parse(
            r#"
                include_n0_relays = true

                [[relays]]
                url = "https://relay.example.org"
            "#,
        )
        .unwrap();

        assert!(!has_inline_token);
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
        let (settings, has_inline_token) = parse(
            r#"
                [[relays]]
                url = "https://relay.example.org"
                token_env = "LOCHO_RELAY_TEST_TOKEN"
            "#,
        )
        .unwrap();
        std::env::remove_var("LOCHO_RELAY_TEST_TOKEN");

        assert!(!has_inline_token);
        let (relays, _) = settings.report();
        let relay = settings.mode().relay_map().get(&relays[0].url).unwrap();
        assert_eq!(
            relay.auth_token.as_deref(),
            Some("secret/token=with+base64")
        );
        assert!(!format!("{settings:?}").contains("secret/token=with+base64"));
    }

    #[test]
    fn resolves_inline_token_and_redacts_secrets_in_debug_and_errors() {
        let (settings, has_inline_token) = parse(
            r#"
                [[relays]]
                url = "https://relay.example.org"
                token = "inline-secret/token=with+base64"
            "#,
        )
        .unwrap();

        assert!(has_inline_token);
        let (relays, _) = settings.report();
        let relay = settings.mode().relay_map().get(&relays[0].url).unwrap();
        assert_eq!(
            relay.auth_token.as_deref(),
            Some("inline-secret/token=with+base64")
        );
        assert!(!format!("{settings:?}").contains("inline-secret/token=with+base64"));

        for invalid_token in ["", "inline-secret\r\n"] {
            let error = parse(&format!(
                "[[relays]]\nurl = \"https://relay.example.org\"\ntoken = {invalid_token:?}\n"
            ))
            .unwrap_err();
            let rendered = format!("{error:#} {error:?}");
            assert!(rendered.contains("relay 1 authentication token is not a valid bearer token"));
            assert!(!rendered.contains("inline-secret"));
        }
    }

    #[test]
    fn rejects_conflicting_inline_token_and_token_env_without_leaking_secrets() {
        let error = parse(
            r#"
                [[relays]]
                url = "https://relay.example.org"
                token = "inline-secret-value"
                token_env = "LOCHO_SECRET_ENV_NAME"
            "#,
        )
        .unwrap_err();
        let rendered = format!("{error:#} {error:?}");
        assert!(rendered.contains("relay 1 cannot set both token and token_env"));
        assert!(!rendered.contains("inline-secret-value"));
        assert!(!rendered.contains("LOCHO_SECRET_ENV_NAME"));
    }

    #[test]
    fn load_accepts_private_inline_token_file_and_enforces_unix_permissions() {
        let path = std::env::temp_dir().join(format!(
            "locho-relay-inline-{}-{}.toml",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::write(
            &path,
            "[[relays]]\nurl = \"https://relay.example.org\"\ntoken = \"inline-file-secret\"\n",
        )
        .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            let error = load(Some(&path)).unwrap_err();
            let rendered = format!("{error:#} {error:?}");
            assert!(rendered.contains("overly broad permissions"));
            assert!(rendered.contains("0600"));
            assert!(!rendered.contains("inline-file-secret"));

            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        let settings = load(Some(&path)).unwrap().unwrap();
        let _ = std::fs::remove_file(&path);
        let (relays, _) = settings.report();
        let relay = settings.mode().relay_map().get(&relays[0].url).unwrap();
        assert_eq!(relay.auth_token.as_deref(), Some("inline-file-secret"));
        assert!(!format!("{settings:?}").contains("inline-file-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn load_allows_non_inline_token_env_file_with_standard_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("LOCHO_RELAY_MODE_TEST_TOKEN", "env-file-secret");
        let path = std::env::temp_dir().join(format!(
            "locho-relay-env-{}-{}.toml",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::write(
            &path,
            "[[relays]]\nurl = \"https://relay.example.org\"\ntoken_env = \"LOCHO_RELAY_MODE_TEST_TOKEN\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let settings = load(Some(&path)).unwrap().unwrap();
        let _ = std::fs::remove_file(&path);
        std::env::remove_var("LOCHO_RELAY_MODE_TEST_TOKEN");

        let (relays, _) = settings.report();
        let relay = settings.mode().relay_map().get(&relays[0].url).unwrap();
        assert_eq!(relay.auth_token.as_deref(), Some("env-file-secret"));
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
        for source in [
            "[[relays]]\nurl = \"https://private-secret@relay.example.org\"\nunknown = \"private-secret\"",
            "[[relays]]\nurl = \"https://relay.example.org\"\ntoken = \"private-secret\"\nmalformed = [",
        ] {
            let error = parse(source).unwrap_err();
            assert!(!format!("{error:#} {error:?}").contains("private-secret"));
        }
    }
}
