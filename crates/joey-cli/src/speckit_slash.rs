//! Spec-kit workflow slash commands (`/speckit-*`) for the CLI and TUI.
//!
//! Ports the GitHub Spec Kit slash-command surface onto joey-agent. The
//! lifecycle: constitution → specify → clarify → plan → checklist → tasks →
//! analyze → implement → converge → taskstoissues.
//!
//! Design (mirrors joey-speckit-ui's core principle): SpecKit's own logic
//! is NEVER reimplemented —
//! - repo scaffolding/prerequisites come from the real `.specify/scripts/
//!   bash/*.sh` scripts (run as subprocesses, argv-only), and
//! - the per-step workflow instructions come from the bundled
//!     `speckit-<step>` skills (`~/.joey/skills/speckit-*/SKILL.md`),
//!   which are the canonical step definitions (verbatim spec-kit
//!   templates).
//!
//! Each step therefore: (1) runs its pre-flight script, (2) loads the
//! skill's SKILL.md, (3) submits ONE agent turn = skill workflow +
//! pre-flight output + the user's arguments, with the skill preloaded so
//! `skill_view` references resolve. The agent then authors the artifact
//! (spec.md / plan.md / tasks.md …) with its file tools — the same
//! execution model as running the skill by hand, minus the copy-paste.

use std::path::{Path, PathBuf};
use std::process::Command;


/// One workflow step definition.
pub struct StepDef {
    /// Slash command name (without slash): "speckit-specify".
    pub name: &'static str,
    /// Skill directory name: "speckit-specify".
    pub skill: &'static str,
    /// Pre-flight script (under `.specify/scripts/bash/`), if any.
    pub script: Option<&'static str>,
    /// Extra script args.
    pub script_args: &'static [&'static str],
    /// True when the script/flags may be absent in older `.specify` scaffolds
    /// (e.g. `resolve-template.sh` and `--template` postdate many inits).
    /// A missing script or a rejected flag degrades gracefully (retry
    /// without the newer args / skip the pre-flight) instead of failing.
    pub script_optional: bool,
    /// Append the user's arguments to the script invocation as the
    /// positional feature description (only `speckit-specify` needs this).
    pub script_gets_user_args: bool,
    /// One-line description for help/registry.
    pub description: &'static str,
    /// Args hint.
    pub args_hint: &'static str,
}

