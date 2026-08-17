//! `/llm-selector` command handler (T020-T022).
//!
//! Text-mode control surface for the dynamic LLM model selector. Implements
//! Constitution II (CLI/TUI parity) — every capability is reachable as text.

use joey_llm_selector::{
    render_status, CandidateModelPool, SelectorConfig, SelectorEngine,
};

/// Entry point for the `/llm-selector` slash command.
pub fn llm_selector_slash(args: &str) -> Result<(), String> {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let sub = parts.first().copied().unwrap_or("status");

    let engine = build_engine();

    match sub {
        "status" => cmd_status(&engine),
        "pool" => cmd_pool(&engine),
        "allocations" => cmd_allocations(&engine),
        "diagnostics" => cmd_diagnostics(&engine, &parts[1..]),
        "pin" => cmd_pin(&engine, &parts[1..]),
        "unpin" => cmd_unpin(&engine, &parts[1..]),
        "budget" => cmd_budget(&engine, &parts[1..]),
        "diagnoser" => cmd_diagnoser(&engine, &parts[1..]),
        "enable" => cmd_enable(&engine),
        "disable" => cmd_disable(&engine),
        "refresh" => cmd_refresh(&engine),
        "help" | "-h" | "--help" => {
            cmd_help();
            Ok(())
        }
        _ => Err(format!(
            "unknown subcommand '{}'. Run /llm-selector help.",
            sub
        )),
    }
}

/// Fetch and consolidate the active provider's candidate pool (T070, FR-003,
/// SC-005). Returns an empty pool on any failure (FR-017 — the caller
/// auto-disables when the pool is empty).
///
/// Source selection mirrors the plan's research.md §6: Copilot uses its own
/// typed catalog fetch; every other catalog-exposing provider is sourced from
/// the models.dev registry (which covers OpenRouter, Anthropic, OpenAI, ZAI,
/// Gemini, xAI, …). This keeps the selector multi-provider without an upward
/// dependency on `joey-cli` from `joey-llm-selector` — the consolidator lives
/// in the selector crate and consumes raw JSON passed in as a parameter.
pub(crate) fn fetch_candidate_pool(provider: &str) -> CandidateModelPool {
    use joey_llm_selector::candidate::{CatalogSource, consolidate_copilot, consolidate_models_dev};

    if joey_providers::profile::is_copilot_wire(provider) {
        // Copilot's catalog fetch is the canonical source (research.md §6).
        // Covers the ai-usage-hud reverse proxy too: it serves the same
        // Copilot wire catalog from its own /models endpoint.
        match joey_providers::copilot::fetch_model_catalog(std::time::Duration::from_secs(10)) {
            Ok(raw) => {
                let (models, _dropped) = consolidate_copilot(&raw);
                CandidateModelPool::from_consolidated(models, CatalogSource::Copilot)
            }
            Err(e) => {
                eprintln!("llm-selector: copilot catalog fetch failed: {e}");
                CandidateModelPool::default()
            }
        }
    } else {
        // models.dev covers the other catalog-exposing providers.
        let raw = crate::model_catalog::models_dev_entries_for_provider(provider);
        if raw.is_empty() {
            eprintln!(
                "llm-selector: no models.dev entries for provider '{}'",
                provider
            );
            return CandidateModelPool::default();
        }
        let (models, _dropped) = consolidate_models_dev(provider, &raw);
        CandidateModelPool::from_consolidated(models, CatalogSource::ModelsDotDev)
    }
}

