//! The provider + model setup wizard (port of `select_provider_and_model` in
//! `hermes_cli/main.py` and the flows in `hermes_cli/model_setup_flows.py`).
//!
//! Shared by `joey model` and the first-run guard. Handles the full flow:
//! provider picker, credential prompting (direct API-key entry with
//! Keep/Replace/Clear), Z.AI endpoint selection, model selection (models.dev
//! → curated → live `/models` probe), and config persistence.
//!
//! UI note: upstream renders these menus with a curses radiolist and falls
//! back to numbered lists on any terminal trouble; this port implements the
//! numbered-list surface (the fallback path) verbatim. Deliberate omissions,
//! all documented in PORTING.md: the curses UI, the Gemini free-tier probe
//! (native Gemini adapter is unported), the Anthropic OAuth subscription
//! login (standing ToS decision — API keys and honest OAuth-shaped env
//! tokens still work), secret-source suffixes (Bitwarden/1Password plugins
//! unported), and the "Configure auxiliary models..." submenu.

use std::io::Write;

use anyhow::Result;
use joey_core::{auth_store, branding, config::save_env_value, Config};
use joey_providers::profile::{get_profile, ProviderProfile};
use joey_providers::zai::ZAI_ENDPOINTS;
use serde_yaml::Value as Yaml;

use crate::model_catalog as catalog;
use crate::render;
use crate::secret_prompt::masked_secret_prompt;

/// Canonical picker order, restricted to the ported providers
/// (models.py `CANONICAL_PROVIDERS` order). Groups (xAI, Google, OpenAI…)
/// all degrade to single rows here — every group has exactly one ported
/// member — so `group_providers` folding is a no-op by its own rules.
const CANONICAL_ORDER: &[&str] = &[
    "nous",
    "openrouter",
    "anthropic",
    "openai-api",
    "gemini",
    "deepseek",
    "xai",
    "zai",
];

// ---------------------------------------------------------------------------
// Small console helpers
// ---------------------------------------------------------------------------

/// Read one line with a prompt. `None` on EOF/read error (upstream's
/// KeyboardInterrupt/EOFError arms).
fn read_line(prompt: &str) -> Option<String> {
    print!("{}", prompt);
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    match std::io::stdin().read_line(&mut buf) {
        Ok(0) => {
            println!();
            None
        }
        Ok(_) => Some(buf.trim().to_string()),
        Err(_) => {
            println!();
            None
        }
    }
}

fn env_value(var: &str) -> String {
    std::env::var(var).map(|v| v.trim().to_string()).unwrap_or_default()
}

/// First `n` characters (not bytes) of a secret, for masked display.
fn key_prefix(key: &str, n: usize) -> String {
    key.chars().take(n).collect()
}

/// Numbered provider-choice prompt (main.py `_prompt_provider_choice`,
/// numbered-list fallback branch). Returns the selected index or None on
/// cancel.
fn prompt_provider_choice(choices: &[String], default: usize, title: &str) -> Option<usize> {
    println!("{}", title);
    for (i, c) in choices.iter().enumerate() {
        let marker = if i == default { "→" } else { " " };
        println!("  {} {}. {}", marker, i + 1, c);
    }
    println!();
    loop {
        let val = read_line(&format!("Choice [1-{}] ({}): ", choices.len(), default + 1))?;
        if val.is_empty() {
            return Some(default);
        }
        match val.parse::<usize>() {
            Ok(n) if (1..=choices.len()).contains(&n) => return Some(n - 1),
            Ok(_) => println!("Please enter 1-{}", choices.len()),
            Err(_) => println!("Please enter a number"),
        }
    }
}

// ---------------------------------------------------------------------------
// Saved custom providers (config.yaml `custom_providers`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct CustomProviderInfo {
    key: String,
    name: String,
    base_url: String,
    api_key: String,
    model: String,
    api_mode: String,
}

