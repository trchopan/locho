use crate::{
    capability::{self, Capability},
    config::ServiceType,
    protocol::{HTTP_REQUEST_TIMEOUT, MAX_HTTP_REQUEST_TIMEOUT_SECS},
};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
    path::Path,
    time::Duration,
};

const MAX_CONFIGURED_SERVICES: usize = 128;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachConfig {
    pub host_id: String,
    #[serde(default)]
    pub direct_address: Option<SocketAddr>,
    #[serde(default = "default_listen_host")]
    pub listen_host: IpAddr,
    pub services: Vec<AttachServiceConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachServiceConfig {
    pub capability: String,
    pub listen_port: u16,
    #[serde(default)]
    pub http_timeout_secs: Option<u64>,
}

#[derive(Debug)]
pub struct AttachmentConfig {
    pub capability: Capability,
    pub listen: SocketAddr,
    pub http_timeout: Duration,
}

pub(crate) fn resolve_http_timeout(timeout_secs: Option<u64>) -> Result<Duration> {
    let timeout_secs = timeout_secs.unwrap_or(HTTP_REQUEST_TIMEOUT.as_secs());
    if timeout_secs == 0 || timeout_secs > MAX_HTTP_REQUEST_TIMEOUT_SECS {
        bail!(
            "HTTP attachment timeout must be between 1 and {} seconds",
            MAX_HTTP_REQUEST_TIMEOUT_SECS
        )
    }
    Ok(Duration::from_secs(timeout_secs))
}

impl AttachConfig {
    pub fn load(path: &Path) -> Result<Self> {
        crate::state::ensure_private_file(path).with_context(|| {
            format!(
                "failed to secure attachment configuration {}",
                path.display()
            )
        })?;
        let contents = std::fs::read_to_string(path).with_context(|| {
            format!("failed to read attachment configuration {}", path.display())
        })?;
        let config: Self = toml::from_str(&contents).with_context(|| {
            format!(
                "failed to parse attachment configuration {}",
                path.display()
            )
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn attachments(&self) -> Result<Vec<AttachmentConfig>> {
        let mut names = HashSet::new();
        let mut ports = HashSet::new();
        self.services
            .iter()
            .enumerate()
            .map(|(index, service)| {
                let capability = capability::parse(&service.capability).with_context(|| {
                    format!("invalid capability for attachment service {}", index + 1)
                })?;
                if !names.insert(capability.service.clone()) {
                    bail!("duplicate attachment service {:?}", capability.service);
                }
                if !ports.insert(service.listen_port) {
                    bail!("duplicate attachment listen port {}", service.listen_port);
                }
                let http_timeout = match capability.service_type {
                    ServiceType::Http => resolve_http_timeout(service.http_timeout_secs)?,
                    ServiceType::Tcp => {
                        if service.http_timeout_secs.is_some() {
                            bail!(
                                "HTTP attachment timeout is only supported for HTTP service {}",
                                index + 1
                            )
                        }
                        HTTP_REQUEST_TIMEOUT
                    }
                };
                Ok(AttachmentConfig {
                    capability,
                    listen: SocketAddr::new(self.listen_host, service.listen_port),
                    http_timeout,
                })
            })
            .collect()
    }

    fn validate(&self) -> Result<()> {
        if self.host_id.trim().is_empty() {
            bail!("attachment configuration host_id cannot be empty");
        }
        if self.services.is_empty() {
            bail!("attachment configuration must define at least one service");
        }
        if self.services.len() > MAX_CONFIGURED_SERVICES {
            bail!("attachment configuration cannot define more than {MAX_CONFIGURED_SERVICES} services");
        }
        for service in &self.services {
            if service.listen_port == 0 {
                bail!("attachment listen port cannot be zero");
            }
        }
        self.attachments().map(|_| ())
    }
}

fn default_listen_host() -> IpAddr {
    IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mixed_attachment_services() {
        let config: AttachConfig = toml::from_str(
            r#"
                host_id = "aabb"
                listen_host = "127.0.0.1"

                [[services]]
                capability = "api:http:api-secret"
                listen_port = 8765
                http_timeout_secs = 90

                [[services]]
                capability = "database:tcp:db-secret"
                listen_port = 5432
            "#,
        )
        .unwrap();

        config.validate().unwrap();
        let attachments = config.attachments().unwrap();
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].capability.service, "api");
        assert_eq!(attachments[0].http_timeout, Duration::from_secs(90));
        assert_eq!(
            attachments[1].capability.service_type,
            crate::config::ServiceType::Tcp
        );
        assert_eq!(attachments[1].http_timeout, HTTP_REQUEST_TIMEOUT);
        assert_eq!(attachments[1].listen, "127.0.0.1:5432".parse().unwrap());
    }

    #[test]
    fn defaults_to_localhost() {
        let config: AttachConfig = toml::from_str(
            r#"
                host_id = "aabb"
                [[services]]
                capability = "api:http:secret"
                listen_port = 8765
            "#,
        )
        .unwrap();
        assert_eq!(
            config.attachments().unwrap()[0].http_timeout,
            HTTP_REQUEST_TIMEOUT
        );
        assert_eq!(config.listen_host, "127.0.0.1".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn rejects_duplicate_services_and_ports() {
        let config: AttachConfig = toml::from_str(
            r#"
                host_id = "aabb"
                [[services]]
                capability = "api:http:one"
                listen_port = 8765
                [[services]]
                capability = "api:http:two"
                listen_port = 8765
            "#,
        )
        .unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_unknown_fields() {
        let result = toml::from_str::<AttachConfig>(
            r#"
                host_id = "aabb"
                secret = "should-not-be-here"
                [[services]]
                capability = "api:http:secret"
                listen_port = 8765
            "#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn rejects_invalid_http_timeout() {
        for timeout_secs in [0, MAX_HTTP_REQUEST_TIMEOUT_SECS + 1] {
            let config: AttachConfig = toml::from_str(&format!(
                "host_id = \"aabb\"\n[[services]]\ncapability = \"api:http:secret\"\nlisten_port = 8765\nhttp_timeout_secs = {timeout_secs}\n"
            ))
            .unwrap();
            assert!(config.validate().is_err());
        }
    }

    #[test]
    fn rejects_http_timeout_for_tcp_service() {
        let config: AttachConfig = toml::from_str(
            r#"
                host_id = "aabb"
                [[services]]
                capability = "database:tcp:secret"
                listen_port = 5432
                http_timeout_secs = 90
            "#,
        )
        .unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn load_redacts_invalid_capability_from_errors() {
        let path = std::env::temp_dir().join(format!(
            "locho-attach-config-{}-{}.toml",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::write(
            &path,
            "host_id = \"aabb\"\n[[services]]\ncapability = \"api:invalid:super-secret\"\nlisten_port = 8765\n",
        )
        .unwrap();

        let error = AttachConfig::load(&path).unwrap_err().to_string();
        let _ = std::fs::remove_file(path);
        assert!(error.contains("service 1"));
        assert!(!error.contains("super-secret"));
    }
}
