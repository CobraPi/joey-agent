//! `joey tools` (port of `hermes_cli/subcommands/tools.py` +
//! `tools_config.py`): `--summary`, `list [--platform P]`,
//! `enable|disable <names…> [--platform P]` reading/writing the upstream
//! `platform_toolsets.<platform>` config key. Bare `joey tools` in a TTY
//! prints the list + hint (the upstream curses UI is not ported); non-TTY
//! keeps the upstream TTY-required error.

use anyhow::Result;
use clap::{Args, Subcommand};
use joey_core::Config;
use nu_ansi_term::Color;

use crate::commands::platform_toolset_names;

/// Platforms this port knows about (upstream `PLATFORMS`, reduced).
const PLATFORMS: &[&str] = &["cli", "cron"];

#[derive(Args, Debug)]
pub struct ToolsArgs {
    /// Print a summary of enabled tools per platform and exit
    #[arg(long)]
    pub summary: bool,
    #[command(subcommand)]
    pub action: Option<ToolsAction>,
}

#[derive(Subcommand, Debug)]
pub enum ToolsAction {
    /// Show all tools and their enabled/disabled status
    List {
        /// Platform to show (default: cli)
        #[arg(long, default_value = "cli")]
        platform: String,
    },
    /// Disable toolsets
    Disable {
        /// Toolset names (e.g. web)
        #[arg(required = true, value_name = "NAME")]
        names: Vec<String>,
        /// Platform to apply to (default: cli)
        #[arg(long, default_value = "cli")]
        platform: String,
    },
    /// Enable toolsets
    Enable {
        /// Toolset names (e.g. web)
        #[arg(required = true, value_name = "NAME")]
        names: Vec<String>,
        /// Platform to apply to (default: cli)
        #[arg(long, default_value = "cli")]
        platform: String,
    },
    #[command(external_subcommand)]
    Other(Vec<String>),
}

pub fn tools_command(args: &ToolsArgs) -> Result<i32> {
    if args.summary {
        return summary();
    }
    match &args.action {
        Some(ToolsAction::List { platform }) => list(platform),
        Some(ToolsAction::Enable { names, platform }) => apply_change(names, platform, true),
        Some(ToolsAction::Disable { names, platform }) => apply_change(names, platform, false),
        Some(ToolsAction::Other(rest)) => {
            let sub = rest.first().map(String::as_str).unwrap_or("");
            if sub == "post-setup" {
                println!("'joey tools post-setup' is not available in joey-agent yet.");
                Ok(1)
            } else {
                eprintln!("Unknown tools command: {}", sub);
                eprintln!("Usage: joey tools [--summary] [list|enable|disable]");
                Ok(2)
            }
        }
        None => {
            // Upstream launches the interactive curses UI here (TTY-gated).
            if let Some(code) = crate::commands::require_tty("tools") {
                return Ok(code);
            }
            println!(
                "{}",
                Color::Cyan.bold().paint("⚕ Joey Tool Configuration")
            );
            println!("{}", Color::DarkGray.paint("  The interactive configuration UI is not ported yet."));
            println!("{}", Color::DarkGray.paint("  Use: joey tools list | joey tools enable <name> | joey tools disable <name>"));
            println!();
            list("cli")
        }
    }
}

// ---------------------------------------------------------------------------
// Effective per-platform toolsets
// ---------------------------------------------------------------------------

/// Leaf (configurable) toolset names: every registered toolset except the
/// platform composites (`joey-*`) and the scenario composites — upstream's
/// `CONFIGURABLE_TOOLSETS` offers only leaf sets in the checklist.
fn configurable_toolsets() -> Vec<&'static str> {
    joey_tools::toolsets::names()
        .into_iter()
        .filter(|n| {
            !n.starts_with(joey_core::branding::TOOLSET_PREFIX)
                && !matches!(*n, "coding" | "debugging" | "safe" | "all")
        })
        .collect()
}

/// Expand the saved list into effective leaf toolsets: names listed directly
/// plus leaves whose static membership is covered by a listed composite
/// (tools_config subset inference).
fn effective_leaf_toolsets(config: &Config, platform: &str) -> Vec<String> {
    let saved = platform_toolset_names(config, platform);
    let leaves = configurable_toolsets();
    let mut enabled: Vec<String> = Vec::new();

    // Tools contributed by composite entries (joey-cli etc.).
    let mut composite_tools: std::collections::BTreeSet<String> = Default::default();
    for name in &saved {
        if leaves.contains(&name.as_str()) {
            if !enabled.contains(name) {
                enabled.push(name.clone());
            }
        } else {
            for t in joey_tools::resolve_toolset(name) {
                composite_tools.insert(t);
            }
        }
    }
    if !composite_tools.is_empty() {
        for leaf in &leaves {
            let ts_tools = joey_tools::resolve_toolset(leaf);
            if !ts_tools.is_empty()
                && ts_tools.iter().all(|t| composite_tools.contains(t))
                && !enabled.iter().any(|e| e == leaf)
            {
                enabled.push(leaf.to_string());
            }
        }
    }
    enabled.sort();
    enabled
}