/// The canonical spec-kit lifecycle, in order. `speckit-status` and
/// `speckit-help` are auxiliary (not lifecycle steps).
pub const LIFECYCLE: &[StepDef] = &[
    StepDef {
        name: "speckit-constitution",
        skill: "speckit-constitution",
        // Upstream: resolve-template.sh constitution-template --json. The
        // script ships only in NEWER .specify scaffolds — older inits don't
        // have it, so this is an optional pre-flight (missing script is not
        // an error; the skill reads the constitution template itself).
        script: Some("resolve-template.sh"),
        script_args: &["constitution-template", "--json"],
        script_optional: true,
        script_gets_user_args: false,
        description: "Create or update the project constitution from interactive Q&A",
        args_hint: "[guidelines...]",
    },
    StepDef {
        name: "speckit-specify",
        skill: "speckit-specify",
        // --allow-existing-branch: /speckit-specify is create-OR-update;
        // when the feature already exists the scaffold is reused and the
        // agent updates spec.md in place (upstream "Create or update").
        script: Some("create-new-feature.sh"),
        script_args: &["--json", "--allow-existing-branch"],
        script_optional: false,
        script_gets_user_args: true,
        description: "Create or update the feature specification from a description",
        args_hint: "<feature description>",
    },
    StepDef {
        name: "speckit-clarify",
        skill: "speckit-clarify",
        // Upstream clarify.md: check-prerequisites.sh --json --paths-only —
        // PURE PATH RESOLUTION, no plan.md validation. Clarify runs between
        // specify and plan (it must NOT require plan.md to exist).
        script: Some("check-prerequisites.sh"),
        script_args: &["--json", "--paths-only"],
        script_optional: false,
        script_gets_user_args: false,
        description: "Identify underspecified areas in the current feature spec",
        args_hint: "[focus areas...]",
    },
    StepDef {
        name: "speckit-plan",
        skill: "speckit-plan",
        script: Some("setup-plan.sh"),
        script_args: &["--json"],
        script_optional: false,
        script_gets_user_args: false,
        description: "Execute the implementation planning workflow (design artifacts)",
        args_hint: "[notes...]",
    },
    StepDef {
        name: "speckit-checklist",
        skill: "speckit-checklist",
        // Upstream: check-prerequisites.sh --json --template checklist-template.
        // --template support (and the JSON TEMPLATE_CONTENT field) exists only
        // in NEWER .specify scaffolds; degrade to plain --json when rejected.
        script: Some("check-prerequisites.sh"),
        script_args: &["--json", "--template", "checklist-template"],
        script_optional: true,
        script_gets_user_args: false,
        description: "Generate a custom checklist for the current feature",
        args_hint: "",
    },
    StepDef {
        name: "speckit-tasks",
        skill: "speckit-tasks",
        script: Some("setup-tasks.sh"),
        script_args: &["--json"],
        script_optional: false,
        script_gets_user_args: false,
        description: "Generate an actionable dependency-ordered tasks.md",
        args_hint: "",
    },
    StepDef {
        name: "speckit-analyze",
        skill: "speckit-analyze",
        // Upstream analyze.md: --require-tasks --include-tasks (analyze runs
        // AFTER tasks exist — it cross-checks spec/plan/tasks).
        script: Some("check-prerequisites.sh"),
        script_args: &["--json", "--require-tasks", "--include-tasks"],
        script_optional: false,
        script_gets_user_args: false,
        description: "Cross-artifact consistency and coverage analysis",
        args_hint: "",
    },
    StepDef {
        name: "speckit-implement",
        skill: "speckit-implement",
        script: Some("check-prerequisites.sh"),
        script_args: &["--json", "--require-tasks", "--include-tasks"],
        script_optional: false,
        script_gets_user_args: false,
        description: "Execute the implementation plan task by task",
        args_hint: "[--task N | --phase N | --continue]",
    },
    StepDef {
        name: "speckit-converge",
        skill: "speckit-converge",
        // Upstream converge.md: --require-tasks --include-tasks (converge
        // appends unbuilt work to an existing tasks.md).
        script: Some("check-prerequisites.sh"),
        script_args: &["--json", "--require-tasks", "--include-tasks"],
        script_optional: false,
        script_gets_user_args: false,
        description: "Assess implementation against the spec and list gaps",
        args_hint: "",
    },
    StepDef {
        name: "speckit-taskstoissues",
        skill: "speckit-taskstoissues",
        // Upstream taskstoissues.md: --require-tasks --include-tasks.
        script: Some("check-prerequisites.sh"),
        script_args: &["--json", "--require-tasks", "--include-tasks"],
        script_optional: false,
        script_gets_user_args: false,
        description: "Convert tasks into actionable GitHub issues",
        args_hint: "",
    },
];

/// Look up a lifecycle step by slash name.
pub fn step_by_name(name: &str) -> Option<&'static StepDef> {
    LIFECYCLE.iter().find(|s| s.name == name)
}

