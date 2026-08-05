//! Allocation map: the persistent mapping of each module to its assigned model
//! (T007, T010). Stored at `~/.joey/llm-selector/allocations.json` via
//! `atomic_json_write`. Versioned schema (`schema_version: 1`).

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::module::ModuleId;

/// On-disk schema version. MUST be 1 for this feature version (FR-014).
pub const SCHEMA_VERSION: u32 = 1;
/// Max diagnostic records retained (ring-buffer trim).
pub const MAX_DIAGNOSTICS: usize = 50;
/// Relative path under `~/.joey/` for the allocation map.
pub const MAP_REL_PATH: &str = "llm-selector/allocations.json";

// ── Persisted types ────────────────────────────────────────────────────────

/// One module's allocation (persisted).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocationEntry {
    pub module: ModuleId,
    pub model_id: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub implicit_pin: bool,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub estimated_performance: Option<f64>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// A diagnoser judgment (persisted inside the map).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticRecord {
    pub at: String,
    pub module: ModuleId,
    pub signal: FailureSignal,
    pub implicated_model: String,
    pub rationale: String,
}

/// The observable failure signal that triggered the diagnoser (FR-009).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureSignal {
    TurnError,
    AuxCallFailure,
    EmptyResponse,
    RetryTriggered,
}

/// Top-level on-disk allocation map (FR-014).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocationMap {
    pub schema_version: u32,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub diagnoser_model: String,
    #[serde(default = "default_budget")]
    pub learning_budget: u32,
    #[serde(default)]
    pub budget_used_this_cycle: u32,
    #[serde(default)]
    pub entries: Vec<AllocationEntry>,
    #[serde(default)]
    pub diagnostics: Vec<DiagnosticRecord>,
}

fn default_budget() -> u32 {
    8
}

impl Default for AllocationMap {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            updated_at: None,
            enabled: false,
            diagnoser_model: String::new(),
            learning_budget: default_budget(),
            budget_used_this_cycle: 0,
            entries: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}

impl AllocationMap {
    /// Resolve the on-disk path under `process_joey_home()` (machine-global,
    /// shared across profiles — FR-014).
    pub fn path() -> PathBuf {
        joey_core::constants::process_joey_home().join(MAP_REL_PATH)
    }

    /// Load from disk. A missing file is NOT an error — it means cold start.
    /// A `schema_version` mismatch IS an error (caller should auto-disable).
    pub fn load() -> Result<AllocationMap, MapError> {
        let path = Self::path();
        Self::load_from(&path)
    }

    /// Load from an explicit path (for testing).
    pub fn load_from(path: &std::path::Path) -> Result<AllocationMap, MapError> {
        if !path.exists() {
            return Ok(AllocationMap::default());
        }
        let bytes = std::fs::read(path).map_err(MapError::Io)?;
        let value: Value = serde_json::from_slice(&bytes).map_err(MapError::Json)?;
        let sv = value
            .get("schema_version")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        if sv != SCHEMA_VERSION {
            return Err(MapError::SchemaVersion(sv));
        }
        let map: AllocationMap =
            serde_json::from_value(value).map_err(MapError::Json)?;
        Ok(map)
    }

    /// Save to disk atomically (atomic_json_write — research.md §3).
    pub fn save(&mut self) -> Result<(), MapError> {
        self.updated_at = Some(Utc::now().to_rfc3339());
        self.trim_diagnostics();
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(MapError::Io)?;
        }
        joey_core::utils::atomic_json_write(&path, &self).map_err(MapError::Anyhow)
    }

    /// Save to an explicit path (for testing).
    pub fn save_to(&mut self, path: &std::path::Path) -> Result<(), MapError> {
        self.updated_at = Some(Utc::now().to_rfc3339());
        self.trim_diagnostics();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(MapError::Io)?;
        }
        joey_core::utils::atomic_json_write(path, &self).map_err(MapError::Anyhow)
    }

    /// Get an entry by module.
    pub fn get(&self, module: &ModuleId) -> Option<&AllocationEntry> {
        self.entries.iter().find(|e| &e.module == module)
    }

    /// Get a mutable entry by module.
    pub fn get_mut(&mut self, module: &ModuleId) -> Option<&mut AllocationEntry> {
        self.entries.iter_mut().find(|e| &e.module == module)
    }

    /// Upsert an entry (replace if module already present).
    pub fn upsert(&mut self, entry: AllocationEntry) {
        if let Some(existing) = self.entries.iter_mut().find(|e| e.module == entry.module) {
            *existing = entry;
        } else {
            self.entries.push(entry);
        }
    }

    /// Remove an entry by module.
    pub fn remove(&mut self, module: &ModuleId) -> Option<AllocationEntry> {
        let idx = self.entries.iter().position(|e| &e.module == module)?;
        Some(self.entries.remove(idx))
    }

    /// Append a diagnostic record, trimming the ring to MAX_DIAGNOSTICS.
    pub fn append_diagnostic(&mut self, rec: DiagnosticRecord) {
        self.diagnostics.push(rec);
        self.trim_diagnostics();
    }

    fn trim_diagnostics(&mut self) {
        if self.diagnostics.len() > MAX_DIAGNOSTICS {
            let drop = self.diagnostics.len() - MAX_DIAGNOSTICS;
            self.diagnostics.drain(0..drop);
        }
    }

    /// Build an index map for fast lookup (used by the engine).
    pub fn entry_index(&self) -> HashMap<ModuleId, &AllocationEntry> {
        self.entries.iter().map(|e| (e.module.clone(), e)).collect()
    }

    /// Build a mutable index map for fast lookup.
    pub fn entry_index_mut(&mut self) -> HashMap<ModuleId, &mut AllocationEntry> {
        self.entries
            .iter_mut()
            .map(|e| (e.module.clone(), e))
            .collect()
    }
}

