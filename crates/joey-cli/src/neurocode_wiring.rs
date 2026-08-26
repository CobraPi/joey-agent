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

/// Like [`try_build_engine`], but scopes tier resolution to the provider an
/// agent built from the given (provider, base_url, model) triple would
/// ACTUALLY run on — the same triple `AgentConfig` feeds `build_client` at
/// `Agent::new`. This keeps the engine scope locked to the live agent
/// provider even when the agent triple diverges from the raw config triple:
/// CLI/session overrides (`--model gpt-5.4` forces `provider: auto` while
/// config's default model stays e.g. `glm-5.2`), and the HUD env magnet
/// (`AI_USAGE_HUD_BASE_URL` → `ai-usage-hud` only when `copilot_servable`
/// holds for the AGENT's model). `/model neurocode …` writes its keys under
/// the live agent provider, so the engine must read them there too.
pub fn try_build_engine_for_agent_inputs(
    config: &joey_core::Config,
    provider_setting: &str,
    base_url: &str,
    model: &str,
) -> Option<Arc<DefaultEngine>> {
    let scope = joey_providers::resolve_profile(provider_setting, base_url, model).name;
    try_build_engine_scoped(config, &scope)
}

/// Whether the given provider's NeuroCode tier models are configured —
/// either a per-provider entry (`neurocode.tier.providers.<id>`) or the flat
/// legacy keys (scoped variant for callers that know the live agent provider,
/// which may diverge from the config-resolved one).
pub fn tier_models_configured_scoped(config: &joey_core::Config, provider: &str) -> bool {
    let nc_cfg = NeuroCodeConfig::from_config(config);
    if !nc_cfg.enabled {
        return true; // nothing to configure when disabled
    }
    let tiers = nc_cfg.tier.tiers_for_provider(provider);
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

    /// RAII guard for tests that set the HUD magnet env var. Serializes on
    /// joey-core's TEST_HOME_OVERRIDE_LOCK (the same workspace-wide lock
    /// llm_selector's TestEnvGuard takes as the second of its pair), so this
    /// can't race those magnet tests; redirects JOEY_HOME at an empty .env so
    /// sibling `Config::load()` calls can't resurrect the scrubbed vars;
    /// restores everything on drop.
    struct HudEnvGuard {
        _override_lock: std::sync::MutexGuard<'static, ()>,
        prev_copilot: Option<String>,
        prev_hud: Option<String>,
        prev_home: Option<std::ffi::OsString>,
        _dir: tempfile::TempDir,
    }

    impl HudEnvGuard {
        fn new() -> Self {
            let _override_lock = joey_core::constants::TEST_HOME_OVERRIDE_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let prev_home = std::env::var_os("JOEY_HOME");
            let dir = tempfile::tempdir().expect("temp joey home");
            std::fs::write(dir.path().join(".env"), "").expect("seed empty .env");
            std::env::set_var("JOEY_HOME", dir.path());
            let prev_copilot = std::env::var("COPILOT_API_BASE_URL").ok();
            let prev_hud = std::env::var("AI_USAGE_HUD_BASE_URL").ok();
            std::env::remove_var("COPILOT_API_BASE_URL");
            std::env::remove_var("AI_USAGE_HUD_BASE_URL");
            Self {
                _override_lock,
                prev_copilot,
                prev_hud,
                prev_home,
                _dir: dir,
            }
        }
    }

    impl Drop for HudEnvGuard {
        fn drop(&mut self) {
            match &self.prev_home {
                Some(v) => std::env::set_var("JOEY_HOME", v),
                None => std::env::remove_var("JOEY_HOME"),
            }
            for (k, v) in [
                ("COPILOT_API_BASE_URL", &self.prev_copilot),
                ("AI_USAGE_HUD_BASE_URL", &self.prev_hud),
            ] {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
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
        assert!(tier_models_configured_scoped(&config, "zai"));
        // Enabled, no tiers at all → needs prompting.
        let config = config_with_yaml(&format!(
            "{}neurocode:\n  enabled: true\n",
            model_yaml
        ));
        assert!(!tier_models_configured_scoped(&config, "zai"));
        // Enabled, per-provider entry complete → configured.
        let config = config_with_yaml(&format!(
            "{}neurocode:\n  enabled: true\n  tier:\n    providers:\n      zai:\n        frontier: glm-5.2\n        economical: glm-4.5-flash\n",
            model_yaml
        ));
        assert!(tier_models_configured_scoped(&config, "zai"));
        // Enabled, per-provider entry partial → still needs prompting.
        let config = config_with_yaml(&format!(
            "{}neurocode:\n  enabled: true\n  tier:\n    providers:\n      zai:\n        frontier: glm-5.2\n",
            model_yaml
        ));
        assert!(!tier_models_configured_scoped(&config, "zai"));
        // Enabled, flat legacy keys complete → configured (backward compat).
        let config = config_with_yaml(&format!(
            "{}neurocode:\n  enabled: true\n  tier:\n    frontier:\n      model: f\n    economical:\n      model: e\n",
            model_yaml
        ));
        assert!(tier_models_configured_scoped(&config, "zai"));
        // Tiers configured for a DIFFERENT provider only → still needs
        // prompting for the active one.
        let config = config_with_yaml(&format!(
            "{}neurocode:\n  enabled: true\n  tier:\n    providers:\n      copilot:\n        frontier: gpt-5.4\n        economical: gpt-4o-mini\n",
            model_yaml
        ));
        assert!(!tier_models_configured_scoped(&config, "zai"));
    }

    /// ai-usage-hud: per-provider tier keys under `ai-usage-hud` are what the
    /// engine scope reads when the agent runs on the HUD proxy. The HUD env
    /// magnet auto-resolves `auto` + a Copilot-servable model onto
    /// `ai-usage-hud` — the scope must follow that resolution so keys written
    /// by `/model neurocode …` (under the live agent provider) are read back.
    #[test]
    fn engine_scopes_tier_resolution_to_ai_usage_hud_via_magnet() {
        let _g = HudEnvGuard::new();
        std::env::set_var("AI_USAGE_HUD_BASE_URL", "http://127.0.0.1:8317");
        // Agent triple: provider=auto, empty base_url, a Copilot-servable
        // model → resolve_profile magnetizes onto ai-usage-hud.
        let engine = try_build_engine_for_agent_inputs(
            &config_with_yaml(
                "neurocode:\n  enabled: true\n  tier:\n    ambiguous_default: frontier\n    frontier:\n      model: legacy-frontier\n    providers:\n      ai-usage-hud:\n        frontier: gpt-5.4\n        economical: gpt-4.1-mini\n",
            ),
            "auto",
            "",
            "gpt-5.4",
        )
        .unwrap();
        assert_eq!(engine.resolve_tier_model().as_deref(), Some("gpt-5.4"));
        // The same config scoped via the raw-config path would resolve a
        // different provider when the config model is NOT copilot-servable
        // (e.g. glm-5.2 → zai) — proving the agent-triple scope is what
        // pins the engine to ai-usage-hud here.
        let config = config_with_yaml(
            "model:\n  provider: auto\n  default: glm-5.2\nneurocode:\n  enabled: true\n  tier:\n    ambiguous_default: frontier\n    frontier:\n      model: legacy-frontier\n    providers:\n      ai-usage-hud:\n        frontier: gpt-5.4\n        economical: gpt-4.1-mini\n",
        );
        let engine = try_build_engine_for_agent_inputs(&config, "auto", "", "glm-5.2").unwrap();
        // glm is NOT copilot-servable → agent runs on zai → the HUD keys are
        // NOT consulted; flat legacy keys apply instead.
        assert_eq!(
            engine.resolve_tier_model().as_deref(),
            Some("legacy-frontier")
        );
    }

    /// ai-usage-hud: a non-Copilot-servable agent model falls through the
    /// magnet to its native provider — per-provider keys under that native
    /// provider are used, not the HUD's (mirrors the magnet's servability
    /// gate; the wiring must not second-guess it).
    #[test]
    fn engine_scope_follows_agent_inputs_not_raw_config_when_diverged() {
        let _g = HudEnvGuard::new();
        std::env::set_var("AI_USAGE_HUD_BASE_URL", "http://127.0.0.1:8317");
        // Config says auto + gpt-5.4 (copilot-servable → HUD), but the AGENT
        // runs with a session model override of glm-5.2 (auto-detected zai).
        // The agent-triple scope must resolve zai and read zai's keys.
        let config = config_with_yaml(
            "model:\n  provider: auto\n  default: gpt-5.4\nneurocode:\n  enabled: true\n  tier:\n    ambiguous_default: frontier\n    providers:\n      ai-usage-hud:\n        frontier: gpt-5.4\n        economical: gpt-4.1-mini\n      zai:\n        frontier: glm-5.2\n        economical: glm-4.5-flash\n",
        );
        let engine = try_build_engine_for_agent_inputs(&config, "auto", "", "glm-5.2").unwrap();
        assert_eq!(engine.resolve_tier_model().as_deref(), Some("glm-5.2"));
    }

    /// The `/neurocode` command engine is provider-scoped: with the live
    /// provider passed in, status/tier text reflects the per-provider keys
    /// (previously the command built an unscoped engine that fell back to
    /// flat keys / the ambiguous default).
    #[test]
    fn neurocode_command_engine_is_provider_scoped() {
        use joey_neurocode::NeuroCodeCommands;
        let config = config_with_yaml(
            "model:\n  provider: zai\n  default: glm-5.2\nneurocode:\n  enabled: true\n  tier:\n    frontier:\n      model: legacy-frontier\n    providers:\n      ai-usage-hud:\n        frontier: gpt-5.4\n        economical: gpt-4.1-mini\n",
        );
        // The helper the command handler uses to scope its engine.
        assert_eq!(
            crate::commands::neurocode::scope_for_config(&config),
            "zai",
            "config-resolved scope must match resolve_profile"
        );
        // Scoped to the LIVE agent provider (ai-usage-hud): tier show
        // displays the HUD's per-provider models, not the flat legacy keys.
        let engine =
            crate::neurocode_wiring::try_build_engine_scoped(&config, "ai-usage-hud").unwrap();
        let tier_text = engine.tier_text("show", None);
        assert!(
            tier_text.contains("frontier=gpt-5.4"),
            "scoped tier text must show the ai-usage-hud frontier model, got: {tier_text}"
        );
        assert!(
            !tier_text.contains("legacy-frontier"),
            "scoped tier text must not fall back to flat legacy keys, got: {tier_text}"
        );
    }
}
