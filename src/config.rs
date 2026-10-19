use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::{
    collections::HashSet,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};
use url::Url;

pub use locho_core::service::{is_safe_service_name, ServiceType};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub name: String,
    #[serde(rename = "type")]
    pub service_type: ServiceType,
    pub upstream: Option<Url>,
    #[serde(default)]
    pub upstream_timeout_secs: Option<u64>,
    pub ca_cert: Option<PathBuf>,
    pub endpoint: Option<SocketAddr>,
    #[serde(default)]
    pub unix_socket: Option<PathBuf>,
    #[serde(default)]
    pub tcp_idle_timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEndpoint {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

impl std::fmt::Display for StreamEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tcp(address) => write!(formatter, "tcp://{address}"),
            Self::Unix(path) => write!(formatter, "unix://{}", path.display()),
        }
    }
}

impl ServiceConfig {
    pub fn stream_endpoint(&self) -> Option<StreamEndpoint> {
        match (self.endpoint, &self.unix_socket) {
            (Some(endpoint), None) => Some(StreamEndpoint::Tcp(endpoint)),
            (None, Some(path)) => Some(StreamEndpoint::Unix(path.clone())),
            _ => None,
        }
    }
}

pub const MAX_TCP_IDLE_TIMEOUT_SECS: u64 = 60 * 60 * 24 * 365 * 100;

pub fn resolve_tcp_idle_timeout(timeout_secs: Option<u64>) -> Result<Option<Duration>> {
    match timeout_secs {
        None => Ok(Some(crate::protocol::TCP_IDLE_TIMEOUT)),
        Some(0) => Ok(None),
        Some(seconds) if seconds <= MAX_TCP_IDLE_TIMEOUT_SECS => {
            Ok(Some(Duration::from_secs(seconds)))
        }
        Some(seconds) => bail!(
            "TCP idle timeout must be 0 or between 1 and {} seconds (got {})",
            MAX_TCP_IDLE_TIMEOUT_SECS,
            seconds
        ),
    }
}

#[cfg(not(unix))]
pub fn validate_unix_socket_path(_path: &Path, what: &str) -> Result<()> {
    bail!("{what} is not supported on this platform; use a TCP endpoint")
}

