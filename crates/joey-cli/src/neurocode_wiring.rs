//! NeuroCode engine construction + wiring (feature 015, T056/T057).
//!
//! Mirrors the `llm_selector::try_build_allocator` pattern: build the engine
//! from config when `neurocode.enabled = true`, inject it into the agent via
//! `set_neurocode_engine` (turn-loop intercept), and bridge it to the
//! `NeuroCodeBackend` trait so the 4 NeuroCode tools are registered for the
//! model.
//!
//! Off-by-default: when `neurocode.enabled` is false (the default), returns
//! `None` and callers skip all wiring — byte-identical to pre-feature-015
//! (Constitution VII, FR-003/FR-020).

use std::path::PathBuf;
use std::sync::Arc;

use joey_neurocode::{DefaultEngine, NeuroCodeCommands, NeuroCodeConfig, NeuroCodeEngine};
use joey_tools::tools::neurocode_tools::NeuroCodeBackend;

/// Build a NeuroCode engine from the current joey config, scoped to the given
/// project root (the cwd for the interactive REPL and oneshot paths).
///
/// Returns `None` when NeuroCode is disabled in config (`neurocode.enabled`
/// is false — the default), so callers skip wiring entirely and stay
/// byte-identical to pre-feature-015 (FR-020).
pub fn try_build_engine(config: &joey_core::Config) -> Option<Arc<DefaultEngine>> {
    let nc_cfg = NeuroCodeConfig::from_config(config);
    if !nc_cfg.enabled {
        return None;
    }
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    Some(Arc::new(DefaultEngine::new(nc_cfg, project_root)))
}

/// Bridge `DefaultEngine` to the `NeuroCodeBackend` trait the joey-tools
/// NeuroCode tools consume (T057).
///
/// The trait lives in joey-tools (DAG constraint: joey-tools cannot depend on
/// joey-neurocode), so the concrete engine is adapted here in joey-cli where
/// both types are available. Delegates to the `NeuroCodeCommands` methods,
/// which return the same plain text the `/neurocode` slash command renders.
struct EngineBackend {
    engine: Arc<DefaultEngine>,
}

impl NeuroCodeBackend for EngineBackend {
    fn index(&self, path: &str, force: bool) -> String {
        // Explicit path overrides the engine's project root for this call.
        if !path.is_empty() && path != "." {
            let config = joey_core::Config::load().unwrap_or_else(|_| joey_core::Config::defaults());
            let nc_cfg = NeuroCodeConfig::from_config(&config);
            let engine = DefaultEngine::new(nc_cfg, PathBuf::from(path));
            return engine.index_text(force);
        }
        self.engine.index_text(force)
    }

    fn query(&self, query_type: &str, symbol: &str, limit: usize) -> String {
        let mut out = self.engine.query_text(query_type, symbol);
        // The command surface has no limit param; note the requested limit for
        // the model when it asks for fewer/more than the default.
        if limit != 10 {
            out.push_str(&format!("\n(requested limit: {})", limit));
        }
        out
    }

    fn status(&self) -> String {
        self.engine.status_text()
    }

    fn ingest(
        &self,
        category: &str,
        source_path: &str,
        version_tag: Option<&str>,
        provenance: &str,
    ) -> String {
        self.engine
            .ingest_text(category, source_path, version_tag, provenance)
    }

    fn is_active(&self) -> bool {
        self.engine.is_active()
    }
}

/// Build the `NeuroCodeBackend` trait object bridged over the engine (T057).
pub fn backend_for_engine(engine: &Arc<DefaultEngine>) -> Arc<dyn NeuroCodeBackend> {
    Arc::new(EngineBackend {
        engine: Arc::clone(engine),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a Config from raw YAML via a temp file (the config layer has no
    /// in-memory constructor for user values).
    fn config_with_yaml(yaml: &str) -> joey_core::Config {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), yaml).unwrap();
        joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap()
    }

    #[test]
    fn disabled_config_yields_none() {
        // Default config has neurocode.enabled = false.
        let config = joey_core::Config::defaults();
        assert!(try_build_engine(&config).is_none());
    }

    #[test]
    fn enabled_config_yields_engine() {
        let config = config_with_yaml("neurocode:\n  enabled: true\n");
        let engine = try_build_engine(&config);
        assert!(engine.is_some());
        assert!(engine.unwrap().is_active());
    }

    #[test]
    fn backend_delegates_to_engine() {
        let config = config_with_yaml("neurocode:\n  enabled: true\n");
        let engine = try_build_engine(&config).unwrap();
        let backend = backend_for_engine(&engine);
        assert!(backend.is_active());
        // status() returns the same text as the engine's status_text().
        let status = backend.status();
        assert!(status.contains("NeuroCode"));
    }
}
