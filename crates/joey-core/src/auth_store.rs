//! Auth store — persistence layer for `~/.joey/auth.json` (port of the
//! `hermes_cli/auth.py` store helpers: `_load_auth_store`, `_save_auth_store`,
//! `_auth_store_lock`, `_load_provider_state`, `_store_provider_state`,
//! `deactivate_provider`).
//!
//! Shape: `{"version": 1, "providers": {<id>: {...}}, "active_provider": ...,
//! "updated_at": "<iso8601>"}`. Writes are atomic (tmp + rename), 0o600, under
//! an advisory flock on `auth.lock` with a bounded wait. Corrupt stores are
//! preserved as `auth.json.corrupt` and replaced with an empty store rather
//! than failing the caller (auth.py:1109-1148).
//!
//! Port scope: the single active `~/.joey` home. Upstream's profile→global-root
//! fallback (`_global_auth_file_path`) rides on the HERMES_HOME profile
//! machinery; the joey profile flag already resolves `joey_home()` per profile,
//! so each profile keeps its own store.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

use crate::constants;

/// Store schema version (auth.py `AUTH_STORE_VERSION`).
pub const AUTH_STORE_VERSION: i64 = 1;

/// Bounded wait for the cross-process lock (auth.py
/// `AUTH_LOCK_TIMEOUT_SECONDS = 15.0`).
const AUTH_LOCK_TIMEOUT: Duration = Duration::from_secs(15);

/// Path to `auth.json` under the active joey home.
pub fn auth_file_path() -> PathBuf {
    constants::joey_home().join("auth.json")
}

fn auth_lock_path() -> PathBuf {
    constants::joey_home().join("auth.lock")
}

/// Advisory cross-process lock guard for one auth.json read/write
/// transaction. Degrades to unlocked operation when the lock file cannot be
/// created or the wait times out (the store write itself is still atomic).
pub struct AuthStoreLock {
    file: Option<std::fs::File>,
}

impl Drop for AuthStoreLock {
    fn drop(&mut self) {
        if let Some(f) = self.file.take() {
            let _ = f.unlock();
        }
    }
}

/// Acquire the auth-store lock (auth.py `_auth_store_lock`), waiting up to
/// [`AUTH_LOCK_TIMEOUT`]. Never fails the caller: on timeout or I/O error the
/// guard simply holds no lock.
pub fn auth_store_lock() -> AuthStoreLock {
    let path = auth_lock_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(_) => return AuthStoreLock { file: None },
    };
    let deadline = Instant::now() + AUTH_LOCK_TIMEOUT;
    loop {
        match file.try_lock() {
            Ok(()) => return AuthStoreLock { file: Some(file) },
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return AuthStoreLock { file: None },
        }
    }
}

fn empty_store() -> Value {
    json!({"version": AUTH_STORE_VERSION, "providers": {}})
}

/// Load the auth store (auth.py `_load_auth_store`). A missing file yields an
/// empty store; an unparseable file is preserved as `auth.json.corrupt` and
/// replaced by an empty store with a warning.
pub fn load_auth_store() -> Value {
    let path = auth_file_path();
    if !path.exists() {
        return empty_store();
    }
    let raw = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return empty_store(),
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(exc) => {
            let corrupt = path.with_extension("json.corrupt");
            let _ = std::fs::copy(&path, &corrupt);
            tracing::warn!(
                "auth: failed to parse {} ({}) — starting with empty store. \
                 Corrupt file preserved at {}",
                path.display(),
                exc,
                corrupt.display()
            );
            return empty_store();
        }
    };
    if let Value::Object(ref obj) = parsed {
        if obj.get("providers").map(|p| p.is_object()).unwrap_or(false)
            || obj.get("credential_pool").map(|p| p.is_object()).unwrap_or(false)
        {
            let mut store = parsed.clone();
            if store.get("providers").map(|p| !p.is_object()).unwrap_or(true) {
                store["providers"] = json!({});
            }
            return store;
        }
    }
    empty_store()
}

