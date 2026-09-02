//! Parsers for GitHub Copilot's repo-level extension files under a project's
//! `.github/` directory:
//!
//! - `.github/copilot-instructions.md` — repo-wide custom instructions
//! - `.github/instructions/*.instructions.md` — path-scoped instruction files
//! - `.github/prompts/*.prompt.md` — reusable prompt ("slash command") files
//! - `.github/skills/*/SKILL.md` — agent skills (same SKILL.md format Joey
//!   already uses)
//! - `.github/mcp.json` — repo-provided MCP server configuration
//!
//! These parsers are total: missing or unreadable files simply yield empty
//! results, never errors, so callers can run `discover` unconditionally on
//! any project. File reads are capped at 64 KiB. This makes the Copilot
//! extension surface available to Joey natively (skills index, system prompt,
//! slash commands, MCP config merge).
//!
//! This crate is a Joey extension — it is not a port of upstream Hermes
//! functionality.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use walkdir::WalkDir;

/// Directory (relative to a project root) holding Copilot extension files.
pub const COPILOT_DIR: &str = ".github";

/// Maximum bytes read from any single Copilot extension file.
const MAX_READ_BYTES: u64 = 64 * 1024;

/// Maximum skill name length (chars), matching `joey-tools` skills_tool.rs.
const MAX_NAME_LENGTH: usize = 100;
/// Maximum skill description length (chars), matching skills_tool.rs.
const MAX_DESCRIPTION_LENGTH: usize = 500;

/// Skill-directory names that hold supporting files, not skills.
const SKIP_DIRS: [&str; 4] = ["references", "templates", "assets", "scripts"];

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// Where installed copilot plugins live (`~/.joey/copilot/plugins`).
pub fn plugins_dir() -> PathBuf {
    joey_core::constants::joey_home().join("copilot/plugins")
}

