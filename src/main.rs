mod attach;
mod attach_config;
mod auth;
mod capability;
mod config;
mod diagnostics;
mod host;
mod http_utils;
mod protocol;
mod relay;
mod state;
mod task;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use tracing_subscriber::{fmt::SubscriberBuilder, EnvFilter};

const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (commit ",
    env!("LOCHO_GIT_COMMIT"),
    ", ",
    env!("LOCHO_GIT_DIRTY"),
    ")"
);
const DEFAULT_ATTACH_LISTEN: &str = "127.0.0.1:8765";

#[derive(Parser)]
#[command(
    name = "locho",
    version = VERSION,
    about = "Private HTTP and TCP service tunnel"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Host {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        relay_config: Option<PathBuf>,
        #[arg(long)]
        bind_address: Option<SocketAddr>,
    },
    ResetIdentity,
    RotateSecret {
        service: String,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        relay_config: Option<PathBuf>,
        #[arg(long)]
        direct_address: Option<SocketAddr>,
    },
    Secret {
        service: String,
        #[arg(long)]
        config: PathBuf,
    },
    Share {
        service: String,
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        relay_config: Option<PathBuf>,
        #[arg(long)]
        direct_address: Option<SocketAddr>,
    },
    Diagnose {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        relay_config: Option<PathBuf>,
        #[arg(long)]
        host_id: Option<String>,
        #[arg(long)]
        direct_address: Option<SocketAddr>,
    },
    Attach {
        host_id: Option<String>,
        capability: Option<String>,
        #[arg(hide = true)]
        legacy_secret: Option<String>,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        relay_config: Option<PathBuf>,
        #[arg(long)]
        direct_address: Option<SocketAddr>,
        #[arg(long, hide = true)]
        tcp: bool,
        #[arg(long, default_value = "127.0.0.1:8765")]
        listen: SocketAddr,
        #[arg(long)]
        http_timeout_secs: Option<u64>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    match Cli::parse().command {
        Command::Host {
            config,
            relay_config,
            bind_address,
        } => host::run(config, relay_config, bind_address).await,
        Command::ResetIdentity => state::reset_identity(),
        Command::RotateSecret {
            service,
            config,
            relay_config,
            direct_address,
        } => {
            let service_type = config
                .as_deref()
                .map(|path| service_type(path, &service))
                .transpose()?;
            let (host_id, secret) = state::rotate_secret(&service)?;
            let attach_options = format_attach_options(direct_address, relay_config.as_deref());
            if let Some(service_type) = service_type {
                println!(
                    "attachment capability rotated for service {:?}\n\nAttach with:\n\nlocho attach {} {}{}",
                    service,
                    host_id,
                    capability::format(&service, &service_type, &secret),
                    attach_options
                );
            } else {
                println!(
                    "attachment capability rotated for service {:?}\n\nAttach with:\n\nlocho attach {} {} {}{}",
                    service, host_id, service, secret, attach_options
                );
            }
            Ok(())
        }
        Command::Secret { service, config } => {
            let service_type = service_type(&config, &service)?;
            let secret = state::read_service_secret(&service)?;
            println!("{}", capability::format(&service, &service_type, &secret));
            Ok(())
        }
        Command::Share {
            service,
            config,
            relay_config,
            direct_address,
        } => {
            let service_type = service_type(&config, &service)?;
            let secret = state::read_service_secret(&service)?;
            let host_id = state::read_host_endpoint_id()?;
            let attach_options = format_attach_options(direct_address, relay_config.as_deref());
            println!(
                "locho attach {} {}{}",
                host_id,
                capability::format(&service, &service_type, &secret),
                attach_options
            );
            Ok(())
        }
        Command::Diagnose {
            config,
            relay_config,
            host_id,
            direct_address,
        } => diagnostics::run(config, relay_config, host_id, direct_address).await,
        Command::Attach {
            host_id,
            capability,
            legacy_secret,
            config,
            relay_config,
            direct_address,
            tcp,
            listen,
            http_timeout_secs,
        } => {
            if let Some(config) = config {
                if host_id.is_some()
                    || capability.is_some()
                    || legacy_secret.is_some()
                    || tcp
                    || http_timeout_secs.is_some()
                    || listen
                        != DEFAULT_ATTACH_LISTEN
                            .parse()
                            .expect("valid default listener")
                {
                    bail!("--config cannot be combined with positional attach arguments, --tcp, --listen, or --http-timeout-secs");
                }
                attach::run_config(config, relay_config, direct_address).await
            } else {
                let host_id = host_id.ok_or_else(|| anyhow::anyhow!("attach requires HOST_ID"))?;
                let capability =
                    capability.ok_or_else(|| anyhow::anyhow!("attach requires CAPABILITY"))?;
                let capability = normalize_capability(&capability, legacy_secret, tcp)?;
                attach::run(
                    host_id,
                    capability,
                    relay_config,
                    direct_address,
                    listen,
                    http_timeout_secs,
                )
                .await
            }
        }
    }
}

