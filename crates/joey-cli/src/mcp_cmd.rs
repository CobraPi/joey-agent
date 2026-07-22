//! `joey mcp` (port of `hermes_cli/subcommands/mcp.py:15-126` +
//! `hermes_cli/mcp_config.py`): add/remove/list/test manage `mcp_servers.*`
//! in config.yaml, validated by `validate_mcp_server_entry`. The port-only
//! ad-hoc `mcp list <command>` form is gone. catalog/login/reauth/picker/
//! install/configure/serve are recognized but deferred.

use std::time::Instant;

use anyhow::Result;
use clap::{Args, Subcommand};
use nu_ansi_term::Color;
use serde_yaml::{Mapping, Value};

fn info(text: &str) {
    println!("{}", Color::DarkGray.paint(format!("  {}", text)));
}
fn success(text: &str) {
    println!("{}", Color::Green.paint(format!("  ✓ {}", text)));
}
fn warning(text: &str) {
    println!("{}", Color::Yellow.paint(format!("  ⚠ {}", text)));
}
fn error(text: &str) {
    println!("{}", Color::Red.paint(format!("  ✗ {}", text)));
}

#[derive(Args, Debug)]
pub struct McpArgs {
    #[command(subcommand)]
    pub action: Option<McpAction>,
}

#[derive(Subcommand, Debug)]
pub enum McpAction {
    /// Add an MCP server to config.yaml
    Add(AddArgs),
    /// Remove an MCP server
    #[command(alias = "rm")]
    Remove {
        /// Server name to remove
        name: String,
    },
    /// List configured MCP servers
    #[command(alias = "ls")]
    List,
    /// Test MCP server connection
    Test {
        /// Server name to test
        name: String,
    },
    #[command(external_subcommand)]
    Other(Vec<String>),
}

#[derive(Args, Debug)]
pub struct AddArgs {
    /// Server name (used as config key)
    pub name: String,
    /// HTTP/SSE endpoint URL
    #[arg(long)]
    pub url: Option<String>,
    /// Stdio command (e.g. npx)
    #[arg(long)]
    pub command: Option<String>,
    /// Transport for URL servers (e.g. sse)
    #[arg(long)]
    pub transport: Option<String>,
    /// Timeout in seconds for initial connection and tool discovery
    #[arg(long = "connect-timeout")]
    pub connect_timeout: Option<f64>,
    /// Environment variables for stdio servers (KEY=VALUE)
    #[arg(long = "env", num_args = 0.., value_name = "KEY=VALUE")]
    pub env: Vec<String>,
    /// Arguments for stdio command; must be the last option
    #[arg(long = "args", num_args = 0.., allow_hyphen_values = true)]
    pub args: Vec<String>,
}

pub async fn mcp_command(args: McpArgs) -> Result<i32> {
    match args.action {
        None => {
            print_mcp_help();
            Ok(0)
        }
        Some(McpAction::Add(a)) => add(&a),
        Some(McpAction::Remove { name }) => remove(&name),
        Some(McpAction::List) => list(),
        Some(McpAction::Test { name }) => test(&name).await,
        Some(McpAction::Other(rest)) => {
            let sub = rest.first().map(String::as_str).unwrap_or("");
            match sub {
                "serve" | "catalog" | "picker" | "install" | "login" | "reauth" | "configure"
                | "config" => {
                    println!("'joey mcp {}' is not available in joey-agent yet.", sub);
                    Ok(1)
                }
                other => {
                    eprintln!("Unknown mcp command: {}", other);
                    eprintln!("Usage: joey mcp [add|remove|list|test]");
                    Ok(2)
                }
            }
        }
    }
}

fn print_mcp_help() {
    println!("{}", Color::Cyan.paint("  Commands:"));
    info("joey mcp add <name> --url <endpoint>          Add a custom MCP server");
    info("joey mcp add <name> --command <cmd>           Add a stdio server");
    info("joey mcp remove <name>                        Remove a server");
    info("joey mcp list                                 List configured servers");
    info("joey mcp test <name>                          Test connection");
    println!();
}

// ---------------------------------------------------------------------------
// Raw config.yaml editing (mcp_config._save_mcp_server / _remove_mcp_server)
// ---------------------------------------------------------------------------

fn load_user_doc() -> Mapping {
    let path = joey_core::constants::config_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_yaml::from_str::<Value>(&s).ok())
        .and_then(|v| v.as_mapping().cloned())
        .unwrap_or_default()
}

fn save_user_doc(doc: &Mapping) -> Result<()> {
    let path = joey_core::constants::config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_yaml::to_string(&Value::Mapping(doc.clone()))?;
    std::fs::write(&path, body)?;
    Ok(())
}

