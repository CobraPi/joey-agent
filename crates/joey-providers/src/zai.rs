//! Z.AI endpoint detection (port of the `hermes_cli/auth.py` Z.AI section:
//! `ZAI_ENDPOINTS`, `detect_zai_endpoint`, `_resolve_zai_base_url`).
//!
//! Z.AI has separate billing for general vs coding plans, and global vs China
//! endpoints. A key that works on one may return "Insufficient balance" on
//! another. We probe at setup time and store the working endpoint. Each entry
//! lists candidate models to try in order — newer coding plan accounts may
//! only have access to recent models (glm-5.1, glm-5v-turbo) while older ones
//! still use glm-4.7.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// One official Z.AI endpoint (auth.py `ZAI_ENDPOINTS` tuple).
#[derive(Debug, Clone, Copy)]
pub struct ZaiEndpoint {
    pub id: &'static str,
    pub base_url: &'static str,
    pub probe_models: &'static [&'static str],
    pub label: &'static str,
}

/// The four official Z.AI endpoints, in probe order (auth.py:623-629).
pub const ZAI_ENDPOINTS: [ZaiEndpoint; 4] = [
    ZaiEndpoint {
        id: "global",
        base_url: "https://api.z.ai/api/paas/v4",
        probe_models: &["glm-5"],
        label: "Global",
    },
    ZaiEndpoint {
        id: "cn",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        probe_models: &["glm-5"],
        label: "China",
    },
    ZaiEndpoint {
        id: "coding-global",
        base_url: "https://api.z.ai/api/coding/paas/v4",
        probe_models: &["glm-5.2", "glm-5.1", "glm-5v-turbo", "glm-4.7"],
        label: "Global (Coding Plan)",
    },
    ZaiEndpoint {
        id: "coding-cn",
        base_url: "https://open.bigmodel.cn/api/coding/paas/v4",
        probe_models: &["glm-5.2", "glm-5.1", "glm-5v-turbo", "glm-4.7"],
        label: "China (Coding Plan)",
    },
];

/// A successful endpoint probe (auth.py `detect_zai_endpoint` return dict).
#[derive(Debug, Clone)]
pub struct DetectedZaiEndpoint {
    pub id: String,
    pub base_url: String,
    pub model: String,
    pub label: String,
}

/// Probe z.ai endpoints to find one that accepts this API key
/// (auth.py `detect_zai_endpoint`). Returns the first working endpoint, or
/// None if all fail. For endpoints with multiple candidate models, tries each
/// in order and returns the first that succeeds.
pub async fn detect_zai_endpoint_async(
    api_key: &str,
    timeout_secs: f64,
) -> Option<DetectedZaiEndpoint> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs_f64(timeout_secs))
        .build()
        .ok()?;
    for ep in ZAI_ENDPOINTS {
        for model in ep.probe_models {
            let body = json!({
                "model": model,
                "stream": false,
                "max_tokens": 1,
                "messages": [{"role": "user", "content": "ping"}],
            });
            let resp = client
                .post(format!("{}/chat/completions", ep.base_url))
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await;
            match resp {
                Ok(r) if r.status().as_u16() == 200 => {
                    tracing::debug!(
                        "Z.AI endpoint probe: {} ({}) model={} OK",
                        ep.id,
                        ep.base_url,
                        model
                    );
                    return Some(DetectedZaiEndpoint {
                        id: ep.id.to_string(),
                        base_url: ep.base_url.to_string(),
                        model: model.to_string(),
                        label: ep.label.to_string(),
                    });
                }
                Ok(r) => {
                    tracing::debug!(
                        "Z.AI endpoint probe: {} model={} returned {}",
                        ep.id,
                        model,
                        r.status().as_u16()
                    );
                }
                Err(exc) => {
                    tracing::debug!("Z.AI endpoint probe: {} model={} failed: {}", ep.id, model, exc);
                }
            }
        }
    }
    None
}

/// Blocking wrapper over [`detect_zai_endpoint_async`] (upstream probes with
/// sync httpx). Runs on a dedicated thread with its own runtime so it is safe
/// to call from both plain threads and inside an async runtime — matching
/// upstream, the calling thread blocks until the probe finishes.
pub fn detect_zai_endpoint(api_key: &str, timeout_secs: f64) -> Option<DetectedZaiEndpoint> {
    let key = api_key.to_string();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        rt.block_on(detect_zai_endpoint_async(&key, timeout_secs))
    })
    .join()
    .ok()
    .flatten()
}

/// First 16 hex chars of sha256(api_key) — the cache identity for a detected
/// endpoint (auth.py:695).
fn key_hash(api_key: &str) -> String {
    let digest = Sha256::digest(api_key.as_bytes());
    hex::encode(digest)[..16].to_string()
}

