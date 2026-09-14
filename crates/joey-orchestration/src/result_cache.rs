//! Feature 030: canonical task signatures, the persistent result cache,
//! and single-flight deduplication for subagent delegation governance.

/// Canonical task signature (feature 030, data-model.md Task Signature).
/// Deterministic-key-order JSON of
/// `{ goal, context, toolsets, model_override, role, budgets { timeout_secs, cpu_ceiling_secs } }`.
/// Every result-affecting dispatch field participates; identical inputs
/// produce byte-identical strings (stable across restarts) and any
/// result-affecting difference produces a different signature.
/// `timeout_secs`/`cpu_ceiling_secs` are the resolved governance budgets
/// for this dispatch (they change what the result will be — a timed-out
/// variant is a different task; research.md R4c).
pub fn task_signature(
    req: &crate::types::DelegationRequest,
    timeout_secs: u64,
    cpu_ceiling_secs: u64,
) -> String {
    let value = serde_json::json!({
        "goal": req.goal,
        "context": match req.context.clone() {
            None => serde_json::Value::Null,
            Some(s) => serde_json::json!(s),
        },
        "toolsets": serde_json::json!(req.toolsets),
        "model_override": req
            .model
            .as_ref()
            .map(|s| serde_json::json!(s))
            .unwrap_or(serde_json::Value::Null),
        "role": serde_json::to_value(&req.role).expect("SubagentRole serializes"),
        "budgets": serde_json::json!({
            "timeout_secs": timeout_secs,
            "cpu_ceiling_secs": cpu_ceiling_secs,
        }),
    });
    // serde_json's default Map is a BTreeMap, so key order is alphabetical
    // and deterministic — no feature flags, no hand-sorting.
    serde_json::to_string(&value).expect("signature value serializes")
}

#[cfg(test)]
mod task_signature_tests {
    use super::*;
    use crate::types::{DelegationRequest, SubagentRole};

    fn req() -> DelegationRequest {
        let mut r = DelegationRequest::single("explore crates/joey-core");
        r.context = Some("find config accessors".into());
        r.model = Some("test-model".into());
        r.toolsets = vec!["file".into(), "terminal".into()];
        r
    }

    #[test]
    fn deterministic_and_stable_across_restarts() {
        let a = task_signature(&req(), 600, 300);
        let b = task_signature(&req(), 600, 300);
        assert_eq!(a, b);
        assert!(!a.is_empty());
        assert!(
            serde_json::from_str::<serde_json::Value>(&a).is_ok(),
            "signature must be valid JSON"
        );
    }

    #[test]
    fn goal_change_differs() {
        let mut r = req();
        r.goal = "explore crates/joey-tools".into();
        assert_ne!(task_signature(&r, 600, 300), task_signature(&req(), 600, 300));
    }

    #[test]
    fn context_change_and_none_differs() {
        let mut r = req();
        r.context = Some("different".into());
        assert_ne!(task_signature(&r, 600, 300), task_signature(&req(), 600, 300));
        r.context = None;
        assert_ne!(task_signature(&r, 600, 300), task_signature(&req(), 600, 300));
    }

    #[test]
    fn toolsets_change_differs() {
        let mut r = req();
        r.toolsets = vec!["web".into()];
        assert_ne!(task_signature(&r, 600, 300), task_signature(&req(), 600, 300));
    }

    #[test]
    fn model_override_differs() {
        let mut r = req();
        r.model = Some("other-model".into());
        assert_ne!(task_signature(&r, 600, 300), task_signature(&req(), 600, 300));
    }

    #[test]
    fn role_differs() {
        let mut r = req();
        r.role = SubagentRole::Orchestrator;
        assert_ne!(task_signature(&r, 600, 300), task_signature(&req(), 600, 300));
    }

    #[test]
    fn timeout_budget_differs() {
        assert_ne!(
            task_signature(&req(), 60, 300),
            task_signature(&req(), 600, 300)
        );
    }

    #[test]
    fn cpu_ceiling_budget_differs() {
        assert_ne!(
            task_signature(&req(), 600, 60),
            task_signature(&req(), 600, 300)
        );
    }