/// Build a selector engine from the current joey config and apply implicit
/// pins (FR-013) from any explicit per-task model config keys.
///
/// Returns `None` when the selector is disabled (`model.selector.enabled`
/// is false AND the configured model is not `auto`), so callers can skip
/// wiring entirely and stay byte-identical to pre-feature-011 (Constitution
/// VII non-regression).
pub fn try_build_allocator(
    config: &joey_core::Config,
) -> Option<std::sync::Arc<joey_llm_selector::SelectorEngine>> {
    let enabled = config.get_bool("model.selector.enabled", false);
    let configured_model = config.model();
    // Off-by-default: only build when explicitly enabled OR when `auto` is
    // the configured model (the activation sentinel, FR-002/FR-020).
    // The engine itself stays inactive until the pool is populated and
    // `auto` is active, but we construct it early so `/llm-selector` can
    // inspect state and so the intercept is ready the moment `auto` engages.
    if !enabled && configured_model != "auto" {
        return None;
    }
    let provider = resolve_provider_name(config);
    let selector_cfg = joey_llm_selector::SelectorConfig {
        enabled,
        configured_model: configured_model.clone(),
        learning_budget: config.get_i64("model.selector.budget", 8).max(0) as u32,
        diagnoser_model: config.get_str("model.selector.diagnoser_model", ""),
    };
    let engine = joey_llm_selector::SelectorEngine::new(selector_cfg);
    // Apply implicit pins from explicit per-task config (FR-013, T066).
    engine.apply_implicit_pins_from_config(config);
    // FR-015 / T073: thread the active profile's curated fallback_models into
    // the engine so the degraded-fallback chain can walk them before falling
    // to cfg.model() (research.md §8 (a)).
    let base_url = config.get_str("model.base_url", "");
    let profile = joey_providers::profile::resolve_profile(&provider, &base_url, &configured_model);
    engine.set_fallback_models(profile.fallback_models.iter().map(|s| s.to_string()).collect());
    engine.set_provider(provider.clone());
    // Populate the candidate pool from the active provider's catalog (T070,
    // FR-003). Without this, `is_active()` is always false (compute_active
    // requires a non-empty pool) and the selector is dead code at runtime.
    let pool = fetch_candidate_pool(&provider);
    engine.set_pool(pool);
    // FR-017: if the pool is empty after the fetch attempt, auto-disable with
    // a notice (the selector cannot operate without a candidate pool).
    engine.auto_disable_on_empty_pool();
    // FR-008/FR-009 T076: construct the LLM judge client so the detached
    // learning loop makes a real provider call instead of relying solely on
    // the signal-driven heuristic. Falls back to heuristic-only when the client
    // can't be built (no credentials / no diagnoser model / unsupported wire).
    let diagnoser_model = engine.map_snapshot().diagnoser_model.clone();
    let judge = joey_llm_selector::diagnoser::LlmDiagnoser::try_new(
        &provider,
        &config.get_str("model.base_url", ""),
        &diagnoser_model,
        None,
    )
    .map(|c| std::sync::Arc::new(c) as std::sync::Arc<dyn joey_llm_selector::diagnoser::DiagnoserClient>);
    engine.set_diagnoser_client(judge);
    let engine = std::sync::Arc::new(engine);
    // FR-009: start the detached diagnoser task (consumes observations from the
    // channel, runs the learning loop off the hot path). Must be inside a tokio
    // runtime — `try_build_allocator` is called from async oneshot/repl paths.
    engine.start_diagnoser();
    Some(engine)
}

/// Resolve the active provider name from config. The provider lives at
/// `model.provider` (the same key `AgentConfig` reads at agent.rs:106); it may
/// be set directly or as `auto` (resolved elsewhere). When it is `auto` or
/// unset we fall back to the model string's vendor prefix (e.g.
/// "anthropic/claude-..." → "anthropic") so the catalog fetch still targets
/// the right source.
fn resolve_provider_name(config: &joey_core::Config) -> String {
    let provider = config.get_str("model.provider", "");
    if !provider.is_empty() && provider != "auto" {
        return provider;
    }
    // Custom Copilot-compatible endpoint active: every model is served by the
    // proxy through a copilot-wire profile, so the candidate-pool fetch must
    // target the copilot catalog source regardless of the model's vendor
    // prefix (mirrors the resolve_profile magnet in joey-providers).
    if joey_providers::copilot::hud_endpoint().is_some() {
        return "ai-usage-hud".to_string();
    }
    if joey_providers::copilot::custom_endpoint().is_some() {
        return "copilot".to_string();
    }
    // Provider is "auto" or unset: derive from the model string's vendor
    // prefix (e.g. "anthropic/claude-..." → "anthropic").
    let model = config.model();
    if let Some((vendor, _)) = model.split_once('/') {
        return vendor.to_string();
    }
    // No signal — return "auto" so the fetch helper returns an empty pool
    // (FR-017 auto-disable path) rather than guessing.
    if provider.is_empty() { "auto".to_string() } else { provider }
}

