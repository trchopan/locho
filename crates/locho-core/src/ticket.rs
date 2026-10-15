//! Single-service portable credentials. Encoding is not encryption or a signature.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fmt};

use crate::capability::{self, Capability};

pub const MAX_TICKET_LEN: usize = 8192;
pub const MAX_PEER_RELAYS: usize = 4;
const PREFIX: &str = "locho1:";

#[derive(Clone, PartialEq, Eq)]
pub struct Ticket {
    host_id: String,
    capability: Capability,
    peer_relay_urls: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketError {
    TooLarge,
    InvalidEncoding,
    UnsupportedVersion,
    InvalidPayload,
    InvalidHostId,
    InvalidCapability,
    InvalidPeerRelay,
}

impl fmt::Display for TicketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLarge => "ticket exceeds size limit",
            Self::InvalidEncoding => "invalid ticket encoding",
            Self::UnsupportedVersion => "unsupported ticket version",
            Self::InvalidPayload => "invalid ticket payload",
            Self::InvalidHostId => "invalid ticket host identity",
            Self::InvalidCapability => "invalid ticket capability",
            Self::InvalidPeerRelay => "invalid ticket peer relay hints",
        })
    }
}
impl std::error::Error for TicketError {}

impl fmt::Debug for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ticket")
            .field("credential", &"[REDACTED]")
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    host_id: String,
    capability: String,
    peer_relay_urls: Vec<String>,
}

impl Ticket {
    pub fn new(
        host_id: &str,
        capability: &str,
        peer_relay_urls: Vec<String>,
    ) -> Result<Self, TicketError> {
        if host_id.len() != 64 || !host_id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(TicketError::InvalidHostId);
        }
        if capability.len() > MAX_TICKET_LEN {
            return Err(TicketError::InvalidCapability);
        }
        let capability =
            capability::parse(capability).map_err(|_| TicketError::InvalidCapability)?;
        if capability.secret.len() > 1024
            || !capability.secret.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(TicketError::InvalidCapability);
        }
        if peer_relay_urls.len() > MAX_PEER_RELAYS {
            return Err(TicketError::InvalidPeerRelay);
        }
        let mut seen = HashSet::new();
        let mut relays = Vec::new();
        for value in peer_relay_urls {
            if value.len() > 512
                || value
                    .bytes()
                    .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
            {
                return Err(TicketError::InvalidPeerRelay);
            }
            let mut url = url::Url::parse(&value).map_err(|_| TicketError::InvalidPeerRelay)?;
            if url.scheme() != "https"
                || !url.has_host()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(TicketError::InvalidPeerRelay);
            }
            if let Some(host) = url.domain().and_then(|host| host.strip_suffix('.')) {
                let host = host.to_owned();
                url.set_host(Some(&host))
                    .map_err(|_| TicketError::InvalidPeerRelay)?;
            }
            let normalized = url.to_string();
            if !seen.insert(normalized.clone()) {
                return Err(TicketError::InvalidPeerRelay);
            }
            relays.push(normalized);
        }
        let ticket = Self {
            host_id: host_id.to_ascii_lowercase(),
            capability,
            peer_relay_urls: relays,
        };
        ticket.encode()?;
        Ok(ticket)
    }

    pub fn host_id(&self) -> &str {
        &self.host_id
    }
    pub fn capability(&self) -> &Capability {
        &self.capability
    }
    pub fn peer_relay_urls(&self) -> &[String] {
        &self.peer_relay_urls
    }

    /// Explicit credential output. Callers must not log this value implicitly.
    pub fn encode(&self) -> Result<String, TicketError> {
        let payload = Payload {
            host_id: self.host_id.clone(),
            capability: capability::format(
                &self.capability.service,
                &self.capability.service_type,
                &self.capability.secret,
            ),
            peer_relay_urls: self.peer_relay_urls.clone(),
        };
        let bytes = serde_json::to_vec(&payload).map_err(|_| TicketError::InvalidPayload)?;
        let value = format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes));
        if value.len() > MAX_TICKET_LEN {
            return Err(TicketError::TooLarge);
        }
        Ok(value)
    }
}

