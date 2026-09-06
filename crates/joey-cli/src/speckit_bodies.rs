//! Bundled spec-kit workflow bodies and resolution chain (spec FR-004/FR-004a).
//!
//! The ten upstream spec-kit workflow command bodies are vendored
//! byte-verbatim under `src/speckit_bodies/` (see that directory's
//! `PROVENANCE.md` for upstream commit and refresh procedure). Resolution
//! prefers project-level overrides (`.github/skills`, `.github/agents`,
//! `.specify/commands`) and user-installed skills over the bundled copies.

use joey_core::constants::joey_home;
use std::path::{Path, PathBuf};

/// The ten vendored spec-kit workflow command names, canonical order.
// Referenced only from `#[cfg(test)]` suites (bodies, hooks, native);
// kept as the canonical-order source of truth.
#[allow(dead_code)]
pub const COMMAND_NAMES: &[&str] = &[
    "specify",
    "clarify",
    "plan",
    "constitution",
    "checklist",
    "tasks",
    "analyze",
    "implement",
    "converge",
    "taskstoissues",
];

/// Return the bundled (vendored) body for a known command name, `None` for
/// unknown names.
pub fn bundled_body(name: &str) -> Option<&'static str> {
    Some(match name {
        "specify" => include_str!("speckit_bodies/specify.md"),
        "clarify" => include_str!("speckit_bodies/clarify.md"),
        "plan" => include_str!("speckit_bodies/plan.md"),
        "constitution" => include_str!("speckit_bodies/constitution.md"),
        "checklist" => include_str!("speckit_bodies/checklist.md"),
        "tasks" => include_str!("speckit_bodies/tasks.md"),
        "analyze" => include_str!("speckit_bodies/analyze.md"),
        "implement" => include_str!("speckit_bodies/implement.md"),
        "converge" => include_str!("speckit_bodies/converge.md"),
        "taskstoissues" => include_str!("speckit_bodies/taskstoissues.md"),
        _ => return None,
    })
}

/// A single handoff target declared in workflow frontmatter.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct HandoffDef {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub send: bool,
}

/// Script invocations declared in workflow frontmatter (per-shell variants).
// Frontmatter data model: `sh`/`ps`/`py` are deserialized for model
// completeness but not yet read by any execution path.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ScriptsDef {
    #[serde(default)]
    pub sh: Option<String>,
    #[serde(default)]
    pub ps: Option<String>,
    #[serde(default)]
    pub py: Option<String>,
}

/// Parsed YAML frontmatter of a workflow body. Every field defaults so
/// partial frontmatter still parses.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct BodyFrontmatter {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub handoffs: Vec<HandoffDef>,
    #[serde(default)]
    pub scripts: Option<ScriptsDef>,
    #[serde(default)]
    pub tools: Vec<String>,
}

/// Parse the `---`-delimited YAML frontmatter of a workflow body. Returns
/// the empty default when frontmatter is absent or fails to parse (never
/// panics).
pub fn parse_frontmatter(raw: &str) -> BodyFrontmatter {
    if let Some(slice) = frontmatter_slice(raw) {
        if let Ok(fm) = serde_yaml::from_str::<BodyFrontmatter>(slice) {
            return fm;
        }
    }
    BodyFrontmatter::default()
}

/// Return the frontmatter YAML slice (between the opening `---\n` and the
/// closing `---` line), or `None` when absent.
fn frontmatter_slice(raw: &str) -> Option<&str> {
    let rest = raw.strip_prefix("---\n")?;
    // Closing marker: a line that is exactly "---" (handled for both
    // "\n---\n" and a trailing "---" at end of input).
    let mut search_from = 0usize;
    loop {
        let idx = rest[search_from..].find("\n---")?;
        let after = idx + search_from + 4; // just past "\n---"
        let rest_after = &rest[after..];
        if rest_after.is_empty() || rest_after.starts_with('\n') || rest_after.starts_with("\r\n") {
            let end = idx + search_from + 1; // exclude the newline before ---
            return Some(&rest[..end]);
        }
        search_from = idx + search_from + 4;
    }
}

/// Return the body text after the closing `---` marker, or the whole input
/// when frontmatter is absent.
pub fn strip_frontmatter(raw: &str) -> &str {
    let rest = match raw.strip_prefix("---\n") {
        Some(r) => r,
        None => return raw,
    };
    let mut search_from = 0usize;
    loop {
        let idx = match rest[search_from..].find("\n---") {
            Some(i) => i + search_from,
            None => return raw,
        };
        let after = idx + 4; // just past "\n---"
        let rest_after = &rest[after..];
        if rest_after.is_empty() || rest_after.starts_with('\n') || rest_after.starts_with("\r\n") {
            let mut body_start = after;
            // Skip the newline that immediately follows the closing marker.
            if rest[body_start..].starts_with("\r\n") {
                body_start += 2;
            } else if rest[body_start..].starts_with('\n') {
                body_start += 1;
            }
            return &rest[body_start..];
        }
        search_from = idx + 4;
    }
}

