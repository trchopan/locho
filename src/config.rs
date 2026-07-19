use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::{collections::HashSet, net::SocketAddr, path::Path};
use url::Url;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceType {
    Http,
    Tcp,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub name: String,
    #[serde(rename = "type")]
    pub service_type: ServiceType,
    pub upstream: Option<Url>,
    pub endpoint: Option<SocketAddr>,
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
                    if service.endpoint.is_some() {
                        bail!("HTTP service {:?} cannot define endpoint", service.name)
                    }
                    let upstream = service.upstream.as_ref().with_context(|| {
                        format!("HTTP service {:?} requires upstream", service.name)
                    })?;
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
                    if service.upstream.is_some() {
                        bail!("TCP service {:?} cannot define upstream", service.name)
                    }
                    if service.endpoint.is_none() {
                        bail!("TCP service {:?} requires endpoint", service.name)
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
                }
            }
        }
        Ok(())
    }
}

fn is_safe_service_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(name: &str) -> ServiceConfig {
        ServiceConfig {
            name: name.into(),
            service_type: ServiceType::Http,
            upstream: Some(Url::parse("https://example.com").unwrap()),
            endpoint: None,
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
                    endpoint: Some("127.0.0.1:5432".parse().unwrap()),
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
                endpoint: None,
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

                [[services]]
                name = "database"
                type = "tcp"
                endpoint = "127.0.0.1:5432"
            "#,
        )
        .unwrap();
        assert!(config.validate().is_ok());
        assert_eq!(config.services.len(), 2);
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
                endpoint: Some("127.0.0.1:0".parse().unwrap()),
            }],
        };
        assert!(config.validate().is_err());
    }
}