fn yaml_str(map: &serde_yaml::Mapping, key: &str) -> String {
    map.get(&Yaml::String(key.to_string()))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Saved custom endpoints, keyed `custom:<name>` (the wizard-facing subset of
/// main.py `_named_custom_provider_map`; env-ref template preservation rides
/// on upstream's raw-config machinery and is not needed here — joey's config
/// loader expands `${VAR}` refs at read time and saved entries are only
/// appended, never rewritten, by this wizard).
fn custom_provider_map(config: &Config) -> Vec<CustomProviderInfo> {
    let mut out = Vec::new();
    let Some(Yaml::Sequence(entries)) = config.get("custom_providers") else {
        return out;
    };
    for entry in entries {
        let Yaml::Mapping(map) = entry else { continue };
        let name = yaml_str(map, "name");
        let base_url = {
            let b = yaml_str(map, "base_url");
            if !b.is_empty() {
                b
            } else {
                let u = yaml_str(map, "url");
                if !u.is_empty() { u } else { yaml_str(map, "api") }
            }
        };
        if name.is_empty() || base_url.is_empty() {
            continue;
        }
        let model = {
            let m = yaml_str(map, "model");
            if !m.is_empty() { m } else { yaml_str(map, "default_model") }
        };
        out.push(CustomProviderInfo {
            key: format!("custom:{}", name.to_lowercase().replace(' ', "-")),
            name,
            base_url,
            api_key: yaml_str(map, "api_key"),
            model,
            api_mode: yaml_str(map, "api_mode"),
        });
    }
    out
}

// ---------------------------------------------------------------------------
// Active-provider detection
// ---------------------------------------------------------------------------

/// Lean auto-resolution for the "← currently active" marker (auth.py
/// `resolve_provider("auto")` priority, restricted to the ported surface):
/// OpenRouter env keys → openrouter; provider-specific keys → that provider;
/// auth.json `active_provider` last.
fn auto_active_provider() -> Option<String> {
    if !env_value("OPENAI_API_KEY").is_empty() || !env_value("OPENROUTER_API_KEY").is_empty() {
        return Some("openrouter".to_string());
    }
    for name in ["zai", "deepseek", "gemini", "anthropic", "nous", "xai"] {
        if let Some(p) = get_profile(name) {
            if p.resolve_api_key().is_some() {
                return Some(name.to_string());
            }
        }
    }
    auth_store::load_auth_store()
        .get("active_provider")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// The wizard entry point
// ---------------------------------------------------------------------------

/// Core provider selection + model picking logic (main.py
/// `select_provider_and_model`). Returns whether a model was configured.
pub fn select_provider_and_model(refresh: bool) -> Result<bool> {
    if refresh {
        catalog::clear_provider_models_cache();
        println!("  Cleared model picker cache.");
    }

    let config = Config::load()?;
    let current_model = {
        let m = config.model();
        if m.is_empty() { "(not set)".to_string() } else { m }
    };

    // Effective provider the same way the CLI does at startup:
    // config.yaml model.provider > env var > auto-detect.
    let config_provider = config.get_str("model.provider", "");
    let effective_provider = if !config_provider.is_empty() {
        config_provider.clone()
    } else {
        let env = env_value(&format!("{}INFERENCE_PROVIDER", branding::ENV_PREFIX));
        if env.is_empty() { "auto".to_string() } else { env }
    };

    let customs = custom_provider_map(&config);

    // Active provider: a custom base_url match wins, then the explicit
    // setting, then auto-detection from credentials.
    let norm = |u: &str| u.trim().trim_end_matches('/').to_lowercase();
    let mut active = String::new();
    if effective_provider == "custom" {
        let current_base = norm(&config.get_str("model.base_url", ""));
        if !current_base.is_empty() {
            if let Some(info) = customs.iter().find(|c| norm(&c.base_url) == current_base) {
                active = info.key.clone();
            }
        }
        if active.is_empty() {
            active = "custom".to_string();
        }
    } else if effective_provider != "auto" {
        match get_profile(&effective_provider) {
            Some(p) => active = p.name.to_string(),
            None => {
                println!(
                    "Warning: Unknown provider '{}'. Check 'joey model' for available \
                     providers, or run 'joey doctor' to diagnose config issues. \
                     Falling back to auto provider detection.",
                    effective_provider
                );
            }
        }
    }
    if active.is_empty() {
        active = auto_active_provider().unwrap_or_default();
    }
    // Detect custom endpoint (main.py:3164-3165).
    if active == "openrouter" && !env_value("OPENAI_BASE_URL").is_empty() {
        active = "custom".to_string();
    }

    let active_label = if let Some(info) = customs.iter().find(|c| c.key == active) {
        info.name.clone()
    } else if active.is_empty() {
        "none".to_string()
    } else {
        get_profile(&active)
            .map(|p| p.display_name.to_string())
            .unwrap_or_else(|| {
                if active == "custom" { "Custom endpoint".to_string() } else { active.clone() }
            })
    };

    println!();
    println!("  Current model:    {}", current_model);
    println!("  Active provider:  {}", active_label);
    println!();

    // Step 1: provider selection. Canonical providers first (honoring
    // model_catalog.excluded_providers like the upstream pickers), then saved
    // custom providers, then the trailing action rows.
    let excluded: Vec<String> = config
        .get_str_list("model_catalog.excluded_providers")
        .iter()
        .map(|p| p.trim().to_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    let is_excluded = |p: &ProviderProfile| {
        excluded.iter().any(|e| {
            e == p.name || p.aliases.iter().any(|a| a == e)
        })
    };

    let mut ordered: Vec<(String, String)> = Vec::new(); // (key, label)
    let mut default_idx = 0usize;
    for slug in CANONICAL_ORDER {
        let Some(profile) = get_profile(slug) else { continue };
        if is_excluded(&profile) {
            continue;
        }
        let label = profile.tui_desc.to_string();
        if !active.is_empty() && *slug == active {
            ordered.push((slug.to_string(), format!("{}  ← currently active", label)));
            default_idx = ordered.len() - 1;
        } else {
            ordered.push((slug.to_string(), label));
        }
    }
    for info in &customs {
        let short_url = info
            .base_url
            .replace("https://", "")
            .replace("http://", "")
            .trim_end_matches('/')
            .to_string();
        let model_hint = if info.model.is_empty() {
            String::new()
        } else {
            format!(" — {}", info.model)
        };
        let label = format!("{} ({}){}", info.name, short_url, model_hint);
        if !active.is_empty() && info.key == active {
            ordered.push((info.key.clone(), format!("{}  ← currently active", label)));
            default_idx = ordered.len() - 1;
        } else {
            ordered.push((info.key.clone(), label));
        }
    }
    ordered.push(("custom".into(), "Custom endpoint (enter URL manually)".into()));
    if !customs.is_empty() {
        ordered.push(("remove-custom".into(), "Remove a saved custom provider".into()));
    }
    ordered.push(("cancel".into(), "Leave unchanged".into()));

    let labels: Vec<String> = ordered.iter().map(|(_, l)| l.clone()).collect();
    let Some(idx) = prompt_provider_choice(&labels, default_idx, "Select provider:") else {
        println!("No change.");
        return Ok(false);
    };
    let selected = ordered[idx].0.clone();
    if selected == "cancel" {
        println!("No change.");
        return Ok(false);
    }

    // Step 2: provider-specific setup + model selection.
    let configured = match selected.as_str() {
        "openrouter" => flow_openrouter(&current_model)?,
        "anthropic" => flow_anthropic(&current_model)?,
        "custom" => flow_custom()?,
        "remove-custom" => {
            remove_custom_provider()?;
            false
        }
        key if key.starts_with("custom:") => {
            match custom_provider_map(&Config::load()?).into_iter().find(|c| c.key == key) {
                Some(info) => flow_named_custom(&info, &current_model)?,
                None => {
                    println!(
                        "Warning: the selected saved custom provider is no longer available. \
                         It may have been removed from config.yaml. No change."
                    );
                    false
                }
            }
        }
        // Every remaining registered provider is an API-key profile
        // (main.py `_is_profile_api_key_provider` catch-all).
        provider_id => flow_api_key_provider(provider_id, &current_model)?,
    };

    // Post-switch cleanup: a leftover OPENAI_BASE_URL in ~/.joey/.env can
    // poison auxiliary clients using provider:auto after switching to a
    // named provider (main.py:3379-3388, issue #5161).
    if selected != "custom" && selected != "remove-custom" && !selected.starts_with("custom:") {
        clear_stale_openai_base_url();
    }
    Ok(configured)
}

/// Remove OPENAI_BASE_URL from `~/.joey/.env` if the active provider is not
/// 'custom' (main.py `_clear_stale_openai_base_url`).
fn clear_stale_openai_base_url() {
    let Ok(cfg) = Config::load() else { return };
    let provider = cfg.get_str("model.provider", "").trim().to_lowercase();
    if provider == "custom" || provider.is_empty() {
        return; // custom provider legitimately uses OPENAI_BASE_URL
    }
    if !env_value("OPENAI_BASE_URL").is_empty() {
        let _ = save_env_value("OPENAI_BASE_URL", "");
        std::env::remove_var("OPENAI_BASE_URL");
    }
}

// ---------------------------------------------------------------------------
// API-key prompting (main.py `_prompt_api_key`)
// ---------------------------------------------------------------------------

struct ApiKeyOutcome {
    key: String,
    abort: bool,
}

/// Shared API-key entry point. Handles both first-time entry and the
/// already-configured case: with a key present, offers [K]eep / [R]eplace /
/// [C]lear so the user can recover from a malformed paste without editing
/// `~/.joey/.env` by hand.
fn prompt_api_key(display_name: &str, env_vars: &[&str], existing_key: &str) -> ApiKeyOutcome {
    let key_env = env_vars.first().copied().unwrap_or("");

    let prompt_new_key = || -> String {
        masked_secret_prompt(&format!("{} (or Enter to cancel): ", key_env))
            .unwrap_or_empty()
            .trim()
            .to_string()
    };

    // First-time entry ────────────────────────────────────────────────────
    if existing_key.is_empty() {
        println!("No {} API key configured.", display_name);
        if key_env.is_empty() {
            return ApiKeyOutcome { key: String::new(), abort: true };
        }
        let new_key = prompt_new_key();
        if new_key.is_empty() {
            println!("Cancelled.");
            return ApiKeyOutcome { key: String::new(), abort: true };
        }
        if let Err(e) = save_env_value(key_env, &new_key) {
            render::error(&e.to_string());
            return ApiKeyOutcome { key: String::new(), abort: true };
        }
        std::env::set_var(key_env, &new_key);
        println!("API key saved.");
        println!();
        return ApiKeyOutcome { key: new_key, abort: false };
    }

    // Already configured — offer K / R / C ────────────────────────────────
    // (No secret-source suffix: the Bitwarden/1Password source plugins are
    // unported, so keys only ever come from .env or the shell here.)
    println!("  {} API key: {}... ✓", display_name, key_prefix(existing_key, 8));
    if key_env.is_empty() {
        println!();
        return ApiKeyOutcome { key: existing_key.to_string(), abort: false };
    }
    let choice = read_line("  [K]eep / [R]eplace / [C]lear (default K): ")
        .unwrap_or_else(|| "k".to_string())
        .to_lowercase();

    if choice.starts_with('r') {
        let new_key = prompt_new_key();
        if new_key.is_empty() {
            println!("  No change.");
            println!();
            return ApiKeyOutcome { key: existing_key.to_string(), abort: false };
        }
        if let Err(e) = save_env_value(key_env, &new_key) {
            render::error(&e.to_string());
            return ApiKeyOutcome { key: existing_key.to_string(), abort: false };
        }
        std::env::set_var(key_env, &new_key);
        println!("  API key updated.");
        println!();
        return ApiKeyOutcome { key: new_key, abort: false };
    }

    if choice.starts_with('c') {
        let _ = save_env_value(key_env, "");
        std::env::remove_var(key_env);
        println!(
            "  API key cleared.  Re-run `joey model` to configure {} again.",
            display_name
        );
        return ApiKeyOutcome { key: String::new(), abort: true };
    }

    // Keep (default, or any other input)
    println!();
    ApiKeyOutcome { key: existing_key.to_string(), abort: false }
}

// ---------------------------------------------------------------------------
// Z.AI endpoint picker (model_setup_flows.py `_select_zai_endpoint`)
// ---------------------------------------------------------------------------

/// Present a picker for Z.AI endpoint selection during setup: the four
/// official endpoints (Global, China, Coding Plan Global, Coding Plan China)
/// plus a custom-proxy option, sourced from the shared `ZAI_ENDPOINTS` list
/// so it stays in sync with the probe list. Falls back to `current_base` on
/// cancel or error.
fn select_zai_endpoint(current_base: &str) -> String {
    let options: Vec<(&str, &str)> =
        ZAI_ENDPOINTS.iter().map(|ep| (ep.label, ep.base_url)).collect();
    let normalized_current = current_base.trim().trim_end_matches('/').to_string();

    // Default to the currently-active option when it matches a known
    // endpoint; a non-matching custom URL defaults to "Custom proxy URL".
    let mut default_idx = 0usize;
    let mut matched = false;
    for (idx, (_, url)) in options.iter().enumerate() {
        if normalized_current == url.trim_end_matches('/') {
            default_idx = idx;
            matched = true;
            break;
        }
    }
    if !matched && !normalized_current.is_empty() {
        default_idx = options.len();
    }

    let mut choices: Vec<String> =
        options.iter().map(|(label, url)| format!("{} ({})", label, url)).collect();
    choices.push("Custom proxy URL".to_string());

    let Some(selected) = prompt_provider_choice(&choices, default_idx, "Select Z.AI / GLM endpoint:")
    else {
        return current_base.to_string();
    };

    if selected == options.len() {
        // Custom proxy URL
        let Some(override_url) = read_line(&format!("Custom base URL [{}]: ", current_base)) else {
            return current_base.to_string();
        };
        if override_url.is_empty() {
            return current_base.to_string();
        }
        if !override_url.starts_with("http://") && !override_url.starts_with("https://") {
            println!("  Invalid URL — must start with http:// or https://. Keeping current value.");
            return current_base.to_string();
        }
        return override_url.trim_end_matches('/').to_string();
    }

    options[selected].1.trim_end_matches('/').to_string()
}

// ---------------------------------------------------------------------------
// Generic API-key provider flow
// (model_setup_flows.py `_model_flow_api_key_provider`)
// ---------------------------------------------------------------------------

fn flow_api_key_provider(provider_id: &str, current_model: &str) -> Result<bool> {
    let Some(profile) = get_profile(provider_id) else {
        render::error(&format!("Unknown provider: {}", provider_id));
        return Ok(false);
    };
    let key_env = profile.env_vars.first().copied().unwrap_or("");
    let base_url_env = profile.base_url_env_var.unwrap_or("");

    // Check / prompt for API key.
    let mut existing_key = String::new();
    for ev in profile.env_vars {
        existing_key = env_value(ev);
        if !existing_key.is_empty() {
            break;
        }
    }
    let outcome = prompt_api_key(profile.display_name, profile.env_vars, &existing_key);
    if outcome.abort {
        return Ok(false);
    }
    let existing_key = outcome.key;

    // Gemini free-tier gate: upstream probes the key's tier via the native
    // Gemini adapter and refuses free-tier keys; that adapter is unported,
    // so the runtime 429 handler is the backstop here (PORTING.md).

    // Optional base URL override. Precedence: env var → config.yaml
    // model.base_url (when this provider is active) → profile default.
    let mut current_base = if base_url_env.is_empty() { String::new() } else { env_value(base_url_env) };
    if current_base.is_empty() {
        if let Ok(cfg) = Config::load() {
            if cfg.get_str("model.provider", "").trim().to_lowercase() == provider_id {
                current_base = cfg.get_str("model.base_url", "").trim().to_string();
            }
        }
    }
    let mut effective_base = if current_base.is_empty() {
        profile.base_url.to_string()
    } else {
        current_base
    };

    if provider_id == "zai" {
        // Z.AI has four official endpoints (Global, China, Coding Plan
        // Global, Coding Plan China) with separate billing paths. Present a
        // picker instead of a plain text input so users can explicitly
        // choose the endpoint that matches their key type.
        let chosen_base = select_zai_endpoint(&effective_base);
        if !chosen_base.is_empty() && chosen_base != effective_base && !base_url_env.is_empty() {
            let _ = save_env_value(base_url_env, &chosen_base);
            std::env::set_var(base_url_env, &chosen_base);
        }
        effective_base = chosen_base;
    } else {
        let override_url = read_line(&format!("Base URL [{}]: ", effective_base)).unwrap_or_default();
        if !override_url.is_empty() && !base_url_env.is_empty() {
            if !override_url.starts_with("http://") && !override_url.starts_with("https://") {
                println!("  Invalid URL — must start with http:// or https://. Keeping current value.");
            } else {
                let _ = save_env_value(base_url_env, &override_url);
                std::env::set_var(base_url_env, &override_url);
                effective_base = override_url;
            }
        }
    }

    // Model selection — resolution order:
    //   1. models.dev registry (cached, filtered for agentic models)
    //   2. Curated static fallback list (offline insurance)
    //   3. Live /models endpoint probe (small providers without models.dev data)
    let curated = catalog::provider_models(provider_id);
    let mdev_models = catalog::list_agentic_models(provider_id);

    let model_list: Vec<String> = if !mdev_models.is_empty() {
        // Merge models.dev with the curated list so newly added models
        // (not yet in models.dev) still appear in the picker.
        let mut merged = mdev_models;
        let mut seen: std::collections::HashSet<String> =
            merged.iter().map(|m| m.to_lowercase()).collect();
        for m in &curated {
            if seen.insert(m.to_lowercase()) {
                merged.push(m.clone());
            }
        }
        println!("  Found {} model(s) from models.dev registry", merged.len());
        merged
    } else if curated.len() >= 8 {
        // Curated list is substantial — use it directly, skip live probe.
        println!(
            "  Showing {} curated models — use \"Enter custom model name\" for others.",
            curated.len()
        );
        curated
    } else {
        let api_key_for_probe = if existing_key.is_empty() {
            if key_env.is_empty() { String::new() } else { env_value(key_env) }
        } else {
            existing_key.clone()
        };
        let live = catalog::fetch_api_models(&api_key_for_probe, &effective_base);
        match live {
            Some(live_models) if live_models.len() >= curated.len() && !live_models.is_empty() => {
                println!(
                    "  Found {} model(s) from {} API",
                    live_models.len(),
                    profile.display_name
                );
                live_models
            }
            _ => {
                if !curated.is_empty() {
                    println!(
                        "  Showing {} curated models — use \"Enter custom model name\" for others.",
                        curated.len()
                    );
                }
                curated
            }
        }
    };

    let selected = if model_list.is_empty() {
        read_line("Model name: ").filter(|s| !s.is_empty())
    } else {
        // Per-model pricing, when the provider supports it (returns an empty
        // map for providers without pricing — never a blocking fetch).
        let pricing = catalog::get_pricing_for_provider(provider_id);
        prompt_model_selection(&model_list, current_model, &pricing, provider_id)
    };

    if let Some(selected) = selected {
        save_model_choice(&selected)?;
        // Update config with provider + base URL; clear stale inline
        // endpoint credentials (config.py `clear_model_endpoint_credentials`,
        // clear_api_mode included — the ported set has no opencode).
        let mut cfg = Config::load()?;
        cfg.set_and_save("model.provider", provider_id)?;
        cfg.set_and_save("model.base_url", &effective_base)?;
        let _ = cfg.unset("model.api_key");
        let _ = cfg.unset("model.api");
        let _ = cfg.unset("model.api_mode");
        auth_store::deactivate_provider();
        println!("Default model set to: {} (via {})", selected, profile.display_name);
        Ok(true)
    } else {
        println!("No change.");
        Ok(false)
    }
}

// ---------------------------------------------------------------------------
// OpenRouter flow (model_setup_flows.py `_model_flow_openrouter`)
// ---------------------------------------------------------------------------

fn flow_openrouter(current_model: &str) -> Result<bool> {
    let existing_key = env_value("OPENROUTER_API_KEY");
    if existing_key.is_empty() {
        println!("Get one at: https://openrouter.ai/keys");
        println!();
    }
    let outcome = prompt_api_key("OpenRouter", &["OPENROUTER_API_KEY"], &existing_key);
    if outcome.abort {
        return Ok(false);
    }

    let openrouter_models = catalog::openrouter_model_ids(true);
    // Live pricing (non-blocking — empty map on failure).
    let pricing = catalog::get_pricing_for_provider("openrouter");

    let selected =
        prompt_model_selection(&openrouter_models, current_model, &pricing, "openrouter");
    if let Some(selected) = selected {
        save_model_choice(&selected)?;
        let mut cfg = Config::load()?;
        cfg.set_and_save("model.provider", "openrouter")?;
        cfg.set_and_save("model.base_url", joey_core::constants::OPENROUTER_BASE_URL)?;
        cfg.set_and_save("model.api_mode", "chat_completions")?;
        let _ = cfg.unset("model.api_key");
        let _ = cfg.unset("model.api");
        auth_store::deactivate_provider();
        println!("Default model set to: {} (via OpenRouter)", selected);
        Ok(true)
    } else {
        println!("No change.");
        Ok(false)
    }
}

// ---------------------------------------------------------------------------
// Anthropic flow (model_setup_flows.py `_model_flow_anthropic`)
// ---------------------------------------------------------------------------

/// Reuse / reauthenticate / cancel prompt (model_setup_flows.py
/// `_prompt_auth_credentials_choice`, numbered fallback).
fn prompt_auth_credentials_choice(title: &str) -> &'static str {
    let choices = [
        "Use existing credentials",
        "Reauthenticate (new OAuth login)",
        "Cancel",
    ];
    println!("{}", title);
    for (i, label) in choices.iter().enumerate() {
        let marker = if i == 0 { "→" } else { " " };
        println!("  {} {}. {}", marker, i + 1, label);
    }
    println!();
    match read_line("  Choice [1/2/3]: ").unwrap_or_else(|| "1".into()).as_str() {
        "2" => "reauth",
        "3" => "cancel",
        _ => "use",
    }
}

fn flow_anthropic(current_model: &str) -> Result<bool> {
    let profile = get_profile("anthropic").expect("anthropic profile registered");

    // Check the env credential sources (ANTHROPIC_API_KEY / ANTHROPIC_TOKEN /
    // CLAUDE_CODE_OAUTH_TOKEN). Upstream additionally auto-discovers Claude
    // Code's on-disk credentials; that rides on the impersonation path this
    // port deliberately omits (see PORTING.md "Deliberate deviations").
    let existing_key = profile.resolve_api_key().unwrap_or_default();
    let mut needs_auth = existing_key.is_empty();

    if !needs_auth {
        println!("  Anthropic credentials: {}... ✓", key_prefix(&existing_key, 12));
        println!();
        match prompt_auth_credentials_choice("Anthropic credentials:") {
            "reauth" => needs_auth = true,
            "cancel" => return Ok(false),
            _ => {}
        }
    }

    if needs_auth {
        println!();
        println!("  Choose authentication method:");
        println!();
        println!("    1. Claude Pro/Max subscription (OAuth login)");
        println!("    2. Anthropic API key (pay-per-token)");
        println!("    3. Cancel");
        println!();
        let Some(choice) = read_line("  Choice [1/2/3]: ") else {
            return Ok(false);
        };
        match choice.as_str() {
            "1" => {
                // Standing decision: upstream's subscription-OAuth flow works
                // by impersonating Claude Code and is deliberately not
                // ported (PORTING.md). Honest Bearer tokens still work.
                println!();
                println!("  Claude subscription OAuth login is not supported by Joey:");
                println!("  upstream's flow impersonates another client to bill consumer");
                println!("  subscriptions, which violates Anthropic's terms. Use an API");
                println!("  key instead (option 2), or export a valid OAuth token via");
                println!("  CLAUDE_CODE_OAUTH_TOKEN if you have one.");
                return Ok(false);
            }
            "2" => {
                println!();
                println!("  Get an API key at: {}", profile.signup_url);
                println!();
                let api_key =
                    masked_secret_prompt("  API key (sk-ant-...): ").unwrap_or_empty().trim().to_string();
                if api_key.is_empty() {
                    println!("  Cancelled.");
                    return Ok(false);
                }
                // save_anthropic_api_key: persist the key and clear the
                // OAuth/setup-token slot (config.py).
                save_env_value("ANTHROPIC_API_KEY", &api_key)?;
                save_env_value("ANTHROPIC_TOKEN", "")?;
                std::env::set_var("ANTHROPIC_API_KEY", &api_key);
                std::env::remove_var("ANTHROPIC_TOKEN");
                println!("  ✓ API key saved.");
            }
            _ => {
                println!("  No change.");
                return Ok(false);
            }
        }
    }
    println!();

    // Model selection.
    let model_list = catalog::provider_models("anthropic");
    let selected = if model_list.is_empty() {
        read_line("Model name (e.g., claude-sonnet-4-20250514): ").filter(|s| !s.is_empty())
    } else {
        prompt_model_selection(&model_list, current_model, &Default::default(), "anthropic")
    };

    if let Some(selected) = selected {
        save_model_choice(&selected)?;
        // Clear base_url: runtime resolution always hardcodes Anthropic's
        // URL, and a stale base_url can contaminate other providers.
        let mut cfg = Config::load()?;
        cfg.set_and_save("model.provider", "anthropic")?;
        let _ = cfg.unset("model.base_url");
        let _ = cfg.unset("model.api_key");
        let _ = cfg.unset("model.api");
        let _ = cfg.unset("model.api_mode");
        auth_store::deactivate_provider();
        println!("Default model set to: {} (via Anthropic)", selected);
        Ok(true)
    } else {
        println!("No change.");
        Ok(false)
    }
}

// ---------------------------------------------------------------------------
// Custom endpoint flow (model_setup_flows.py `_model_flow_custom`)
// ---------------------------------------------------------------------------

/// Auto-detect api_mode from a base URL (runtime_provider.py
/// `_detect_api_mode_for_url`, ported hosts only).
fn detect_api_mode_for_url(base_url: &str) -> Option<&'static str> {
    let normalized = base_url.trim().to_lowercase();
    let normalized = normalized.trim_end_matches('/');
    let hostname = joey_core::utils::base_url_hostname(base_url);
    if hostname == "api.x.ai" || hostname == "api.openai.com" {
        return Some("codex_responses");
    }
    if hostname == "api.anthropic.com" {
        return Some("anthropic_messages");
    }
    // Anthropic-compatible gateways conventionally expose the native
    // protocol under an /anthropic suffix.
    let path = normalized.splitn(2, "://").nth(1).and_then(|rest| rest.find('/').map(|i| &rest[i..]))
        .unwrap_or("");
    let path = path.trim_end_matches('/');
    if path.ends_with("/anthropic") || path.ends_with("/anthropic/v1") {
        return Some("anthropic_messages");
    }
    if hostname == "api.kimi.com" && normalized.contains("/coding") {
        return Some("anthropic_messages");
    }
    None
}

/// Prompt for a custom provider API mode (main.py
/// `_prompt_custom_api_mode_selection`). Returns the explicit mode, or None
/// to keep auto-detect behavior.
fn prompt_custom_api_mode_selection(base_url: &str, current_api_mode: &str) -> Option<String> {
    let detected = detect_api_mode_for_url(base_url).unwrap_or("");
    let normalized_current = current_api_mode.trim().to_lowercase();
    let default_mode = if !normalized_current.is_empty() {
        normalized_current.clone()
    } else {
        detected.to_string()
    };

    let mode_options: [(&str, &str, &str); 4] = [
        ("", "Auto-detect", "Use Joey URL heuristics; best for standard OpenAI-compatible endpoints."),
        ("chat_completions", "Chat Completions", "Use /chat/completions for standard OpenAI-compatible servers."),
        ("codex_responses", "Responses / Codex", "Use /responses for Codex-compatible tool-calling backends."),
        ("anthropic_messages", "Anthropic Messages", "Use /v1/messages for Anthropic-compatible endpoints."),
    ];

    println!();
    println!("Select API compatibility mode:");
    for (idx, (value, label, description)) in mode_options.iter().enumerate() {
        let mut markers: Vec<&str> = Vec::new();
        if *value == detected {
            markers.push("detected");
        }
        if *value == default_mode {
            markers.push("current");
        }
        let suffix = if markers.is_empty() {
            String::new()
        } else {
            format!(" [{}]", markers.join(" / "))
        };
        println!("  {}. {}{}", idx + 1, label, suffix);
        println!("     {}", description);
    }

    let raw = read_line("Choice [1-4, Enter to keep current/detected]: ")
        .unwrap_or_default()
        .to_lowercase();
    if raw.is_empty() {
        return (!default_mode.is_empty()).then_some(default_mode);
    }
    match raw.as_str() {
        "1" | "auto" | "detect" | "auto-detect" => None,
        "2" | "chat" | "chat_completions" | "completions" => Some("chat_completions".into()),
        "3" | "responses" | "codex" | "codex_responses" => Some("codex_responses".into()),
        "4" | "anthropic" | "anthropic_messages" | "messages" => Some("anthropic_messages".into()),
        other => {
            println!("Invalid API mode choice: {}. Falling back to auto-detect.", other);
            None
        }
    }
}

/// Generate a display name from a custom endpoint URL (main.py
/// `_auto_provider_name`): "Local (localhost:11434)", "RunPod (x.runpod.io)".
fn auto_provider_name(base_url: &str) -> String {
    let clean = base_url.replace("https://", "").replace("http://", "");
    let clean = clean.trim_end_matches('/');
    let clean = regex::Regex::new(r"/v1/?$").unwrap().replace(clean, "").to_string();
    let name = clean.split('/').next().unwrap_or("").to_string();
    if name.contains("localhost") || name.contains("127.0.0.1") {
        format!("Local ({})", name)
    } else if name.to_lowercase().contains("runpod") {
        format!("RunPod ({})", name)
    } else {
        let mut chars = name.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => name,
        }
    }
}

/// Save a custom endpoint to `custom_providers` in config.yaml, deduplicated
/// by base_url (main.py `_save_custom_provider`).
fn save_custom_provider(
    base_url: &str,
    api_key: &str,
    model: &str,
    context_length: Option<i64>,
    name: &str,
    api_mode: &str,
) -> Result<()> {
    let mut cfg = Config::load()?;
    let mut providers: Vec<Yaml> = match cfg.get("custom_providers") {
        Some(Yaml::Sequence(seq)) => seq.clone(),
        _ => Vec::new(),
    };

    // Existing URL → update model/context_length/api_mode in place.
    for entry in providers.iter_mut() {
        let Yaml::Mapping(map) = entry else { continue };
        if yaml_str(map, "base_url").trim_end_matches('/') != base_url.trim_end_matches('/') {
            continue;
        }
        if !model.is_empty() {
            map.insert(Yaml::String("model".into()), Yaml::String(model.into()));
            if let Some(ctx) = context_length {
                let mut models_cfg = match map.get(&Yaml::String("models".into())) {
                    Some(Yaml::Mapping(m)) => m.clone(),
                    _ => serde_yaml::Mapping::new(),
                };
                let mut entry_map = serde_yaml::Mapping::new();
                entry_map.insert(Yaml::String("context_length".into()), Yaml::Number(ctx.into()));
                models_cfg.insert(Yaml::String(model.into()), Yaml::Mapping(entry_map));
                map.insert(Yaml::String("models".into()), Yaml::Mapping(models_cfg));
            }
        }
        if !api_mode.is_empty() {
            map.insert(Yaml::String("api_mode".into()), Yaml::String(api_mode.into()));
        } else {
            map.remove(&Yaml::String("api_mode".into()));
        }
        cfg.set_value_and_save("custom_providers", Yaml::Sequence(providers))?;
        return Ok(());
    }

    let display_name = if name.is_empty() { auto_provider_name(base_url) } else { name.to_string() };
    let mut map = serde_yaml::Mapping::new();
    map.insert(Yaml::String("name".into()), Yaml::String(display_name.clone().into()));
    map.insert(Yaml::String("base_url".into()), Yaml::String(base_url.into()));
    if !api_key.is_empty() {
        map.insert(Yaml::String("api_key".into()), Yaml::String(api_key.into()));
    }
    if !model.is_empty() {
        map.insert(Yaml::String("model".into()), Yaml::String(model.into()));
    }
    if !api_mode.is_empty() {
        map.insert(Yaml::String("api_mode".into()), Yaml::String(api_mode.into()));
    }
    if let (false, Some(ctx)) = (model.is_empty(), context_length) {
        let mut models_cfg = serde_yaml::Mapping::new();
        let mut entry_map = serde_yaml::Mapping::new();
        entry_map.insert(Yaml::String("context_length".into()), Yaml::Number(ctx.into()));
        models_cfg.insert(Yaml::String(model.into()), Yaml::Mapping(entry_map));
        map.insert(Yaml::String("models".into()), Yaml::Mapping(models_cfg));
    }
    providers.push(Yaml::Mapping(map));
    cfg.set_value_and_save("custom_providers", Yaml::Sequence(providers))?;
    println!("  💾 Saved to custom providers as \"{}\" (edit in config.yaml)", display_name);
    Ok(())
}

fn flow_custom() -> Result<bool> {
    let config = Config::load()?;
    let current_url = env_value("OPENAI_BASE_URL");
    let current_key = env_value("OPENAI_API_KEY");

    println!("Custom OpenAI-compatible endpoint configuration:");
    if !current_url.is_empty() {
        println!("  Current URL: {}", current_url);
    }
    if !current_key.is_empty() {
        println!("  Current key: {}...", key_prefix(&current_key, 8));
    }
    println!();

    let url_prompt = if current_url.is_empty() {
        "API base URL [e.g. https://api.example.com/v1]: ".to_string()
    } else {
        format!("API base URL [{}]: ", current_url)
    };
    let Some(base_url_input) = read_line(&url_prompt) else {
        println!("Cancelled.");
        return Ok(false);
    };
    let key_prompt = if current_key.is_empty() {
        "API key [optional]: ".to_string()
    } else {
        format!("API key [{}...]: ", key_prefix(&current_key, 8))
    };
    let api_key_input = match masked_secret_prompt(&key_prompt) {
        crate::secret_prompt::SecretInput::Value(v) => v.trim().to_string(),
        crate::secret_prompt::SecretInput::Cancelled => {
            println!("Cancelled.");
            return Ok(false);
        }
    };

    if base_url_input.is_empty() && current_url.is_empty() {
        println!("No URL provided. Cancelled.");
        return Ok(false);
    }
    let mut effective_url =
        if base_url_input.is_empty() { current_url.clone() } else { base_url_input.clone() };
    if !effective_url.starts_with("http://") && !effective_url.starts_with("https://") {
        println!("Invalid URL: {} (must start with http:// or https://)", effective_url);
        return Ok(false);
    }
    let effective_key = if api_key_input.is_empty() { current_key.clone() } else { api_key_input };

    // Local-server hint: most local model servers (Ollama, vLLM, llama.cpp)
    // require /v1 for OpenAI-compatible chat completions.
    let url_lower = effective_url.trim_end_matches('/').to_lowercase();
    let looks_local = ["localhost", "127.0.0.1", "0.0.0.0", ":11434", ":8080", ":5000"]
        .iter()
        .any(|h| url_lower.contains(h));
    if looks_local && !url_lower.ends_with("/v1") {
        println!();
        println!("  Hint: Did you mean to add /v1 at the end?");
        println!("  Most local model servers (Ollama, vLLM, llama.cpp) require it.");
        println!("  e.g. {}/v1", effective_url.trim_end_matches('/'));
        let add_v1 = read_line("  Add /v1? [Y/n]: ").unwrap_or_else(|| "n".into()).to_lowercase();
        if add_v1.is_empty() || add_v1 == "y" || add_v1 == "yes" {
            effective_url = format!("{}/v1", effective_url.trim_end_matches('/'));
            println!("  Updated URL: {}", effective_url);
        }
        println!();
    }

    let probe = catalog::probe_api_models(&effective_key, &effective_url, "");
    if probe.used_fallback && !probe.resolved_base_url.is_empty() {
        println!(
            "Warning: endpoint verification worked at {}/models, not the exact URL you entered. \
             Saving the working base URL instead.",
            probe.resolved_base_url
        );
        effective_url = probe.resolved_base_url.clone();
    } else if let Some(models) = &probe.models {
        println!(
            "Verified endpoint via {} ({} model(s) visible)",
            probe.probed_url.as_deref().unwrap_or(""),
            models.len()
        );
    } else {
        println!(
            "Warning: could not verify this endpoint via {}. Joey will still save it.",
            probe.probed_url.as_deref().unwrap_or("")
        );
        if let Some(suggested) = &probe.suggested_base_url {
            if suggested.ends_with("/v1") {
                println!("  If this server expects /v1 in the path, try base URL: {}", suggested);
            } else {
                println!("  If /v1 should not be in the base URL, try: {}", suggested);
            }
        }
    }

    // Explicit API compatibility mode so codex-compatible custom providers
    // don't silently fall back to chat_completions.
    let current_api_mode = config.get_str("model.api_mode", "");
    let api_mode = prompt_custom_api_mode_selection(&effective_url, &current_api_mode);
    match &api_mode {
        Some(mode) => println!("  API mode: {}", mode),
        None => println!("  API mode: auto-detect"),
    }

    // Model selection — probe results when available, manual input otherwise.
    let detected_models = probe.models.clone().unwrap_or_default();
    let model_name: String = if detected_models.len() == 1 {
        println!("  Detected model: {}", detected_models[0]);
        let confirm = read_line("  Use this model? [Y/n]: ").unwrap_or_else(|| "n".into()).to_lowercase();
        if confirm.is_empty() || confirm == "y" || confirm == "yes" {
            detected_models[0].clone()
        } else {
            read_line("Model name (e.g. gpt-4, llama-3-70b): ").unwrap_or_default()
        }
    } else if detected_models.len() > 1 {
        println!("  Available models:");
        for (i, m) in detected_models.iter().enumerate() {
            println!("    {}. {}", i + 1, m);
        }
        let pick = read_line(&format!(
            "  Select model [1-{}] or type name: ",
            detected_models.len()
        ))
        .unwrap_or_default();
        match pick.parse::<usize>() {
            Ok(n) if (1..=detected_models.len()).contains(&n) => detected_models[n - 1].clone(),
            _ => pick,
        }
    } else {
        read_line("Model name (e.g. gpt-4, llama-3-70b): ").unwrap_or_default()
    };

    let context_length_str =
        read_line("Context length in tokens [leave blank for auto-detect]: ").unwrap_or_default();
    let context_length: Option<i64> = if context_length_str.is_empty() {
        None
    } else {
        let cleaned = context_length_str.replace(',', "").replace(['k', 'K'], "000");
        match cleaned.parse::<i64>() {
            Ok(n) if n > 0 => Some(n),
            _ => {
                println!("Invalid context length: {} — will auto-detect.", context_length_str);
                None
            }
        }
    };

    let default_name = auto_provider_name(&effective_url);
    let display_name = read_line(&format!("Display name [{}]: ", default_name))
        .unwrap_or_default();
    let display_name = if display_name.is_empty() { default_name } else { display_name };

    let api_mode_str = api_mode.clone().unwrap_or_default();
    let configured = if !model_name.is_empty() {
        save_model_choice(&model_name)?;
        let mut cfg = Config::load()?;
        cfg.set_and_save("model.provider", "custom")?;
        cfg.set_and_save("model.base_url", &effective_url)?;
        if !effective_key.is_empty() {
            cfg.set_and_save("model.api_key", &effective_key)?;
        }
        if api_mode_str.is_empty() {
            let _ = cfg.unset("model.api_mode");
        } else {
            cfg.set_and_save("model.api_mode", &api_mode_str)?;
        }
        auth_store::deactivate_provider();
        println!("Default model set to: {} (via {})", model_name, effective_url);
        true
    } else {
        if !base_url_input.is_empty() || !effective_key.is_empty() {
            auth_store::deactivate_provider();
        }
        // Even without a model name, persist the endpoint so it isn't lost.
        let mut cfg = Config::load()?;
        cfg.set_and_save("model.provider", "custom")?;
        cfg.set_and_save("model.base_url", &effective_url)?;
        if !effective_key.is_empty() {
            cfg.set_and_save("model.api_key", &effective_key)?;
        }
        if api_mode_str.is_empty() {
            let _ = cfg.unset("model.api_mode");
        } else {
            cfg.set_and_save("model.api_mode", &api_mode_str)?;
        }
        println!("Endpoint saved. Use `/model` in chat or `joey model` to set a model.");
        false
    };

    // Auto-save to custom_providers so it appears in the menu next time.
    save_custom_provider(
        &effective_url,
        &effective_key,
        &model_name,
        context_length,
        &display_name,
        &api_mode_str,
    )?;
    Ok(configured)
}

/// Re-apply a saved custom provider (the wizard-facing core of
/// model_setup_flows.py `_model_flow_named_custom`): reuse its endpoint +
/// key, pick a model (live probe first, saved model as the current default),
/// persist as `provider: custom`.
fn flow_named_custom(info: &CustomProviderInfo, current_model: &str) -> Result<bool> {
    println!("{} ({})", info.name, info.base_url);
    println!();

    let model_list = catalog::fetch_api_models(&info.api_key, &info.base_url).unwrap_or_default();
    let current = if info.model.is_empty() { current_model } else { &info.model };
    let selected = if model_list.is_empty() {
        let prompt = if info.model.is_empty() {
            "Model name: ".to_string()
        } else {
            format!("Model name [{}]: ", info.model)
        };
        match read_line(&prompt) {
            None => None,
            Some(s) if s.is_empty() && !info.model.is_empty() => Some(info.model.clone()),
            Some(s) if s.is_empty() => None,
            Some(s) => Some(s),
        }
    } else {
        println!("  Found {} model(s) from {}", model_list.len(), info.name);
        prompt_model_selection(&model_list, current, &Default::default(), "custom")
    };

    if let Some(selected) = selected {
        save_model_choice(&selected)?;
        let mut cfg = Config::load()?;
        cfg.set_and_save("model.provider", "custom")?;
        cfg.set_and_save("model.base_url", &info.base_url)?;
        if info.api_key.is_empty() {
            let _ = cfg.unset("model.api_key");
        } else {
            cfg.set_and_save("model.api_key", &info.api_key)?;
        }
        if info.api_mode.is_empty() {
            let _ = cfg.unset("model.api_mode");
        } else {
            cfg.set_and_save("model.api_mode", &info.api_mode)?;
        }
        auth_store::deactivate_provider();
        // Remember the chosen model on the saved entry.
        save_custom_provider(&info.base_url, &info.api_key, &selected, None, &info.name, &info.api_mode)?;
        println!("Default model set to: {} (via {})", selected, info.name);
        Ok(true)
    } else {
        println!("No change.");
        Ok(false)
    }
}

/// Remove a saved custom provider (main.py `_remove_custom_provider`,
/// numbered fallback).
fn remove_custom_provider() -> Result<()> {
    let mut cfg = Config::load()?;
    let providers: Vec<Yaml> = match cfg.get("custom_providers") {
        Some(Yaml::Sequence(seq)) if !seq.is_empty() => seq.clone(),
        _ => {
            println!("No custom providers configured.");
            return Ok(());
        }
    };

    println!("Remove a custom provider:\n");
    let mut choices: Vec<String> = providers
        .iter()
        .map(|entry| match entry {
            Yaml::Mapping(map) => {
                let name = {
                    let n = yaml_str(map, "name");
                    if n.is_empty() { "unnamed".to_string() } else { n }
                };
                let url = yaml_str(map, "base_url");
                let short = url.replace("https://", "").replace("http://", "");
                format!("{} ({})", name, short.trim_end_matches('/'))
            }
            other => format!("{:?}", other),
        })
        .collect();
    choices.push("Cancel".to_string());

    for (i, c) in choices.iter().enumerate() {
        println!("  {}. {}", i + 1, c);
    }
    println!();
    let idx = read_line(&format!("Choice [1-{}]: ", choices.len()))
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<usize>().ok())
        .map(|n| n - 1);

    let Some(idx) = idx else {
        println!("No change.");
        return Ok(());
    };
    if idx >= providers.len() {
        println!("No change.");
        return Ok(());
    }

    let mut remaining = providers;
    let removed = remaining.remove(idx);
    cfg.set_value_and_save("custom_providers", Yaml::Sequence(remaining))?;
    let removed_name = match &removed {
        Yaml::Mapping(map) => {
            let n = yaml_str(map, "name");
            if n.is_empty() { "unnamed".to_string() } else { n }
        }
        other => format!("{:?}", other),
    };
    println!("✅ Removed \"{}\" from custom providers.", removed_name);
    Ok(())
}

