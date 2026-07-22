//! `joey config` (port of `hermes_cli/subcommands/config.py` +
//! `hermes_cli/config.py:8286-9093`): show|edit|get|set|unset|path|env-path,
//! bare = show; env-shaped keys route to `.env`; credential values are masked
//! on echo; missing keys exit 1 with `Config key not set: <key>` on stderr.

use anyhow::Result;
use clap::{Args, Subcommand};
use joey_core::config::{is_env_config_key, remove_env_value, save_env_value};
use joey_core::redact::mask_secret_default;
use joey_core::Config;
use nu_ansi_term::Color;

use crate::render;

#[derive(Args, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: Option<ConfigAction>,
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Show current configuration
    Show,
    /// Open config file in editor
    Edit,
    /// Print a resolved configuration value
    Get {
        /// Configuration key (e.g., model)
        key: Option<String>,
        /// Print value as JSON
        #[arg(long)]
        json: bool,
    },
    /// Set a configuration value
    Set {
        /// Configuration key (e.g., model, terminal.backend)
        key: Option<String>,
        /// Value to set
        value: Option<String>,
        /// Skip the unknown-key notice
        #[arg(long)]
        force: bool,
    },
    /// Remove a configuration value
    Unset {
        /// Configuration key to remove
        key: Option<String>,
    },
    /// Print config file path
    Path,
    /// Print .env file path
    #[command(name = "env-path")]
    EnvPath,
    /// Check for missing/outdated config (not yet ported)
    Check,
    /// Update config with new options (not yet ported)
    Migrate,
}

pub fn config_command(args: &ConfigArgs) -> Result<i32> {
    match &args.action {
        None | Some(ConfigAction::Show) => {
            show_config()?;
            Ok(0)
        }
        Some(ConfigAction::Edit) => edit_config(),
        Some(ConfigAction::Get { key, json }) => {
            let Some(key) = key else {
                println!("Usage: joey config get <key> [--json]");
                println!();
                println!("Examples:");
                println!("  joey config get model");
                println!("  joey config get terminal.backend");
                return Ok(1);
            };
            let config = Config::load()?;
            match get_value(&config, key, *json) {
                Some(v) => {
                    println!("{}", v);
                    Ok(0)
                }
                None => {
                    eprintln!("Config key not set: {}", key);
                    Ok(1)
                }
            }
        }
        Some(ConfigAction::Set { key, value, force: _ }) => {
            let (Some(key), Some(value)) = (key, value) else {
                println!("Usage: joey config set [--force] <key> <value>");
                println!();
                println!("Examples:");
                println!("  joey config set model.default anthropic/claude-sonnet-4.6");
                println!("  joey config set terminal.backend docker");
                println!("  joey config set OPENROUTER_API_KEY sk-or-...");
                return Ok(1);
            };
            set_value(key, value)
        }
        Some(ConfigAction::Unset { key }) => {
            let Some(key) = key else {
                println!("Usage: joey config unset <key>");
                println!();
                println!("Examples:");
                println!("  joey config unset model.default");
                println!("  joey config unset OPENROUTER_API_KEY");
                return Ok(1);
            };
            unset_value(key)
        }
        Some(ConfigAction::Path) => {
            println!("{}", joey_core::constants::config_path().display());
            Ok(0)
        }
        Some(ConfigAction::EnvPath) => {
            println!("{}", joey_core::constants::env_path().display());
            Ok(0)
        }
        Some(ConfigAction::Check) => {
            println!("'joey config check' is not available in joey-agent yet.");
            Ok(1)
        }
        Some(ConfigAction::Migrate) => {
            println!("'joey config migrate' is not available in joey-agent yet.");
            Ok(1)
        }
    }
}

// ---------------------------------------------------------------------------
// get / set / unset (config.py:8721-8927)
// ---------------------------------------------------------------------------

/// Resolve a key for display: env-shaped keys read the environment (which
/// `.env` was loaded into); everything else reads the merged config tree.
fn get_value(config: &Config, key: &str, as_json: bool) -> Option<String> {
    if is_env_config_key(key) {
        let value = std::env::var(key.to_uppercase()).ok()?;
        return Some(if as_json { serde_json::to_string(&value).ok()? } else { value });
    }
    let value = config.get(key)?;
    if as_json {
        serde_json::to_string(&yaml_to_json(value)).ok()
    } else {
        Some(format_scalar(value))
    }
}

/// Plain-string view of a resolved value for the REPL `/config get`.
pub fn get_value_string(config: &Config, key: &str) -> Option<String> {
    get_value(config, key, false)
}

fn yaml_to_json(v: &serde_yaml::Value) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
}

fn format_scalar(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Null => "null".to_string(),
        other => serde_yaml::to_string(other).unwrap_or_default().trim_end().to_string(),
    }
}

/// Leaf key names whose values are masked when echoed (config.py
/// `_SECRET_CONFIG_KEYS`).
const SECRET_CONFIG_KEYS: &[&str] = &["api_key", "token", "secret", "password", "auth_token"];

