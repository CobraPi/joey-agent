//! `joey doctor` (port of `hermes_cli/doctor.py` output conventions:
//! boxed `🩺` header, `◆ Section` banners, ✓/⚠/✗/→ marks, and the
//! `Found N issue(s) to address` / `All checks passed! 🎉` summary).

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::Result;
use clap::Args;
use joey_core::Config;
use nu_ansi_term::Color;

use crate::render::{check_fail, check_info, check_ok, check_warn, section};

#[derive(Args, Debug)]
pub struct DoctorArgs {
    /// Attempt to fix issues automatically
    #[arg(long)]
    pub fix: bool,
    /// Acknowledge a security advisory by ID and exit (not yet ported)
    #[arg(long, value_name = "ADVISORY_ID")]
    pub ack: Option<String>,
}

pub fn doctor_command(args: &DoctorArgs) -> Result<i32> {
    if args.ack.is_some() {
        println!("'joey doctor --ack' is not available in joey-agent yet.");
        return Ok(1);
    }
    let should_fix = args.fix;
    let mut issues: Vec<String> = Vec::new();
    let mut fixed_count = 0usize;

    println!();
    crate::render::boxed_header("🩺 Joey Doctor");

    // ── Configuration Files ────────────────────────────────────────────
    section("Configuration Files");
    let config_path = joey_core::constants::config_path();
    if config_path.exists() {
        match std::fs::read_to_string(&config_path)
            .map_err(anyhow::Error::from)
            .and_then(|s| serde_yaml::from_str::<serde_yaml::Value>(&s).map_err(Into::into))
        {
            Ok(_) => check_ok("config.yaml parses", &format!("({})", config_path.display())),
            Err(e) => {
                check_fail("config.yaml has a YAML syntax error", &format!("({})", e));
                issues.push(format!("Fix the YAML syntax in {}", config_path.display()));
            }
        }
    } else {
        check_ok("config.yaml not present (defaults in effect)", "");
    }
    let env_path = joey_core::constants::env_path();
    if env_path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&env_path).map(|m| m.permissions().mode() & 0o777).unwrap_or(0);
            if mode & 0o077 != 0 {
                if should_fix {
                    let _ = std::fs::set_permissions(&env_path, std::fs::Permissions::from_mode(0o600));
                    check_ok(".env permissions tightened to 600", "");
                    fixed_count += 1;
                } else {
                    check_warn(&format!(".env is group/world accessible (mode {:o})", mode), "");
                    issues.push(format!("Run `chmod 600 {}` (or `joey doctor --fix`)", env_path.display()));
                }
            } else {
                check_ok(".env permissions are restrictive (600)", "");
            }
        }
        #[cfg(not(unix))]
        check_ok(".env present", "");
    } else {
        check_ok(".env not present (no stored credentials)", "");
    }

    // ── Directory Structure ────────────────────────────────────────────
    section("Directory Structure");
    let home = joey_core::joey_home();
    for (label, path) in [
        ("home", home.clone()),
        ("skills", joey_core::constants::skills_dir()),
        ("logs", joey_core::logging::logs_dir()),
    ] {
        if path.exists() {
            check_ok(&format!("{} directory exists", label), &format!("({})", path.display()));
        } else if should_fix {
            match std::fs::create_dir_all(&path) {
                Ok(()) => {
                    check_ok(&format!("{} directory created", label), &format!("({})", path.display()));
                    fixed_count += 1;
                }
                Err(e) => {
                    check_fail(&format!("{} directory missing", label), &format!("({})", e));
                    issues.push(format!("Create {}", path.display()));
                }
            }
        } else {
            check_warn(&format!("{} directory missing", label), &format!("({})", path.display()));
            issues.push(format!("Create {} (or run `joey doctor --fix`)", path.display()));
        }
    }

    // ── Command Installation ───────────────────────────────────────────
    section("Command Installation");
    match which::which("joey") {
        Ok(p) => check_ok("joey is on PATH", &format!("({})", p.display())),
        Err(_) => {
            check_warn("joey is not on PATH", "");
            issues.push("Add the joey binary's directory to PATH (e.g. ~/.cargo/bin)".to_string());
        }
    }

    // ── Configuration / Provider ───────────────────────────────────────
    section("Model & Credentials");
    let config = Config::load()?;
    let model = config.model();
    if model.is_empty() {
        check_warn("no default model configured", "");
        issues.push("Pick a model with `joey model` or `joey config set model.default <name>`".to_string());
    } else {
        check_ok("default model configured", &format!("({})", model));
    }
    let provider_setting = config.get_str("model.provider", "auto");
    let base_url = config.get_str("model.base_url", "");
    let profile = joey_providers::resolve_profile(&provider_setting, &base_url, &model);
    let has_credentials = if profile.name == "copilot" {
        joey_providers::copilot::resolve_copilot_token()
            .map(|(token, _)| !token.is_empty())
            .unwrap_or(false)
    } else {
        profile.resolve_api_key().is_some()
    };
    if has_credentials {
        check_ok("provider credentials found", &format!("(provider: {})", profile.name));
    } else {
        check_fail("no API key for the active provider", &format!("(provider: {})", profile.name));
        if profile.name == "copilot" {
            issues.push("Authenticate with `joey auth copilot login`".to_string());
        } else {
            issues.push(format!(
                "Set an API key: `joey config set {} <key>`",
                profile.env_vars.first().copied().unwrap_or("PROVIDER_API_KEY")
            ));
        }
    }

    // ── External Tools ─────────────────────────────────────────────────
    section("External Tools");
    for (bin, required) in [("git", false), ("rg", false), ("bash", true), ("node", false), ("docker", false)] {
        match which::which(bin) {
            Ok(p) => check_ok(&format!("{} available", bin), &format!("({})", p.display())),
            Err(_) => {
                if required {
                    check_fail(&format!("{} not found on PATH", bin), "");
                    issues.push(format!("Install {}", bin));
                } else {
                    check_warn(&format!("{} not found on PATH", bin), "(optional)");
                }
            }
        }
    }

    // ── API Connectivity ───────────────────────────────────────────────
    section("API Connectivity");
    let effective_base = if base_url.is_empty() { profile.base_url.to_string() } else { base_url.clone() };
    match probe_https(&effective_base) {
        ProbeResult::Ok(ms) => {
            check_ok(&format!("{} reachable", host_of(&effective_base)), &format!("({} ms)", ms))
        }
        ProbeResult::Offline => {
            check_warn("network unreachable — skipping connectivity checks", "");
        }
        ProbeResult::Failed(e) => {
            check_fail(&format!("{} unreachable", host_of(&effective_base)), &format!("({})", e));
            issues.push(format!("Check network access to {}", effective_base));
        }
    }

    // ── Tool Availability ──────────────────────────────────────────────
    section("Tool Availability");
    let registry = joey_tools::ToolRegistry::with_builtins();
    let registered = registry.names().len();
    check_ok(&format!("{} built-in tools registered", registered), "");
    let cli_tools = crate::commands::platform_tools(&config, "cli");
    if cli_tools.is_empty() {
        check_fail("no toolsets enabled for the cli platform", "");
        issues.push("Enable toolsets with `joey tools enable <name> --platform cli`".to_string());
    } else {
        check_ok(&format!("{} tools enabled for platform 'cli'", cli_tools.len()), "");
    }

    // ── Skills ─────────────────────────────────────────────────────────
    section("Skills");
    let skills_dir = joey_core::constants::skills_dir();
    let skills = joey_tools::tools::skills_tool::discover();
    if skills_dir.exists() {
        check_ok(
            &format!("{} skill(s) discovered", skills.len()),
            &format!("({})", skills_dir.display()),
        );
    } else {
        check_warn("skills directory missing", &format!("({})", skills_dir.display()));
    }

    // ── Profiles ───────────────────────────────────────────────────────
    section("Profiles");
    let profiles_dir = joey_core::constants::default_root().join("profiles");
    if profiles_dir.is_dir() {
        let names: Vec<String> = std::fs::read_dir(&profiles_dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().is_dir())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        if names.is_empty() {
            check_ok("no extra profiles configured", "");
        } else {
            check_ok(&format!("{} profile(s)", names.len()), &format!("({})", names.join(", ")));
            check_info(&format!("active profile: {}", crate::active_profile()));
        }
    } else {
        check_ok("no extra profiles configured", "");
    }

    // ── Summary (doctor.py:2526-2553) ──────────────────────────────────
    println!();
    if should_fix && fixed_count > 0 {
        println!("{}", Color::Green.paint("─".repeat(60)));
        print!("{}", Color::Green.bold().paint(format!("  Fixed {} issue(s).", fixed_count)));
        if issues.is_empty() {
            println!();
        } else {
            println!(
                "{}",
                Color::Yellow.bold().paint(format!(" {} issue(s) require manual intervention.", issues.len()))
            );
        }
        println!();
        if !issues.is_empty() {
            for (i, issue) in issues.iter().enumerate() {
                println!("  {}. {}", i + 1, issue);
            }
            println!();
        }
    } else if !issues.is_empty() {
        println!("{}", Color::Yellow.paint("─".repeat(60)));
        println!(
            "{}",
            Color::Yellow.bold().paint(format!("  Found {} issue(s) to address:", issues.len()))
        );
        println!();
        for (i, issue) in issues.iter().enumerate() {
            println!("  {}. {}", i + 1, issue);
        }
        println!();
        if !should_fix {
            println!(
                "{}",
                Color::DarkGray.paint("  Tip: run 'joey doctor --fix' to auto-fix what's possible.")
            );
        }
    } else {
        println!("{}", Color::Green.paint("─".repeat(60)));
        println!("{}", Color::Green.bold().paint("  All checks passed! 🎉"));
    }
    println!();
    Ok(0)
}