/// Build a selector engine from the current joey config.
fn build_engine() -> SelectorEngine {
    let config = joey_core::Config::load().unwrap_or_else(|_| {
        // If config can't load, use an empty config (all defaults).
        joey_core::Config::load_from(std::path::PathBuf::new())
            .unwrap_or_else(|_| panic!("config load failed"))
    });
    let selector_cfg = SelectorConfig {
        enabled: config.get_bool("model.selector.enabled", false),
        configured_model: config.model(),
        learning_budget: config.get_i64("model.selector.budget", 8).max(0) as u32,
        diagnoser_model: config.get_str("model.selector.diagnoser_model", ""),
    };
    let engine = SelectorEngine::new(selector_cfg);
    // Populate the pool so `/llm-selector status` reports real state even
    // when invoked outside the agent construction path.
    let provider = resolve_provider_name(&config);
    // FR-015 / T073: also thread the fallback models for the standalone path.
    let configured = config.model();
    let base_url = config.get_str("model.base_url", "");
    let profile = joey_providers::profile::resolve_profile(&provider, &base_url, &configured);
    engine.set_fallback_models(profile.fallback_models.iter().map(|s| s.to_string()).collect());
    engine.set_provider(provider.clone());
    let pool = fetch_candidate_pool(&provider);
    engine.set_pool(pool);
    engine.auto_disable_on_empty_pool();
    engine
}

fn cmd_status(engine: &SelectorEngine) -> Result<(), String> {
    use joey_llm_selector::SelectorQuery;
    let q = SelectorQuery::new(engine);
    let report = q.status();
    print!("{}", render_status(&report));
    Ok(())
}

fn cmd_pool(engine: &SelectorEngine) -> Result<(), String> {
    use joey_llm_selector::SelectorQuery;
    let q = SelectorQuery::new(engine);
    let pool = q.pool();
    if pool.is_empty() {
        println!("Candidate pool is empty (no catalog-exposing provider active).");
    } else {
        println!("Candidate pool ({} models):", pool.len());
        for m in &pool {
            println!(
                "  {:<30} {:<12} ctx={:<6} {:<10} {}{}",
                m.id,
                m.tier,
                m.context_window / 1000,
                m.provider,
                if m.supports_tools { "[tools]" } else { "" },
                if m.supports_vision { "[vision]" } else { "" },
            );
        }
    }
    Ok(())
}

fn cmd_enable(engine: &SelectorEngine) -> Result<(), String> {
    use joey_llm_selector::SelectorQuery;
    let q = SelectorQuery::new(engine);
    q.enable();
    println!("LLM Selector enabled. Select the 'auto' model to engage dynamic allocation.");
    Ok(())
}

/// `/llm-selector refresh` (T071, contracts/llm-selector-command.md row 11):
/// force-refresh the candidate pool from the live catalog. Re-fetches using
/// the active provider, replaces the pool, and reports the new size. Exits
/// with an error (non-zero) when the refresh yields an empty pool (degraded).
fn cmd_refresh(engine: &SelectorEngine) -> Result<(), String> {
    // Use the provider recorded at construction (handles the `auto` model case
    // where the model string has no vendor prefix).
    let provider = engine.provider();
    if provider.is_empty() || provider == "auto" {
        return Err(
            "cannot refresh: active provider is unknown or 'auto' with no model prefix".to_string(),
        );
    }
    let pool = fetch_candidate_pool(&provider);
    let n = pool.len();
    engine.set_pool(pool);
    engine.auto_disable_on_empty_pool();
    if n == 0 {
        return Err(format!(
            "catalog refresh yielded 0 models for provider '{}'",
            provider
        ));
    }
    println!("Candidate pool refreshed: {} models.", n);
    Ok(())
}

