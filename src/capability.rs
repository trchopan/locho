use crate::config::{is_safe_service_name, ServiceType};
use anyhow::{bail, Result};

#[derive(Debug, PartialEq, Eq)]
pub struct Capability {
    pub service: String,
    pub service_type: ServiceType,
    pub secret: String,
}

pub fn format(service: &str, service_type: &ServiceType, secret: &str) -> String {
    format!("{service}:{}:{secret}", service_type.as_str())
}

pub fn parse(value: &str) -> Result<Capability> {
    let mut parts = value.split(':');
    let service = parts.next().unwrap_or_default();
    let service_type = parts.next().unwrap_or_default();
    let secret = parts.next().unwrap_or_default();
    if parts.next().is_some() || !is_safe_service_name(service) {
        bail!("invalid capability; expected <service>:<type>:<secret>");
    }
    let service_type = match service_type {
        "http" => ServiceType::Http,
        "tcp" => ServiceType::Tcp,
        _ => bail!("invalid capability service type {service_type:?}; expected http or tcp"),
    };
    if secret.is_empty() {
        bail!("invalid capability; secret cannot be empty");
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
}
