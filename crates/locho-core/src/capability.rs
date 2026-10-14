use crate::service::{is_safe_service_name, ServiceType};
use std::fmt;

#[derive(Clone, PartialEq, Eq)]
pub struct Capability {
    pub service: String,
    pub service_type: ServiceType,
    pub secret: String,
}

pub fn format(service: &str, service_type: &ServiceType, secret: &str) -> String {
    format!("{service}:{}:{secret}", service_type.as_str())
}

/// Stable validation categories; errors never contain the supplied credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityError {
    InvalidFormat,
    InvalidServiceType,
    EmptySecret,
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidFormat => "invalid capability; expected <service>:<type>:<secret>",
            Self::InvalidServiceType => "invalid capability service type; expected http or tcp",
            Self::EmptySecret => "invalid capability; secret cannot be empty",
        })
    }
}

impl std::error::Error for CapabilityError {}

impl fmt::Debug for Capability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Capability")
            .field("service", &self.service)
            .field("service_type", &self.service_type)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

pub fn parse(value: &str) -> Result<Capability, CapabilityError> {
    let mut parts = value.split(':');
    let service = parts.next().unwrap_or_default();
    let service_type = parts.next().unwrap_or_default();
    let secret = parts.next().unwrap_or_default();
    if parts.next().is_some() || !is_safe_service_name(service) {
        return Err(CapabilityError::InvalidFormat);
    }
    let service_type = match service_type {
        "http" => ServiceType::Http,
        "tcp" => ServiceType::Tcp,
        _ => return Err(CapabilityError::InvalidServiceType),
    };
    if secret.is_empty() {
        return Err(CapabilityError::EmptySecret);
    }
    Ok(Capability {
        service: service.to_string(),
        service_type,
        secret: secret.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_parses_capability() {
        let value = format("database", &ServiceType::Tcp, "secret");
        assert_eq!(
            parse(&value).unwrap(),
            Capability {
                service: "database".into(),
                service_type: ServiceType::Tcp,
                secret: "secret".into(),
            }
        );
    }

    #[test]
    fn rejects_malformed_capability() {
        for value in [
            "database",
            "database:udp:secret",
            "database:tcp",
            "database:tcp:x:y",
        ] {
            assert!(
                parse(value).is_err(),
                "accepted malformed capability {value}"
            );
        }
    }

    #[test]
    fn malformed_inputs_and_debug_do_not_expose_credentials() {
        let secret = "sensitive-credential";
        for value in [
            format!("api:{secret}:x"),
            format!("{secret}!:tcp:x"),
            format!("api:tcp:{secret}:extra"),
        ] {
            let error = parse(&value).unwrap_err();
            assert!(!format!("{error} {error:?}").contains(secret));
        }
        let capability = parse(&format!("api:tcp:{secret}")).unwrap();
        assert!(!format!("{capability:?}").contains(secret));
    }
}