/// Return the correct Z.AI base URL by probing endpoints
/// (auth.py `_resolve_zai_base_url`).
///
/// If the user has explicitly set GLM_BASE_URL (`env_override`), that always
/// wins. Otherwise, probe the candidate endpoints to find one that accepts
/// the key. The detected endpoint is cached in provider state (auth.json)
/// keyed on a hash of the API key so subsequent starts skip the probe.
pub fn resolve_zai_base_url(api_key: &str, default_url: &str, env_override: &str) -> String {
    if !env_override.is_empty() {
        return env_override.to_string();
    }

    // No API key set → don't probe (would fire N×M HTTPS requests with an
    // empty Bearer token, all returning 401). This path is hit during
    // auxiliary-client auto-detection when the user has no Z.AI credentials
    // at all — the caller discards the result immediately, so the probe is
    // pure latency for every agent construction.
    if api_key.is_empty() {
        return default_url.to_string();
    }

    // Check provider-state cache for a previously-detected endpoint.
    if let Some(state) = joey_core::auth_store::read_provider_state("zai") {
        if let Some(cached) = state.get("detected_endpoint").and_then(Value::as_object) {
            let base = cached.get("base_url").and_then(Value::as_str).unwrap_or("");
            let hash = cached.get("key_hash").and_then(Value::as_str).unwrap_or("");
            if !base.is_empty() && hash == key_hash(api_key) {
                tracing::debug!("Z.AI: using cached endpoint {}", base);
                return base.to_string();
            }
        }
    }

    // Probe — may take up to ~8s per endpoint.
    let Some(detected) = detect_zai_endpoint(api_key, 8.0) else {
        tracing::debug!("Z.AI: probe failed, falling back to default {}", default_url);
        return default_url.to_string();
    };

    // Persist the detection result keyed on the API key hash. Persist failure
    // (disk full, permissions, lock timeout) must not break resolution —
    // detection already succeeded; worst case the next start re-probes.
    let mut detected_endpoint = Map::new();
    detected_endpoint.insert("base_url".into(), json!(detected.base_url));
    detected_endpoint.insert("endpoint_id".into(), json!(detected.id));
    detected_endpoint.insert("model".into(), json!(detected.model));
    detected_endpoint.insert("label".into(), json!(detected.label));
    detected_endpoint.insert("key_hash".into(), json!(key_hash(api_key)));
    {
        let _lock = joey_core::auth_store::auth_store_lock();
        // Reload under the lock to avoid overwriting concurrent changes.
        let mut store = joey_core::auth_store::load_auth_store();
        let mut state = joey_core::auth_store::load_provider_state(&store, "zai")
            .unwrap_or_default();
        state.insert("detected_endpoint".into(), Value::Object(detected_endpoint));
        // set_active=false: this runs from generic client construction for
        // ANY user with a Z.AI key in env, and caching a probe result must
        // not flip their active provider.
        joey_core::auth_store::store_provider_state(&mut store, "zai", state, false);
        if let Err(exc) = joey_core::auth_store::save_auth_store(&mut store) {
            tracing::warn!(
                "Z.AI: could not persist detected endpoint ({}); will re-probe next start",
                exc
            );
        }
    }
    tracing::info!(
        "Z.AI: auto-detected endpoint {} ({})",
        detected.label,
        detected.base_url
    );
    detected.base_url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_match_upstream() {
        // auth.py:623-629, verbatim.
        assert_eq!(ZAI_ENDPOINTS[0].id, "global");
        assert_eq!(ZAI_ENDPOINTS[0].base_url, "https://api.z.ai/api/paas/v4");
        assert_eq!(ZAI_ENDPOINTS[0].probe_models, &["glm-5"]);
        assert_eq!(ZAI_ENDPOINTS[0].label, "Global");
        assert_eq!(ZAI_ENDPOINTS[1].id, "cn");
        assert_eq!(ZAI_ENDPOINTS[1].base_url, "https://open.bigmodel.cn/api/paas/v4");
        assert_eq!(ZAI_ENDPOINTS[1].label, "China");
        assert_eq!(ZAI_ENDPOINTS[2].id, "coding-global");
        assert_eq!(ZAI_ENDPOINTS[2].base_url, "https://api.z.ai/api/coding/paas/v4");
        assert_eq!(
            ZAI_ENDPOINTS[2].probe_models,
            &["glm-5.2", "glm-5.1", "glm-5v-turbo", "glm-4.7"]
        );
        assert_eq!(ZAI_ENDPOINTS[2].label, "Global (Coding Plan)");
        assert_eq!(ZAI_ENDPOINTS[3].id, "coding-cn");
        assert_eq!(
            ZAI_ENDPOINTS[3].base_url,
            "https://open.bigmodel.cn/api/coding/paas/v4"
        );
        assert_eq!(ZAI_ENDPOINTS[3].label, "China (Coding Plan)");
    }

    #[test]
    fn env_override_always_wins_and_empty_key_skips_probe() {
        // env override wins without touching the store or the network.
        assert_eq!(
            resolve_zai_base_url("sk-x", "https://api.z.ai/api/paas/v4", "https://proxy.example/v4"),
            "https://proxy.example/v4"
        );
        // No key → default, no probe.
        assert_eq!(
            resolve_zai_base_url("", "https://api.z.ai/api/paas/v4", ""),
            "https://api.z.ai/api/paas/v4"
        );
    }

    #[test]
    fn cached_endpoint_short_circuits_probe() {
        let _lock = joey_core::constants::TEST_HOME_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let _guard = joey_core::constants::HomeOverrideGuard::new(dir.path().to_path_buf());

        let mut state = Map::new();
        state.insert(
            "detected_endpoint".into(),
            json!({
                "base_url": "https://api.z.ai/api/coding/paas/v4",
                "endpoint_id": "coding-global",
                "model": "glm-5.2",
                "label": "Global (Coding Plan)",
                "key_hash": key_hash("sk-cached"),
            }),
        );
        joey_core::auth_store::persist_provider_state("zai", state, false).unwrap();

        // Matching key hash → cached endpoint, no network.
        assert_eq!(
            resolve_zai_base_url("sk-cached", "https://api.z.ai/api/paas/v4", ""),
            "https://api.z.ai/api/coding/paas/v4"
        );
        // Cache write must not have flipped the active provider.
        assert!(joey_core::auth_store::load_auth_store()
            .get("active_provider")
            .is_none());
    }

    #[test]
    fn key_hash_is_sha256_prefix() {
        // sha256("abc") = ba7816bf8f01cfea41...
        assert_eq!(key_hash("abc"), "ba7816bf8f01cfea");
    }
}
