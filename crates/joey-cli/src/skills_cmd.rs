//! `joey skills` (port of `hermes_cli/subcommands/skills.py` +
//! `skills_hub.skills_command`): bare prints the usage line like upstream;
//! `list [--enabled-only]` renders the installed-skills table; every other
//! upstream subcommand is recognized but deferred.

use anyhow::Result;
use clap::{Args, Subcommand};
use nu_ansi_term::Color;

#[derive(Args, Debug)]
pub struct SkillsArgs {
    #[command(subcommand)]
    pub action: Option<SkillsAction>,
}

#[derive(Subcommand, Debug)]
pub enum SkillsAction {
    /// List installed skills
    List {
        /// Hide disabled skills from the output
        #[arg(long = "enabled-only")]
        enabled_only: bool,
    },
    #[command(external_subcommand)]
    Other(Vec<String>),
}

/// Upstream subcommands that exist but are not ported.
const DEFERRED: &[&str] = &[
    "browse", "search", "install", "inspect", "audit", "check", "update", "uninstall", "reset",
    "list-modified", "diff", "opt-out", "opt-in", "repair-official", "publish", "snapshot", "tap",
    "config", "pending", "approve", "reject", "approval",
];

pub fn skills_command(args: &SkillsArgs) -> Result<i32> {
    match &args.action {
        None => {
            // Bare `joey skills` prints the subcommand usage (skills_hub.py).
            println!("Usage: joey skills [browse|search|install|inspect|list|list-modified|diff|check|update|audit|uninstall|reset|opt-out|opt-in|publish|snapshot|tap]");
            println!();
            println!("Run 'joey skills <command> --help' for details.");
            println!("(only 'list' is available in joey-agent so far)");
            Ok(0)
        }
        Some(SkillsAction::List { enabled_only }) => list(*enabled_only),
        Some(SkillsAction::Other(rest)) => {
            let sub = rest.first().map(String::as_str).unwrap_or("");
            if DEFERRED.contains(&sub) {
                println!("'joey skills {}' is not available in joey-agent yet.", sub);
                Ok(1)
            } else {
                eprintln!("Unknown skills command: {}", sub);
                eprintln!("Usage: joey skills [list]");
                Ok(2)
            }
        }
    }
}

/// `joey skills list` — Name/Category/Source/Status table
/// (skills_hub.do_list approximation; Trust column not ported).
fn list(enabled_only: bool) -> Result<i32> {
    let config = joey_core::Config::load()?;
    let disabled: Vec<String> = config.get_str_list("skills.disabled");
    let skills = joey_tools::tools::skills_tool::discover();

    let mut title = "Installed Skills".to_string();
    if enabled_only {
        title.push_str(" (enabled only)");
    }
    println!();
    println!("{}", Color::Cyan.bold().paint(title));
    println!();
    println!("  {:<28} {:<16} {:<10} {:<10}", "Name", "Category", "Source", "Status");
    println!("  {} {} {} {}", "─".repeat(28), "─".repeat(16), "─".repeat(10), "─".repeat(10));

    let local_dir = joey_core::constants::skills_dir();
    let mut enabled_count = 0usize;
    let mut disabled_count = 0usize;
    let mut rows = 0usize;
    let mut sorted = skills;
    sorted.sort_by(|a, b| {
        (a.category.clone().unwrap_or_default(), a.name.clone())
            .cmp(&(b.category.clone().unwrap_or_default(), b.name.clone()))
    });
    for s in &sorted {
        let is_disabled = disabled.iter().any(|d| d == &s.name);
        if is_disabled {
            disabled_count += 1;
        } else {
            enabled_count += 1;
        }
        if enabled_only && is_disabled {
            continue;
        }
        let source = if s.path.starts_with(&local_dir) { "local" } else { "builtin" };
        let status = if is_disabled {
            Color::DarkGray.paint("disabled").to_string()
        } else {
            Color::Green.paint("enabled").to_string()
        };
        println!(
            "  {:<28} {:<16} {:<10} {}",
            s.name,
            s.category.clone().unwrap_or_default(),
            source,
            status
        );
        rows += 1;
    }
    if rows == 0 {
        println!("  {}", Color::DarkGray.paint("(no skills installed)"));
    }
    println!();
    println!(
        "{}",
        Color::DarkGray.paint(format!("  {} enabled, {} disabled", enabled_count, disabled_count))
    );
    println!();
    Ok(0)
}