fn cmd_disable(engine: &SelectorEngine) -> Result<(), String> {
    use joey_llm_selector::SelectorQuery;
    let q = SelectorQuery::new(engine);
    q.disable();
    println!("LLM Selector disabled. Using the configured model for all modules.");
    Ok(())
}

/// Parse a module argument using ModuleId::parse (contracts/llm-selector-command.md
/// "Module argument grammar"). Returns Err with a helpful message on failure.
fn parse_module(s: Option<&str>) -> Result<joey_llm_selector::ModuleId, String> {
    let s = s.ok_or_else(|| "missing <module> argument".to_string())?;
    joey_llm_selector::ModuleId::parse(s)
}

fn cmd_allocations(engine: &SelectorEngine) -> Result<(), String> {
    use joey_llm_selector::SelectorQuery;
    let q = SelectorQuery::new(engine);
    let report = q.status();
    if report.entries.is_empty() {
        println!("No allocations yet (selector has not resolved any modules).");
    } else {
        println!("Allocation map:");
        for e in &report.entries {
            let flags = match (e.pinned, e.implicit_pin) {
                (true, _) => " [pinned]",
                (false, true) => " [implicit pin]",
                (false, false) => "",
            };
            let perf = e
                .estimated_performance
                .map(|p| format!(" p_j={:.2}", p))
                .unwrap_or_default();
            let updated = e.updated_at.as_deref().unwrap_or("");
            println!(
                "  {:<14} -> {:<24}{}{}",
                e.module, e.model_id, flags, perf
            );
            if !e.reason.is_empty() {
                println!("                 reason: {}", e.reason);
            }
            if !updated.is_empty() {
                println!("                 updated: {}", updated);
            }
        }
    }
    Ok(())
}

fn cmd_diagnostics(engine: &SelectorEngine, args: &[&str]) -> Result<(), String> {
    use joey_llm_selector::SelectorQuery;
    // Parse optional `-n <count>`.
    let mut limit: usize = 20;
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if a == "-n" {
            i += 1;
            let n_str = args.get(i).ok_or_else(|| "-n requires a count".to_string())?;
            limit = n_str
                .parse::<usize>()
                .map_err(|_| format!("invalid count '{}'", n_str))?;
        } else {
            return Err(format!("unknown diagnostics argument '{}'", a));
        }
        i += 1;
    }
    let q = SelectorQuery::new(engine);
    let rows = q.diagnostics(limit);
    if rows.is_empty() {
        println!("No diagnostics recorded.");
    } else {
        println!("Diagnostics (last {}):", rows.len());
        for d in &rows {
            println!(
                "  [{}] module={} signal={} model={}",
                d.at, d.module, d.signal, d.implicated_model
            );
            if !d.rationale.is_empty() {
                println!("         {}", d.rationale);
            }
        }
    }
    Ok(())
}

fn cmd_pin(engine: &SelectorEngine, args: &[&str]) -> Result<(), String> {
    use joey_llm_selector::SelectorQuery;
    let module = parse_module(args.first().copied())?;
    let model_id = args
        .get(1)
        .ok_or_else(|| "missing <model_id> argument".to_string())?
        .to_string();
    let q = SelectorQuery::new(engine);
    q.pin(module, model_id.clone())?;
    println!("Pinned module to model '{}'. Exempt from reallocation.", model_id);
    Ok(())
}

fn cmd_unpin(engine: &SelectorEngine, args: &[&str]) -> Result<(), String> {
    use joey_llm_selector::SelectorQuery;
    let module = parse_module(args.first().copied())?;
    let q = SelectorQuery::new(engine);
    q.unpin(&module)?;
    println!("Unpinned module {}.", module);
    Ok(())
}

fn cmd_budget(engine: &SelectorEngine, args: &[&str]) -> Result<(), String> {
    use joey_llm_selector::SelectorQuery;
    let n_str = args
        .first()
        .ok_or_else(|| "missing <n> argument (0 disables learning)".to_string())?;
    let n: u32 = n_str
        .parse::<i64>()
        .map_err(|_| format!("invalid budget '{}'", n_str))?
        .max(0) as u32;
    let q = SelectorQuery::new(engine);
    q.set_budget(n);
    if n == 0 {
        println!("Learning budget set to 0 — learning disabled (routing from cold-start map only).");
    } else {
        println!("Learning budget set to {}.", n);
    }
    Ok(())
}

