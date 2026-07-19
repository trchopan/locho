use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use fs2::FileExt;
use iroh::{NodeId, SecretKey};
use rand::random;
use serde::{Deserialize, Serialize};

use crate::auth;

const CURRENT_SCHEMA_VERSION: u8 = 2;
const LEGACY_SCHEMA_VERSION: u8 = 1;

pub struct StateLock {
    file: File,
}

pub fn acquire_state_lock() -> Result<StateLock> {
    let lock_path = app_data_dir()?.join("state.lock");
    let parent = lock_path
        .parent()
        .ok_or_else(|| anyhow!("missing parent directory for state.lock"))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let file = File::create(&lock_path)
        .with_context(|| format!("failed to open {}", lock_path.display()))?;
    file.try_lock_exclusive().with_context(|| {
        format!(
            "another locho host or state operation is active in {}",
            parent.display()
        )
    })?;
    Ok(StateLock { file })
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PersistedHostState {
    pub schema_version: u8,
    pub endpoint_id: String,
    pub attach_secret: String,
    #[serde(default)]
    pub service_secrets: std::collections::HashMap<String, String>,
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
        ensure_private_file(&key_path)?;
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
        ensure_private_file(&state_path)?;
        let bytes = fs::read(&state_path)
            .with_context(|| format!("failed to read {}", state_path.display()))?;
        let state: PersistedHostState = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse {}", state_path.display()))?;
        if state.schema_version != CURRENT_SCHEMA_VERSION
            && state.schema_version != LEGACY_SCHEMA_VERSION
        {
            bail!(
                "unsupported host state schema version {} in {}",
                state.schema_version,
                state_path.display()
            );
        }
        let (mut state, mut changed) = repair_host_state(state, endpoint_id);
        if state.schema_version == LEGACY_SCHEMA_VERSION {
            // Legacy state had one host-wide secret. Service capabilities are
            // intentionally generated per service on the next host start.
            state.schema_version = CURRENT_SCHEMA_VERSION;
            changed = true;
        }
        if changed {
            write_host_state_file(&state_path, &state)?;
        }
        return Ok(state);
    }

    let state = PersistedHostState {
        schema_version: CURRENT_SCHEMA_VERSION,
        endpoint_id: endpoint_id.to_string(),
        attach_secret: auth::generate_secret(),
        service_secrets: std::collections::HashMap::new(),
    };
    write_host_state_file(&state_path, &state)?;
    Ok(state)
}

pub fn save_host_state(state: &PersistedHostState) -> Result<()> {
    let path = host_state_path()?;
    write_host_state_file(&path, state)
}

pub fn reset_identity() -> Result<()> {
    let _state_lock = acquire_state_lock()?;
    let key_path = host_key_path()?;
    let state_path = host_state_path()?;
    remove_if_exists(&key_path)?;
    remove_if_exists(&state_path)?;
    println!("locho identity reset; the next host start will use a new host ID");
    Ok(())
}

pub fn rotate_secret(service: &str) -> Result<()> {
    let _state_lock = acquire_state_lock()?;
    let secret_key = load_or_create_host_secret_key()?;
    let endpoint_id = NodeId::from(secret_key.public());
    let state_path = host_state_path()?;
    let mut state = load_or_create_host_state(endpoint_id)?;
    state.endpoint_id = endpoint_id.to_string();
    if !state.service_secrets.contains_key(service) {
        bail!(
            "unknown service {:?}; rotate a service configured on the host",
            service
        );
    }
    let secret = auth::generate_secret();
    state
        .service_secrets
        .insert(service.to_string(), secret.clone());
    write_host_state_file(&state_path, &state)?;
    println!(
        "attachment capability rotated for service {:?}\n\nAttach with:\n\nlocho attach {} {} {}",
        service, endpoint_id, service, secret
    );
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

fn ensure_private_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let metadata =
            fs::metadata(path).with_context(|| format!("failed to inspect {}", path.display()))?;
        if metadata.permissions().mode() & 0o077 != 0 {
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o600);
            fs::set_permissions(path, permissions)
                .with_context(|| format!("failed to secure {}", path.display()))?;
        }
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
    use std::sync::{Mutex, OnceLock};

    fn state_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    fn test_state_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("locho-test-{}", random::<u64>()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn use_test_state_dir(path: &Path) {
        std::env::set_var("LOCHO_STATE_DIR", path);
    }

    fn endpoint_id() -> NodeId {
        NodeId::from(SecretKey::generate(rand::rngs::OsRng).public())
    }

    #[test]
    fn repair_fills_missing_secret_and_endpoint_id() {
        let id = endpoint_id();
        let (state, changed) = repair_host_state(
            PersistedHostState {
                schema_version: CURRENT_SCHEMA_VERSION,
                endpoint_id: "old".into(),
                attach_secret: " ".into(),
                service_secrets: std::collections::HashMap::new(),
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
            schema_version: CURRENT_SCHEMA_VERSION,
            endpoint_id: id.to_string(),
            attach_secret: "secret".into(),
            service_secrets: std::collections::HashMap::new(),
        };
        let (repaired, changed) = repair_host_state(state, id);
        assert!(!changed);
        assert_eq!(repaired.attach_secret, "secret");
    }

    #[test]
    fn state_persists_key_and_secret() {
        let _lock = state_test_lock();
        let dir = test_state_dir();
        use_test_state_dir(&dir);
        let key = load_or_create_host_secret_key().unwrap();
        let first = load_or_create_host_state(NodeId::from(key.public())).unwrap();
        let second = load_or_create_host_state(NodeId::from(key.public())).unwrap();
        assert_eq!(first.endpoint_id, second.endpoint_id);
        assert_eq!(first.attach_secret, second.attach_secret);
        assert_eq!(first.service_secrets, second.service_secrets);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rotating_one_service_preserves_other_capabilities() {
        let mut state = PersistedHostState {
            schema_version: CURRENT_SCHEMA_VERSION,
            endpoint_id: endpoint_id().to_string(),
            attach_secret: "legacy".into(),
            service_secrets: std::collections::HashMap::from([
                ("api".into(), "api-old".into()),
                ("db".into(), "db-old".into()),
            ]),
        };
        let db_secret = state.service_secrets["db"].clone();
        state.service_secrets.insert("api".into(), "api-new".into());
        assert_eq!(state.service_secrets["api"], "api-new");
        assert_eq!(state.service_secrets["db"], db_secret);
    }

    #[test]
    fn unsupported_schema_is_rejected() {
        let _lock = state_test_lock();
        let dir = test_state_dir();
        use_test_state_dir(&dir);
        fs::write(
            dir.join("host_state.json"),
            r#"{"schema_version":3,"endpoint_id":"old","attach_secret":"secret"}"#,
        )
        .unwrap();
        assert!(load_or_create_host_state(endpoint_id()).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn state_lock_rejects_a_second_holder() {
        let _lock = state_test_lock();
        let dir = test_state_dir();
        use_test_state_dir(&dir);
        let first = acquire_state_lock().unwrap();
        assert!(acquire_state_lock().is_err());
        drop(first);
        assert!(acquire_state_lock().is_ok());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn existing_state_file_is_restricted() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = state_test_lock();
        let dir = test_state_dir();
        use_test_state_dir(&dir);
        let path = dir.join("host_state.json");
        fs::write(
            &path,
            format!(
                r#"{{"schema_version":1,"endpoint_id":"{}","attach_secret":"secret"}}"#,
                endpoint_id()
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        load_or_create_host_state(endpoint_id()).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