/// Where a resolved workflow body came from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WorkflowBodySource {
    GithubSkills,
    GithubAgents,
    SpecifyDir,
    UserSkills,
    Bundled,
}

impl WorkflowBodySource {
    /// Human-facing label for the source.
    // Not yet surfaced in any output path; retained for upcoming
    // resolution-chain surfacing.
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            WorkflowBodySource::GithubSkills => "project .github/skills",
            WorkflowBodySource::GithubAgents => "project .github/agents",
            WorkflowBodySource::SpecifyDir => "project .specify/commands",
            WorkflowBodySource::UserSkills => "user skills",
            WorkflowBodySource::Bundled => "bundled",
        }
    }
}

/// A resolved workflow body: raw text, parsed frontmatter, source and (for
/// file-backed sources) the file it was read from.
#[derive(Debug, Clone)]
pub struct WorkflowBody {
    pub raw: String,
    pub frontmatter: BodyFrontmatter,
    pub source: WorkflowBodySource,
    // Populated by file-backed resolution layers but not yet read.
    #[allow(dead_code)]
    pub path: Option<PathBuf>,
}

impl WorkflowBody {
    /// The body text with frontmatter stripped.
    pub fn body(&self) -> &str {
        strip_frontmatter(&self.raw)
    }
}

/// Resolve a workflow body for `name` against the given repo root (any
/// override layer must live under `repo_root`), falling back to user skills
/// under `home` and finally the bundled copy.
pub(crate) fn resolve_with_home(
    repo_root: Option<&Path>,
    name: &str,
    home: &Path,
) -> WorkflowBody {
    // Candidate file sources in precedence order (steps 1-4).
    let file_candidates: Vec<(WorkflowBodySource, PathBuf)> = match repo_root {
        Some(root) => vec![
            (
                WorkflowBodySource::GithubSkills,
                root.join(format!(".github/skills/speckit-{name}/SKILL.md")),
            ),
            (
                WorkflowBodySource::GithubAgents,
                root.join(format!(".github/agents/speckit.{name}.agent.md")),
            ),
            (
                WorkflowBodySource::GithubAgents,
                root.join(format!(".github/prompts/speckit.{name}.prompt.md")),
            ),
            (
                WorkflowBodySource::SpecifyDir,
                root.join(format!(".specify/commands/speckit-{name}.md")),
            ),
            (
                WorkflowBodySource::UserSkills,
                home.join(format!("skills/speckit-{name}/SKILL.md")),
            ),
            (
                WorkflowBodySource::UserSkills,
                home.join(format!("optional-skills/speckit-{name}/SKILL.md")),
            ),
        ],
        None => vec![
            (
                WorkflowBodySource::UserSkills,
                home.join(format!("skills/speckit-{name}/SKILL.md")),
            ),
            (
                WorkflowBodySource::UserSkills,
                home.join(format!("optional-skills/speckit-{name}/SKILL.md")),
            ),
        ],
    };

    let mut resolved: Option<WorkflowBody> = None;
    for (source, path) in file_candidates {
        // Step 2 fallback: the prompt.md candidate only applies when the
        // agent.md counterpart is missing (it is listed right after it, so
        // reaching it here already means agent.md did not exist).
        if !path.is_file() {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(raw) => {
                resolved = Some(WorkflowBody {
                    frontmatter: parse_frontmatter(&raw),
                    raw,
                    source,
                    path: Some(path),
                });
                break;
            }
            Err(_) => continue, // unreadable: fall through to next candidate
        }
    }

    let mut body = resolved.unwrap_or_else(|| WorkflowBody {
        raw: bundled_body(name).unwrap_or_default().to_string(),
        frontmatter: BodyFrontmatter::default(),
        source: WorkflowBodySource::Bundled,
        path: None,
    });
    if body.source == WorkflowBodySource::Bundled {
        body.frontmatter = parse_frontmatter(&body.raw);
    }

    // METADATA FLOOR: any non-bundled override keeps its raw text verbatim,
    // but missing frontmatter fields (description/handoffs/scripts/tools)
    // are filled from the bundled body's frontmatter so an override that
    // omits handoffs still gets upstream handoff semantics. Raw text is
    // never modified.
    if body.source != WorkflowBodySource::Bundled {
        if let Some(bundled) = bundled_body(name) {
            let floor = parse_frontmatter(bundled);
            if body.frontmatter.description.is_none() {
                body.frontmatter.description = floor.description;
            }
            if body.frontmatter.handoffs.is_empty() {
                body.frontmatter.handoffs = floor.handoffs;
            }
            if body.frontmatter.scripts.is_none() {
                body.frontmatter.scripts = floor.scripts;
            }
            if body.frontmatter.tools.is_empty() {
                body.frontmatter.tools = floor.tools;
            }
        }
    }
    body
}

