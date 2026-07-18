use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use iroh::{NodeId, SecretKey};
use rand::random;
use serde::{Deserialize, Serialize};

use crate::auth;

#[derive(Debug, Serialize, Deserialize)]
pub struct PersistedHostState {
    pub schema_version: u8,
    pub endpoint_id: String,
    pub attach_secret: String,
}

pub fn app_data_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("LOCHO_STATE_DIR") {
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow!("could not determine home directory"))?;
    Ok(home.join(".local").join("share").join("locho"))
}

fn host_key_path() -> Result<PathBuf> {
    Ok(app_data_dir()?.join("host.key"))
}

fn host_state_path() -> Result<PathBuf> {
    Ok(app_data_dir()?.join("host_state.json"))
}

pub fn load_or_create_host_secret_key() -> Result<SecretKey> {
    let key_path = host_key_path()?;
    let app_dir = app_data_dir()?;
    fs::create_dir_all(&app_dir)
        .with_context(|| format!("failed to create {}", app_dir.display()))?;

    if key_path.exists() {
        let bytes = fs::read(&key_path)
            .with_context(|| format!("failed to read {}", key_path.display()))?;
        if bytes.len() != 32 {
            bail!("invalid host key length in {}", key_path.display());
        }
        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(&bytes);
        return Ok(SecretKey::from_bytes(&key_bytes));
    }

    let secret_key = SecretKey::generate(rand::rngs::OsRng);
    write_file_atomic(&key_path, &secret_key.to_bytes())?;
    Ok(secret_key)
}

pub fn load_or_create_host_state(endpoint_id: NodeId) -> Result<PersistedHostState> {
    let state_path = host_state_path()?;
    if state_path.exists() {
        let bytes = fs::read(&state_path)
            .with_context(|| format!("failed to read {}", state_path.display()))?;
        let state: PersistedHostState = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse {}", state_path.display()))?;
        let (state, changed) = repair_host_state(state, endpoint_id);
        if changed {
            write_host_state_file(&state_path, &state)?;
        }
        return Ok(state);
    }

    let state = PersistedHostState {
        schema_version: 1,
        endpoint_id: endpoint_id.to_string(),
        attach_secret: auth::generate_secret(),
    };
    write_host_state_file(&state_path, &state)?;
    Ok(state)
}

pub fn reset_identity() -> Result<()> {
    let key_path = host_key_path()?;
    let state_path = host_state_path()?;
    remove_if_exists(&key_path)?;
    remove_if_exists(&state_path)?;
    println!("locho identity reset; the next host start will use a new host ID");
    Ok(())
}

pub fn rotate_secret() -> Result<()> {
    let secret_key = load_or_create_host_secret_key()?;
    let endpoint_id = NodeId::from(secret_key.public());
    let state_path = host_state_path()?;
    let mut state = load_or_create_host_state(endpoint_id)?;
    state.endpoint_id = endpoint_id.to_string();
    state.attach_secret = auth::generate_secret();
    write_host_state_file(&state_path, &state)?;
    println!(
        "attach secret rotated\n\nAttach with:\n\nlocho attach {} {}",
        endpoint_id, state.attach_secret
    );
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn write_host_state_file(path: &Path, state: &PersistedHostState) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(state)?;
    write_file_atomic(path, &bytes)
}

fn write_file_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("missing parent directory for {}", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let temp_path = path.with_file_name(format!(
        ".{}.tmp-{}",
        path.file_name().unwrap().to_string_lossy(),
        random::<u64>()
    ));

    let result: Result<()> = (|| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&temp_path)
                .with_context(|| format!("failed to create {}", temp_path.display()))?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp_path)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        fs::rename(&temp_path, path)
            .with_context(|| format!("failed to replace {}", path.display()))?;
        #[cfg(unix)]
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn repair_host_state(
    mut state: PersistedHostState,
    endpoint_id: NodeId,
) -> (PersistedHostState, bool) {
    let mut changed = false;
    let endpoint_id = endpoint_id.to_string();
    if state.endpoint_id != endpoint_id {
        state.endpoint_id = endpoint_id;
        changed = true;
    }
    if state.attach_secret.trim().is_empty() {
        state.attach_secret = auth::generate_secret();
        changed = true;
    }
    (state, changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint_id() -> NodeId {
        NodeId::from(SecretKey::generate(rand::rngs::OsRng).public())
    }

    #[test]
    fn repair_fills_missing_secret_and_endpoint_id() {
        let id = endpoint_id();
        let (state, changed) = repair_host_state(
            PersistedHostState {
                schema_version: 1,
                endpoint_id: "old".into(),
                attach_secret: " ".into(),
            },
            id,
        );
        assert!(changed);
        assert_eq!(state.endpoint_id, id.to_string());
        assert!(!state.attach_secret.is_empty());
    }

    #[test]
    fn repair_keeps_valid_state() {
        let id = endpoint_id();
        let state = PersistedHostState {
            schema_version: 1,
            endpoint_id: id.to_string(),
            attach_secret: "secret".into(),
        };
        let (repaired, changed) = repair_host_state(state, id);
        assert!(!changed);
        assert_eq!(repaired.attach_secret, "secret");
    }
}
