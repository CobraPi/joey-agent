//! `joey copilot` — manage GitHub-Copilot-style repo extensions for Joey.
//!
//! Built on the `joey-copilot` crate (parsers for `.github/` extension files
//! plus the plugin manifest). This module provides:
//!
//! - the `joey copilot` CLI surface (install/list/remove/update/status),
//!   modeled on `skills_cmd.rs` / `mcp_cmd.rs`,
//! - `copilot_status_text` — a pure renderer of what the current project's
//!   `.github/` provides,
//! - slash-command helpers (`slash_response_lines`, `find_prompt_body`) for
//!   the REPL/TUI wiring (registered in a later wave).
//!
//! Plugins are installed under `~/.joey/copilot/plugins/<name>`; any
//! `SKILL.md` folders they ship are copied to
//! `~/.joey/skills/copilot/<plugin>/<skill>/` so the regular skills
//! discovery picks them up. User skills are never touched on remove.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use nu_ansi_term::Color;
use walkdir::WalkDir;

use joey_copilot::PluginRecord;

/// Skill-directory names that hold supporting files, not skills (mirrors
/// `joey_copilot`'s SKIP_DIRS).
const SKIP_DIRS: [&str; 4] = ["references", "templates", "assets", "scripts"];

#[derive(Args, Debug)]
pub struct CopilotArgs {
    #[command(subcommand)]
    pub action: Option<CopilotAction>,
}

#[derive(Subcommand, Debug)]
pub enum CopilotAction {
    /// Install a Copilot plugin/skill collection from a git URL, owner/repo, or local path
    Install {
        source: String,
        #[arg(long = "ref", value_name = "REF")]
        git_ref: Option<String>,
    },
    /// List installed plugins and per-plugin skills/prompts
    List,
    /// Remove an installed plugin (keeps user skills untouched)
    Remove { name: String },
    /// Re-pull installed plugin(s) from their source
    Update { name: Option<String> },
    /// Show what the current project's .github/ provides (instructions, prompts, skills, mcp.json)
    Status,
}

// ---------------------------------------------------------------------------
// Source classification
// ---------------------------------------------------------------------------

/// How an install source string was resolved.
#[derive(Debug)]
pub(crate) enum SourceKind {
    /// A full git URL (`https://…`, `ssh://…`, …) — used as-is.
    Git(String),
    /// `owner/repo` — resolved to `https://github.com/<owner>/<repo>.git`.
    OwnerRepo(String, String),
    /// An existing local directory — copied.
    Local(PathBuf),
    /// Anything else.
    Unsupported,
}

/// Classify an install source: git URL (`contains "://"`), `owner/repo`
/// (`^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$`), existing local path, else unsupported.
pub(crate) fn classify_source(s: &str) -> SourceKind {
    if s.contains("://") {
        SourceKind::Git(s.to_string())
    } else if let Some((owner, repo)) = parse_owner_repo(s) {
        SourceKind::OwnerRepo(owner, repo)
    } else {
        let p = Path::new(s);
        if p.exists() {
            SourceKind::Local(p.to_path_buf())
        } else {
            SourceKind::Unsupported
        }
    }
}

/// `owner/repo` with each side non-empty and drawn from `[A-Za-z0-9_.-]`.
fn parse_owner_repo(s: &str) -> Option<(String, String)> {
    let (owner, repo) = s.split_once('/')?;
    let ok = |x: &str| {
        !x.is_empty()
            && x.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    };
    if ok(owner) && ok(repo) {
        Some((owner.to_string(), repo.to_string()))
    } else {
        None
    }
}

/// GitHub URL for Git/OwnerRepo sources; `None` for local/unsupported.
fn git_url(kind: &SourceKind) -> Option<String> {
    match kind {
        SourceKind::Git(url) => Some(url.clone()),
        SourceKind::OwnerRepo(owner, repo) => {
            Some(format!("https://github.com/{owner}/{repo}.git"))
        }
        _ => None,
    }
}