/// Errors from the allocation map.
#[derive(Debug, thiserror::Error)]
pub enum MapError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("atomic write failed: {0}")]
    Anyhow(#[from] anyhow::Error),
    #[error(
        "schema_version mismatch: found {0}, expected 1. Refusing to silently migrate."
    )]
    SchemaVersion(u32),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_map() -> AllocationMap {
        let mut m = AllocationMap::default();
        m.enabled = true;
        m.diagnoser_model = "gpt-4.1".to_string();
        m.upsert(AllocationEntry {
            module: ModuleId::MainTurn,
            model_id: "gpt-4.1".to_string(),
            pinned: false,
            implicit_pin: false,
            reason: "cold-start".to_string(),
            estimated_performance: None,
            updated_at: None,
        });
        m.upsert(AllocationEntry {
            module: ModuleId::Compression,
            model_id: "claude-haiku-4-5".to_string(),
            pinned: true,
            implicit_pin: false,
            reason: "user pin".to_string(),
            estimated_performance: Some(0.81),
            updated_at: None,
        });
        m
    }

    #[test]
    fn test_round_trip_preserves_entries() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("allocations.json");
        let mut m = sample_map();
        m.save_to(&path).unwrap();
        let loaded = AllocationMap::load_from(&path).unwrap();
        assert_eq!(loaded.enabled, true);
        assert_eq!(loaded.diagnoser_model, "gpt-4.1");
        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(
            loaded.get(&ModuleId::MainTurn).unwrap().model_id,
            "gpt-4.1"
        );
        let comp = loaded.get(&ModuleId::Compression).unwrap();
        assert_eq!(comp.model_id, "claude-haiku-4-5");
        assert!(comp.pinned);
        assert_eq!(comp.estimated_performance, Some(0.81));
    }

    #[test]
    fn test_round_trip_default_fields_when_absent() {
        // A minimal JSON with only required fields should load with defaults.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("allocations.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "schema_version": 1,
                "entries": [
                    {"module": "main_turn", "model_id": "x"}
                ]
            })
            .to_string(),
        )
        .unwrap();
        let m = AllocationMap::load_from(&path).unwrap();
        let e = m.get(&ModuleId::MainTurn).unwrap();
        assert!(!e.pinned);
        assert!(!e.implicit_pin);
        assert_eq!(e.reason, "");
        assert!(e.estimated_performance.is_none());
    }

    #[test]
    fn test_round_trip_custom_module() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("allocations.json");
        let mut m = AllocationMap::default();
        m.upsert(AllocationEntry {
            module: ModuleId::Custom("vision_check".to_string()),
            model_id: "gpt-4o".to_string(),
            pinned: false,
            implicit_pin: true,
            reason: "implicit".to_string(),
            estimated_performance: None,
            updated_at: None,
        });
        m.save_to(&path).unwrap();
        let loaded = AllocationMap::load_from(&path).unwrap();
        let e = loaded
            .get(&ModuleId::Custom("vision_check".to_string()))
            .unwrap();
        assert_eq!(e.model_id, "gpt-4o");
        assert!(e.implicit_pin);
    }

    #[test]
    fn test_missing_file_is_cold_start() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nope.json");
        let m = AllocationMap::load_from(&path).unwrap();
        assert_eq!(m.entries.len(), 0);
        assert!(!m.enabled);
    }

    #[test]
    fn test_schema_version_mismatch_is_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("allocations.json");
        std::fs::write(
            &path,
            serde_json::json!({"schema_version": 99, "entries": []}).to_string(),
        )
        .unwrap();
        let err = AllocationMap::load_from(&path).unwrap_err();
        assert!(matches!(err, MapError::SchemaVersion(99)));
    }

    #[test]
    fn test_diagnostics_ring_trim() {
        let mut m = AllocationMap::default();
        for i in 0..(MAX_DIAGNOSTICS + 10) {
            m.append_diagnostic(DiagnosticRecord {
                at: format!("2026-01-0{}", i % 9),
                module: ModuleId::MainTurn,
                signal: FailureSignal::EmptyResponse,
                implicated_model: format!("m{}", i),
                rationale: "test".to_string(),
            });
        }
        assert_eq!(m.diagnostics.len(), MAX_DIAGNOSTICS);
        // Oldest dropped, newest kept.
        assert_eq!(m.diagnostics[0].implicated_model, "m10");
        assert_eq!(m.diagnostics.last().unwrap().implicated_model, "m59");
    }

    #[test]
    fn test_upsert_replaces_existing() {
        let mut m = AllocationMap::default();
        m.upsert(AllocationEntry {
            module: ModuleId::MainTurn,
            model_id: "a".to_string(),
            pinned: false,
            implicit_pin: false,
            reason: "".to_string(),
            estimated_performance: None,
            updated_at: None,
        });
        m.upsert(AllocationEntry {
            module: ModuleId::MainTurn,
            model_id: "b".to_string(),
            pinned: false,
            implicit_pin: false,
            reason: "".to_string(),
            estimated_performance: None,
            updated_at: None,
        });
        assert_eq!(m.entries.len(), 1);
        assert_eq!(m.get(&ModuleId::MainTurn).unwrap().model_id, "b");
    }
}