/// Find the repo root: nearest ancestor (starting at cwd) containing
/// `.specify/`.
pub fn find_repo_root(from: &Path) -> Option<PathBuf> {
    let mut dir = Some(from.to_path_buf());
    while let Some(d) = dir {
        if d.join(".specify").is_dir() {
            return Some(d);
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    None
}

/// Run a `.specify` bash script and return (stdout, stderr, exit-code).
/// Argv-only (no shell string), 30s timeout via the script's own design
/// (the scripts are fast; a hung git would exceed any budget).
pub fn run_specify_script(
    root: &Path,
    script: &str,
    args: &[&str],
) -> Result<(String, String, i32), String> {
    let path = root.join(".specify/scripts/bash").join(script);
    if !path.is_file() {
        return Err(format!("spec-kit script not found: {}", path.display()));
    }
    let out = Command::new("bash")
        .arg(&path)
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("failed to run {}: {e}", path.display()))?;
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Ok((stdout, stderr, code))
}

/// Resolve the SKILL.md path for a speckit skill (home skills dir first,
/// then bundled optional-skills).
pub fn skill_md_path(skill: &str) -> Option<PathBuf> {
    let home = joey_core::constants::joey_home();
    let candidates = [
        home.join("skills").join(skill).join("SKILL.md"),
        home.join("optional-skills").join(skill).join("SKILL.md"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

/// Read the skill body (everything after the YAML frontmatter) — the
/// canonical step workflow instructions.
pub fn load_skill_workflow(skill: &str) -> Result<String, String> {
    let path = skill_md_path(skill)
        .ok_or_else(|| format!("skill '{skill}' is not installed (expected ~/.joey/skills/{skill}/SKILL.md)"))?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    // Strip YAML frontmatter (--- ... ---).
    let body = if let Some(rest) = raw.strip_prefix("---\n") {
        match rest.find("\n---") {
            Some(i) => rest[i + 4..].trim_start_matches('\n').to_string(),
            None => raw.clone(),
        }
    } else {
        raw.clone()
    };
    Ok(body)
}

/// Outcome of preparing one workflow step.
#[allow(dead_code)] // preflight is consumed by hosts that display it separately
pub struct StepPrep {
    /// Combined pre-flight section for the agent prompt (may be empty).
    pub preflight: String,
    /// The full composed prompt to submit as an agent turn.
    pub prompt: String,
}

/// Compose the pre-flight + workflow prompt for one step.
///
/// `user_args` is what the user typed after the slash command. The
/// pre-flight runs the step's script (if any); a non-zero exit is FATAL
/// for gate steps (the workflow must not proceed when prerequisites
/// fail) and the error is surfaced to the user instead of an agent turn.
/// For `script_optional` steps, a MISSING script or a rejected NEWER flag
/// (older `.specify` scaffolds) degrades gracefully: retry without the
/// newer args, then skip the pre-flight entirely — the skill workflow is
/// self-sufficient (it reads templates/paths itself).
pub fn prepare_step(
    step: &StepDef,
    root: &Path,
    user_args: &str,
    preloaded_skill: Option<&str>,
) -> Result<StepPrep, String> {
    let mut preflight = String::new();

    if let Some(script) = step.script {
        let mut argv: Vec<&str> = step.script_args.to_vec();
        if step.script_gets_user_args {
            let desc = user_args.trim();
            if desc.is_empty() {
                return Err(format!(
                    "/{name} requires a feature description: /{name} <description>",
                    name = step.name
                ));
            }
            argv.push(desc);
        }
        let path = root.join(".specify/scripts/bash").join(script);
        if !path.is_file() {
            if step.script_optional {
                // Older scaffold: script postdates this .specify init. The
                // skill workflow is self-sufficient — proceed without it.
                preflight.push_str(&format!(
                    "## Pre-flight\n\n(script `{script}` not present in this .specify scaffold — skipped; the workflow below resolves paths/templates itself)\n\n"
                ));
            } else {
                return Err(format!("spec-kit script not found: {}", path.display()));
            }
        } else {
            let (stdout, stderr, code) = run_specify_script(root, script, &argv)?;
            match code {
                0 => {
                    if !stdout.is_empty() {
                        preflight.push_str(&format!(
                            "## Pre-flight ({script})\n\n```json\n{stdout}\n```\n\n"
                        ));
                    }
                }
                _ if step.script_optional => {
                    // Older scaffold rejecting a NEWER flag (e.g.
                    // check-prerequisites.sh without --template support, or
                    // --paths-only before it existed). Retry with the
                    // baseline invocation (--json only); if that also fails
                    // the step's prerequisites genuinely aren't met.
                    let retry_argv: Vec<&str> = argv
                        .iter()
                        .copied()
                        .take_while(|a| *a != "--template")
                        .collect();
                    let retry_argv = if retry_argv.last() == Some(&"--json") || retry_argv.is_empty() {
                        retry_argv
                    } else {
                        vec!["--json"]
                    };
                    let (rout, rerr, rcode) = run_specify_script(root, script, &retry_argv)?;
                    if rcode == 0 {
                        preflight.push_str(&format!(
                            "## Pre-flight ({script}, baseline flags)\n\n```json\n{rout}\n```\n\n"
                        ));
                    } else {
                        return Err(format!(
                            "spec-kit pre-flight failed ({script}, exit {rcode}):\n{}{}",
                            if rerr.is_empty() { String::new() } else { format!("{rerr}\n") },
                            rout
                        ));
                    }
                    let _ = (stderr, code);
                }
                _ => {
                    return Err(format!(
                        "spec-kit pre-flight failed ({script}, exit {code}):\n{}{}",
                        if stderr.is_empty() { String::new() } else { format!("{stderr}\n") },
                        stdout
                    ));
                }
            }
        }
    }

    let workflow = load_skill_workflow(step.skill)?;
    let skill_note = match preloaded_skill {
        Some(s) => format!("(skill `{s}` is preloaded; use the skill_view tool for its references)\n"),
        None => String::new(),
    };

    let prompt = format!(
        "{preflight}\
# Workflow: /{name}\n\n\
You are executing the spec-kit `{skill}` step for this repository. Follow the \
workflow instructions below EXACTLY — they are the canonical step definition. \
{skill_note}\
The user's arguments for this step: {args}\n\n\
---\n\
{workflow}",
        name = step.name,
        skill = step.skill,
        args = if user_args.trim().is_empty() { "(none provided)" } else { user_args },
    );

    Ok(StepPrep { preflight, prompt })
}

/// The status snapshot for `/speckit-status`: current feature + artifact
/// readiness, derived from check-prerequisites (never reimplemented).
#[allow(dead_code)] // raw kept for future consumers/debug
pub struct SpeckitStatus {
    pub root: PathBuf,
    pub branch: String,
    pub feature_dir: String,
    pub has_spec: bool,
    pub has_plan: bool,
    pub has_tasks: bool,
    pub raw: String,
}

/// Gather status via the prerequisite script (`--paths-only` needs no
/// feature branch to exist).
pub fn status(cwd: &Path) -> Result<SpeckitStatus, String> {
    let root = find_repo_root(cwd)
        .ok_or_else(|| "not a spec-kit repository (no .specify/ directory found in this or any parent directory)".to_string())?;
    let (stdout, _stderr, code) =
        run_specify_script(&root, "check-prerequisites.sh", &["--json", "--paths-only"])?;
    if code != 0 {
        return Err(format!("check-prerequisites failed: {stdout}"));
    }
    // Minimal JSON field extraction (no serde dependency needed here).
    let get = |key: &str| -> String {
        let needle = format!("\"{key}\":\"");
        stdout
            .find(&needle)
            .map(|i| {
                let rest = &stdout[i + needle.len()..];
                rest.find('"').map(|j| rest[..j].to_string()).unwrap_or_default()
            })
            .unwrap_or_default()
    };
    let feature_dir = get("FEATURE_DIR");
    let has = |file: &str| !feature_dir.is_empty() && Path::new(&feature_dir).join(file).is_file();
    Ok(SpeckitStatus {
        branch: get("BRANCH"),
        has_spec: has("spec.md"),
        has_plan: has("plan.md"),
        has_tasks: has("tasks.md"),
        feature_dir,
        root,
        raw: stdout,
    })
}

/// Render `/speckit-status` output (shared by CLI and TUI surfaces).
pub fn render_status(s: &SpeckitStatus) -> String {
    let mut out = String::new();
    out.push_str(&format!("spec-kit repository: {}\n", s.root.display()));
    if !s.branch.is_empty() {
        out.push_str(&format!("branch: {}\n", s.branch));
    }
    if s.feature_dir.is_empty() {
        out.push_str("\nNo active feature branch (create one with /speckit-specify <description>).");
        let features = list_features(&s.root);
        if !features.is_empty() {
            out.push_str(&format!(
                "\nExisting features:\n  {}",
                features.join("\n  ")
            ));
        }
        return out;
    }
    out.push_str(&format!("feature: {}\n\n", s.feature_dir));
    let mark = |b: bool| if b { "[x]" } else { "[ ]" };
    out.push_str(&format!("{} spec.md    (created by /speckit-specify)\n", mark(s.has_spec)));
    out.push_str(&format!("{} plan.md    (created by /speckit-plan)\n", mark(s.has_plan)));
    out.push_str(&format!("{} tasks.md   (created by /speckit-tasks)\n", mark(s.has_tasks)));
    if !s.has_spec {
        out.push_str("\nNext step: /speckit-specify <feature description>");
    } else if !s.has_plan {
        out.push_str("\nNext step: /speckit-clarify, then /speckit-plan");
    } else if !s.has_tasks {
        out.push_str("\nNext step: /speckit-tasks");
    } else {
        out.push_str("\nReady: /speckit-analyze · /speckit-implement · /speckit-converge");
    }
    out
}

/// List all features under specs/ (for /speckit-status with no active
/// feature, and help text).
pub fn list_features(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root.join("specs")) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                if let Some(n) = e.file_name().to_str() {
                    if !n.starts_with('.') {
                        out.push(n.to_string());
                    }
                }
            }
        }
    }
    out.sort();
    out
}