pub fn parse(value: &str) -> Result<Ticket, TicketError> {
    if value.len() > MAX_TICKET_LEN {
        return Err(TicketError::TooLarge);
    }
    let encoded = value.strip_prefix(PREFIX).ok_or_else(|| {
        if value.starts_with("locho") && value.contains(':') {
            TicketError::UnsupportedVersion
        } else {
            TicketError::InvalidEncoding
        }
    })?;
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| TicketError::InvalidEncoding)?;
    let payload: Payload =
        serde_json::from_slice(&bytes).map_err(|_| TicketError::InvalidPayload)?;
    Ticket::new(
        &payload.host_id,
        &payload.capability,
        payload.peer_relay_urls,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_vectors() {
        let vectors: serde_json::Value =
            serde_json::from_str(include_str!("../tests/vectors/tickets-v1.json")).unwrap();
        for vector in vectors["valid"].as_array().unwrap() {
            let ticket = parse(vector["ticket"].as_str().unwrap()).unwrap();
            assert_eq!(ticket.host_id(), vector["host_id"].as_str().unwrap());
            assert_eq!(
                ticket.encode().unwrap(),
                vector["canonical"].as_str().unwrap()
            );
            assert_eq!(parse(&ticket.encode().unwrap()).unwrap(), ticket);
        }
        for vector in vectors["invalid"].as_array().unwrap() {
            assert!(parse(vector.as_str().unwrap()).is_err());
        }
    }

    #[test]
    fn validation_and_redaction() {
        let host = "01".repeat(32);
        let secret = "sensitive-credential";
        let cap = format!("api:tcp:{secret}");
        let ticket = Ticket::new(&host, &cap, vec!["https://RELAY.example.org./".into()]).unwrap();
        assert_eq!(ticket.peer_relay_urls(), &["https://relay.example.org/"]);
        assert!(!format!("{ticket:?}").contains(secret));
        for relays in [
            vec!["http://relay.example.org".into()],
            vec![format!("https://user:{secret}@relay.example.org/")],
            vec!["https://relay.example.org/?token=secret".into()],
            vec!["https://relay.example.org/path".into()],
            vec!["https://relay.example.org/#secret".into()],
            vec![
                "https://relay.example.org/".into(),
                "https://RELAY.example.org./".into(),
            ],
            vec!["https://relay.example.org/".into(); 5],
        ] {
            let err = Ticket::new(&host, &cap, relays).unwrap_err();
            assert!(!format!("{err} {err:?}").contains(secret));
        }
        assert_eq!(
            parse(&"x".repeat(MAX_TICKET_LEN + 1)),
            Err(TicketError::TooLarge)
        );
        assert!(Ticket::new(&host, "api:tcp:has space", vec![]).is_err());
        assert!(Ticket::new(&host, &format!("api:tcp:{}", "s".repeat(1025)), vec![]).is_err());
    }

    #[test]
    fn rejects_duplicate_missing_fields_padding_and_non_json_payloads() {
        let host = "01".repeat(32);
        for payload in [
            format!(
                r#"{{"host_id":"{host}","host_id":"{host}","capability":"api:tcp:secret","peer_relay_urls":[]}}"#
            ),
            format!(r#"{{"host_id":"{host}","capability":"api:tcp:secret"}}"#),
            "null".into(),
            "[]".into(),
        ] {
            assert_eq!(
                parse(&format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(payload))),
                Err(TicketError::InvalidPayload)
            );
        }
        let value = Ticket::new(&host.to_uppercase(), "api:http:secret", vec![])
            .unwrap()
            .encode()
            .unwrap();
        assert_eq!(parse(&value).unwrap().host_id(), host);
        assert!(parse(&format!("{value}=")).is_err());
        assert!(parse(&format!(" {value}")).is_err());
        assert!(parse(&format!("{value}\n")).is_err());
    }
}
