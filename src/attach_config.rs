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
    path::{Component, Path, PathBuf},
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
    #[serde(default)]
    pub listen_port: Option<u16>,
    #[serde(default)]
    pub listen_unix: Option<PathBuf>,
    #[serde(default)]
    pub http_timeout_secs: Option<u64>,
    #[serde(default)]
    pub tcp_idle_timeout_secs: Option<u64>,
}

#[derive(Debug)]
pub struct AttachmentConfig {
    pub capability: Capability,
    pub listen: ListenerAddress,
    pub http_timeout: Duration,
    pub tcp_idle_timeout: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenerAddress {
    Tcp(SocketAddr),
    Unix(PathBuf),
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
        let mut unix_paths = HashSet::new();
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
                if let Some(port) = service.listen_port {
                    if !ports.insert(port) {
                        bail!("duplicate attachment listen port {}", port);
                    }
                }
                let (listen, http_timeout, tcp_idle_timeout) = match capability.service_type {
                    ServiceType::Http => {
                        if service.listen_unix.is_some() {
                            bail!(
                                "HTTP attachment cannot use listen_unix for service {}",
                                index + 1
                            )
                        }
                        if service.tcp_idle_timeout_secs.is_some() {
                            bail!("TCP idle timeout is only supported for TCP attachments")
                        }
                        let port = service.listen_port.with_context(|| {
                            format!("HTTP attachment service {} requires listen_port", index + 1)
                        })?;
                        (
                            ListenerAddress::Tcp(SocketAddr::new(self.listen_host, port)),
                            resolve_http_timeout(service.http_timeout_secs)?,
                            None,
                        )
                    }
                    ServiceType::Tcp => {
                        if service.http_timeout_secs.is_some() {
                            bail!(
                                "HTTP attachment timeout is only supported for HTTP service {}",
                                index + 1
                            )
                        }
                        let listen = match (service.listen_port, &service.listen_unix) {
                            (Some(port), None) => {
                                ListenerAddress::Tcp(SocketAddr::new(self.listen_host, port))
                            }
                            (None, Some(path)) => {
                                crate::config::validate_unix_socket_path(
                                    path,
                                    &format!("attachment service {} listen_unix", index + 1),
                                )?;
                                ListenerAddress::Unix(normalize_unix_path(path))
                            }
                            (Some(_), Some(_)) => bail!(
                                "attachment service {} must define exactly one of listen_port or listen_unix",
                                index + 1
                            ),
                            (None, None) => bail!(
                                "attachment service {} requires exactly one of listen_port or listen_unix",
                                index + 1
                            ),
                        };
                        (
                            listen,
                            HTTP_REQUEST_TIMEOUT,
                            crate::config::resolve_tcp_idle_timeout(
                                service.tcp_idle_timeout_secs,
                            )?,
                        )
                    }
                };
                if let ListenerAddress::Unix(path) = &listen {
                    if !unix_paths.insert(path.clone()) {
                        bail!("duplicate attachment Unix listener path {}", path.display());
                    }
                }
                Ok(AttachmentConfig {
                    capability,
                    listen,
                    http_timeout,
                    tcp_idle_timeout,
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
            if service.listen_port == Some(0) {
                bail!("attachment listen port cannot be zero");
            }
        }
        self.attachments().map(|_| ())
    }
}

fn default_listen_host() -> IpAddr {
    IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

fn normalize_unix_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => normalized.push("/"),
            Component::CurDir => {}
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir | Component::Prefix(_) => unreachable!("validated Unix path"),
        }
    }
    normalized
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
        assert_eq!(
            attachments[1].listen,
            ListenerAddress::Tcp("127.0.0.1:5432".parse().unwrap())
        );
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
    fn accepts_http_timeout_boundaries() {
        for timeout_secs in [1, MAX_HTTP_REQUEST_TIMEOUT_SECS] {
            let config: AttachConfig = toml::from_str(&format!(
                "host_id = \"aabb\"\n[[services]]\ncapability = \"api:http:secret\"\nlisten_port = 8765\nhttp_timeout_secs = {timeout_secs}\n"
            ))
            .unwrap();
            assert_eq!(
                config.attachments().unwrap()[0].http_timeout,
                Duration::from_secs(timeout_secs)
            );
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

    #[cfg(unix)]
    #[test]
    fn accepts_unix_tcp_listener_and_rejects_duplicate_paths() {
        let config: AttachConfig = toml::from_str(
            r#"
                host_id = "aabb"
                [[services]]
                capability = "api:tcp:one"
                listen_unix = "/tmp/locho-api.sock"
                tcp_idle_timeout_secs = 0
            "#,
        )
        .unwrap();
        let attachments = config.attachments().unwrap();
        assert_eq!(attachments[0].tcp_idle_timeout, None);
        assert_eq!(
            attachments[0].listen,
            ListenerAddress::Unix("/tmp/locho-api.sock".into())
        );

        let duplicate: AttachConfig = toml::from_str(
            r#"
                host_id = "aabb"
                [[services]]
                capability = "api:tcp:one"
                listen_unix = "/tmp/locho-api.sock"
                [[services]]
                capability = "database:tcp:two"
                listen_unix = "/tmp/./locho-api.sock"
            "#,
        )
        .unwrap();
        assert!(duplicate.validate().is_err());
    }

    #[test]
    fn rejects_mixed_listener_fields_and_tcp_idle_on_http() {
        let mixed: AttachConfig = toml::from_str(
            r#"
                host_id = "aabb"
                [[services]]
                capability = "api:tcp:one"
                listen_port = 8765
                listen_unix = "/tmp/locho-api.sock"
            "#,
        )
        .unwrap();
        assert!(mixed.validate().is_err());

        let http: AttachConfig = toml::from_str(
            r#"
                host_id = "aabb"
                [[services]]
                capability = "api:http:one"
                listen_port = 8765
                tcp_idle_timeout_secs = 0
            "#,
        )
        .unwrap();
        assert!(http.validate().is_err());
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