/// Plugin manifest path (`~/.joey/copilot/plugins.json`).
pub fn plugins_manifest_path() -> PathBuf {
    joey_core::constants::joey_home().join("copilot/plugins.json")
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A `.github/skills/<dir>/SKILL.md` skill.
#[derive(Debug, Clone)]
pub struct CopilotSkill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// A `.github/prompts/<name>.prompt.md` prompt file.
#[derive(Debug, Clone)]
pub struct CopilotPrompt {
    /// File stem, e.g. `"deploy"` from `deploy.prompt.md`.
    pub name: String,
    /// Frontmatter `description`, else the first non-heading body line, else "".
    pub description: String,
    /// Frontmatter `mode` (`chat`|`agent`|`ask`|`edit`), `None` if absent.
    pub mode: Option<String>,
    /// Markdown body after frontmatter.
    pub body: String,
    pub path: PathBuf,
}

/// A `.github/instructions/<name>.instructions.md` instruction file.
#[derive(Debug, Clone)]
pub struct InstructionFile {
    pub path: PathBuf,
    /// Frontmatter `applyTo` glob, `None` if absent.
    pub apply_to: Option<String>,
    pub body: String,
}

/// Everything found under a project's `.github/` Copilot extension surface.
#[derive(Debug, Clone, Default)]
pub struct CopilotBundle {
    /// `.github/copilot-instructions.md` content (raw; no frontmatter expected).
    pub instructions: Option<String>,
    /// `.github/instructions/*.instructions.md`, sorted by path.
    pub instruction_files: Vec<InstructionFile>,
    /// `.github/prompts/*.prompt.md`, sorted by name.
    pub prompts: Vec<CopilotPrompt>,
    /// `.github/skills/*/SKILL.md`, sorted by name.
    pub skills: Vec<CopilotSkill>,
    /// The `servers` object from `.github/mcp.json` (raw JSON value);
    /// `None` when the file is missing, invalid, or has no `servers` object.
    pub mcp_servers: Option<Value>,
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// Run all Copilot-extension parsers for the project rooted at `cwd`.
pub fn discover(cwd: &Path) -> CopilotBundle {
    CopilotBundle {
        instructions: parse_instructions(cwd),
        instruction_files: parse_instruction_files(cwd),
        prompts: parse_prompts(cwd),
        skills: parse_skills(cwd),
        mcp_servers: parse_mcp_servers(cwd),
    }
}

/// Read `<cwd>/.github/copilot-instructions.md` (raw content, no frontmatter
/// expected). `None` if missing or unreadable.
pub fn parse_instructions(cwd: &Path) -> Option<String> {
    let path = cwd.join(COPILOT_DIR).join("copilot-instructions.md");
    read_capped(&path)
}

/// Parse `.github/instructions/*.instructions.md` (non-recursive), sorted by
/// path. YAML frontmatter may carry `applyTo: <glob string>`; the body is
/// everything after the frontmatter.
pub fn parse_instruction_files(cwd: &Path) -> Vec<InstructionFile> {
    let dir = cwd.join(COPILOT_DIR).join("instructions");
    let mut out = Vec::new();
    if !dir.is_dir() {
        return out;
    }
    for entry in WalkDir::new(&dir).max_depth(1).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        if !entry.file_name().to_string_lossy().ends_with(".instructions.md") {
            continue;
        }
        let Some(content) = read_capped(entry.path()) else {
            continue;
        };
        let (frontmatter, body) = split_frontmatter(&content);
        let apply_to = frontmatter
            .get("applyTo")
            .and_then(Value::as_str)
            .map(str::to_string);
        out.push(InstructionFile {
            path: entry.path().to_path_buf(),
            apply_to,
            body,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Parse `.github/prompts/*.prompt.md` (non-recursive), sorted by name.
pub fn parse_prompts(cwd: &Path) -> Vec<CopilotPrompt> {
    let dir = cwd.join(COPILOT_DIR).join("prompts");
    let mut out = Vec::new();
    if !dir.is_dir() {
        return out;
    }
    for entry in WalkDir::new(&dir).max_depth(1).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if !file_name.ends_with(".prompt.md") {
            continue;
        }
        let Some(content) = read_capped(entry.path()) else {
            continue;
        };
        let (frontmatter, body) = split_frontmatter(&content);
        let body = body.trim().to_string();
        let description = match frontmatter.get("description").and_then(Value::as_str) {
            Some(d) => d.to_string(),
            None => first_body_line(&body).unwrap_or_default(),
        };
        let mode = frontmatter
            .get("mode")
            .and_then(Value::as_str)
            .map(str::to_string);
        out.push(CopilotPrompt {
            name: file_name.trim_end_matches(".prompt.md").to_string(),
            description,
            mode,
            body,
            path: entry.path().to_path_buf(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Parse `.github/skills/*/SKILL.md` (depth-2 scan), sorted by name. Skill
/// directories named `references`, `templates`, `assets`, or `scripts` are
/// skipped.
pub fn parse_skills(cwd: &Path) -> Vec<CopilotSkill> {
    let dir = cwd.join(COPILOT_DIR).join("skills");
    let mut out = Vec::new();
    if !dir.is_dir() {
        return out;
    }
    for entry in WalkDir::new(&dir).max_depth(2).into_iter().flatten() {
        if !entry.file_type().is_file() || entry.file_name() != "SKILL.md" {
            continue;
        }
        let skill_md = entry.path().to_path_buf();
        let Some(dir_name) = skill_md
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
        else {
            continue;
        };
        if SKIP_DIRS.contains(&dir_name.as_str()) {
            continue;
        }
        let Some(content) = read_capped(&skill_md) else {
            continue;
        };
        let (frontmatter, body) = split_frontmatter(&content);
        let name: String = frontmatter
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or(dir_name)
            .chars()
            .take(MAX_NAME_LENGTH)
            .collect();
        let mut description = frontmatter
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_default();
        if description.is_empty() {
            description = first_body_line(&body).unwrap_or_default();
        }
        if description.chars().count() > MAX_DESCRIPTION_LENGTH {
            let kept: String = description.chars().take(MAX_DESCRIPTION_LENGTH - 3).collect();
            description = format!("{}...", kept);
        }
        out.push(CopilotSkill {
            name,
            description,
            path: skill_md,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Parse `.github/mcp.json`, returning its `servers` object if (and only if)
/// it is a JSON object containing a `servers` key whose value is an object.
/// `None` on missing file, invalid JSON, or absent/non-object `servers`.
pub fn parse_mcp_servers(cwd: &Path) -> Option<Value> {
    let path = cwd.join(COPILOT_DIR).join("mcp.json");
    let content = read_capped(&path)?;
    let value: Value = serde_json::from_str(&content).ok()?;
    let servers = value.get("servers")?;
    if servers.is_object() {
        Some(servers.clone())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Plugin manifest
// ---------------------------------------------------------------------------

/// One installed Copilot plugin (a cloned/copied repo under `plugins_dir()`).
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct PluginRecord {
    /// Plugin slug (repo directory name).
    pub name: String,
    /// Original git URL / owner-repo / local path it was installed from.
    pub source: String,
    /// Install timestamp, RFC3339 UTC (`YYYY-MM-DDTHH:MM:SSZ`).
    pub installed_at: String,
    /// `git rev-parse HEAD` at install time, if installed from git.
    pub commit: Option<String>,
    /// Installed skill names.
    pub skills: Vec<String>,
    /// Prompt names found in the plugin tree.
    pub prompts: Vec<String>,
}

/// The on-disk plugin manifest (`~/.joey/copilot/plugins.json`).
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
pub struct PluginManifest {
    pub plugins: Vec<PluginRecord>,
}

/// Load the plugin manifest; a missing or corrupt file yields an empty
/// manifest (never an error).
pub fn load_manifest() -> PluginManifest {
    let path = plugins_manifest_path();
    let Ok(content) = std::fs::read_to_string(&path) else {
        return PluginManifest::default();
    };
    serde_json::from_str(&content).unwrap_or_default()
}

/// Save the plugin manifest, creating parent directories as needed.
pub fn save_manifest(m: &PluginManifest) -> std::io::Result<()> {
    let path = plugins_manifest_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(m).unwrap_or_default())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Read up to [`MAX_READ_BYTES`] from `path` as UTF-8 (lossy). `None` if the
/// file cannot be read.
fn read_capped(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    let mut limited = (&mut file).take(MAX_READ_BYTES);
    limited.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Split YAML frontmatter from a markdown document. Same semantics as
/// `joey-tools/src/tools/skills_tool.rs::parse_frontmatter`: text must start
/// (after leading whitespace) with `---`; the frontmatter ends at the next
/// `\n---`; the body follows. Unterminated frontmatter means no frontmatter.
fn split_frontmatter(text: &str) -> (Map<String, Value>, String) {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("---") {
        return (Map::new(), text.to_string());
    }
    let after = &trimmed[3..];
    let Some(end) = after.find("\n---") else {
        return (Map::new(), text.to_string());
    };
    let front = &after[..end];
    let body = after[end + 4..].trim_start_matches('\n').to_string();
    let yaml: Value = serde_yaml::from_str::<Value>(front).unwrap_or(Value::Null);
    let map = yaml.as_object().cloned().unwrap_or_default();
    (map, body)
}

/// First non-empty, non-`#` line of a markdown body, if any.
fn first_body_line(body: &str) -> Option<String> {
    body.trim().split('\n').find_map(|line| {
        let line = line.trim();
        if !line.is_empty() && !line.starts_with('#') {
            Some(line.to_string())
        } else {
            None
        }
    })
}

/// Current UTC time as RFC3339 (`YYYY-MM-DDTHH:MM:SSZ`), computed from
/// `SystemTime` without a datetime crate, via Howard Hinnant's
/// `civil_from_days` algorithm.
// Private for now: consumed by the upcoming plugin-install CLI layer and the
// tests below.
#[allow(dead_code)]
fn rfc3339_utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 → (y, m, d).
/// <https://howardhinnant.github.io/date_algorithms.html#civil_from_days>
#[allow(dead_code)]
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// Serialize JOEY_HOME-touching tests (env vars are process-global).
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    fn temp_project(tag: &str) -> (PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join(format!("proj-{tag}"));
        fs::create_dir_all(&root).expect("create root");
        (root, dir)
    }

    fn write(rel: &str, content: &str, root: &Path) -> PathBuf {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
        fs::write(&path, content).expect("write");
        path
    }

    // 1. empty dir
    #[test]
    fn discover_empty_dir() {
        let (root, _t) = temp_project("empty");
        let b = discover(&root);
        assert!(b.instructions.is_none());
        assert!(b.instruction_files.is_empty());
        assert!(b.prompts.is_empty());
        assert!(b.skills.is_empty());
        assert!(b.mcp_servers.is_none());
    }

    // 2. copilot-instructions.md verbatim
    #[test]
    fn instructions_verbatim() {
        let (root, _t) = temp_project("ci");
        let content = "# Repo instructions\n\nAlways use tabs.\nNever commit binaries.\n";
        write(".github/copilot-instructions.md", content, &root);
        assert_eq!(parse_instructions(&root).as_deref(), Some(content));
        assert_eq!(discover(&root).instructions.as_deref(), Some(content));
    }

    // 3. instruction files with/without applyTo
    #[test]
    fn instruction_files_apply_to() {
        let (root, _t) = temp_project("if");
        write(
            ".github/instructions/ts.instructions.md",
            "---\napplyTo: \"**/*.ts\"\n---\nUse strict mode.",
            &root,
        );
        write(".github/instructions/plain.instructions.md", "Be terse.", &root);
        let files = parse_instruction_files(&root);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, root.join(".github/instructions/plain.instructions.md"));
        assert!(files[0].apply_to.is_none());
        assert_eq!(files[0].body, "Be terse.");
        assert_eq!(files[1].apply_to.as_deref(), Some("**/*.ts"));
        assert_eq!(files[1].body, "Use strict mode.");
    }

    // 4. prompts, sorted, fallback description
    #[test]
    fn prompts_sorted_and_frontmatter() {
        let (root, _t) = temp_project("pr");
        write(
            ".github/prompts/deploy.prompt.md",
            "---\ndescription: Deploys the app\nmode: agent\n---\nShip it now.",
            &root,
        );
        write(".github/prompts/plain.prompt.md", "First body line.\nMore.", &root);
        let prompts = parse_prompts(&root);
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts[0].name, "deploy");
        assert_eq!(prompts[0].description, "Deploys the app");
        assert_eq!(prompts[0].mode.as_deref(), Some("agent"));
        assert_eq!(prompts[0].body, "Ship it now.");
        assert_eq!(prompts[1].name, "plain");
        assert_eq!(prompts[1].description, "First body line.");
        assert_eq!(prompts[1].mode, None);
        assert_eq!(prompts[1].body, "First body line.\nMore.");
        let bundle = discover(&root);
        let names: Vec<&str> = bundle.prompts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["deploy", "plain"]);
    }

    // 5. skills: frontmatter, dir-name fallback, scripts/ skip
    #[test]
    fn skills_names_and_skip_dirs() {
        let (root, _t) = temp_project("sk");
        write(
            ".github/skills/pdf/SKILL.md",
            "---\nname: pdf-tools\ndescription: Work with PDFs\n---\n# PDF",
            &root,
        );
        write(".github/skills/legacy/SKILL.md", "Legacy skill body line.", &root);
        write(".github/skills/scripts/SKILL.md", "should be skipped", &root);
        // nested subdir inside pdf/ must not become a skill (max_depth 2 + no SKILL.md)
        write(".github/skills/pdf/references/note.md", "x", &root);
        let skills = parse_skills(&root);
        assert_eq!(skills.len(), 2);
        assert_eq!(skills[0].name, "legacy");
        assert_eq!(skills[0].description, "Legacy skill body line.");
        assert_eq!(skills[1].name, "pdf-tools");
        assert_eq!(skills[1].description, "Work with PDFs");
        assert_eq!(skills[1].path, root.join(".github/skills/pdf/SKILL.md"));
    }

    // 6. description truncation > 500 chars
    #[test]
    fn skill_description_truncated() {
        let (root, _t) = temp_project("tr");
        let long = "x".repeat(600);
        write(
            ".github/skills/big/SKILL.md",
            &format!("---\ndescription: {long}\n---\nbody"),
            &root,
        );
        let skills = parse_skills(&root);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description.chars().count(), 500);
        assert!(skills[0].description.ends_with("..."));
        assert_eq!(skills[0].description.chars().take(497).collect::<String>(), "x".repeat(497));
    }

    // 7. mcp.json: valid, invalid, missing servers key
    #[test]
    fn mcp_servers_parsing() {
        let (root, _t) = temp_project("mcp");
        assert_eq!(parse_mcp_servers(&root), None);
        write(
            ".github/mcp.json",
            r#"{"servers": {"fetch": {"command": "uvx", "args": ["mcp-server-fetch"]}, "remote": {"type": "http", "url": "https://example.com/mcp"}}}"#,
            &root,
        );
        let servers = parse_mcp_servers(&root).expect("servers");
        let obj = servers.as_object().expect("object");
        assert_eq!(obj.len(), 2);
        assert_eq!(obj["fetch"]["command"], "uvx");
        assert_eq!(obj["fetch"]["args"][0], "mcp-server-fetch");
        assert_eq!(obj["remote"]["type"], "http");
        assert_eq!(obj["remote"]["url"], "https://example.com/mcp");

        write(".github/mcp.json", "{not json", &root);
        assert_eq!(parse_mcp_servers(&root), None);

        write(".github/mcp.json", r#"{"other": 1}"#, &root);
        assert_eq!(parse_mcp_servers(&root), None);
    }

    // 8. frontmatter splitter edge cases + name >100 chars
    #[test]
    fn frontmatter_edge_cases() {
        let (no_fm, body) = split_frontmatter("just text\nno frontmatter\n");
        assert!(no_fm.is_empty());
        assert_eq!(body, "just text\nno frontmatter\n");

        let (unterminated, body) = split_frontmatter("---\nname: x\nno end marker");
        assert!(unterminated.is_empty());
        assert_eq!(body, "---\nname: x\nno end marker");

        // name >100 chars truncated to 100
        let (root, _t) = temp_project("nm");
        let long_name = "n".repeat(150);
        write(
            ".github/skills/longname/SKILL.md",
            &format!("---\nname: {long_name}\ndescription: d\n---\nbody"),
            &root,
        );
        let skills = parse_skills(&root);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name.chars().count(), 100);
        assert_eq!(skills[0].name, "n".repeat(100));
    }

    // 9. rfc3339_utc_now shape
    #[test]
    fn rfc3339_shape() {
        let s = rfc3339_utc_now();
        fn digit(c: Option<&char>) -> bool {
            c.map(|c| c.is_ascii_digit()).unwrap_or(false)
        }
        let chars: Vec<char> = s.chars().collect();
        assert_eq!(chars.len(), 20, "got {s}");
        let expect_digits = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18];
        for &i in &expect_digits {
            assert!(digit(chars.get(i)), "pos {i} not a digit in {s}");
        }
        assert_eq!(chars[4], '-');
        assert_eq!(chars[7], '-');
        assert_eq!(chars[10], 'T');
        assert_eq!(chars[13], ':');
        assert_eq!(chars[16], ':');
        assert_eq!(chars[19], 'Z');
        // sanity: month 01-12, day 01-31, hour<24, min<60, sec<60
        let month: u32 = s[5..7].parse().unwrap();
        let day: u32 = s[8..10].parse().unwrap();
        let hour: u32 = s[11..13].parse().unwrap();
        let min: u32 = s[14..16].parse().unwrap();
        let sec: u32 = s[17..19].parse().unwrap();
        assert!((1..=12).contains(&month));
        assert!((1..=31).contains(&day));
        assert!(hour < 24 && min < 60 && sec < 60);
    }

    // 9b. civil_from_days known dates
    #[test]
    fn civil_from_days_known_values() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1)); // 2024-01-01
        assert_eq!(civil_from_days(20_696), (2026, 8, 31)); // 2026-08-31
        assert_eq!(civil_from_days(20_697), (2026, 9, 1)); // 2026-09-01
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    // 10. manifest round-trip under a JOEY_HOME override
    #[test]
    fn manifest_round_trip() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let prev = std::env::var("JOEY_HOME").ok();
        std::env::set_var("JOEY_HOME", home.path());

        let record = PluginRecord {
            name: "demo-plugin".to_string(),
            source: "https://github.com/octo/demo-plugin".to_string(),
            installed_at: rfc3339_utc_now(),
            commit: Some("abc123def456".to_string()),
            skills: vec!["pdf-tools".to_string()],
            prompts: vec!["deploy".to_string()],
        };
        let manifest = PluginManifest { plugins: vec![record] };

        let result = (|| {
            assert!(plugins_dir().starts_with(home.path()));
            assert!(plugins_manifest_path().starts_with(home.path()));

            save_manifest(&manifest).expect("save");
            let path = plugins_manifest_path();
            assert!(path.is_file(), "{}", path.display());
            let loaded = load_manifest();
            assert_eq!(loaded.plugins.len(), 1);
            assert_eq!(loaded.plugins[0].name, "demo-plugin");
            assert_eq!(loaded.plugins[0].source, "https://github.com/octo/demo-plugin");
            assert_eq!(loaded.plugins[0].commit.as_deref(), Some("abc123def456"));
            assert_eq!(loaded.plugins[0].skills, ["pdf-tools".to_string()]);
            assert_eq!(loaded.plugins[0].prompts, ["deploy".to_string()]);
        })();

        // Restore env even if assertions fired.
        match prev {
            Some(v) => std::env::set_var("JOEY_HOME", v),
            None => std::env::remove_var("JOEY_HOME"),
        }
        result
    }

    // 10b. load_manifest on corrupt file => empty
    #[test]
    fn manifest_corrupt_loads_empty() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().expect("home tempdir");
        let prev = std::env::var("JOEY_HOME").ok();
        std::env::set_var("JOEY_HOME", home.path());

        let result = (|| {
            let path = plugins_manifest_path();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "{corrupt json").unwrap();
            assert!(load_manifest().plugins.is_empty());
            // missing file
            fs::remove_file(&path).unwrap();
            assert!(load_manifest().plugins.is_empty());
        })();

        match prev {
            Some(v) => std::env::set_var("JOEY_HOME", v),
            None => std::env::remove_var("JOEY_HOME"),
        }
        result
    }
}