/// Save the auth store (auth.py `_save_auth_store`): stamp version +
/// `updated_at`, write atomically with owner-only permissions, and tighten the
/// parent dir to 0o700.
pub fn save_auth_store(store: &mut Value) -> Result<PathBuf> {
    let path = auth_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    constants::secure_parent_dir(&path);
    store["version"] = json!(AUTH_STORE_VERSION);
    store["updated_at"] = json!(chrono::Utc::now().to_rfc3339());
    let payload = format!("{}\n", serde_json::to_string_pretty(store)?);

    let tmp = path.with_file_name(format!(
        "{}.tmp.{}.{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("auth.json"),
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // 0o600 at creation closes the TOCTOU window (auth.py:1168-1176).
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(payload.as_bytes())?;
        f.flush()?;
        f.sync_all()?;
    }
    let renamed = std::fs::rename(&tmp, &path);
    if renamed.is_err() {
        let _ = std::fs::remove_file(&tmp);
        renamed.with_context(|| format!("replacing {}", path.display()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(path)
}

/// Read one provider's state map (auth.py `_load_provider_state`).
pub fn load_provider_state(store: &Value, provider_id: &str) -> Option<Map<String, Value>> {
    store
        .get("providers")?
        .get(provider_id)?
        .as_object()
        .cloned()
}

/// Merge one provider's state into the store (auth.py `_store_provider_state`).
/// `set_active` also points `active_provider` at it.
pub fn store_provider_state(
    store: &mut Value,
    provider_id: &str,
    state: Map<String, Value>,
    set_active: bool,
) {
    if store.get("providers").map(|p| !p.is_object()).unwrap_or(true) {
        store["providers"] = json!({});
    }
    store["providers"][provider_id] = Value::Object(state);
    if set_active {
        store["active_provider"] = json!(provider_id);
    }
}

/// Clear `active_provider` without deleting credentials (auth.py
/// `deactivate_provider`) — used when the user switches to a non-OAuth
/// provider so auto-resolution doesn't keep picking the OAuth provider.
pub fn deactivate_provider() {
    let _lock = auth_store_lock();
    let mut store = load_auth_store();
    store["active_provider"] = Value::Null;
    let _ = save_auth_store(&mut store);
}

/// Convenience: read one provider's state under no lock (read-only callers).
pub fn read_provider_state(provider_id: &str) -> Option<Map<String, Value>> {
    load_provider_state(&load_auth_store(), provider_id)
}

/// Convenience: merge one provider's state under the store lock
/// (auth.py `_persist_provider_state_to_store`).
pub fn persist_provider_state(
    provider_id: &str,
    state: Map<String, Value>,
    set_active: bool,
) -> Result<PathBuf> {
    let _lock = auth_store_lock();
    let mut store = load_auth_store();
    store_provider_state(&mut store, provider_id, state, set_active);
    save_auth_store(&mut store)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        let _lock = constants::TEST_HOME_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let _guard = constants::HomeOverrideGuard::new(dir.path().to_path_buf());
        f()
    }

    #[test]
    fn missing_store_loads_empty() {
        with_temp_home(|| {
            let store = load_auth_store();
            assert_eq!(store["version"], json!(AUTH_STORE_VERSION));
            assert!(store["providers"].as_object().unwrap().is_empty());
        });
    }

    #[test]
    fn round_trip_provider_state_and_active() {
        with_temp_home(|| {
            let mut state = Map::new();
            state.insert("detected_endpoint".into(), json!({"base_url": "https://api.z.ai/api/coding/paas/v4"}));
            persist_provider_state("zai", state, false).unwrap();

            let loaded = read_provider_state("zai").unwrap();
            assert_eq!(
                loaded["detected_endpoint"]["base_url"],
                json!("https://api.z.ai/api/coding/paas/v4")
            );
            // set_active=false must not flip the active provider.
            assert!(load_auth_store().get("active_provider").is_none());

            persist_provider_state("zai", loaded, true).unwrap();
            assert_eq!(load_auth_store()["active_provider"], json!("zai"));

            deactivate_provider();
            assert_eq!(load_auth_store()["active_provider"], Value::Null);
            // Credentials survive deactivation.
            assert!(read_provider_state("zai").is_some());
        });
    }

    #[test]
    fn corrupt_store_preserved_and_replaced() {
        with_temp_home(|| {
            let path = auth_file_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "{not json").unwrap();
            let store = load_auth_store();
            assert!(store["providers"].as_object().unwrap().is_empty());
            assert!(path.with_extension("json.corrupt").exists());
        });
    }

    #[cfg(unix)]
    #[test]
    fn saved_store_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        with_temp_home(|| {
            let mut store = load_auth_store();
            save_auth_store(&mut store).unwrap();
            let mode = std::fs::metadata(auth_file_path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        });
    }
}