/// Plugin name derived from a git URL: last path segment sans `.git`.
fn plugin_name_from_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    last.trim_end_matches(".git").to_string()
}

/// Validate a plugin name: non-empty, `[A-Za-z0-9._-]` only.
pub(crate) fn sanitize_name(name: &str) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    let valid = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if valid {
        Some(name.to_string())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Command dispatch
// ---------------------------------------------------------------------------

pub fn copilot_command(args: &CopilotArgs) -> Result<i32> {
    match &args.action {
        None => {
            // Bare `joey copilot` prints the subcommand usage (skills_cmd style).
            println!("Usage: joey copilot [install|list|remove|update|status]");
            println!();
            println!("Run 'joey copilot <command> --help' for details.");
            Ok(0)
        }
        Some(CopilotAction::Install { source, git_ref }) => install(source, git_ref.as_deref()),
        Some(CopilotAction::List) => {
            println!("{}", list_text());
            Ok(0)
        }
        Some(CopilotAction::Remove { name }) => remove(name),
        Some(CopilotAction::Update { name }) => update(name.as_deref()),
        Some(CopilotAction::Status) => {
            let cwd = std::env::current_dir().unwrap_or_default();
            println!("{}", copilot_status_text(&cwd));
            Ok(0)
        }
    }
}

// ---------------------------------------------------------------------------
// Status (pure)
// ---------------------------------------------------------------------------

/// Render what the project rooted at `cwd` provides via its `.github/`
/// Copilot extension files. Pure: no agent, no side effects.
pub fn copilot_status_text(cwd: &Path) -> String {
    let bundle = joey_copilot::discover(cwd);
    // Same config-reload style as slash_extra::reload_mcp_lines.
    let config = joey_core::Config::load().unwrap_or_else(|_| joey_core::Config::defaults());
    let enabled = config.get_bool("copilot.enabled", true);

    let mut out = format!(
        "Copilot integration: {}\n",
        if enabled { "enabled" } else { "disabled" }
    );

    out.push_str("\nInstructions:\n");
    match &bundle.instructions {
        Some(text) => {
            for line in text.lines().take(3) {
                out.push_str("  ");
                out.push_str(line);
                out.push('\n');
            }
        }
        None => out.push_str("  (none)\n"),
    }

    out.push_str(&format!(
        "\nInstruction files ({}):\n",
        bundle.instruction_files.len()
    ));
    for f in &bundle.instruction_files {
        out.push_str(&format!("  {}\n", f.path.display()));
    }

    out.push_str(&format!("\nPrompts ({}):\n", bundle.prompts.len()));
    for p in &bundle.prompts {
        match &p.mode {
            Some(mode) => out.push_str(&format!("  · {} ({}) — {}\n", p.name, mode, p.description)),
            None => out.push_str(&format!("  · {} — {}\n", p.name, p.description)),
        }
    }

    out.push_str(&format!("\nSkills ({}):\n", bundle.skills.len()));
    for s in &bundle.skills {
        out.push_str(&format!("  · {} — {}\n", s.name, s.description));
    }

    out.push_str(&format!("\nMCP servers ({}):\n", server_count(&bundle)));
    if let Some(names) = server_names(&bundle) {
        for n in names {
            out.push_str(&format!("  · {}\n", n));
        }
    }
    out.push_str(
        "\nnote: project mcp.json merges into mcp_servers (user config wins); servers connect per joey mcp test\n",
    );
    out
}

fn server_count(bundle: &joey_copilot::CopilotBundle) -> usize {
    bundle
        .mcp_servers
        .as_ref()
        .and_then(|v| v.as_object())
        .map(|o| o.len())
        .unwrap_or(0)
}

fn server_names(bundle: &joey_copilot::CopilotBundle) -> Option<Vec<String>> {
    let obj = bundle.mcp_servers.as_ref()?.as_object()?;
    Some(obj.keys().cloned().collect())
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

/// Render the installed-plugin list (shared by `joey copilot list` and the
/// slash helper).
fn list_text() -> String {
    let manifest = joey_copilot::load_manifest();
    if manifest.plugins.is_empty() {
        return "No copilot plugins installed (joey copilot install <source>)".to_string();
    }
    let mut out = format!("Installed copilot plugins ({}):\n", manifest.plugins.len());
    for p in &manifest.plugins {
        out.push_str(&format!(
            "\n  {} — installed {}\n    source:  {}\n    skills:  {}\n    prompts: {}\n",
            p.name,
            p.installed_at,
            p.source,
            p.skills.len(),
            p.prompts.len()
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

fn install(source: &str, git_ref: Option<&str>) -> Result<i32> {
    let kind = classify_source(source);
    let raw_name = match &kind {
        SourceKind::Git(url) => plugin_name_from_url(url),
        SourceKind::OwnerRepo(_, repo) => repo.clone(),
        SourceKind::Local(p) => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        SourceKind::Unsupported => {
            eprintln!("unsupported source (use git URL, owner/repo, or local path)");
            return Ok(2);
        }
    };
    let Some(name) = sanitize_name(&raw_name) else {
        eprintln!(
            "invalid plugin name derived from source: '{raw_name}' (allowed: A-Za-z0-9._-)"
        );
        return Ok(2);
    };
    let target = joey_copilot::plugins_dir().join(&name);
    if target.exists() {
        println!("already installed (use joey copilot update {name})");
        return Ok(1);
    }
    match perform_install(&kind, source, &name, git_ref) {
        Ok(record) => {
            let skills_dest =
                joey_core::constants::skills_dir().join("copilot").join(&name);
            println!(
                "{}",
                Color::Green.paint(format!("✓ Installed copilot plugin '{name}'"))
            );
            println!("  source:  {}", record.source);
            println!(
                "  commit:  {}",
                record.commit.as_deref().unwrap_or("n/a")
            );
            println!(
                "  skills:  {} copied to {}",
                record.skills.len(),
                skills_dest.display()
            );
            println!("  prompts: {} available", record.prompts.len());
            println!("  path:    {}", target.display());
            Ok(0)
        }
        Err(e) => {
            eprintln!("{e}");
            Ok(1)
        }
    }
}

/// Shared install core (steps d–f): clone/copy into the plugins dir, scan for
/// skills/prompts, copy skill folders, upsert the manifest record.
fn perform_install(
    kind: &SourceKind,
    source: &str,
    name: &str,
    git_ref: Option<&str>,
) -> Result<PluginRecord> {
    let target = joey_copilot::plugins_dir().join(name);
    let mut commit = None;
    if let Some(url) = git_url(kind) {
        if which::which("git").is_err() {
            anyhow::bail!("git not found on PATH — required to install from git");
        }
        let target_str = target.display().to_string();
        let mut args: Vec<String> = ["clone", "--depth", "1"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        if let Some(r) = git_ref {
            args.push("--branch".to_string());
            args.push(r.to_string());
        }
        args.push(url);
        args.push(target_str);
        let out = Command::new("git")
            .args(&args)
            .output()
            .context("failed to spawn git")?;
        if !out.status.success() {
            let _ = std::fs::remove_dir_all(&target);
            eprintln!("{}", String::from_utf8_lossy(&out.stderr).trim_end());
            anyhow::bail!("git clone failed");
        }
        commit = rev_parse(&target);
    } else if let SourceKind::Local(src) = kind {
        copy_tree(src, &target)
            .with_context(|| format!("copying {} -> {}", src.display(), target.display()))?;
    } else {
        anyhow::bail!("unsupported source");
    }

    let (skills, prompts) = scan_and_install_skills(&target, name);
    let mut manifest = joey_copilot::load_manifest();
    manifest.plugins.retain(|r| r.name != name);
    let record = PluginRecord {
        name: name.to_string(),
        source: source.to_string(),
        installed_at: now_rfc3339(),
        commit: commit.clone(),
        skills: skills.clone(),
        prompts: prompts.clone(),
    };
    manifest.plugins.push(record.clone());
    joey_copilot::save_manifest(&manifest).context("saving plugin manifest")?;
    Ok(record)
}

/// `git -C <target> rev-parse HEAD`, if it works.
fn rev_parse(target: &Path) -> Option<String> {
    let t = target.display().to_string();
    let out = Command::new("git")
        .args(["-C", &t, "rev-parse", "HEAD"])
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Remove / Update
// ---------------------------------------------------------------------------

fn remove(name: &str) -> Result<i32> {
    let mut manifest = joey_copilot::load_manifest();
    let Some(idx) = manifest.plugins.iter().position(|r| r.name == name) else {
        println!("No copilot plugin named '{name}' installed (joey copilot list)");
        return Ok(1);
    };
    manifest.plugins.remove(idx);
    joey_copilot::save_manifest(&manifest).context("saving plugin manifest")?;
    // Remove copied skills and the plugin tree; user skills are untouched.
    let _ = std::fs::remove_dir_all(
        joey_core::constants::skills_dir().join("copilot").join(name),
    );
    let _ = std::fs::remove_dir_all(joey_copilot::plugins_dir().join(name));
    println!("{}", Color::Green.paint(format!("✓ Removed copilot plugin '{name}'")));
    Ok(0)
}

fn update(name: Option<&str>) -> Result<i32> {
    let mut manifest = joey_copilot::load_manifest();
    if manifest.plugins.is_empty() {
        println!("No copilot plugins installed (joey copilot install <source>)");
        return Ok(0);
    }
    if let Some(n) = name {
        if !manifest.plugins.iter().any(|r| r.name == n) {
            println!("No copilot plugin named '{n}' installed (joey copilot list)");
            return Ok(1);
        }
    }
    for record in manifest.plugins.iter_mut() {
        if let Some(n) = name {
            if record.name != n {
                continue;
            }
        }
        let kind = classify_source(&record.source);
        let target = joey_copilot::plugins_dir().join(&record.name);
        let mut commit = record.commit.clone();
        match &kind {
            SourceKind::Git(_) | SourceKind::OwnerRepo(..) => {
                if which::which("git").is_err() {
                    println!("skip {} (git not found on PATH)", record.name);
                    continue;
                }
                if !target.exists() {
                    match perform_install(&kind, &record.source, &record.name, None) {
                        Ok(r) => commit = r.commit,
                        Err(e) => {
                            println!("skip {} ({e})", record.name);
                            continue;
                        }
                    }
                } else {
                    let t = target.display().to_string();
                    let pull = Command::new("git")
                        .args(["-C", &t, "pull", "--ff-only"])
                        .output()
                        .context("git pull")?;
                    if pull.status.success() {
                        commit = rev_parse(&target);
                    } else {
                        // Shallow (depth-1) clones often cannot ff-pull; fall
                        // back to a fresh re-clone.
                        let _ = std::fs::remove_dir_all(&target);
                        match perform_install(&kind, &record.source, &record.name, None) {
                            Ok(r) => commit = r.commit,
                            Err(e) => {
                                println!("skip {} ({e})", record.name);
                                continue;
                            }
                        }
                    }
                }
            }
            SourceKind::Local(src) => {
                if !src.exists() {
                    println!(
                        "skip {} (local source {} no longer exists)",
                        record.name,
                        src.display()
                    );
                    continue;
                }
                let _ = std::fs::remove_dir_all(&target);
                copy_tree(src, &target)
                    .with_context(|| format!("copying {}", src.display()))?;
                commit = None;
            }
            SourceKind::Unsupported => {
                println!(
                    "skip {} (source neither git nor local: {})",
                    record.name, record.source
                );
                continue;
            }
        }
        // Re-sync the copied skills dir, then refresh the record.
        let _ = std::fs::remove_dir_all(
            joey_core::constants::skills_dir().join("copilot").join(&record.name),
        );
        let (skills, prompts) = scan_and_install_skills(&target, &record.name);
        record.skills = skills;
        record.prompts = prompts;
        record.installed_at = now_rfc3339();
        record.commit = commit;
        println!(
            "{}",
            Color::Green.paint(format!("✓ Updated copilot plugin '{}'", record.name))
        );
    }
    joey_copilot::save_manifest(&manifest).context("saving plugin manifest")?;
    Ok(0)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Recursive copy preserving relative paths (walkdir; symlinks are not
/// followed and are skipped, matching walkdir's default).
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in WalkDir::new(src).into_iter().flatten() {
        let rel = match entry.path().strip_prefix(src) {
            Ok(r) if r.as_os_str().is_empty() => continue,
            Ok(r) => r,
            Err(_) => continue,
        };
        let dest = dst.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}

/// Scan a freshly installed plugin tree for `SKILL.md` skill folders and
/// `*.prompt.md` files (walkdir, max_depth 4, skipping
/// references/templates/assets/scripts skill dirs). Each skill folder is
/// copied whole to `~/.joey/skills/copilot/<plugin>/<skill-dir>/`.
/// Returns (skill dir names, prompt names).
fn scan_and_install_skills(target: &Path, plugin: &str) -> (Vec<String>, Vec<String>) {
    let mut skills = Vec::new();
    let mut prompts = Vec::new();
    if !target.is_dir() {
        return (skills, prompts);
    }
    for entry in WalkDir::new(target).max_depth(4).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let fname = entry.file_name().to_string_lossy().into_owned();
        if fname == "SKILL.md" {
            let Some(dir) = entry.path().parent() else { continue };
            let Some(dir_name) = dir.file_name().map(|n| n.to_string_lossy().into_owned())
            else {
                continue;
            };
            if SKIP_DIRS.contains(&dir_name.as_str()) {
                continue;
            }
            let dest = joey_core::constants::skills_dir()
                .join("copilot")
                .join(plugin)
                .join(&dir_name);
            if copy_tree(dir, &dest).is_ok() {
                skills.push(dir_name);
            }
        } else if fname.ends_with(".prompt.md") {
            prompts.push(fname.trim_end_matches(".prompt.md").to_string());
        }
    }
    (skills, prompts)
}

/// Current UTC time as RFC3339 (`YYYY-MM-DDTHH:MM:SSZ`), matching the
/// manifest timestamp format used by `joey-copilot`.
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------------------
// Slash support helpers (pure; consumed by the REPL/TUI wiring, wave 3)
// ---------------------------------------------------------------------------

/// Map a `/copilot <sub> <args>` slash invocation to a response string.
/// `None` means "not a copilot slash subcommand".
pub fn slash_response_lines(sub: &str, args: &str) -> Option<String> {
    let args = args.trim();
    match sub {
        "" | "status" => {
            let cwd = std::env::current_dir().unwrap_or_default();
            Some(copilot_status_text(&cwd))
        }
        "list" => Some(list_text()),
        "install" => Some(format!(
            "git operations run via: joey copilot install {}",
            if args.is_empty() { "<source>" } else { args }
        )),
        "remove" => Some(format!(
            "removal runs via: joey copilot remove {}",
            if args.is_empty() { "<name>" } else { args }
        )),
        "update" => Some(format!(
            "updates run via: joey copilot update {}",
            if args.is_empty() { "[name]" } else { args }
        )),
        _ => None,
    }
}

/// Find a prompt's `(body, mode)` by name: first in `<cwd>/.github/prompts/`
/// (via `joey_copilot::parse_prompts`), then in any installed plugin under
/// `~/.joey/copilot/plugins/` (walking for `<name>.prompt.md`, hand-splitting
/// frontmatter). `None` if not found anywhere.
pub fn find_prompt_body(name: &str, cwd: &Path) -> Option<(String, Option<String>)> {
    if let Some(p) = joey_copilot::parse_prompts(cwd)
        .into_iter()
        .find(|p| p.name == name)
    {
        return Some((p.body, p.mode));
    }
    let plugins = joey_copilot::plugins_dir();
    if !plugins.is_dir() {
        return None;
    }
    let wanted = format!("{name}.prompt.md");
    for entry in WalkDir::new(&plugins).max_depth(4).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.file_name().to_string_lossy() != wanted {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let (mode, body) = split_prompt_frontmatter(&text);
        return Some((body, mode));
    }
    None
}

/// Split YAML frontmatter from a prompt file, extracting `mode` (same splitter
/// semantics as `joey_copilot`'s internal `split_frontmatter`).
fn split_prompt_frontmatter(text: &str) -> (Option<String>, String) {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("---") {
        return (None, text.trim().to_string());
    }
    let after = &trimmed[3..];
    let Some(end) = after.find("\n---") else {
        return (None, text.trim().to_string());
    };
    let front = &after[..end];
    let body = after[end + 4..].trim_start_matches('\n').trim().to_string();
    let mode = serde_yaml::from_str::<serde_yaml::Value>(front)
        .ok()
        .and_then(|v| v.get("mode").and_then(|m| m.as_str()).map(str::to_string));
    (mode, body)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    /// Serialize JOEY_HOME-touching tests. Taken in pairs: joey-core's
    /// cross-crate TEST_HOME_OVERRIDE_LOCK (so we can't race the
    /// llm_selector/neurocode tests in this binary that redirect home) and a
    /// module-local lock for our own tests — same pattern as
    /// `llm_selector::tests::ENV_LOCK`.
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    /// Acquire both home-mutation locks for the duration of a test body.
    fn home_guard() -> (std::sync::MutexGuard<'static, ()>, std::sync::MutexGuard<'static, ()>) {
        let core = joey_core::constants::TEST_HOME_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let local = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        (core, local)
    }

    fn write(rel: &str, content: &str, root: &Path) -> PathBuf {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
        fs::write(&path, content).expect("write");
        path
    }

    #[test]
    fn classify_source_all_arms() {
        // git URLs pass through as-is
        match classify_source("https://github.com/octo/demo.git") {
            SourceKind::Git(url) => {
                assert_eq!(url, "https://github.com/octo/demo.git");
            }
            other => panic!("expected Git, got {other:?}"),
        }
        assert!(matches!(
            classify_source("ssh://git@host/x/y"),
            SourceKind::Git(_)
        ));
        // owner/repo
        match classify_source("octo/demo") {
            SourceKind::OwnerRepo(owner, repo) => {
                assert_eq!(owner, "octo");
                assert_eq!(repo, "demo");
            }
            other => panic!("expected OwnerRepo, got {other:?}"),
        }
        // local existing path
        let tmp = tempfile::tempdir().expect("tempdir");
        match classify_source(tmp.path().to_str().unwrap()) {
            SourceKind::Local(p) => assert_eq!(p, tmp.path()),
            other => panic!("expected Local, got {other:?}"),
        }
        // unsupported: garbage, and multi-segment non-URL
        assert!(matches!(classify_source("not a source!!"), SourceKind::Unsupported));
        assert!(matches!(classify_source("a/b/c"), SourceKind::Unsupported));
    }

    #[test]
    fn sanitize_name_rules() {
        assert_eq!(sanitize_name("my-plugin_2.0"), Some("my-plugin_2.0".to_string()));
        assert_eq!(sanitize_name("A1.b-c"), Some("A1.b-c".to_string()));
        assert_eq!(sanitize_name(""), None);
        assert_eq!(sanitize_name("bad name"), None);
        assert_eq!(sanitize_name("a/b"), None);
        assert_eq!(sanitize_name("a:b"), None);
    }

    #[test]
    fn plugin_name_from_url_strips_git() {
        assert_eq!(plugin_name_from_url("https://github.com/octo/demo.git"), "demo");
        assert_eq!(plugin_name_from_url("https://github.com/octo/demo"), "demo");
        assert_eq!(plugin_name_from_url("https://github.com/octo/demo/"), "demo");
    }

    #[test]
    fn slash_response_lines_dispatch_shape() {
        let (_core, _local) = home_guard();
        let home = tempfile::tempdir().expect("home tempdir");
        let prev = std::env::var("JOEY_HOME").ok();
        std::env::set_var("JOEY_HOME", home.path());

        let result = (|| {
            let bare = slash_response_lines("", "").expect("bare -> status");
            assert!(bare.contains("Copilot integration:"), "bare: {bare}");
            assert!(bare.contains("Instructions:"));
            assert!(bare.contains("MCP servers"));

            let status = slash_response_lines("status", "").expect("status");
            assert!(status.contains("Copilot integration:"));

            let list = slash_response_lines("list", "").expect("list");
            assert!(list.contains("No copilot plugins installed"), "list: {list}");

            let install = slash_response_lines("install", "octo/demo").expect("install");
            assert!(install.contains("joey copilot install"));
            assert!(install.contains("octo/demo"));

            let remove = slash_response_lines("remove", "demo").expect("remove");
            assert!(remove.contains("joey copilot remove"));
            let update = slash_response_lines("update", "").expect("update");
            assert!(update.contains("joey copilot update"));

            assert!(slash_response_lines("bogus", "").is_none());
        })();

        match prev {
            Some(v) => std::env::set_var("JOEY_HOME", v),
            None => std::env::remove_var("JOEY_HOME"),
        }
        result
    }

    #[test]
    fn find_prompt_body_project_then_plugin() {
        let (_core, _local) = home_guard();
        let home = tempfile::tempdir().expect("home tempdir");
        let prev = std::env::var("JOEY_HOME").ok();
        std::env::set_var("JOEY_HOME", home.path());

        let result = (|| {
            // Project prompt.
            let proj = tempfile::tempdir().expect("proj tempdir");
            write(
                ".github/prompts/hello.prompt.md",
                "---\ndescription: greets\nmode: agent\n---\nHello body.",
                proj.path(),
            );
            // Fake installed plugin with its own prompt.
            let plugins = joey_copilot::plugins_dir();
            assert!(plugins.starts_with(home.path()));
            write(
                "fakepl/.github/prompts/bye.prompt.md",
                "---\nmode: ask\n---\nBye body.",
                &plugins,
            );

            // 1) project prompt wins: body + frontmatter mode.
            assert_eq!(
                find_prompt_body("hello", proj.path()),
                Some(("Hello body.".to_string(), Some("agent".to_string())))
            );
            // 2) plugin prompt found by walking the plugins dir.
            assert_eq!(
                find_prompt_body("bye", proj.path()),
                Some(("Bye body.".to_string(), Some("ask".to_string())))
            );
            // 3) missing prompt.
            assert_eq!(find_prompt_body("nope", proj.path()), None);
        })();

        match prev {
            Some(v) => std::env::set_var("JOEY_HOME", v),
            None => std::env::remove_var("JOEY_HOME"),
        }
        result
    }

    #[test]
    fn prompt_frontmatter_splitter() {
        let (mode, body) = split_prompt_frontmatter("---\nmode: chat\n---\nDo the thing.");
        assert_eq!(mode.as_deref(), Some("chat"));
        assert_eq!(body, "Do the thing.");

        let (mode, body) = split_prompt_frontmatter("No frontmatter here.");
        assert_eq!(mode, None);
        assert_eq!(body, "No frontmatter here.");

        let (mode, body) = split_prompt_frontmatter("---\nunterminated");
        assert_eq!(mode, None);
        assert!(body.contains("unterminated"));
    }

    #[test]
    fn copy_tree_preserves_relative_paths() {
        let src = tempfile::tempdir().expect("src");
        let dst_root = tempfile::tempdir().expect("dst");
        write("SKILL.md", "skill", src.path());
        write("references/note.md", "note", src.path());
        let dst = dst_root.path().join("out");
        copy_tree(src.path(), &dst).expect("copy_tree");
        assert!(dst.join("SKILL.md").is_file());
        assert!(dst.join("references/note.md").is_file());
    }
}
