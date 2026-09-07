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
    let mut list: Vec<String> = disabled_list(&config);
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
    // Write a proper YAML sequence — get_str_list (the reader) only parses
    // sequences, so a comma-joined scalar would be inert.
    let seq = serde_yaml::Value::Sequence(
        list.iter()
            .cloned()
            .map(serde_yaml::Value::String)
            .collect(),
    );
    config.set_value_and_save("skills.disabled", seq)?;
    println!("{}", Color::Green.paint(format!("✓ {verb}d skill '{name}'")));
    println!("  (applies to new sessions and after /reload-skills)");
    Ok(0)
}

/// Read the disabled-skill list. Sequences (the canonical form this command
/// writes) parse via `get_str_list`; a legacy comma-joined scalar (written
/// by older builds) is split on commas as a read-side migration nicety.
fn disabled_list(config: &joey_core::Config) -> Vec<String> {
    let list = config.get_str_list("skills.disabled");
    if list.is_empty() {
        if let Some(scalar) = config.get("skills.disabled").and_then(|v| v.as_str()) {
            return scalar
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
        }
    }
    list
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the process-global joey home to a temp dir under the shared
    /// override lock (same pattern as the neurocode tests) so config.yaml
    /// writes and skill discovery land in a temp home, never `~/.joey`.
    struct PinnedHome {
        _lock: std::sync::MutexGuard<'static, ()>,
        _guard: joey_core::constants::HomeOverrideGuard,
        _dir: tempfile::TempDir,
    }

    fn pinned_home() -> (PinnedHome, std::path::PathBuf) {
        let lock = joey_core::constants::TEST_HOME_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let guard = joey_core::constants::HomeOverrideGuard::new(home.clone());
        (PinnedHome { _lock: lock, _guard: guard, _dir: dir }, home)
    }

    fn make_skill(home: &std::path::Path, name: &str) {
        let dir = home.join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name} skill\n---\n\n# {name}\n"),
        )
        .unwrap();
    }

    /// Regression (#8): disable/enable must write a YAML SEQUENCE — the
    /// reader (`get_str_list`) only parses sequences, so the old
    /// comma-joined scalar was inert.
    #[test]
    fn disable_writes_sequence_and_round_trips() {
        let (_h, home) = pinned_home();
        make_skill(&home, "demo");
        make_skill(&home, "other");

        // Disable two skills → both survive a reload through the reader.
        assert_eq!(set_disabled("demo", true).unwrap(), 0);
        assert_eq!(set_disabled("other", true).unwrap(), 0);
        let cfg = joey_core::Config::load().unwrap();
        assert_eq!(
            cfg.get_str_list("skills.disabled"),
            vec!["demo".to_string(), "other".to_string()]
        );
        // On disk it is a sequence, not a comma-joined scalar.
        let raw = std::fs::read_to_string(home.join("config.yaml")).unwrap();
        assert!(
            !raw.contains("disabled: demo,other") && !raw.contains("disabled: 'demo,other'"),
            "scalar form must not be written, got: {raw}"
        );

        // Enable round-trips the same way.
        assert_eq!(set_disabled("demo", false).unwrap(), 0);
        let cfg = joey_core::Config::load().unwrap();
        assert_eq!(cfg.get_str_list("skills.disabled"), vec!["other".to_string()]);
    }

    /// Read-side migration nicety (#8): a legacy comma-joined scalar is
    /// parsed as a list by the command's read helper only.
    #[test]
    fn legacy_scalar_disabled_list_is_comma_split() {
        let (_h, home) = pinned_home();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("config.yaml"), "skills:\n  disabled: 'alpha, beta'\n").unwrap();
        let cfg = joey_core::Config::load().unwrap();
        // The generic reader still returns nothing for a scalar...
        assert!(cfg.get_str_list("skills.disabled").is_empty());
        // ...but the command's read helper migrates it.
        assert_eq!(
            disabled_list(&cfg),
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }
}