enum ProbeResult {
    Ok(u128),
    Offline,
    Failed(String),
}

fn host_of(url: &str) -> String {
    let stripped = url.trim_start_matches("https://").trim_start_matches("http://");
    stripped.split('/').next().unwrap_or(stripped).to_string()
}

/// Quick TCP probe to the provider endpoint's host:443 with a short timeout;
/// distinguishes "this host down" from "no network at all" via a second probe.
fn probe_https(url: &str) -> ProbeResult {
    let host = host_of(url);
    let host_port = if host.contains(':') { host.clone() } else { format!("{}:443", host) };
    let start = std::time::Instant::now();
    let addrs: Vec<_> = match host_port.to_socket_addrs() {
        Ok(a) => a.collect(),
        Err(_) => return offline_or(&format!("DNS resolution failed for {}", host)),
    };
    let Some(addr) = addrs.first() else {
        return offline_or(&format!("no address for {}", host));
    };
    match TcpStream::connect_timeout(addr, Duration::from_secs(3)) {
        Ok(_) => ProbeResult::Ok(start.elapsed().as_millis()),
        Err(e) => offline_or(&format!("{}", e)),
    }
}

/// Downgrade a failure to "offline" when a well-known anchor host is also
/// unreachable (skip cleanly offline).
fn offline_or(err: &str) -> ProbeResult {
    let anchor = ("1.1.1.1", 443u16);
    let addr = std::net::SocketAddr::from(([1, 1, 1, 1], anchor.1));
    match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
        Ok(_) => ProbeResult::Failed(err.to_string()),
        Err(_) => ProbeResult::Offline,
    }
}