// ---------------------------------------------------------------------------
// Model selection (auth.py `_prompt_model_selection`, numbered fallback)
// ---------------------------------------------------------------------------

/// Prompt before saving a model whose known pricing exceeds guardrails
/// (auth.py `_confirm_expensive_model_selection`).
fn confirm_expensive_model_selection(model_id: &str, provider: &str) -> bool {
    let Some(warning) = catalog::expensive_model_warning(model_id, provider) else {
        return true;
    };
    println!();
    println!("{}", "=".repeat(72));
    println!("{}", warning);
    println!("{}", "=".repeat(72));
    let response = read_line("Switch anyway? [y/N]: ").unwrap_or_default().to_lowercase();
    response == "y" || response == "yes"
}

/// Interactive model selection. Puts the current model first with a marker;
/// aligned $/Mtok pricing columns when available; "Enter custom model name"
/// and "Skip (keep current)" trailing rows. Returns the chosen model or None.
fn prompt_model_selection(
    model_ids: &[String],
    current_model: &str,
    pricing: &std::collections::HashMap<String, catalog::ModelPricing>,
    confirm_provider: &str,
) -> Option<String> {
    let confirmed = |mid: String| -> Option<String> {
        if mid.is_empty() {
            return None;
        }
        if !confirm_provider.is_empty() && !confirm_expensive_model_selection(&mid, confirm_provider) {
            return None;
        }
        Some(mid)
    };

    // Reorder: current model first, then the rest (deduplicated).
    let mut ordered: Vec<String> = Vec::new();
    if !current_model.is_empty() && model_ids.iter().any(|m| m == current_model) {
        ordered.push(current_model.to_string());
    }
    for mid in model_ids {
        if !ordered.contains(mid) {
            ordered.push(mid.clone());
        }
    }

    // Column-aligned labels when pricing is available.
    let has_pricing = ordered.iter().any(|m| pricing.contains_key(m));
    let name_col = if has_pricing {
        ordered.iter().map(|m| m.len()).max().unwrap_or(0) + 2
    } else {
        0
    };
    let mut price_col = 3usize;
    let mut cache_col = 0usize;
    let mut has_cache = false;
    let mut price_cache: std::collections::HashMap<&str, (String, String, String)> =
        Default::default();
    if has_pricing {
        for mid in &ordered {
            let (inp, out, cache) = match pricing.get(mid) {
                Some(p) => {
                    let cache = p
                        .input_cache_read
                        .as_deref()
                        .map(catalog::format_price_per_mtok)
                        .unwrap_or_default();
                    if !cache.is_empty() {
                        has_cache = true;
                    }
                    (
                        catalog::format_price_per_mtok(&p.prompt),
                        catalog::format_price_per_mtok(&p.completion),
                        cache,
                    )
                }
                None => (String::new(), String::new(), String::new()),
            };
            price_col = price_col.max(inp.len()).max(out.len());
            cache_col = cache_col.max(cache.len());
            price_cache.insert(mid.as_str(), (inp, out, cache));
        }
        if has_cache {
            cache_col = cache_col.max(5); // minimum: "Cache" header
        }
    }

    let label = |mid: &str| -> String {
        let mut base = if has_pricing {
            let (inp, out, cache) = price_cache.get(mid).cloned().unwrap_or_default();
            let mut price_part =
                format!(" {:>pw$}  {:>pw$}", inp, out, pw = price_col);
            if has_cache {
                price_part.push_str(&format!("  {:>cw$}", cache, cw = cache_col));
            }
            format!("{:<nw$}{}", mid, price_part, nw = name_col)
        } else {
            mid.to_string()
        };
        if mid == current_model {
            base.push_str("  ← currently in use");
        }
        base
    };

    // Menu title with an aligned pricing header when applicable.
    let mut menu_title = "Select default model:".to_string();
    if has_pricing {
        let mut header = format!(
            "\n{}{:>nw$} {:>pw$}  {:>pw$}",
            " ".repeat(5),
            "",
            "In",
            "Out",
            nw = name_col,
            pw = price_col
        );
        if has_cache {
            header.push_str(&format!("  {:>cw$}", "Cache", cw = cache_col));
        }
        menu_title.push_str(&header);
        menu_title.push_str("  /Mtok");
    }

    // Numbered list (upstream's fallback branch — the curses radiolist is a
    // deliberate omission; see the module docs).
    println!("{}", menu_title);
    let n = ordered.len();
    let num_width = (n + 2).to_string().len();
    for (i, mid) in ordered.iter().enumerate() {
        println!("  {:>w$}. {}", i + 1, label(mid), w = num_width);
    }
    println!("  {:>w$}. Enter custom model name", n + 1, w = num_width);
    println!("  {:>w$}. Skip (keep current)", n + 2, w = num_width);
    println!();

    loop {
        let Some(choice) = read_line(&format!("Choice [1-{}] (default: skip): ", n + 2)) else {
            return None;
        };
        if choice.is_empty() {
            return None;
        }
        match choice.parse::<usize>() {
            Ok(idx) if (1..=n).contains(&idx) => return confirmed(ordered[idx - 1].clone()),
            Ok(idx) if idx == n + 1 => {
                let custom = read_line("Enter model name: ")?;
                return if custom.is_empty() { None } else { confirmed(custom) };
            }
            Ok(idx) if idx == n + 2 => return None,
            Ok(_) => println!("Please enter 1-{}", n + 2),
            Err(_) => println!("Please enter a number"),
        }
    }
}