fn skey(s: &str) -> Value {
    Value::String(s.to_string())
}

fn get_servers(doc: &Mapping) -> Mapping {
    doc.get(skey("mcp_servers"))
        .and_then(|v| v.as_mapping())
        .cloned()
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// add (mcp_config.py:415-520, non-interactive subset)
// ---------------------------------------------------------------------------

fn parse_env_assignments(raw: &[String]) -> Result<Mapping, String> {
    let mut out = Mapping::new();
    for item in raw {
        let text = item.trim();
        if text.is_empty() {
            continue;
        }
        let Some((key, value)) = text.split_once('=') else {
            return Err(format!("Invalid --env value '{}' (expected KEY=VALUE)", text));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(format!("Invalid --env value '{}' (missing variable name)", text));
        }
        let valid = key
            .chars()
            .enumerate()
            .all(|(i, c)| c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()));
        if !valid {
            return Err(format!("Invalid --env variable name '{}'", key));
        }
        out.insert(skey(key), skey(value));
    }
    Ok(out)
}

fn add(a: &AddArgs) -> Result<i32> {
    let mut cmd_args: Vec<String> = a.args.clone();
    if cmd_args.first().map(String::as_str) == Some("--") {
        cmd_args.remove(0);
    }

    let explicit_env = match parse_env_assignments(&a.env) {
        Ok(e) => e,
        Err(msg) => {
            error(&msg);
            return Ok(1);
        }
    };

    if a.url.is_some() && !explicit_env.is_empty() {
        error("--env is only supported for stdio MCP servers (--command)");
        return Ok(1);
    }
    if a.url.is_none() && a.command.is_none() {
        error("Must specify --url <endpoint> or --command <cmd>");
        info("Examples:");
        info("  joey mcp add ink --url \"https://mcp.ml.ink/mcp\"");
        info("  joey mcp add github --command npx --args @modelcontextprotocol/server-github");
        return Ok(1);
    }

    // Build the server entry.
    let mut server = Mapping::new();
    if let Some(url) = &a.url {
        server.insert(skey("url"), skey(url));
        if let Some(t) = &a.transport {
            server.insert(skey("transport"), skey(t));
        }
    } else {
        server.insert(skey("command"), skey(a.command.as_deref().unwrap_or("")));
        if !cmd_args.is_empty() {
            server.insert(
                skey("args"),
                Value::Sequence(cmd_args.iter().map(|s| skey(s)).collect()),
            );
        }
        if !explicit_env.is_empty() {
            server.insert(skey("env"), Value::Mapping(explicit_env));
        }
    }
    if let Some(t) = a.connect_timeout {
        server.insert(
            skey("connect_timeout"),
            Value::Number(serde_yaml::Number::from(t)),
        );
    }

    // Security validation (mcp_security.validate_mcp_server_entry).
    let entry = Value::Mapping(server.clone());
    let issues = joey_mcp::validate_mcp_server_entry(&a.name, &entry);
    if !issues.is_empty() {
        for issue in issues {
            warning(&issue);
        }
        warning(&format!("Server '{}' was NOT saved due to suspicious configuration.", a.name));
        return Ok(1);
    }

    let mut doc = load_user_doc();
    let mut servers = get_servers(&doc);
    let existed = servers.contains_key(skey(&a.name));
    servers.insert(skey(&a.name), entry);
    doc.insert(skey("mcp_servers"), Value::Mapping(servers));
    save_user_doc(&doc)?;
    if existed {
        success(&format!("Updated '{}' in config", a.name));
    } else {
        success(&format!("Added '{}' to config", a.name));
    }
    info("Test it with: joey mcp test <name>");
    Ok(0)
}

// ---------------------------------------------------------------------------
// remove (mcp_config.py:620-648, confirmation skipped: non-interactive port)
// ---------------------------------------------------------------------------

fn remove(name: &str) -> Result<i32> {
    let mut doc = load_user_doc();
    let mut servers = get_servers(&doc);
    if !servers.contains_key(skey(name)) {
        error(&format!("Server '{}' not found in config.", name));
        let names: Vec<String> = servers
            .keys()
            .filter_map(|k| k.as_str().map(str::to_string))
            .collect();
        if !names.is_empty() {
            info(&format!("Available servers: {}", names.join(", ")));
        }
        return Ok(1);
    }
    servers.remove(skey(name));
    if servers.is_empty() {
        doc.remove(skey("mcp_servers"));
    } else {
        doc.insert(skey("mcp_servers"), Value::Mapping(servers));
    }
    save_user_doc(&doc)?;
    success(&format!("Removed '{}' from config", name));
    Ok(0)
}