fn init_tracing() {
    let filter = if std::env::var_os("RUST_LOG").is_some() {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| default_log_filter())
    } else {
        default_log_filter()
    };

    SubscriberBuilder::default().with_env_filter(filter).init();
}

fn default_log_filter() -> EnvFilter {
    EnvFilter::new("info,iroh::net_report::report=error")
}

fn format_attach_options(
    direct_address: Option<SocketAddr>,
    relay_config: Option<&Path>,
) -> String {
    let mut options = direct_address
        .map(|address| format!(" --direct-address {address}"))
        .unwrap_or_default();
    if let Some(path) = relay_config {
        options.push_str(" --relay-config ");
        options.push_str(&shell_quote(path));
    }
    options
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    if value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'\\' | b'.' | b'_' | b'-' | b':')
    }) {
        value.into_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn service_type(config_path: &Path, service: &str) -> Result<config::ServiceType> {
    let config = config::Config::load(config_path)?;
    config
        .services
        .iter()
        .find(|configured| configured.name == service)
        .map(|configured| configured.service_type)
        .ok_or_else(|| anyhow::anyhow!("unknown service {:?}", service))
}

fn normalize_capability(
    value: &str,
    legacy_secret: Option<String>,
    legacy_tcp: bool,
) -> Result<String> {
    match legacy_secret {
        Some(secret) => {
            if capability::parse(value).is_ok() {
                bail!("cannot combine legacy service/secret arguments with a capability token")
            }
            let service_type = if legacy_tcp {
                config::ServiceType::Tcp
            } else {
                config::ServiceType::Http
            };
            Ok(capability::format(value, &service_type, &secret))
        }
        None => {
            if legacy_tcp {
                bail!("--tcp is only supported with the legacy attach syntax")
            }
            capability::parse(value)?;
            Ok(value.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_legacy_attach_syntax() {
        assert_eq!(
            normalize_capability("api", Some("secret".into()), false).unwrap(),
            "api:http:secret"
        );
        assert_eq!(
            normalize_capability("database", Some("secret".into()), true).unwrap(),
            "database:tcp:secret"
        );
    }

    #[test]
    fn rejects_tcp_flag_with_capability_syntax() {
        assert!(normalize_capability("database:tcp:secret", None, true).is_err());
    }

    #[test]
    fn parses_http_timeout_for_positional_attach() {
        let cli = Cli::try_parse_from([
            "locho",
            "attach",
            "host",
            "api:http:secret",
            "--http-timeout-secs",
            "90",
        ])
        .unwrap();
        match cli.command {
            Command::Attach {
                http_timeout_secs, ..
            } => assert_eq!(http_timeout_secs, Some(90)),
            _ => panic!("parsed the wrong command"),
        }
    }

    #[test]
    fn parses_relay_configuration_for_host_attach_and_diagnose() {
        for args in [
            &[
                "locho",
                "host",
                "--config",
                "locho.toml",
                "--relay-config",
                "relay.toml",
            ][..],
            &[
                "locho",
                "attach",
                "host",
                "api:http:secret",
                "--relay-config",
                "relay.toml",
            ][..],
            &["locho", "diagnose", "--relay-config", "relay.toml"][..],
            &[
                "locho",
                "share",
                "api",
                "--config",
                "locho.toml",
                "--relay-config",
                "relay.toml",
            ][..],
            &[
                "locho",
                "rotate-secret",
                "api",
                "--relay-config",
                "relay.toml",
            ][..],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
    }

    #[test]
    fn quotes_relay_config_paths_in_generated_commands() {
        assert_eq!(
            format_attach_options(None, Some(Path::new("relay config.toml"))),
            " --relay-config 'relay config.toml'"
        );
        assert_eq!(
            format_attach_options(
                Some("127.0.0.1:12345".parse().unwrap()),
                Some(Path::new("relay.toml"))
            ),
            " --direct-address 127.0.0.1:12345 --relay-config relay.toml"
        );
    }

    #[test]
    fn leaves_relay_config_out_of_generated_commands_when_not_supplied() {
        assert_eq!(format_attach_options(None, None), "");
    }
}