fn set_value(key: &str, value: &str) -> Result<i32> {
    if is_env_config_key(key) {
        save_env_value(&key.to_uppercase(), value)?;
        println!("✓ Set {} in {}", key, joey_core::constants::env_path().display());
        return Ok(0);
    }
    let mut config = Config::load()?;
    config.set_and_save(key, value)?;
    let leaf = key.rsplit('.').next().unwrap_or(key).to_lowercase();
    let display_value = if SECRET_CONFIG_KEYS.contains(&leaf.as_str()) && !value.is_empty() {
        mask_secret_default(value)
    } else {
        value.to_string()
    };
    println!("✓ Set {} = {} in {}", key, display_value, config.path().display());
    Ok(0)
}

fn unset_value(key: &str) -> Result<i32> {
    if is_env_config_key(key) {
        if !remove_env_value(&key.to_uppercase())? {
            eprintln!("Config key not set: {}", key);
            return Ok(1);
        }
        println!("✓ Unset {} from {}", key, joey_core::constants::env_path().display());
        return Ok(0);
    }
    let mut config = Config::load()?;
    if !config.unset(key)? {
        eprintln!("Config key not set: {}", key);
        return Ok(1);
    }
    println!("✓ Unset {} from {}", key, config.path().display());
    Ok(0)
}

// ---------------------------------------------------------------------------
// edit (config.py:8493-8530)
// ---------------------------------------------------------------------------

fn edit_config() -> Result<i32> {
    let config_path = joey_core::constants::config_path();
    if !config_path.exists() {
        // Materialize an empty user config so the editor has a file.
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&config_path, "")?;
        println!("Created {}", config_path.display());
    }

    let editor = std::env::var("EDITOR")
        .ok()
        .filter(|e| !e.trim().is_empty())
        .or_else(|| std::env::var("VISUAL").ok().filter(|e| !e.trim().is_empty()))
        .or_else(|| {
            for cmd in ["nano", "vim", "vi", "code"] {
                if which::which(cmd).is_ok() {
                    return Some(cmd.to_string());
                }
            }
            None
        });
    let Some(editor) = editor else {
        println!("No editor found. Config file is at:");
        println!("  {}", config_path.display());
        return Ok(0);
    };
    println!("Opening {} in {}...", config_path.display(), editor);
    let status = std::process::Command::new(&editor).arg(&config_path).status();
    match status {
        Ok(_) => Ok(0),
        Err(e) => {
            render::error(&format!("failed to launch {}: {}", editor, e));
            Ok(1)
        }
    }
}

// ---------------------------------------------------------------------------
// show (config.py:8286-8492, reduced to ported sections; exact redaction)
// ---------------------------------------------------------------------------

fn kv(label: &str, value: &str) {
    println!("  {:<14}{}", label, value);
}

fn env_display(var: &str) -> String {
    match std::env::var(var) {
        Ok(v) if !v.trim().is_empty() => mask_secret_default(v.trim()),
        _ => Color::DarkGray.paint("(not set)").to_string(),
    }
}

pub fn show_config() -> Result<i32> {
    let config = Config::load()?;

    println!();
    render::boxed_header("⚕ Joey Configuration");

    render::section("Paths");
    kv("Config:", &joey_core::constants::config_path().display().to_string());
    kv("Secrets:", &joey_core::constants::env_path().display().to_string());
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.display().to_string()))
        .unwrap_or_default();
    kv("Install:", &exe_dir);

    render::section("API Keys");
    for name in joey_providers::profile::provider_names() {
        let Some(profile) = joey_providers::profile::get_profile(name) else { continue };
        let Some(var) = profile.env_vars.first() else { continue };
        kv(&format!("{}:", name), &env_display(var));
    }

    render::section("Model");
    let model = config.model();
    kv("Model:", if model.is_empty() { "not set" } else { &model });
    kv("Provider:", &config.get_str("model.provider", "auto"));
    let base_url = config.get_str("model.base_url", "");
    if !base_url.is_empty() {
        kv("Base URL:", &base_url);
    }
    kv("Max turns:", &config.get_i64("agent.max_turns", 90).to_string());

    render::section("Display");
    kv(
        "Reasoning:",
        if config.get_bool("display.show_reasoning", true) { "on" } else { "off" },
    );
    kv("Streaming:", &config.get_str("display.streaming", "false"));
    kv("Progress:", &config.get_str("display.tool_progress", "all"));

    render::section("Terminal");
    kv("Backend:", &config.get_str("terminal.backend", "local"));
    kv("Working dir:", &config.get_str("terminal.cwd", "."));
    kv("Timeout:", &format!("{}s", config.get_i64("terminal.timeout", 180)));

    render::section("Timezone");
    let tz = config.get_str("timezone", "");
    if tz.is_empty() {
        kv("Timezone:", &Color::DarkGray.paint("(server-local)").to_string());
    } else {
        kv("Timezone:", &tz);
    }

    println!();
    println!("{}", Color::DarkGray.paint("─".repeat(60)));
    println!("{}", Color::DarkGray.paint("  joey config edit     # Edit config file"));
    println!("{}", Color::DarkGray.paint("  joey config set <key> <value>"));
    println!("{}", Color::DarkGray.paint("  joey model           # Select default model"));
    println!();
    Ok(0)
}