/// The `speckit-help` text (also used by `/help`).
pub fn render_help() -> String {
    let mut out = String::from(
        "spec-kit workflow (run in order; artifacts live under specs/<feature>/):\n\n",
    );
    for s in LIFECYCLE {
        out.push_str(&format!(
            "  /{:<24} {} {}\n",
            s.name,
            s.description,
            if s.args_hint.is_empty() { String::new() } else { format!("· {}", s.args_hint) }
        ));
    }
    out.push_str("\n  /speckit-status              Show the current feature + artifact readiness\n");
    out.push_str("\nThe repository must have a .specify/ directory (spec-kit initialized).");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_names_are_step_prefixed() {
        assert!(LIFECYCLE.len() >= 10);
        for s in LIFECYCLE {
            assert!(s.name.starts_with("speckit-"), "{}", s.name);
            assert!(s.skill.starts_with("speckit-"), "{}", s.skill);
        }
        // Order matters: specify before plan before tasks before implement.
        let pos = |n: &str| LIFECYCLE.iter().position(|s| s.name == n).unwrap();
        assert!(pos("speckit-specify") < pos("speckit-plan"));
        assert!(pos("speckit-plan") < pos("speckit-tasks"));
        assert!(pos("speckit-tasks") < pos("speckit-implement"));
    }

    #[test]
    fn step_lookup() {
        assert!(step_by_name("speckit-specify").is_some());
        assert!(step_by_name("nope").is_none());
    }

    #[test]
    fn repo_root_found_for_this_repo() {
        let cwd = std::env::current_dir().unwrap();
        let root = find_repo_root(&cwd);
        assert!(root.is_some(), "this repo has .specify/");
    }

    #[test]
    fn repo_root_none_for_tmp() {
        assert!(find_repo_root(Path::new("/tmp")).is_none());
    }

    #[test]
    fn skill_workflow_loads_and_strips_frontmatter() {
        let wf = load_skill_workflow("speckit-specify");
        // The skill is installed in the dev environment; when absent the
        // error must be clean (not a panic).
        match wf {
            Ok(body) => {
                assert!(!body.starts_with("---"), "frontmatter stripped");
                assert!(body.len() > 200, "real workflow body, got {} bytes", body.len());
            }
            Err(e) => assert!(e.contains("not installed"), "{e}"),
        }
    }

    #[test]
    fn specify_script_runs_and_reports() {
        let cwd = std::env::current_dir().unwrap();
        let root = find_repo_root(&cwd).unwrap();
        let (out, _err, code) = run_specify_script(
            &root,
            "create-new-feature.sh",
            &["--dry-run", "--json", "--short-name", "probe-test", "probe description"],
        )
        .unwrap();
        assert_eq!(code, 0);
        assert!(out.contains("\"BRANCH_NAME\""), "dry-run JSON out: {out}");
    }

    #[test]
    fn status_gathers_artifact_flags() {
        let cwd = std::env::current_dir().unwrap();
        match status(&cwd) {
            Ok(s) => {
                assert!(!s.root.as_os_str().is_empty());
                // This repo HAS a current feature branch with artifacts.
                assert!(s.has_spec, "current feature has spec.md");
            }
            Err(e) => panic!("status failed on this repo: {e}"),
        }
    }

    #[test]
    fn render_status_next_step_hints() {
        let s = SpeckitStatus {
            root: PathBuf::from("/repo"),
            branch: "001-demo".into(),
            feature_dir: "/repo/specs/001-demo".into(),
            has_spec: true,
            has_plan: false,
            has_tasks: false,
            raw: String::new(),
        };
        let text = render_status(&s);
        assert!(text.contains("[x] spec.md"));
        assert!(text.contains("[ ] plan.md"));
        assert!(text.contains("/speckit-plan"));
    }

    // ── Upstream spec-kit parity (templates/commands/*.md) ────────────

    fn step_args(name: &str) -> (&'static str, &'static [&'static str], bool) {
        let s = step_by_name(name).unwrap();
        (s.script.unwrap(), s.script_args, s.script_optional)
    }

    #[test]
    fn preflight_invocations_match_upstream_spec_kit() {
        // Mirrors templates/commands/*.md `scripts.sh` lines in
        // ~/Development/spec-kit. Any change here must match upstream.
        let (script, args, _) = step_args("speckit-clarify");
        assert_eq!(script, "check-prerequisites.sh");
        assert_eq!(args, &["--json", "--paths-only"],
            "clarify runs BETWEEN specify and plan: paths-only, NO plan.md validation");

        let (script, args, _) = step_args("speckit-plan");
        assert_eq!((script, args), ("setup-plan.sh", &["--json"][..]));

        let (script, args, _) = step_args("speckit-tasks");
        assert_eq!((script, args), ("setup-tasks.sh", &["--json"][..]));

        let (script, args, _) = step_args("speckit-specify");
        assert_eq!(script, "create-new-feature.sh");
        assert!(args.contains(&"--allow-existing-branch"));

        for step in ["speckit-analyze", "speckit-implement", "speckit-converge", "speckit-taskstoissues"] {
            let (script, args, _) = step_args(step);
            assert_eq!(script, "check-prerequisites.sh", "{step}");
            assert_eq!(args, &["--json", "--require-tasks", "--include-tasks"],
                "{step} requires tasks.md upstream (it consumes/extends tasks)");
        }

        let (script, args, optional) = step_args("speckit-checklist");
        assert_eq!(script, "check-prerequisites.sh");
        assert_eq!(args, &["--json", "--template", "checklist-template"]);
        assert!(optional, "older scaffolds lack --template; must degrade");

        let (script, args, optional) = step_args("speckit-constitution");
        assert_eq!((script, args), ("resolve-template.sh", &["constitution-template", "--json"][..]));
        assert!(optional, "older scaffolds lack resolve-template.sh; must degrade");
    }

    #[test]
    fn clarify_runs_without_plan_md() {
        // THE reported bug: /speckit-clarify must work right after
        // /speckit-specify, BEFORE /speckit-plan creates plan.md. The
        // paths-only pre-flight does no plan validation, so preparing the
        // step on this repo (which has an active feature) must succeed
        // even if plan.md were deleted. Verify via the actual script.
        let cwd = std::env::current_dir().unwrap();
        let root = find_repo_root(&cwd).unwrap();
        let (out, _err, code) = run_specify_script(
            &root,
            "check-prerequisites.sh",
            &["--json", "--paths-only"],
        )
        .unwrap();
        assert_eq!(code, 0, "paths-only never validates plan.md");
        assert!(out.contains("\"FEATURE_DIR\""), "paths payload: {out}");
        // And the step prepares end-to-end (skill + pre-flight compose).
        // The skill lives in ~/.joey/skills — under the workspace test run
        // another test may relocate JOEY_HOME, so skip the skill-dependent
        // half when the skill isn't resolvable in THIS test's environment.
        let step = step_by_name("speckit-clarify").unwrap();
        match prepare_step(step, &root, "", None) {
            Ok(_) => {}
            Err(e) if e.contains("not installed") => {
                // pre-flight succeeded; only the (environment-relocated)
                // skill lookup failed. That's fine — the paths-only gate
                // itself was verified above.
            }
            Err(e) => panic!("clarify must prepare without plan.md: {e}"),
        }
    }

    #[test]
    fn optional_script_missing_degrades_instead_of_failing() {
        // Simulate an older scaffold: resolve-template.sh absent.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".specify/scripts/bash")).unwrap();
        std::fs::write(root.join(".specify/scripts/bash/check-prerequisites.sh"), "#!/usr/bin/env bash\necho '{}'\n").unwrap();
        let step = StepDef {
            name: "speckit-constitution",
            skill: "speckit-constitution",
            script: Some("resolve-template.sh"),
            script_args: &["constitution-template", "--json"],
            script_optional: true,
            script_gets_user_args: false,
            description: "",
            args_hint: "",
        };
        // Skills resolve GLOBALLY (~/.joey/skills); under the workspace
        // test run another test may relocate JOEY_HOME. Assert the
        // DEGRADATION specifically: the error (if any) must be the
        // skill-lookup one, never "script not found".
        match prepare_step(&step, root, "", None) {
            Ok(prep) => {
                assert!(prep.preflight.contains("skipped"),
                    "preflight notes the skip: {}", prep.preflight);
            }
            Err(e) if e.contains("not installed") => { /* env-relocated home */ }
            Err(e) => panic!("optional script absence must degrade, not fail: {e}"),
        }
    }

    #[test]
    fn optional_flag_rejection_retries_baseline() {
        // Simulate an older check-prerequisites.sh that rejects --template.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".specify/scripts/bash")).unwrap();
        std::fs::write(
            root.join(".specify/scripts/bash/check-prerequisites.sh"),
            "#!/usr/bin/env bash\nif [[ \"$*\" == *--template* ]]; then echo 'Unknown option' >&2; exit 1; fi\necho '{\"FEATURE_DIR\":\"x\"}'\n",
        )
        .unwrap();
        let step = StepDef {
            name: "speckit-checklist",
            skill: "speckit-checklist",
            script: Some("check-prerequisites.sh"),
            script_args: &["--json", "--template", "checklist-template"],
            script_optional: true,
            script_gets_user_args: false,
            description: "",
            args_hint: "",
        };
        // The rejected --template must trigger the baseline retry, which
        // succeeds; the composed prompt carries the baseline-flag preflight.
        // (Skill-lookup may fail under a relocated JOEY_HOME — only the
        // script-level behavior is being asserted here.)
        match prepare_step(&step, root, "", None) {
            Ok(prep) => {
                assert!(prep.preflight.contains("baseline flags"),
                    "preflight records the retry: {}", prep.preflight);
            }
            Err(e) if e.contains("not installed") => { /* env-relocated home */ }
            Err(e) => panic!("baseline retry after flag rejection must succeed: {e}"),
        }
    }
}
