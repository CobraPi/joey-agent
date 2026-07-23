//! Shared command helpers: version info (main.py:4578-4625), the `joey
//! model` picker (main.py:2925-2935 + model_setup_flows), the first-run
//! guard (main.py:2497-2527), and toolset resolution shared by chat/oneshot.

use std::io::{IsTerminal, Write};

use anyhow::Result;
use joey_core::{branding, Config};

use crate::render;

// ---------------------------------------------------------------------------
// version (main.py:4578-4625) — `joey version` and `joey -V` are identical
// ---------------------------------------------------------------------------

/// Guess the install method from the binary location (upstream
/// `detect_install_method`: pipx/pip/git — here: cargo vs source build).
fn detect_install_method(exe: &std::path::Path) -> &'static str {
    let s = exe.to_string_lossy();
    if s.contains("/.cargo/") || s.contains("\\.cargo\\") {
        "cargo"
    } else {
        "source"
    }
}

pub fn print_version_info() {
    // Version label (banner.py:506 `format_banner_version_label`, minus the
    // git-upstream decoration this port doesn't track).
    println!("{} v{}", branding::AGENT_NAME, branding::VERSION);
    let exe = std::env::current_exe().unwrap_or_default();
    let dir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    println!("Install directory: {}", dir.display());
    println!("Install method: {}", detect_install_method(&exe));
    // Toolchain line (upstream prints the Python version; the rustc version
    // is only known when baked in at build time).
    if let Some(rustc) = option_env!("JOEY_RUSTC_VERSION") {
        println!("Rust: {}", rustc);
    }
    println!("{}", branding::UPSTREAM_ATTRIBUTION);
    // Update check: deferred (upstream checks git/pip freshness here).
}

// ---------------------------------------------------------------------------
// TTY guard (main.py:443-459)
// ---------------------------------------------------------------------------

/// Exit with the upstream error if stdin is not a terminal. Returns the exit
/// code to propagate (1) instead of exiting so callers stay testable.
pub fn require_tty(command_name: &str) -> Option<i32> {
    if std::io::stdin().is_terminal() {
        return None;
    }
    eprintln!(
        "Error: 'joey {}' requires an interactive terminal.\n\
         It cannot be run through a pipe or non-interactive subprocess.\n\
         Run it directly in your terminal instead.",
        command_name
    );
    Some(1)
}

// ---------------------------------------------------------------------------
// Provider-configured check + first-run setup (main.py:2497-2527, 919-...)
// ---------------------------------------------------------------------------

/// True when at least one inference provider is usable (any provider API-key
/// env var resolves, or a custom OPENAI_BASE_URL is set, or the user pinned a
/// model in config).
pub fn has_any_provider_configured(config: &Config) -> bool {
    if joey_providers::copilot::resolve_copilot_token()
        .map(|(token, _)| !token.is_empty())
        .unwrap_or(false)
    {
        return true;
    }
    for name in joey_providers::profile::provider_names() {
        if let Some(profile) = joey_providers::profile::get_profile(name) {
            if profile.resolve_api_key().is_some() {
                return true;
            }
        }
    }
    if std::env::var("OPENAI_BASE_URL").map(|v| !v.trim().is_empty()).unwrap_or(false) {
        return true;
    }
    // Explicit model + base_url config counts (local servers need no key).
    let base_url = config.get_str("model.base_url", "");
    !config.model().is_empty() && !base_url.is_empty()
}

fn read_line_prompt(prompt: &str) -> Option<String> {
    print!("{}", prompt);
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    match std::io::stdin().read_line(&mut buf) {
        Ok(0) => None,
        Ok(_) => Some(buf.trim().to_string()),
        Err(_) => None,
    }
}

/// Non-interactive setup guidance (setup.py:182-205, branded).
fn print_noninteractive_setup_guidance(reason: &str) {
    println!();
    println!("⚕ Joey Setup — Non-interactive mode");
    println!();
    if !reason.is_empty() {
        println!("  {}", reason);
    }
    println!("  The interactive wizard cannot be used here.");
    println!();
    println!("  Configure Joey using environment variables or config commands:");
    println!("    joey config set model.provider custom");
    println!("    joey config set model.base_url http://localhost:8080/v1");
    println!("    joey config set model.default your-model-name");
    println!();
    println!("  Or set OPENROUTER_API_KEY / OPENAI_API_KEY in your environment.");
    println!("  Run 'joey model' in an interactive terminal to use the picker.");
    println!();
}