    #[test]
    fn field_set_exactly_data_model() {
        let sig = task_signature(&req(), 600, 300);
        let v: serde_json::Value =
            serde_json::from_str(&sig).expect("signature must be valid JSON");
        let obj = v.as_object().expect("signature is a JSON object");
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                &"budgets".to_string(),
                &"context".to_string(),
                &"goal".to_string(),
                &"model_override".to_string(),
                &"role".to_string(),
                &"toolsets".to_string(),
            ]
        );
        let budgets = obj["budgets"].as_object().expect("budgets is an object");
        let mut budget_keys: Vec<&String> = budgets.keys().collect();
        budget_keys.sort();
        assert_eq!(
            budget_keys,
            vec![&"cpu_ceiling_secs".to_string(), &"timeout_secs".to_string()]
        );
        assert_eq!(budgets["timeout_secs"], serde_json::json!(600));
        assert_eq!(budgets["cpu_ceiling_secs"], serde_json::json!(300));
    }
}

/// One cached successful result, keyed by exact signature.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CacheEntry {
    pub signature: String,
    /// Serialized DelegationResult summary (JSON).
    pub result_json: String,
    /// ISO 8601.
    pub created_at: String,
    /// ISO 8601; bumped on every hit (LRU).
    pub last_used_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CacheEnvelope {
    pub schema_version: u32,
    pub entries: Vec<CacheEntry>,
}

/// Stable per-field JSON serialization of a `DelegationResult` (the type
/// itself has no serde derives; feature 030, data-model.md Result Cache
/// Entry — the persisted `result_json` payload).
pub fn result_to_json(result: &crate::types::DelegationResult) -> String {
    let value = serde_json::json!({
        "goal": result.goal,
        "summary": result.summary,
        "success": result.success,
        "error": result.error,
        "model": result.model,
        "iterations": result.iterations,
        "token_usage": serde_json::to_value(&result.token_usage).unwrap(),
        "wall_clock_ms": result.wall_clock.as_millis() as u64,
        "persisted_session_id": result.persisted_session_id,
    });
    serde_json::to_string(&value).expect("result summary serializes")
}

/// Lenient inverse of [`result_to_json`]: missing keys become sensible
/// defaults, parse failure returns `None` (a cache must never panic).
pub fn result_from_json(s: &str) -> Option<crate::types::DelegationResult> {
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    Some(crate::types::DelegationResult {
        goal: v
            .get("goal")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        summary: v
            .get("summary")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        success: v.get("success").and_then(|x| x.as_bool()).unwrap_or(false),
        error: v
            .get("error")
            .and_then(|x| x.as_str())
            .map(|x| x.to_string()),
        token_usage: v
            .get("token_usage")
            .cloned()
            .map(|x| {
                serde_json::from_value::<joey_providers::Usage>(x).unwrap_or_default()
            })
            .unwrap_or_default(),
        wall_clock: std::time::Duration::from_millis(
            v.get("wall_clock_ms")
                .and_then(|x| x.as_u64())
                .unwrap_or(0),
        ),
        model: v
            .get("model")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        iterations: v
            .get("iterations")
            .and_then(|x| x.as_u64())
            .unwrap_or(0) as usize,
        persisted_session_id: v
            .get("persisted_session_id")
            .and_then(|x| x.as_str())
            .map(|x| x.to_string()),
        stop_reason: None,
    })
}

/// Persistent result cache keyed by exact task signature (feature 030,
/// FR-007). LRU eviction at `max_entries`, TTL on read, success-only
/// storage, atomic secure persistence.
pub struct ResultCache {
    path: std::path::PathBuf,
    max_entries: usize,
    ttl_hours: u64,
    entries: std::collections::HashMap<String, CacheEntry>,
    /// LRU order: least-recently-used first.
    order: Vec<String>,
}

impl ResultCache {
    /// Open (or start) the cache at `path`. Tolerant load: parse errors and
    /// `schema_version != 1` start empty. `max_entries == 0` disables the
    /// cache (lookups always miss, stores never persist).
    pub fn open(path: std::path::PathBuf, max_entries: usize, ttl_hours: u64) -> ResultCache {
        let mut cache = ResultCache {
            path,
            max_entries,
            ttl_hours,
            entries: std::collections::HashMap::new(),
            order: Vec::new(),
        };
        let Ok(text) = std::fs::read_to_string(&cache.path) else {
            return cache;
        };
        let Ok(envelope) = serde_json::from_str::<CacheEnvelope>(&text) else {
            return cache; // tolerant: parse errors -> empty
        };
        if envelope.schema_version != 1 {
            return cache; // unknown schema -> empty
        }
        for entry in envelope.entries {
            if !cache.entries.contains_key(&entry.signature) {
                cache.order.push(entry.signature.clone());
            }
            let sig = entry.signature.clone();
            cache.entries.insert(sig, entry);
        }
        cache
    }