#[cfg(unix)]
pub fn validate_unix_socket_path(path: &Path, what: &str) -> Result<()> {
    use std::{os::unix::ffi::OsStrExt, path::Component};

    if !path.is_absolute() {
        bail!("{what} must be an absolute filesystem path")
    }
    if path.as_os_str().is_empty() {
        bail!("{what} cannot be empty")
    }
    if path.file_name().is_none() {
        bail!("{what} must name a socket, not a directory")
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("{what} must not contain '..' path traversal")
    }
    let bytes = path.as_os_str().as_bytes();
    if bytes.contains(&0) {
        bail!("{what} must not contain a NUL byte")
    }
    #[cfg(target_os = "macos")]
    const MAX_PATH_BYTES: usize = 103;
    #[cfg(not(target_os = "macos"))]
    const MAX_PATH_BYTES: usize = 107;
    if bytes.len() > MAX_PATH_BYTES {
        bail!(
            "{what} is too long for a Unix socket path ({} bytes; maximum {})",
            bytes.len(),
            MAX_PATH_BYTES
        )
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub services: Vec<ServiceConfig>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read configuration {}", path.display()))?;
        let config: Self = toml::from_str(&contents)
            .with_context(|| format!("failed to parse configuration {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.services.is_empty() {
            bail!("configuration must define at least one service")
        }

        let mut names = HashSet::new();
        for service in &self.services {
            if !is_safe_service_name(&service.name) {
                bail!("service name {:?} is invalid", service.name)
            }
            if !names.insert(&service.name) {
                bail!("duplicate service name {:?}", service.name)
            }

            match service.service_type {
                ServiceType::Http => {
                    if service.endpoint.is_some() || service.unix_socket.is_some() {
                        bail!(
                            "HTTP service {:?} cannot define endpoint or unix_socket",
                            service.name
                        )
                    }
                    if service.tcp_idle_timeout_secs.is_some() {
                        bail!(
                            "HTTP service {:?} cannot define tcp_idle_timeout_secs",
                            service.name
                        )
                    }
                    let upstream = service.upstream.as_ref().with_context(|| {
                        format!("HTTP service {:?} requires upstream", service.name)
                    })?;
                    if let Some(ca_cert) = &service.ca_cert {
                        let certificate = std::fs::read(ca_cert).with_context(|| {
                            format!(
                                "HTTP service {:?} CA certificate cannot be read from {}",
                                service.name,
                                ca_cert.display()
                            )
                        })?;
                        reqwest::Certificate::from_pem(&certificate).with_context(|| {
                            format!(
                                "HTTP service {:?} CA certificate is not valid PEM: {}",
                                service.name,
                                ca_cert.display()
                            )
                        })?;
                    }
                    if let Some(timeout_secs) = service.upstream_timeout_secs {
                        if timeout_secs == 0
                            || timeout_secs > crate::protocol::MAX_HTTP_REQUEST_TIMEOUT_SECS
                        {
                            bail!(
                                "HTTP service {:?} upstream_timeout_secs must be between 1 and {}",
                                service.name,
                                crate::protocol::MAX_HTTP_REQUEST_TIMEOUT_SECS
                            )
                        }
                    }
                    let local_test_http = cfg!(feature = "integration-test")
                        && upstream.scheme() == "http"
                        && upstream.host_str() == Some("127.0.0.1");
                    if (upstream.scheme() != "https" && !local_test_http)
                        || upstream.host().is_none()
                    {
                        bail!(
                            "HTTP service {:?} upstream must be an HTTPS URL with a host",
                            service.name
                        )
                    }
                }
                ServiceType::Tcp => {
                    if service.upstream.is_some()
                        || service.upstream_timeout_secs.is_some()
                        || service.ca_cert.is_some()
                    {
                        bail!(
                            "TCP service {:?} cannot define upstream, upstream_timeout_secs, or ca_cert",
                            service.name
                        )
                    }
                    if service.endpoint.is_some() == service.unix_socket.is_some() {
                        bail!(
                            "TCP service {:?} requires exactly one of endpoint or unix_socket",
                            service.name
                        )
                    }
                    if service
                        .endpoint
                        .is_some_and(|endpoint| endpoint.port() == 0)
                    {
                        bail!(
                            "TCP service {:?} endpoint must use a non-zero port",
                            service.name
                        )
                    }
                    if let Some(path) = &service.unix_socket {
                        validate_unix_socket_path(
                            path,
                            &format!("TCP service {:?} unix_socket", service.name),
                        )?;
                    }
                    resolve_tcp_idle_timeout(service.tcp_idle_timeout_secs)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(name: &str) -> ServiceConfig {
        ServiceConfig {
            name: name.into(),
            service_type: ServiceType::Http,
            upstream: Some(Url::parse("https://example.com").unwrap()),
            upstream_timeout_secs: None,
            ca_cert: None,
            endpoint: None,
            unix_socket: None,
            tcp_idle_timeout_secs: None,
        }
    }

    #[test]
    fn accepts_multiple_service_types() {
        let config = Config {
            services: vec![
                http("api"),
                ServiceConfig {
                    name: "database".into(),
                    service_type: ServiceType::Tcp,
                    upstream: None,
                    upstream_timeout_secs: None,
                    ca_cert: None,
                    endpoint: Some("127.0.0.1:5432".parse().unwrap()),
                    unix_socket: None,
                    tcp_idle_timeout_secs: None,
                },
            ],
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn rejects_duplicate_and_unsafe_names() {
        let mut config = Config {
            services: vec![http("api"), http("api")],
        };
        assert!(config.validate().is_err());
        config.services[1].name = "api/service".into();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_wrong_endpoint_fields() {
        let mut config = Config {
            services: vec![ServiceConfig {
                name: "api".into(),
                service_type: ServiceType::Http,
                upstream: Some(Url::parse("http://example.com").unwrap()),
                upstream_timeout_secs: None,
                ca_cert: None,
                endpoint: None,
                unix_socket: None,
                tcp_idle_timeout_secs: None,
            }],
        };
        assert!(config.validate().is_err());
        config.services[0].service_type = ServiceType::Tcp;
        config.services[0].upstream = None;
        assert!(config.validate().is_err());
    }

    #[test]
    fn parses_toml_service_configuration() {
        let config: Config = toml::from_str(
            r#"
                [[services]]
                name = "api"
                type = "http"
                upstream = "https://example.com"
                upstream_timeout_secs = 90

                [[services]]
                name = "database"
                type = "tcp"
                endpoint = "127.0.0.1:5432"
            "#,
        )
        .unwrap();
        assert!(config.validate().is_ok());
        assert_eq!(config.services.len(), 2);
        assert_eq!(config.services[0].upstream_timeout_secs, Some(90));
        assert_eq!(config.services[1].upstream_timeout_secs, None);
    }

    #[test]
    fn rejects_unknown_toml_fields() {
        let result = toml::from_str::<Config>(
            r#"
                [[services]]
                name = "api"
                type = "http"
                upstream = "https://example.com"
                upsteam = "https://typo.example.com"
            "#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn rejects_zero_tcp_port() {
        let config = Config {
            services: vec![ServiceConfig {
                name: "database".into(),
                service_type: ServiceType::Tcp,
                upstream: None,
                upstream_timeout_secs: None,
                ca_cert: None,
                endpoint: Some("127.0.0.1:0".parse().unwrap()),
                unix_socket: None,
                tcp_idle_timeout_secs: None,
            }],
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_invalid_http_timeout() {
        for timeout_secs in [0, crate::protocol::MAX_HTTP_REQUEST_TIMEOUT_SECS + 1] {
            let mut config = Config {
                services: vec![http("api")],
            };
            config.services[0].upstream_timeout_secs = Some(timeout_secs);
            assert!(config.validate().is_err());
        }
    }

    #[test]
    fn accepts_http_timeout_boundaries() {
        for timeout_secs in [1, crate::protocol::MAX_HTTP_REQUEST_TIMEOUT_SECS] {
            let mut config = Config {
                services: vec![http("api")],
            };
            config.services[0].upstream_timeout_secs = Some(timeout_secs);
            assert!(config.validate().is_ok());
        }
    }

    #[test]
    fn rejects_timeout_on_tcp_service() {
        let config = Config {
            services: vec![ServiceConfig {
                name: "database".into(),
                service_type: ServiceType::Tcp,
                upstream: None,
                upstream_timeout_secs: Some(90),
                ca_cert: None,
                endpoint: Some("127.0.0.1:5432".parse().unwrap()),
                unix_socket: None,
                tcp_idle_timeout_secs: Some(90),
            }],
        };
        assert!(config.validate().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn accepts_unix_tcp_service_and_idle_policy() {
        let config = Config {
            services: vec![ServiceConfig {
                name: "herdr".into(),
                service_type: ServiceType::Tcp,
                upstream: None,
                upstream_timeout_secs: None,
                ca_cert: None,
                endpoint: None,
                unix_socket: Some("/tmp/herdr.sock".into()),
                tcp_idle_timeout_secs: Some(0),
            }],
        };
        assert!(config.validate().is_ok());
        assert_eq!(
            config.services[0].stream_endpoint(),
            Some(StreamEndpoint::Unix("/tmp/herdr.sock".into()))
        );
        assert_eq!(resolve_tcp_idle_timeout(Some(0)).unwrap(), None);
        assert_eq!(
            resolve_tcp_idle_timeout(Some(30)).unwrap(),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn rejects_invalid_unix_endpoint_combinations_and_idle_values() {
        let mut service = ServiceConfig {
            name: "database".into(),
            service_type: ServiceType::Tcp,
            upstream: None,
            upstream_timeout_secs: None,
            ca_cert: None,
            endpoint: Some("127.0.0.1:5432".parse().unwrap()),
            unix_socket: Some("/tmp/database.sock".into()),
            tcp_idle_timeout_secs: None,
        };
        assert!(Config {
            services: vec![service.clone()]
        }
        .validate()
        .is_err());
        service.endpoint = None;
        service.unix_socket = None;
        assert!(Config {
            services: vec![service.clone()]
        }
        .validate()
        .is_err());
        service.unix_socket = Some("relative.sock".into());
        assert!(Config {
            services: vec![service.clone()]
        }
        .validate()
        .is_err());
        service.unix_socket = Some("/tmp/database.sock".into());
        service.tcp_idle_timeout_secs = Some(MAX_TCP_IDLE_TIMEOUT_SECS + 1);
        assert!(Config {
            services: vec![service]
        }
        .validate()
        .is_err());
    }

    #[test]
    fn rejects_unix_fields_on_http_services() {
        let mut service = http("api");
        service.unix_socket = Some("/tmp/api.sock".into());
        assert!(Config {
            services: vec![service]
        }
        .validate()
        .is_err());
        let mut service = http("api");
        service.tcp_idle_timeout_secs = Some(0);
        assert!(Config {
            services: vec![service]
        }
        .validate()
        .is_err());
    }

    #[test]
    fn tcp_idle_policy_defaults_and_boundaries() {
        assert_eq!(
            resolve_tcp_idle_timeout(None).unwrap(),
            Some(crate::protocol::TCP_IDLE_TIMEOUT)
        );
        assert_eq!(resolve_tcp_idle_timeout(Some(0)).unwrap(), None);
        for seconds in [1, MAX_TCP_IDLE_TIMEOUT_SECS] {
            assert_eq!(
                resolve_tcp_idle_timeout(Some(seconds)).unwrap(),
                Some(Duration::from_secs(seconds))
            );
        }
        assert!(resolve_tcp_idle_timeout(Some(MAX_TCP_IDLE_TIMEOUT_SECS + 1)).is_err());
        assert!(resolve_tcp_idle_timeout(Some(u64::MAX)).is_err());
    }

    #[cfg(not(unix))]
    #[test]
    fn unix_paths_report_unsupported_platform_explicitly() {
        let error = validate_unix_socket_path(Path::new("/unix.sock"), "unix_socket").unwrap_err();
        assert!(error.to_string().contains("not supported"));
    }
}
