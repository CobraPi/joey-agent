//! Non-interactive subcommand handlers (port of the `hermes_cli` command
//! surface: model, tools, config, doctor, cron, version, mcp, skills).

use std::path::PathBuf;

use anyhow::Result;
use joey_agent_core::{Agent, AgentConfig};
use joey_core::{branding, Config};
use joey_tools::{ToolContext, ToolRegistry};

use crate::render;

/// `joey version`
pub fn version() {
    println!("{} v{}", branding::AGENT_NAME, branding::VERSION);
    println!("{}", branding::UPSTREAM_ATTRIBUTION);
}

/// `joey config get|set|show|path`
pub fn config_cmd(action: &str, key: Option<&str>, value: Option<&str>) -> Result<()> {
    let mut cfg = Config::load()?;
    match action {
        "path" => println!("{}", cfg.path().display()),
        "get" => {
            let Some(key) = key else {
                render::error("config get requires a key");
                return Ok(());
            };
            match cfg.get(key) {
                Some(v) => println!("{}", serde_yaml::to_string(v)?.trim()),
                None => render::info("(unset)"),
            }
        }
        "set" => {
            let (Some(key), Some(value)) = (key, value) else {
                render::error("config set requires a key and value");
                return Ok(());
            };
            cfg.set_and_save(key, value)?;
            render::success(&format!("set {} = {}", key, value));
        }
        "show" => {
            println!("{}", serde_yaml::to_string(cfg.root())?);
        }
        other => render::error(&format!("unknown config action: {}", other)),
    }
    Ok(())
}

/// `joey model` — show current model + provider resolution.
pub fn model_cmd() -> Result<()> {
    let cfg = Config::load()?;
    let model = cfg.model();
    let provider_setting = cfg.get_str("model.provider", "auto");
    let base_url = cfg.get_str("model.base_url", "https://openrouter.ai/api/v1");
    let profile = joey_providers::resolve_profile(&provider_setting, &base_url, &model);
    println!("Model:        {}", model);
    println!("Provider:     {} (resolved: {})", provider_setting, profile.name);
    println!("Wire mode:    {}", profile.api_mode.as_str());
    println!("Base URL:     {}", if base_url.is_empty() { profile.base_url } else { &base_url });
    let key = profile.resolve_api_key();
    println!(
        "Credentials:  {}",
        if key.is_some() { "present" } else { "MISSING — set via `joey config set`" }
    );
    println!();
    println!("Available providers: {}", joey_providers::profile::provider_names().join(", "));
    Ok(())
}

/// `joey tools` — list toolsets and their tools.
pub fn tools_cmd() -> Result<()> {
    println!("Toolsets:");
    for ts in joey_tools::toolsets::names() {
        let desc = joey_tools::toolsets::description(ts).unwrap_or("");
        let tools = joey_tools::resolve_toolset(ts);
        println!("  {:<12} {} ({} tools)", ts, desc, tools.len());
    }
    println!("\nBuilt-in tools:");
    let registry = ToolRegistry::with_builtins();
    for name in registry.names() {
        println!("  {}", name);
    }
    Ok(())
}

/// `joey doctor` — environment diagnostics.
pub fn doctor() -> Result<()> {
    println!("{} doctor\n", branding::AGENT_NAME);
    let mut ok = 0;
    let mut warn = 0;

    let check = |label: &str, pass: bool, detail: &str, ok: &mut i32, warn: &mut i32| {
        if pass {
            *ok += 1;
            println!("  {} {} {}", render::check_mark(), label, detail);
        } else {
            *warn += 1;
            println!("  {} {} {}", render::warn_mark(), label, detail);
        }
    };

    // Home directory
    let home = joey_core::joey_home();
    check(
        "home directory",
        home.exists(),
        &format!("({})", home.display()),
        &mut ok,
        &mut warn,
    );

    // Config
    let cfg = Config::load()?;
    check("config loaded", true, &format!("model={}", cfg.model()), &mut ok, &mut warn);

    // Provider credentials
    let provider_setting = cfg.get_str("model.provider", "auto");
    let base_url = cfg.get_str("model.base_url", "");
    let profile = joey_providers::resolve_profile(&provider_setting, &base_url, &cfg.model());
    check(
        "provider credentials",
        profile.resolve_api_key().is_some(),
        &format!("(provider: {})", profile.name),
        &mut ok,
        &mut warn,
    );

    // External tools
    for bin in ["git", "rg", "bash"] {
        let found = which::which(bin).is_ok();
        check(&format!("`{}` on PATH", bin), found, "", &mut ok, &mut warn);
    }

    // State DB
    let db_ok = joey_core::SessionDb::open_default().is_ok();
    check("session database", db_ok, "", &mut ok, &mut warn);

    println!("\n{} checks passed, {} warnings.", ok, warn);
    Ok(())
}

/// `joey -q "prompt"` — headless single-shot query. Prints only the final text.
pub async fn oneshot(config: Config, cwd: PathBuf, query: &str) -> Result<()> {
    let agent_cfg = AgentConfig::from_config(&config);
    let ctx = ToolContext::new(cwd, config, "oneshot").with_interactive(false);
    let registry = ToolRegistry::with_builtins();
    let mut agent = Agent::new(agent_cfg, registry, ctx)?;

    if !agent.client().has_credentials() {
        anyhow::bail!("no API key configured for the current provider (set one with `joey config set`)");
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    // Drain events quietly; we only print the final text.
    let drain = tokio::spawn(async move {
        let mut final_text = String::new();
        while let Some(ev) = rx.recv().await {
            if let joey_agent_core::AgentEvent::Done { final_text: t, .. } = ev {
                final_text = t;
                break;
            }
        }
        final_text
    });
    let result = agent.run_turn(query, tx).await;
    let _ = drain.await;
    println!("{}", result.final_text);
    Ok(())
}
