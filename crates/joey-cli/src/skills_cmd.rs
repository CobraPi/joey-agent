//! `joey skills` (port of `hermes_cli/subcommands/skills.py` +
//! `skills_hub.skills_command`): bare prints the usage line like upstream;
//! `list [--enabled-only]`, `inspect`, `enable`, `disable`, `config` are
//! fully local; marketplace subcommands (browse/search/install/publish) are
//! recognized but deferred (they need the registry service).

use anyhow::Result;
use clap::{Args, Subcommand};
use nu_ansi_term::Color;

#[derive(Args, Debug)]
pub struct SkillsArgs {
    #[command(subcommand)]
    pub action: Option<SkillsAction>,
}

/// Upstream subcommands that exist but are not ported (they need the skills
/// marketplace/registry service).
const DEFERRED: &[&str] = &[
    "browse", "search", "install", "publish", "repair-official", "tap",
];

#[derive(Subcommand, Debug)]
pub enum SkillsAction {
    /// List installed skills
    List {
        /// Hide disabled skills from the output
        #[arg(long = "enabled-only")]
        enabled_only: bool,
    },
    /// Inspect a skill's SKILL.md (description, path, body)
    Inspect { name: String },
    /// Enable a disabled skill by name
    Enable { name: String },
    /// Disable a skill by name (hidden from the agent until re-enabled)
    Disable { name: String },
    /// Show where skills live and how to install them manually
    Config,
    #[command(external_subcommand)]
    Other(Vec<String>),
}

pub fn skills_command(args: &SkillsArgs) -> Result<i32> {
    match &args.action {
        None => {
            // Bare `joey skills` prints the subcommand usage (skills_hub.py).
            println!("Usage: joey skills [list|inspect|enable|disable|config]");
            println!();
            println!("Run 'joey skills <command> --help' for details.");
            println!("(marketplace subcommands — browse/search/install/publish — are deferred)");
            Ok(0)
        }
        Some(SkillsAction::List { enabled_only }) => list(*enabled_only),
        Some(SkillsAction::Inspect { name }) => inspect(name),
        Some(SkillsAction::Enable { name }) => set_disabled(name, false),
        Some(SkillsAction::Disable { name }) => set_disabled(name, true),
        Some(SkillsAction::Config) => config_info(),
        Some(SkillsAction::Other(rest)) => {
            let sub = rest.first().map(String::as_str).unwrap_or("");
            if DEFERRED.contains(&sub) {
                println!("'joey skills {sub}' needs the skills marketplace service, which is not part of this port.");
                println!("Install skills manually: git clone <repo> ~/.joey/skills/<name> (dir with SKILL.md)");
                Ok(1)
            } else {
                eprintln!("Unknown skills command: {}", sub);
                eprintln!("Usage: joey skills [list|inspect|enable|disable|config]");
                Ok(2)
            }
        }
    }
}

/// `joey skills inspect <name>` — description, path, and SKILL.md body.
fn inspect(name: &str) -> Result<i32> {
    let skills = joey_tools::tools::skills_tool::discover();
    let Some(skill) = skills.iter().find(|s| s.name == name) else {
        println!("Skill '{name}' not found. Installed skills:");
        for s in &skills {
            println!("  · {}", s.name);
        }
        return Ok(1);
    };
    println!();
    println!("{}", Color::Cyan.bold().paint(format!("Skill: {}", skill.name)));
    if let Some(cat) = &skill.category {
        println!("  Category:    {cat}");
    }
    println!("  Description: {}", skill.description);
    println!("  Path:        {}", skill.path.display());
    let body = std::fs::read_to_string(skill.path.join("SKILL.md")).unwrap_or_default();
    println!();
    println!("{}", Color::DarkGray.paint("── SKILL.md ──"));
    println!("{}", body.trim_end());
    Ok(0)
}

/// `joey skills enable|disable <name>` — manage the skills.disabled list.
fn set_disabled(name: &str, disable: bool) -> Result<i32> {
    // Verify the skill exists.
    let skills = joey_tools::tools::skills_tool::discover();
    let all: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    let known = skills.iter().any(|s| s.name == name);
    if !known {
        println!("Skill '{name}' not found. Discovered: {}", all.join(", "));
        return Ok(1);
    }
    let mut config = joey_core::Config::load()?;
    let mut list: Vec<String> = config.get_str_list("skills.disabled");
    let verb = if disable { "disable" } else { "enable" };
    if disable {
        if list.iter().any(|s| s == name) {
            println!("Skill '{name}' is already disabled.");
            return Ok(0);
        }
        list.push(name.to_string());
    } else {
        let before = list.len();
        list.retain(|s| s != name);
        if list.len() == before {
            println!("Skill '{name}' is not disabled.");
            return Ok(0);
        }
    }
    let joined = list.join(",");
    config.set_and_save("skills.disabled", &joined)?;
    println!("{}", Color::Green.paint(format!("✓ {verb}d skill '{name}'")));
    println!("  (applies to new sessions and after /reload-skills)");
    Ok(0)
}

/// `joey skills config` — where skills live + manual install instructions.
fn config_info() -> Result<i32> {
    let local = joey_core::constants::skills_dir();
    let bundled = joey_core::constants::bundled_skills_dir(None);
    println!();
    println!("{}", Color::Cyan.bold().paint("Skills Configuration"));
    println!();
    println!("  Local skills:     {}", local.display());
    println!("  Bundled skills:   {}", bundled.display());
    println!("  Disabled list:    skills.disabled in config.yaml");
    println!();
    println!("Install a skill manually:");
    println!("  git clone <repo> {}", local.join("<name>").display());
    println!("  (the directory must contain a SKILL.md with name/description frontmatter)");
    println!();
    println!("Reload after changes: /reload-skills (or restart joey)");
    Ok(0)
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
