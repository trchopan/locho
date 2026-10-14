//! Validated in-memory relay policy, separate from host addressing hints.

use http::HeaderValue;
use iroh::{endpoint::default_relay_mode, RelayConfig, RelayMap, RelayMode, RelayUrl};
use std::{collections::HashSet, fmt, sync::Arc};
use url::Url;

#[derive(Clone)]
pub struct RelayOptions {
    pub url: String,
    pub auth_token: Option<String>,
    pub quic_address_discovery: bool,
}

impl fmt::Debug for RelayOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Unvalidated URLs can themselves contain credentials.
        formatter
            .debug_struct("RelayOptions")
            .field("url", &"[UNVALIDATED]")
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("quic_address_discovery", &self.quic_address_discovery)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct ConfiguredRelay {
    pub url: RelayUrl,
    pub quic_address_discovery: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayError {
    InvalidUrl,
    HttpsRequired,
    HostRequired,
    CredentialsInUrl,
    BaseUrlRequired,
    DuplicateUrl,
    InvalidToken,
    EmptyConfiguration,
}

impl fmt::Display for RelayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidUrl => "relay URL is not valid",
            Self::HttpsRequired => "relay URL must use HTTPS",
            Self::HostRequired => "relay URL must include a host",
            Self::CredentialsInUrl => "relay URL must not contain credentials",
            Self::BaseUrlRequired => {
                "relay URL must be a base URL without a path, query, or fragment"
            }
            Self::DuplicateUrl => "duplicate relay URL",
            Self::InvalidToken => "relay authentication token is not a valid bearer token",
            Self::EmptyConfiguration => {
                "relay configuration must define at least one relay when include_n0_relays is false"
            }
        })
    }
}

impl std::error::Error for RelayError {}

/// Browser authentication needs its own feasibility proof: accepting a token
/// here does not guarantee a browser can transmit it to a relay.
#[derive(Clone)]
pub struct RelaySettings {
    mode: RelayMode,
    relays: Vec<ConfiguredRelay>,
    include_n0_relays: bool,
}

impl fmt::Debug for RelaySettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelaySettings")
            .field("relays", &self.relays)
            .field("include_n0_relays", &self.include_n0_relays)
            .finish_non_exhaustive()
    }
}

impl RelaySettings {
    pub fn new(include_n0_relays: bool, options: Vec<RelayOptions>) -> Result<Self, RelayError> {
        let mut urls = HashSet::new();
        let custom_map = RelayMap::empty();
        let mut relays = Vec::with_capacity(options.len());
        for option in options {
            let url = parse_relay_url(&option.url)?;
            if !urls.insert(url.clone()) {
                return Err(RelayError::DuplicateUrl);
            }
            let mut config = if option.quic_address_discovery {
                RelayConfig::from(url.clone())
            } else {
                RelayConfig::new(url.clone(), None)
            };
            if let Some(token) = option.auth_token {
                validate_token(&token)?;
                config = config.with_auth_token(token);
            }
            custom_map.insert(url.clone(), Arc::new(config));
            relays.push(ConfiguredRelay {
                url,
                quic_address_discovery: option.quic_address_discovery,
            });
        }
        if !include_n0_relays && custom_map.is_empty() {
            return Err(RelayError::EmptyConfiguration);
        }
        let relay_map = if include_n0_relays {
            default_relay_mode().relay_map()
        } else {
            RelayMap::empty()
        };
        relay_map.extend(&custom_map);
        Ok(Self {
            mode: RelayMode::Custom(relay_map),
            relays,
            include_n0_relays,
        })
    }

    pub fn mode(&self) -> RelayMode {
        self.mode.clone()
    }

    pub fn report(&self) -> (&[ConfiguredRelay], bool) {
        (&self.relays, self.include_n0_relays)
    }
}

pub fn parse_relay_url(value: &str) -> Result<RelayUrl, RelayError> {
    let url = Url::parse(value).map_err(|_| RelayError::InvalidUrl)?;
    if url.scheme() != "https" {
        return Err(RelayError::HttpsRequired);
    }
    if !url.has_host() {
        return Err(RelayError::HostRequired);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(RelayError::CredentialsInUrl);
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(RelayError::BaseUrlRequired);
    }
    Ok(RelayUrl::from(url))
}

pub fn validate_token(token: &str) -> Result<(), RelayError> {
    if token.is_empty() || HeaderValue::from_str(&format!("Bearer {token}")).is_err() {
        return Err(RelayError::InvalidToken);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(url: &str, token: Option<&str>) -> RelayOptions {
        RelayOptions {
            url: url.into(),
            auth_token: token.map(str::to_owned),
            quic_address_discovery: false,
        }
    }

    #[test]
    fn memory_credentials_are_used_and_redacted() {
        let input = options("https://relay.example.org", Some("private-token"));
        assert!(!format!("{input:?}").contains("private-token"));
        let settings = RelaySettings::new(false, vec![input]).unwrap();
        let (relays, defaults) = settings.report();
        assert!(!defaults);
        assert!(!relays[0].quic_address_discovery);
        assert_eq!(settings.mode().relay_map().len(), 1);
        assert_eq!(
            settings
                .mode()
                .relay_map()
                .get(&relays[0].url)
                .unwrap()
                .auth_token
                .as_deref(),
            Some("private-token")
        );
        assert!(!format!("{settings:?}").contains("private-token"));
    }

    #[test]
    fn validates_normalized_duplicates_and_empty_policy() {
        assert_eq!(
            RelaySettings::new(false, vec![]).unwrap_err(),
            RelayError::EmptyConfiguration
        );
        assert_eq!(
            RelaySettings::new(
                false,
                vec![
                    options("https://relay.example.org", None),
                    options("https://relay.example.org/", None),
                ]
            )
            .unwrap_err(),
            RelayError::DuplicateUrl
        );
        assert!(RelaySettings::new(true, vec![]).is_ok());
    }

    #[test]
    fn untrusted_urls_and_tokens_never_appear_in_errors() {
        for url in [
            "not-a-url-private-secret",
            "https://user:private-secret@relay.example.org",
            "https://relay.example.org?token=private-secret",
            "http://relay.example.org/private-secret",
        ] {
            let input = options(url, None);
            assert!(!format!("{input:?}").contains("private-secret"));
            let error = RelaySettings::new(false, vec![input]).unwrap_err();
            assert!(!format!("{error} {error:?}").contains("private-secret"));
        }
        let error = RelaySettings::new(
            false,
            vec![options(
                "https://relay.example.org",
                Some("private-secret\r\n"),
            )],
        )
        .unwrap_err();
        assert_eq!(error, RelayError::InvalidToken);
        assert!(!format!("{error} {error:?}").contains("private-secret"));
    }
}
