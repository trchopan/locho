use crate::{config::Config, protocol::ALPN, state};
use anyhow::{bail, Context, Result};
use iroh::{endpoint::ConnectionType, Endpoint, NodeAddr, NodeId, SecretKey};
use std::{fs, net::SocketAddr, path::PathBuf, time::Duration};
use tokio::time::timeout;

const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn run(
    config_path: Option<PathBuf>,
    host_id: Option<String>,
    direct_address: Option<SocketAddr>,
) -> Result<()> {
    if direct_address.is_some() && host_id.is_none() {
        bail!("--direct-address requires --host-id");
    }
    println!("locho diagnostics");
    println!("state directory: {}", state::app_data_dir()?.display());

    let state_dir = state::app_data_dir()?;
    let host_key_path = state_dir.join("host.key");
    report_state_file(&host_key_path, "host identity")?;
    let host_identity = host_key_path
        .exists()
        .then(|| validate_host_key(&host_key_path))
        .transpose()?;
    let host_state_path = state_dir.join("host_state.json");
    report_state_file(&host_state_path, "host state")?;
    if host_state_path.exists() {
        validate_host_state(&host_state_path, host_identity)?;
    }

    if let Some(config_path) = config_path {
        let config = Config::load(&config_path)
            .with_context(|| format!("configuration check failed for {}", config_path.display()))?;
        println!(
            "configuration: valid ({} services from {})",
            config.services.len(),
            config_path.display()
        );
        for service in config.services {
            println!("service: {} ({:?})", service.name, service.service_type);
        }
    } else {
        println!("configuration: not checked (use --config PATH)");
    }

    if let Some(host_id) = host_id {
        let node_id = host_id.parse().context("invalid host ID")?;
        let endpoint = Endpoint::builder().discovery_n0().bind().await?;
        if let Some(address) = direct_address {
            endpoint.add_node_addr(NodeAddr::new(node_id).with_direct_addresses([address]))?;
        }
        let connection = match timeout(DEFAULT_PROBE_TIMEOUT, endpoint.connect(node_id, ALPN)).await
        {
            Ok(Ok(connection)) => connection,
            Ok(Err(error)) => {
                endpoint.close().await;
                bail!("connectivity probe failed: {error:#}");
            }
            Err(_) => {
                endpoint.close().await;
                bail!("connectivity probe timed out after {DEFAULT_PROBE_TIMEOUT:?}");
            }
        };
        println!("connectivity: reachable");
        let connection_type = endpoint
            .conn_type(node_id)
            .ok()
            .and_then(|watcher| watcher.get().ok())
            .unwrap_or(ConnectionType::None);
        println!("transport path: {connection_type}");
        connection.close(0u32.into(), b"diagnostic complete");
        endpoint.close().await;
    } else {
        println!("connectivity: not checked (use --host-id HOST_ID)");
    }

    Ok(())
}

fn report_state_file(path: &std::path::Path, label: &str) -> Result<()> {
    if !path.exists() {
        println!("{label}: not initialized");
        return Ok(());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            anyhow::bail!(
                "{label} permissions are too broad ({mode:o}); expected private permissions"
            )
        }
    }
    #[cfg(not(unix))]
    std::fs::metadata(path).with_context(|| format!("failed to inspect {}", path.display()))?;
    println!("{label}: present and private");
    Ok(())
}

fn validate_host_key(path: &std::path::Path) -> Result<NodeId> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let key_bytes: [u8; 32] = bytes.try_into().map_err(|_| {
        anyhow::anyhow!(
            "host identity is invalid: {} must contain 32 bytes",
            path.display()
        )
    })?;
    Ok(NodeId::from(SecretKey::from_bytes(&key_bytes).public()))
}

fn validate_host_state(path: &std::path::Path, host_identity: Option<NodeId>) -> Result<()> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let state: state::PersistedHostState = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if state.schema_version != 1 && state.schema_version != 2 {
        bail!(
            "host state is invalid: unsupported schema version {}",
            state.schema_version
        );
    }
    let endpoint_id = state.endpoint_id.parse::<NodeId>().with_context(|| {
        format!(
            "host state has an invalid endpoint ID in {}",
            path.display()
        )
    })?;
    if host_identity.is_some_and(|identity| identity != endpoint_id) {
        bail!("host state endpoint ID does not match the persisted host identity");
    }
    if state.attach_secret.trim().is_empty()
        || state
            .service_secrets
            .iter()
            .any(|(name, secret)| name.trim().is_empty() || secret.trim().is_empty())
    {
        bail!("host state contains an empty capability value");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn missing_state_is_reported_without_failure() {
        let path =
            std::env::temp_dir().join(format!("locho-diagnostics-{}", rand::random::<u64>()));
        assert!(report_state_file(&path, "test state").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn broad_state_permissions_are_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let path =
            std::env::temp_dir().join(format!("locho-diagnostics-{}", rand::random::<u64>()));
        fs::write(&path, b"state").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(report_state_file(&path, "test state").is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn malformed_host_state_is_rejected_without_exposing_values() {
        let path =
            std::env::temp_dir().join(format!("locho-diagnostics-{}", rand::random::<u64>()));
        fs::write(
            &path,
            r#"{"schema_version":2,"endpoint_id":"not-an-id","attach_secret":"secret"}"#,
        )
        .unwrap();
        let error = validate_host_state(&path, None).unwrap_err().to_string();
        assert!(error.contains("endpoint ID"));
        assert!(!error.contains("secret"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn invalid_host_key_is_rejected() {
        let path =
            std::env::temp_dir().join(format!("locho-diagnostics-{}", rand::random::<u64>()));
        fs::write(&path, [0u8; 31]).unwrap();
        assert!(validate_host_key(&path).is_err());
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn direct_address_requires_host_id() {
        let result = tokio::runtime::Runtime::new().unwrap().block_on(run(
            None,
            None,
            Some("127.0.0.1:12345".parse().unwrap()),
        ));
        let error = result.unwrap_err().to_string();
        assert!(error.contains("--direct-address requires --host-id"));
    }
}