fn check_platform(platform: &str) -> Option<i32> {
    if PLATFORMS.contains(&platform) {
        return None;
    }
    eprintln!(
        "{}",
        Color::Red.paint(format!("Unknown platform '{}'. Valid: {}", platform, PLATFORMS.join(", ")))
    );
    Some(1)
}

// ---------------------------------------------------------------------------
// summary (tools_config.py:4227-4244)
// ---------------------------------------------------------------------------

fn summary() -> Result<i32> {
    let config = Config::load()?;
    let total = configurable_toolsets().len();
    println!();
    println!("{}", Color::Cyan.bold().paint("⚕ Tool Summary"));
    println!();
    for platform in PLATFORMS {
        let enabled = effective_leaf_toolsets(&config, platform);
        println!(
            "{}{}",
            Color::White.bold().paint(format!("  {}", platform)),
            Color::DarkGray.paint(format!("  ({}/{})", enabled.len(), total))
        );
        if enabled.is_empty() {
            println!("{}", Color::DarkGray.paint("    (none enabled)"));
        } else {
            for ts in &enabled {
                println!("{}", Color::Green.paint(format!("    ✓ {}", ts)));
            }
        }
    }
    println!();
    Ok(0)
}

// ---------------------------------------------------------------------------
// list (tools_config._print_tools_list approximation)
// ---------------------------------------------------------------------------

fn list(platform: &str) -> Result<i32> {
    if let Some(code) = check_platform(platform) {
        return Ok(code);
    }
    let config = Config::load()?;
    let enabled = effective_leaf_toolsets(&config, platform);
    println!();
    println!("{}", Color::Cyan.bold().paint(format!("Toolsets for platform '{}'", platform)));
    println!();
    for ts in configurable_toolsets() {
        let desc = joey_tools::toolsets::description(ts).unwrap_or("");
        let on = enabled.iter().any(|e| e == ts);
        let mark = if on {
            Color::Green.paint("✓ enabled ").to_string()
        } else {
            Color::DarkGray.paint("✗ disabled").to_string()
        };
        println!("  {} {:<14} {}", mark, ts, Color::DarkGray.paint(desc));
    }
    println!();
    println!(
        "{}",
        Color::DarkGray.paint(format!(
            "  Enable/disable with: joey tools enable <name> --platform {}",
            platform
        ))
    );
    println!();
    Ok(0)
}

// ---------------------------------------------------------------------------
// enable / disable (tools_config.tools_disable_enable_command)
// ---------------------------------------------------------------------------

fn apply_change(names: &[String], platform: &str, enable: bool) -> Result<i32> {
    if let Some(code) = check_platform(platform) {
        return Ok(code);
    }
    let mut config = Config::load()?;
    let valid = configurable_toolsets();

    let mut targets: Vec<String> = Vec::new();
    for name in names {
        if name.contains(':') {
            eprintln!(
                "{}",
                Color::Red.paint(format!(
                    "MCP tool toggling ('{}') is not available in joey-agent yet",
                    name
                ))
            );
            continue;
        }
        if !valid.contains(&name.as_str()) {
            eprintln!("{}", Color::Red.paint(format!("Unknown toolset '{}'", name)));
            continue;
        }
        targets.push(name.clone());
    }
    if targets.is_empty() {
        return Ok(1);
    }

    let mut enabled = effective_leaf_toolsets(&config, platform);
    for t in &targets {
        if enable {
            if !enabled.contains(t) {
                enabled.push(t.clone());
            }
        } else {
            enabled.retain(|e| e != t);
        }
    }
    enabled.sort();

    // Persist the explicit leaf list (upstream `_save_platform_tools` writes
    // sorted enabled keys into platform_toolsets.<platform>).
    let joined = enabled.join(",");
    save_platform_list(&mut config, platform, &enabled)
        .map_err(|e| anyhow::anyhow!("failed to save platform toolsets ({}): {}", joined, e))?;

    let verb = if enable { "Enabled" } else { "Disabled" };
    println!("{}", Color::Green.paint(format!("✓ {}: {}", verb, targets.join(", "))));
    println!(
        "{}",
        Color::DarkGray.paint("  Start a new session for changes to take effect.")
    );
    Ok(0)
}

/// Write `platform_toolsets.<platform>: [...]` into the user config document.
fn save_platform_list(config: &mut Config, platform: &str, enabled: &[String]) -> Result<()> {
    // Config::set_and_save only takes scalars; write the sequence through the
    // raw user file (same shape upstream `save_config` produces).
    let path = config.path().clone();
    let mut doc: serde_yaml::Mapping = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_yaml::from_str::<serde_yaml::Value>(&s).ok())
        .and_then(|v| v.as_mapping().cloned())
        .unwrap_or_default();
    let key = serde_yaml::Value::String("platform_toolsets".to_string());
    let mut pt = doc
        .get(&key)
        .and_then(|v| v.as_mapping())
        .cloned()
        .unwrap_or_default();
    pt.insert(
        serde_yaml::Value::String(platform.to_string()),
        serde_yaml::Value::Sequence(
            enabled.iter().map(|s| serde_yaml::Value::String(s.clone())).collect(),
        ),
    );
    doc.insert(key, serde_yaml::Value::Mapping(pt));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_yaml::to_string(&serde_yaml::Value::Mapping(doc))?)?;
    Ok(())
}