// ---------------------------------------------------------------------------
// list (mcp_config.py:652-717 table)
// ---------------------------------------------------------------------------

fn list() -> Result<i32> {
    let config = joey_core::Config::load()?;
    let servers = match config.get("mcp_servers") {
        Some(Value::Mapping(m)) => m.clone(),
        _ => Mapping::new(),
    };
    if servers.is_empty() {
        println!();
        info("No MCP servers configured.");
        println!();
        info("Add one with:");
        info("  joey mcp add <name> --url <endpoint>");
        info("  joey mcp add <name> --command <cmd> --args <args...>");
        println!();
        return Ok(0);
    }

    println!();
    println!("{}", Color::Cyan.bold().paint("  MCP Servers:"));
    println!();
    println!("  {:<16} {:<30} {:<12} {:<10}", "Name", "Transport", "Tools", "Status");
    println!("  {} {} {} {}", "─".repeat(16), "─".repeat(30), "─".repeat(12), "─".repeat(10));

    for (k, cfg) in &servers {
        let name = k.as_str().unwrap_or("?");
        let transport = if let Some(url) = cfg.get("url").and_then(|v| v.as_str()) {
            truncate(url, 28)
        } else if let Some(cmd) = cfg.get("command").and_then(|v| v.as_str()) {
            let args: Vec<String> = cfg
                .get("args")
                .and_then(|v| v.as_sequence())
                .map(|s| s.iter().filter_map(|a| a.as_str().map(str::to_string)).take(2).collect())
                .unwrap_or_default();
            if args.is_empty() {
                truncate(cmd, 28)
            } else {
                truncate(&format!("{} {}", cmd, args.join(" ")), 28)
            }
        } else {
            "?".to_string()
        };

        let tools_str = match cfg.get("tools") {
            Some(Value::Mapping(t)) => {
                if let Some(Value::Sequence(inc)) = t.get(skey("include")) {
                    format!("{} selected", inc.len())
                } else if let Some(Value::Sequence(exc)) = t.get(skey("exclude")) {
                    format!("-{} excluded", exc.len())
                } else {
                    "all".to_string()
                }
            }
            _ => "all".to_string(),
        };

        let enabled = match cfg.get("enabled") {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => matches!(s.to_lowercase().as_str(), "true" | "1" | "yes"),
            _ => true,
        };
        let status = if enabled {
            Color::Green.paint("✓ enabled").to_string()
        } else {
            Color::DarkGray.paint("✗ disabled").to_string()
        };
        println!("  {:<16} {:<30} {:<12} {}", name, transport, tools_str, status);
    }
    println!();
    Ok(0)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        format!("{}...", s.chars().take(max.saturating_sub(3)).collect::<String>())
    } else {
        s.to_string()
    }
}

// ---------------------------------------------------------------------------
// test (mcp_config.py:721-780)
// ---------------------------------------------------------------------------

async fn test(name: &str) -> Result<i32> {
    let config = joey_core::Config::load()?;
    let servers = joey_mcp::load_server_configs(&config);
    let Some(server_cfg) = servers.get(name) else {
        error(&format!("Server '{}' not found in config.", name));
        let available: Vec<&String> = servers.keys().collect();
        if !available.is_empty() {
            info(&format!(
                "Available: {}",
                available.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
        return Ok(1);
    };

    println!();
    println!("{}", Color::Cyan.paint(format!("  Testing '{}'...", name)));
    if let Some(url) = &server_cfg.url {
        info(&format!("Transport: HTTP → {}", url));
    } else {
        info(&format!("Transport: stdio → {}", server_cfg.command.as_deref().unwrap_or("?")));
    }
    if server_cfg.headers.is_empty() {
        info("Auth: none");
    } else {
        info("Auth: headers configured");
    }

    let start = Instant::now();
    match joey_mcp::McpClient::connect(name, server_cfg).await {
        Err(e) => {
            let ms = start.elapsed().as_millis();
            error(&format!("Connection failed ({}ms): {}", ms, e));
            Ok(1)
        }
        Ok(client) => {
            let ms = start.elapsed().as_millis();
            match client.list_tools().await {
                Err(e) => {
                    error(&format!("Tool discovery failed: {}", e));
                    client.shutdown().await;
                    Ok(1)
                }
                Ok(tools) => {
                    success(&format!("Connected ({}ms)", ms));
                    success(&format!("Tools discovered: {}", tools.len()));
                    if !tools.is_empty() {
                        println!();
                        for t in &tools {
                            let short = truncate(&t.description, 55);
                            println!("    {:<36} {}", Color::Green.paint(&t.name).to_string(), short);
                        }
                    }
                    println!();
                    client.shutdown().await;
                    Ok(0)
                }
            }
        }
    }
}