/// First-run guard before launching chat (main.py:2497-2527). Returns
/// `Some(exit_code)` when startup must stop, `None` to continue.
pub fn first_run_guard(config: &Config) -> Option<i32> {
    if has_any_provider_configured(config) {
        return None;
    }
    println!();
    println!("It looks like Joey isn't configured yet -- no API keys or providers found.");
    println!();
    println!("  Run:  joey model");
    println!();

    if !std::io::stdin().is_terminal() {
        print_noninteractive_setup_guidance("No interactive TTY detected for the first-run setup prompt.");
        return Some(1);
    }

    let reply = read_line_prompt("Run setup now? [Y/n] ").unwrap_or_else(|| "n".to_string());
    let reply = reply.to_lowercase();
    if reply.is_empty() || reply == "y" || reply == "yes" {
        match interactive_model_picker(false) {
            Ok(true) => return None,
            Ok(false) => {
                // A key may have been saved even without a model pick —
                // that's enough to start chatting.
                let now_configured = Config::load()
                    .map(|c| has_any_provider_configured(&c))
                    .unwrap_or(false);
                return if now_configured { None } else { Some(1) };
            }
            Err(e) => {
                render::error(&e.to_string());
                return Some(1);
            }
        }
    }
    println!();
    println!("You can run 'joey model' at any time to configure.");
    Some(1)
}

// ---------------------------------------------------------------------------
// `joey model` (main.py:2925-2935 — TTY-only interactive picker)
// ---------------------------------------------------------------------------

/// `joey model [--refresh]` — the full provider + model setup wizard
/// (`cmd_model`). `--refresh` clears the picker cache first.
pub fn model_command(refresh: bool) -> Result<i32> {
    if let Some(code) = require_tty("model") {
        return Ok(code);
    }
    match interactive_model_picker(refresh)? {
        true => Ok(0),
        false => Ok(1),
    }
}

/// The interactive setup wizard (port of `select_provider_and_model` — see
/// `setup_wizard.rs`). Returns whether a model was configured.
pub fn interactive_model_picker(refresh: bool) -> Result<bool> {
    crate::setup_wizard::select_provider_and_model(refresh)
}

// ---------------------------------------------------------------------------
// Toolset resolution shared by chat + oneshot (tools_config._get_platform_tools)
// ---------------------------------------------------------------------------

/// The toolset names enabled for a platform: the `platform_toolsets.<name>`
/// config list when present, else the platform default composite
/// (`joey-<platform>`), minus `agent.disabled_toolsets`.
pub fn platform_toolset_names(config: &Config, platform: &str) -> Vec<String> {
    let key = format!("platform_toolsets.{}", platform);
    let mut names: Vec<String> = match config.get(&key) {
        Some(serde_yaml::Value::Sequence(seq)) => seq
            .iter()
            .filter_map(|v| match v {
                serde_yaml::Value::String(s) => Some(s.clone()),
                serde_yaml::Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .collect(),
        _ => vec![format!("{}{}", branding::TOOLSET_PREFIX, platform)],
    };
    let disabled = config.get_str_list("agent.disabled_toolsets");
    if !disabled.is_empty() {
        names.retain(|n| !disabled.contains(n));
    }
    names
}

/// Resolve the flat tool-name list for a platform (sorted, deduped).
pub fn platform_tools(config: &Config, platform: &str) -> Vec<String> {
    joey_tools::resolve_toolsets(&platform_toolset_names(config, platform))
}

/// Split a comma-separated toolsets argument (oneshot.py `_normalize_toolsets`).
pub fn normalize_toolsets(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_toolsets_splits_and_trims() {
        assert_eq!(normalize_toolsets(" web , file ,,"), vec!["web", "file"]);
        assert!(normalize_toolsets("  ").is_empty());
    }

    #[test]
    fn platform_toolsets_default_to_composite() {
        let cfg = Config::defaults();
        assert_eq!(platform_toolset_names(&cfg, "cli"), vec!["joey-cli"]);
        assert!(!platform_tools(&cfg, "cli").is_empty());
    }
}