    /// Lookup by exact byte-for-byte signature compare; expires entries older
    /// than ttl_hours (evaluated against `now`); a hit bumps last_used_at and
    /// LRU order and returns the deserialized result.
    pub fn lookup(
        &mut self,
        signature: &str,
        now: &chrono::DateTime<chrono::Utc>,
    ) -> Option<crate::types::DelegationResult> {
        if self.max_entries == 0 {
            return None; // disabled
        }
        if !self.entries.contains_key(signature) {
            return None;
        }
        let expired = {
            let entry = &self.entries[signature];
            match chrono::DateTime::parse_from_rfc3339(&entry.created_at) {
                Ok(created) => {
                    self.ttl_hours > 0
                        && *now - created.with_timezone(&chrono::Utc)
                            > chrono::Duration::hours(self.ttl_hours as i64)
                }
                Err(_) => false, // lenient: unparseable stamp -> not expired
            }
        };
        if expired {
            self.entries.remove(signature);
            self.order.retain(|s| s != signature);
            self.persist();
            return None;
        }
        if let Some(entry) = self.entries.get_mut(signature) {
            entry.last_used_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        }
        self.order.retain(|s| s != signature);
        self.order.push(signature.to_string());
        self.persist();
        self.entries
            .get(signature)
            .and_then(|e| result_from_json(&e.result_json))
    }

    /// Store a SUCCESSFUL result only (failures are not cached).
    pub fn store(
        &mut self,
        signature: String,
        result: &crate::types::DelegationResult,
        now: &chrono::DateTime<chrono::Utc>,
    ) {
        if self.max_entries == 0 {
            return; // disabled
        }
        if !result.success {
            return; // success-only
        }
        let stamp = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let entry = CacheEntry {
            signature: signature.clone(),
            result_json: result_to_json(result),
            created_at: stamp.clone(),
            last_used_at: stamp,
        };
        self.entries.insert(signature.clone(), entry);
        self.order.retain(|s| *s != signature);
        self.order.push(signature);
        // Evict least-recently-used (front of order) while over capacity;
        // max_entries >= 1 here (the disabled case returned above).
        while self.entries.len() > self.max_entries {
            let Some(oldest) = self.order.first().cloned() else {
                break;
            };
            self.order.remove(0);
            self.entries.remove(&oldest);
        }
        self.persist();
    }