fn cmd_diagnoser(engine: &SelectorEngine, args: &[&str]) -> Result<(), String> {
    use joey_llm_selector::SelectorQuery;
    // No args: show current diagnoser model.
    if args.is_empty() {
        let q = SelectorQuery::new(engine);
        let report = q.status();
        let m = if report.diagnoser_model.is_empty() {
            "(unset)"
        } else {
            &report.diagnoser_model
        };
        println!("Diagnoser model: {}", m);
        return Ok(());
    }
    // Otherwise: set the diagnoser model.
    let model_id = args[0];
    let q = SelectorQuery::new(engine);
    q.set_diagnoser_model(model_id)?;
    println!("Diagnoser model set to '{}'.", model_id);
    Ok(())
}

fn cmd_help() {
    println!("Usage: /llm-selector <subcommand>");
    println!();
    println!("Subcommands:");
    println!("  status                 Show enabled/disabled state, pool size, diagnoser model");
    println!("  pool                   List all candidate models in the active catalog");
    println!("  allocations            Print the full allocation map (per module -> model)");
    println!("  diagnostics [-n <n>]   Print the last N diagnoser judgments (default 20)");
    println!("  pin <module> <model>   Pin a module to a model; exempt from reallocation");
    println!("  unpin <module>         Remove a user pin");
    println!("  budget <n>             Set the learning budget (0 disables learning)");
    println!("  diagnoser [<model>]    Show or set the diagnoser model (versatile tier only)");
    println!("  enable                 Enable dynamic allocation (engages when model is 'auto')");
    println!("  disable                Disable dynamic allocation (fall back to configured model)");
    println!("  refresh                Force-refresh the candidate pool from the live catalog");
    println!("  help                   Show this help message");
    println!();
    println!("Module argument: main_turn | compression | subagent | custom:<name>");
    println!();
    println!("Alias: /llm-s (prefix abbreviation)");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shared lock for tests that mutate COPILOT_API_BASE_URL (process-global
    /// env; parallel test threads would race otherwise).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Constitution VII non-regression: `try_build_allocator` returns None when
    /// the selector is neither enabled nor engaged via the `auto` sentinel.
    /// This is the byte-identical-to-pre-feature-011 invariant — when None,
    /// the agent never wires the intercept and uses the configured model verbatim.
    #[test]
    fn try_build_allocator_none_when_disabled_and_not_auto() {
        let mut cfg = joey_core::Config::defaults();
        // Concretely configured model, selector not enabled.
        cfg.set_model_override("gpt-4o");
        assert_ne!(cfg.model(), "auto");
        assert!(!cfg.get_bool("model.selector.enabled", false));
        assert!(try_build_allocator(&cfg).is_none());
    }

    /// `try_build_allocator` returns Some when `auto` is the configured model
    /// (the activation sentinel, FR-002/FR-020) even when `model.selector.enabled`
    /// is false — the engine is constructed early so `/llm-selector` can inspect
    /// state and the intercept is ready the moment `auto` engages.
    #[test]
    fn try_build_allocator_some_when_auto_active() {
        let mut cfg = joey_core::Config::defaults();
        cfg.set_model_override("auto");
        assert_eq!(cfg.model(), "auto");
        assert!(try_build_allocator(&cfg).is_some());
    }

    /// `/llm-selector help` succeeds (exit-success path). Contracts require
    /// exit 0 on `help` (T024).
    #[test]
    fn llm_selector_help_succeeds() {
        assert!(llm_selector_slash("help").is_ok());
        // The `-h` / `--help` aliases also succeed.
        assert!(llm_selector_slash("-h").is_ok());
        assert!(llm_selector_slash("--help").is_ok());
    }

    /// Unknown subcommand returns an error (non-zero exit).
    #[test]
    fn llm_selector_unknown_subcommand_errors() {
        assert!(llm_selector_slash("nonsense").is_err());
    }

    /// `resolve_provider_name` reads the provider from `model.provider` (not the
    /// top-level `provider` key), matching how AgentConfig reads it. This is a
    /// regression test for a Phase-11 bug where the wrong key was read and the
    /// pool fetch always got an empty provider (FR-003).
    #[test]
    fn resolve_provider_name_reads_model_provider_key() {
        use tempfile::NamedTempFile;
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "model:\n  provider: zai\n  model: glm-5.2\n").unwrap();
        let cfg = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        assert_eq!(resolve_provider_name(&cfg), "zai");
    }

    /// `resolve_provider_name` falls back to the model's vendor prefix when
    /// provider is "auto" (e.g. "anthropic/claude-..." → "anthropic").
    #[test]
    fn resolve_provider_name_falls_back_to_model_vendor() {
        use tempfile::NamedTempFile;
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("COPILOT_API_BASE_URL");
        // A prior test's Config::load() may have loaded ~/.joey/.env with
        // OVERRIDE semantics into the process env — scrub the HUD var too,
        // or the magnet in resolve_provider_name fires and we get
        // "ai-usage-hud" instead of the vendor prefix.
        std::env::remove_var("AI_USAGE_HUD_BASE_URL");
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            "model:\n  provider: auto\n  default: anthropic/claude-sonnet-4\n",
        )
        .unwrap();
        let cfg = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        assert_eq!(resolve_provider_name(&cfg), "anthropic");
    }

    /// Custom Copilot-compatible endpoint active: the auto-derived provider
    /// is magnetized to "copilot" so the candidate-pool fetch targets the
    /// proxy's catalog regardless of the model's vendor prefix (mirrors the
    /// resolve_profile magnet in joey-providers).
    #[test]
    fn resolve_provider_name_magnetizes_to_copilot_on_custom_endpoint() {
        use tempfile::NamedTempFile;
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("COPILOT_API_BASE_URL", "http://127.0.0.1:8317");
        // HUD takes precedence over the copilot magnet — scrub it so this
        // test observes the copilot path even when ~/.joey/.env set it.
        std::env::remove_var("AI_USAGE_HUD_BASE_URL");
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            "model:\n  provider: auto\n  default: anthropic/claude-sonnet-4\n",
        )
        .unwrap();
        let cfg = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        assert_eq!(resolve_provider_name(&cfg), "copilot");
        std::env::remove_var("COPILOT_API_BASE_URL");
    }

    /// AI_USAGE_HUD_BASE_URL active: the auto-derived provider magnetizes to
    /// "ai-usage-hud" (its own copilot-wire profile).
    #[test]
    fn resolve_provider_name_magnetizes_to_hud_on_hud_env_var() {
        use tempfile::NamedTempFile;
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("COPILOT_API_BASE_URL");
        std::env::set_var("AI_USAGE_HUD_BASE_URL", "http://127.0.0.1:8317");
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            "model:\n  provider: auto\n  default: anthropic/claude-sonnet-4\n",
        )
        .unwrap();
        let cfg = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        assert_eq!(resolve_provider_name(&cfg), "ai-usage-hud");
        std::env::remove_var("AI_USAGE_HUD_BASE_URL");
    }

    /// The copilot-wire catalog fetch covers ai-usage-hud: with the HUD env
    /// var set, fetch_candidate_pool returns a pool from the proxy catalog
    /// (empty is acceptable when the proxy is down — FR-017 auto-disable).
    #[test]
    fn candidate_pool_fetch_targets_hud_via_copilot_wire() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("COPILOT_API_BASE_URL");
        std::env::set_var("AI_USAGE_HUD_BASE_URL", "http://127.0.0.1:1");
        // is_copilot_wire dispatch — unreachable endpoint yields an empty
        // pool, not a panic or a models.dev lookup.
        let pool = fetch_candidate_pool("ai-usage-hud");
        assert!(pool.is_empty());
        std::env::remove_var("AI_USAGE_HUD_BASE_URL");
    }
}
