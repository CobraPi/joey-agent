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
    let mut engine = DefaultEngine::new(nc_cfg, project_root);
    // Scope tier-model resolution to the active provider's per-provider tier
    // config (`neurocode.tier.providers.<id>`), resolved the same way the
    // agent resolves it (resolve_profile over provider/base_url/model).
    let provider = config.get_str("model.provider", "auto");
    let base_url = config.get_str("model.base_url", "");
    let model = config.model();
    let profile = joey_providers::resolve_profile(&provider, &base_url, &model);
    engine.set_provider(profile.name);
    Some(Arc::new(engine))
}

/// Like [`try_build_engine`], but scopes the tier resolution to an EXPLICIT
/// provider name (the agent's live provider after a runtime `/model`
/// switch, which may differ from config's `model.provider` for the session).
pub fn try_build_engine_scoped(
    config: &joey_core::Config,
    provider: &str,
) -> Option<Arc<DefaultEngine>> {
    let nc_cfg = NeuroCodeConfig::from_config(config);
    if !nc_cfg.enabled {
        return None;
    }
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut engine = DefaultEngine::new(nc_cfg, project_root);
    engine.set_provider(provider);
    Some(Arc::new(engine))
}

/// Whether the active provider's NeuroCode tier models are configured —
/// either a per-provider entry (`neurocode.tier.providers.<id>`) or the flat
/// legacy keys. When false and NeuroCode is enabled, callers should prompt
/// the user to pick frontier/economical models from the provider's catalog.
pub fn tier_models_configured(config: &joey_core::Config) -> bool {
    let nc_cfg = NeuroCodeConfig::from_config(config);
    if !nc_cfg.enabled {
        return true; // nothing to configure when disabled
    }
    let provider = config.get_str("model.provider", "auto");
    let base_url = config.get_str("model.base_url", "");
    let model = config.model();
    let profile = joey_providers::resolve_profile(&provider, &base_url, &model);
    let tiers = nc_cfg.tier.tiers_for_provider(profile.name);
    !tiers.frontier.is_empty() && !tiers.economical.is_empty()
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
        if symbol.is_empty() {
            return "Usage: neurocode_query <query_type> <symbol>".to_string();
        }
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

    #[test]
    fn engine_scopes_tier_resolution_to_active_provider() {
        // Per-provider tier entry for the resolved profile wins over the flat
        // legacy keys, so provider switches can't leak another provider's
        // models into the tier routing. ambiguous_default: frontier makes
        // resolve_tier_model consult the frontier tier.
        let model_yaml = "model:\n  provider: zai\n  default: glm-5.2\n";
        let config = config_with_yaml(&format!(
            "{}neurocode:\n  enabled: true\n  tier:\n    ambiguous_default: frontier\n    frontier:\n      model: legacy-frontier\n    providers:\n      zai:\n        frontier: glm-5.2\n",
            model_yaml
        ));
        let engine = try_build_engine(&config).unwrap();
        assert_eq!(engine.resolve_tier_model().as_deref(), Some("glm-5.2"));
    }

    #[test]
    fn tier_models_configured_reflects_per_provider_entries() {
        let model_yaml = "model:\n  provider: zai\n  default: glm-5.2\n";
        // NeuroCode disabled → nothing to configure.
        let config = config_with_yaml(model_yaml);
        assert!(tier_models_configured(&config));
        // Enabled, no tiers at all → needs prompting.
        let config = config_with_yaml(&format!(
            "{}neurocode:\n  enabled: true\n",
            model_yaml
        ));
        assert!(!tier_models_configured(&config));
        // Enabled, per-provider entry complete → configured.
        let config = config_with_yaml(&format!(
            "{}neurocode:\n  enabled: true\n  tier:\n    providers:\n      zai:\n        frontier: glm-5.2\n        economical: glm-4.5-flash\n",
            model_yaml
        ));
        assert!(tier_models_configured(&config));
        // Enabled, per-provider entry partial → still needs prompting.
        let config = config_with_yaml(&format!(
            "{}neurocode:\n  enabled: true\n  tier:\n    providers:\n      zai:\n        frontier: glm-5.2\n",
            model_yaml
        ));
        assert!(!tier_models_configured(&config));
        // Enabled, flat legacy keys complete → configured (backward compat).
        let config = config_with_yaml(&format!(
            "{}neurocode:\n  enabled: true\n  tier:\n    frontier:\n      model: f\n    economical:\n      model: e\n",
            model_yaml
        ));
        assert!(tier_models_configured(&config));
        // Tiers configured for a DIFFERENT provider only → still needs
        // prompting for the active one.
        let config = config_with_yaml(&format!(
            "{}neurocode:\n  enabled: true\n  tier:\n    providers:\n      copilot:\n        frontier: gpt-5.4\n        economical: gpt-4o-mini\n",
            model_yaml
        ));
        assert!(!tier_models_configured(&config));
    }
}