/// Resolve a workflow body for `name`: project overrides under
/// `repo_root` (when given) → user skills under the joey home → bundled.
pub fn resolve_body(repo_root: Option<&Path>, name: &str) -> WorkflowBody {
    resolve_with_home(repo_root, name, &joey_home())
}

/// Substitute workflow-body placeholders:
/// - every literal `{SCRIPT}` becomes `script_display` (pass `"-"` when
///   there is no script);
/// - every `__SPECKIT_COMMAND_<NAME>__` placeholder becomes
///   `/speckit-<name>` with the capture lowercased and underscores removed
///   (e.g. `__SPECKIT_COMMAND_SPECIFY__` → `/speckit-specify`;
///   `__SPECKIT_COMMAND_TASKS_TO_ISSUES__` → `/speckit-taskstoissues`).
pub fn substitute_placeholders(body: &str, script_display: &str) -> String {
    let replaced = body.replace("{SCRIPT}", script_display);
    let re = regex::Regex::new(r"__SPECKIT_COMMAND_([A-Z_]+)__").expect("valid placeholder regex");
    re.replace_all(&replaced, |caps: &regex::Captures| {
        format!("/speckit-{}", caps[1].to_lowercase().replace('_', ""))
    })
    .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn bundled_bodies_have_parseable_frontmatter() {
        for name in COMMAND_NAMES {
            let raw = bundled_body(name).expect("bundled body");
            assert!(raw.starts_with("---\n"), "{name} must start with frontmatter");
            let fm = parse_frontmatter(raw);
            assert!(fm.description.is_some(), "{name} description missing");
        }
    }

    #[test]
    fn bundled_frontmatter_shapes() {
        for name in ["specify", "clarify", "plan", "tasks", "constitution"] {
            let fm = parse_frontmatter(bundled_body(name).unwrap());
            assert!(
                !fm.handoffs.is_empty(),
                "{name} bundled frontmatter must expose handoffs"
            );
        }
        let fm = parse_frontmatter(bundled_body("taskstoissues").unwrap());
        assert_eq!(fm.tools.len(), 2, "taskstoissues tools: {:?}", fm.tools);
        let scripts = fm.scripts.expect("taskstoissues scripts block");
        assert!(scripts.sh.is_some());
        assert!(scripts.ps.is_some());
        assert!(scripts.py.is_some());
    }

    #[test]
    fn bundled_body_unknown_name_is_none() {
        assert!(bundled_body("nope").is_none());
    }

    #[test]
    fn resolution_precedence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let home = tempfile::tempdir().expect("home tempdir");

        let skills = root.join(".github/skills/speckit-plan/SKILL.md");
        fs::create_dir_all(skills.parent().unwrap()).unwrap();
        fs::write(&skills, "---\ndescription: A\n---\nA body").unwrap();
        let agents = root.join(".github/agents/speckit.plan.agent.md");
        fs::create_dir_all(agents.parent().unwrap()).unwrap();
        fs::write(&agents, "---\ndescription: B\n---\nB body").unwrap();
        let specify_cmd = root.join(".specify/commands/speckit-plan.md");
        fs::create_dir_all(specify_cmd.parent().unwrap()).unwrap();
        fs::write(&specify_cmd, "---\ndescription: C\n---\nC body").unwrap();

        let r = resolve_with_home(Some(root), "plan", home.path());
        assert_eq!(r.source, WorkflowBodySource::GithubSkills);
        assert!(r.body().contains("A body"));

        fs::remove_file(&skills).unwrap();
        let r = resolve_with_home(Some(root), "plan", home.path());
        assert_eq!(r.source, WorkflowBodySource::GithubAgents);
        assert!(r.body().contains("B body"));

        fs::remove_file(&agents).unwrap();
        let r = resolve_with_home(Some(root), "plan", home.path());
        assert_eq!(r.source, WorkflowBodySource::SpecifyDir);
        assert!(r.body().contains("C body"));

        fs::remove_file(&specify_cmd).unwrap();
        let r = resolve_with_home(Some(root), "plan", home.path());
        assert_eq!(r.source, WorkflowBodySource::Bundled);
        assert!(r.raw.starts_with("---\n"));
        assert!(r.path.is_none());
    }

    #[test]
    fn resolution_prompt_md_fallback_is_github_agents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let home = tempfile::tempdir().expect("home tempdir");
        let prompt = root.join(".github/prompts/speckit.plan.prompt.md");
        fs::create_dir_all(prompt.parent().unwrap()).unwrap();
        fs::write(&prompt, "---\ndescription: P\n---\nP body").unwrap();
        let r = resolve_with_home(Some(root), "plan", home.path());
        assert_eq!(r.source, WorkflowBodySource::GithubAgents);
        assert!(r.body().contains("P body"));
    }

    #[test]
    fn user_skills_hop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = tempfile::tempdir().expect("home tempdir");
        let skill = home.path().join("skills/speckit-plan/SKILL.md");
        fs::create_dir_all(skill.parent().unwrap()).unwrap();
        fs::write(&skill, "---\ndescription: U\n---\nU body").unwrap();
        let r = resolve_with_home(Some(dir.path()), "plan", home.path());
        assert_eq!(r.source, WorkflowBodySource::UserSkills);
        assert!(r.body().contains("U body"));

        // Optional-skills variant.
        fs::remove_file(&skill).unwrap();
        let opt = home.path().join("optional-skills/speckit-plan/SKILL.md");
        fs::create_dir_all(opt.parent().unwrap()).unwrap();
        fs::write(&opt, "---\ndescription: O\n---\nO body").unwrap();
        let r = resolve_with_home(Some(dir.path()), "plan", home.path());
        assert_eq!(r.source, WorkflowBodySource::UserSkills);
        assert!(r.body().contains("O body"));
    }

    #[test]
    fn metadata_floor_fills_missing_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = tempfile::tempdir().expect("home tempdir");
        let skills = dir.path().join(".github/skills/speckit-specify/SKILL.md");
        fs::create_dir_all(skills.parent().unwrap()).unwrap();
        // Override with ONLY a description — no handoffs.
        fs::write(&skills, "---\ndescription: custom\n---\nCustom body").unwrap();
        let r = resolve_with_home(Some(dir.path()), "specify", home.path());
        assert_eq!(r.source, WorkflowBodySource::GithubSkills);
        assert!(r.body().contains("Custom body"), "raw text stays override");
        assert_eq!(
            r.frontmatter.description.as_deref(),
            Some("custom"),
            "override keeps its own description"
        );
        assert!(
            !r.frontmatter.handoffs.is_empty(),
            "handoffs filled from bundled floor"
        );
        let floor = parse_frontmatter(bundled_body("specify").unwrap());
        assert_eq!(r.frontmatter.handoffs.len(), floor.handoffs.len());
    }

    #[test]
    fn substitute_placeholders_basic() {
        let out = substitute_placeholders(
            "run {SCRIPT} then __SPECKIT_COMMAND_SPECIFY__ / __SPECKIT_COMMAND_TASKS_TO_ISSUES__",
            ".specify/scripts/bash/check-prerequisites.sh --json",
        );
        assert!(out.contains(".specify/scripts/bash/check-prerequisites.sh --json"));
        assert!(out.contains("/speckit-specify"));
        assert!(out.contains("/speckit-taskstoissues"));
        assert!(!out.contains("{SCRIPT}"));
        assert!(!out.contains("__SPECKIT_COMMAND_"));
    }

    #[test]
    fn parse_frontmatter_garbage_returns_default() {
        let fm = parse_frontmatter("---\n:~this is ][ not yaml — well, it might be...\n[]{}\n---\nbody");
        // Must not panic; garbage yields empty default (no handoffs/tools).
        assert!(fm.handoffs.is_empty());
        assert!(fm.tools.is_empty());
    }

    #[test]
    fn strip_frontmatter_roundtrip() {
        for name in COMMAND_NAMES {
            let raw = bundled_body(name).unwrap();
            let body = strip_frontmatter(raw);
            // body is exactly the region after the closing marker: it is a
            // suffix of raw, and raw minus body is the frontmatter region.
            assert!(raw.ends_with(body), "{name} body must be a suffix of raw");
            let fm_region = &raw[..raw.len() - body.len()];
            assert!(fm_region.starts_with("---\n"));
            assert!(fm_region.contains("---"), "{name} closing marker");
            assert!(!body.starts_with("---\n"), "{name} body past marker");
            // strip is idempotent on the body itself.
            assert_eq!(strip_frontmatter(body), body);
        }
    }
}