/// Save the selected model to config.yaml — the single source of truth; NOT
/// .env (auth.py `_save_model_choice`).
fn save_model_choice(model_id: &str) -> Result<()> {
    let mut cfg = Config::load()?;
    cfg.set_and_save("model.default", model_id)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use joey_providers::profile::provider_names;

    #[test]
    fn zai_endpoint_picker_options_track_probe_list() {
        // The picker is sourced from ZAI_ENDPOINTS so it stays in sync with
        // the probe list (model_setup_flows.py:2540-2555).
        let labels: Vec<&str> = ZAI_ENDPOINTS.iter().map(|e| e.label).collect();
        assert_eq!(
            labels,
            vec!["Global", "China", "Global (Coding Plan)", "China (Coding Plan)"]
        );
    }

    #[test]
    fn canonical_order_covers_all_registered_providers() {
        // Every registered provider must be reachable from the picker.
        for name in provider_names() {
            assert!(
                CANONICAL_ORDER.contains(&name),
                "provider {} missing from the wizard's canonical order",
                name
            );
        }
    }

    #[test]
    fn api_mode_detection_matches_upstream_hosts() {
        assert_eq!(detect_api_mode_for_url("https://api.openai.com/v1"), Some("codex_responses"));
        assert_eq!(detect_api_mode_for_url("https://api.x.ai/v1"), Some("codex_responses"));
        assert_eq!(
            detect_api_mode_for_url("https://api.anthropic.com"),
            Some("anthropic_messages")
        );
        assert_eq!(
            detect_api_mode_for_url("https://api.minimax.io/anthropic"),
            Some("anthropic_messages")
        );
        assert_eq!(
            detect_api_mode_for_url("https://gateway.test/anthropic/v1"),
            Some("anthropic_messages")
        );
        assert_eq!(
            detect_api_mode_for_url("https://api.kimi.com/coding/v1"),
            Some("anthropic_messages")
        );
        assert_eq!(detect_api_mode_for_url("https://openrouter.ai/api/v1"), None);
        // Lookalike subdomains must NOT match (exact-hostname rule, #32243).
        assert_eq!(detect_api_mode_for_url("https://api.anthropic.com.attacker.test/v1"), None);
    }

    #[test]
    fn auto_provider_name_matches_upstream() {
        assert_eq!(auto_provider_name("http://localhost:11434/v1"), "Local (localhost:11434)");
        assert_eq!(auto_provider_name("https://xyz.runpod.io/v1"), "RunPod (xyz.runpod.io)");
        assert_eq!(auto_provider_name("https://api.example.com/v1"), "Api.example.com");
    }
}