    /// Atomic save: temp file + fsync + rename, file mode 0600 (user-only;
    /// SC-008). Mirrors the joey-cron atomic_write_secure pattern (jobs.rs:2171)
    /// using std::fs — tempfile is only a dev-dependency of this crate.
    fn persist(&self) {
        // Entries serialize in LRU order (oldest first) so the file itself
        // is LRU-ordered. Best-effort: a cache must never fail delegation.
        let entries: Vec<CacheEntry> = self
            .order
            .iter()
            .filter_map(|sig| self.entries.get(sig).cloned())
            .collect();
        let json =
            serde_json::to_string_pretty(&CacheEnvelope { schema_version: 1, entries }).unwrap();
        let parent = self
            .path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!("result cache: cannot create {}: {e}", parent.display());
            return;
        }
        let tmp = self.path.with_extension("json.tmp");
        let write = || -> std::io::Result<()> {
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(json.as_bytes())?;
            file.flush()?;
            file.sync_all()?;
            drop(file);
            // SC-008: user-only permissions on the temp file BEFORE the
            // atomic rename, so the final path is never briefly group- or
            // world-readable (the post-rename chmod below re-asserts it).
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(
                    &tmp,
                    std::fs::Permissions::from_mode(0o600),
                );
            }
            std::fs::rename(&tmp, &self.path)?;
            Ok(())
        };
        if let Err(e) = write() {
            tracing::warn!("result cache: persist to {} failed: {e}", self.path.display());
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                &self.path,
                std::fs::Permissions::from_mode(0o600),
            );
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod result_cache_tests {
    use super::*;
    use crate::types::DelegationResult;
    use joey_providers::Usage;
    use std::time::Duration;

    fn sample_result(success: bool) -> DelegationResult {
        DelegationResult {
            goal: "g".into(),
            summary: "s".into(),
            success,
            error: success
                .then(|| None)
                .unwrap_or_else(|| Some("boom".into())),
            token_usage: Usage {
                prompt_tokens: 1,
                ..Default::default()
            },
            wall_clock: Duration::from_millis(1234),
            model: "m".into(),
            iterations: 2,
            persisted_session_id: None,
            stop_reason: None,
        }
    }

    #[test]
    fn round_trip_result_json() {
        let r = sample_result(true);
        let j = result_to_json(&r);
        let back = result_from_json(&j).expect("round trip parses");
        assert_eq!(back.goal, r.goal);
        assert_eq!(back.summary, r.summary);
        assert_eq!(back.success, r.success);
        assert_eq!(back.error, r.error);
        assert_eq!(back.token_usage, Usage {
            prompt_tokens: 1,
            ..Default::default()
        });
        assert_eq!(back.wall_clock, Duration::from_millis(1234));
        assert_eq!(back.model, r.model);
        assert_eq!(back.iterations, r.iterations);
        assert_eq!(back.persisted_session_id, r.persisted_session_id);
        assert!(back.stop_reason.is_none());
    }

    #[test]
    fn store_then_lookup_hits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut cache = ResultCache::open(dir.path().join("result-cache.json"), 4, 24);
        let now = chrono::Utc::now();
        cache.store("A".into(), &sample_result(true), &now);
        let hit = cache.lookup("A", &now).expect("first lookup hits");
        assert_eq!(hit.goal, "g");
        assert!(
            cache.lookup("A", &now).is_some(),
            "second lookup also hits"
        );
    }

    #[test]
    fn failures_not_cached() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut cache = ResultCache::open(dir.path().join("result-cache.json"), 4, 24);
        let now = chrono::Utc::now();
        cache.store("B".into(), &sample_result(false), &now);
        assert!(cache.lookup("B", &now).is_none());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn ttl_expiry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut cache = ResultCache::open(dir.path().join("result-cache.json"), 4, 24);
        let t0 = chrono::Utc::now();
        cache.store("C".into(), &sample_result(true), &t0);
        assert!(
            cache.lookup("C", &(t0 + chrono::Duration::hours(25))).is_none(),
            "expired entry is a miss"
        );
        assert!(
            cache.lookup("C", &t0).is_none(),
            "expired entry was removed"
        );
    }

    #[test]
    fn lru_eviction() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut cache = ResultCache::open(dir.path().join("result-cache.json"), 2, 24);
        let t0 = chrono::Utc::now();
        cache.store("A".into(), &sample_result(true), &t0);
        cache.store("B".into(), &sample_result(true), &t0);
        cache.store("C".into(), &sample_result(true), &t0); // evicts A
        assert!(cache.lookup("A", &t0).is_none());
        assert!(cache.lookup("B", &t0).is_some());
        assert!(cache.lookup("C", &t0).is_some());
        cache.store("D".into(), &sample_result(true), &t0); // evicts B
        assert!(cache.lookup("A", &t0).is_none());
        assert!(cache.lookup("B", &t0).is_none(), "B evicted after D");
        assert!(cache.lookup("C", &t0).is_some());
        assert!(cache.lookup("D", &t0).is_some());
    }

    #[test]
    fn persistence_across_reload() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("result-cache.json");
        let now = chrono::Utc::now();
        {
            let mut cache = ResultCache::open(path.clone(), 4, 24);
            cache.store("A".into(), &sample_result(true), &now);
        }
        let mut reloaded = ResultCache::open(path, 4, 24);
        assert!(reloaded.lookup("A", &now).is_some());
    }

    #[test]
    fn no_cross_serving_differing_signatures() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut cache = ResultCache::open(dir.path().join("result-cache.json"), 4, 24);
        let now = chrono::Utc::now();
        cache.store("X".into(), &sample_result(true), &now);
        assert!(cache.lookup("Y", &now).is_none());
        let prefix = "X".to_string() + "1"; // "X1": prefix must not match "X"
        assert!(cache.lookup(&prefix, &now).is_none());
    }

    #[test]
    fn disabled_when_max_entries_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("result-cache.json");
        let now = chrono::Utc::now();
        let mut cache = ResultCache::open(path.clone(), 0, 24);
        cache.store("A".into(), &sample_result(true), &now);
        assert!(cache.lookup("A", &now).is_none());
        assert!(!path.exists(), "disabled cache never persists");
    }

    #[test]
    fn tolerant_load_bad_envelope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("result-cache.json");
        std::fs::write(&path, "{not json").expect("write garbage");
        let mut cache = ResultCache::open(path, 4, 24);
        assert_eq!(cache.len(), 0);
        let now = chrono::Utc::now();
        assert!(cache.lookup("A", &now).is_none());
    }
}
