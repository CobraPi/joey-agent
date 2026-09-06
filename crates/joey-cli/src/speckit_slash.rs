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
/// Argv-only (no shell string). The child runs on a worker thread and is
/// bounded by a 30s timeout: on expiry the child is killed and the call
/// errors (a hung git/subprocess must not wedge the dispatch surface).
pub fn run_specify_script(
    root: &Path,
    script: &str,
    args: &[&str],
) -> Result<(String, String, i32), String> {
    let path = root.join(".specify/scripts/bash").join(script);
    if !path.is_file() {
        return Err(format!("spec-kit script not found: {}", path.display()));
    }
    // Worker thread + bounded recv: `Command::output` has no timeout, and
    // handle_slash/status must never block the UI indefinitely. The child's
    // pid comes back first so a timeout can kill it.
    let root = root.to_path_buf();
    let script = script.to_string();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let path_display = path.display().to_string();
    let (pid_tx, pid_rx) = std::sync::mpsc::channel::<u32>();
    let (out_tx, out_rx) = std::sync::mpsc::channel::<std::io::Result<std::process::Output>>();
    std::thread::spawn(move || {
        let spawned = Command::new("bash")
            .arg(&path)
            .args(&args)
            .current_dir(&root)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();
        match spawned {
            Ok(child) => {
                let _ = pid_tx.send(child.id());
                let _ = out_tx.send(child.wait_with_output());
            }
            Err(e) => {
                let _ = out_tx.send(Err(e));
            }
        }
    });
    match out_rx.recv_timeout(std::time::Duration::from_secs(30)) {
        Ok(Ok(out)) => {
            let code = out.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Ok((stdout, stderr, code))
        }
        Ok(Err(e)) => Err(format!("failed to run {path_display}: {e}")),
        Err(_) => {
            // Kill the hung child (by pid — the Child handle lives on the
            // worker thread) so it can't outlive the call.
            if let Ok(pid) = pid_rx.try_recv() {
                let _ = Command::new("kill").arg(pid.to_string()).output();
            }
            Err(format!("script '{script}' timed out after 30s"))
        }
    }
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
    /// Resolved `before_<step>` extension-hook state (feature 026, T018),
    /// also prepended to `preflight` on the Native path. Empty on Legacy.
    pub hooks_note: String,
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
// Test-facing convenience wrapper (Native policy); production call sites
// invoke [`prepare_step_opts`] directly with an explicit policy.
#[allow(dead_code)]
pub fn prepare_step(
    step: &StepDef,
    root: &Path,
    user_args: &str,
    preloaded_skill: Option<&str>,
) -> Result<StepPrep, String> {
    prepare_step_opts(step, root, user_args, preloaded_skill, PrepPolicy::Native)
}

/// Which workflow-body resolution path (and pre-flight behavior) a step
/// preparation uses (feature 026, T007).
///
/// - `Native`: the speckit_bodies resolution chain (project overrides →
///   user skills → bundled floor) plus the internal pre-flight fallback
///   registry (FR-002). The default.
/// - `Legacy`: the exact pre-feature behavior — home-skills-only body
///   loading, missing required scripts are hard errors, no fallbacks.
///   Selected when `speckit.enabled=false` (FR-013).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepPolicy {
    Native,
    Legacy,
}

/// Master switch for the native spec-kit surface (`speckit.enabled`,
/// default true). When false every feature-026 code path short-circuits
/// and dispatch is identical to pre-feature (FR-013).
pub fn speckit_enabled(config: &joey_core::Config) -> bool {
    config.get_bool("speckit.enabled", true)
}

/// Policy-selecting core of [`prepare_step`].
pub fn prepare_step_opts(
    step: &StepDef,
    root: &Path,
    user_args: &str,
    preloaded_skill: Option<&str>,
    policy: PrepPolicy,
) -> Result<StepPrep, String> {
    let mut preflight = String::new();

    // Feature 026 (T018): resolved `before_<step>` extension-hook state,
    // surfaced ahead of the pre-flight section (hooks first). Native only —
    // Legacy keeps the exact pre-feature prompt (FR-013).
    let hooks_note = if policy == PrepPolicy::Native {
        let short = step.name.strip_prefix("speckit-").unwrap_or(step.name);
        gather_hook_notes(root, &format!("before_{short}"))
    } else {
        String::new()
    };
    if !hooks_note.is_empty() {
        preflight.push_str(&hooks_note);
        preflight.push_str("\n\n");
    }

    // Native-only suffix naming the platform variants of a failing script
    // (contract invariant 4). Legacy keeps the exact pre-feature message.
    let variants_suffix = |script: &str| -> String {
        if policy == PrepPolicy::Native {
            let stem = script.strip_suffix(".sh").unwrap_or(script);
            format!(
                "; platform variants: powershell/{stem}.ps1, python/{}.py",
                stem.replace('-', "_")
            )
        } else {
            String::new()
        }
    };

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
            // Feature 026 (T008, FR-002): when the bash script is missing,
            // a known scaffold script falls back to an internal equivalent
            // (with a warning) instead of erroring — for ANY step. Legacy
            // policy keeps the pre-feature behavior verbatim.
            let fallback = if policy == PrepPolicy::Native {
                internal_preflight_fallback(root, script, &argv, user_args)
            } else {
                None
            };
            match fallback {
                Some(Ok(section)) => preflight.push_str(&section),
                Some(Err(e)) => return Err(e),
                None => {
                    if step.script_optional {
                        // Older scaffold: script postdates this .specify init. The
                        // skill workflow is self-sufficient — proceed without it.
                        preflight.push_str(&format!(
                            "## Pre-flight\n\n(script `{script}` not present in this .specify scaffold — skipped; the workflow below resolves paths/templates itself)\n\n"
                        ));
                    } else {
                        return Err(format!("spec-kit script not found: {}", path.display()));
                    }
                }
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
                            "spec-kit pre-flight failed ({script}, exit {rcode}{}):\n{}{}",
                            variants_suffix(script),
                            if rerr.is_empty() { String::new() } else { format!("{rerr}\n") },
                            rout
                        ));
                    }
                    let _ = (stderr, code);
                }
                _ => {
                    return Err(format!(
                        "spec-kit pre-flight failed ({script}, exit {code}{}):\n{}{}",
                        variants_suffix(script),
                        if stderr.is_empty() { String::new() } else { format!("{stderr}\n") },
                        stdout
                    ));
                }
            }
        }
    }

    // Workflow body: Legacy keeps the pre-feature home-skills load verbatim;
    // Native resolves through the speckit_bodies chain (project overrides →
    // user skills → bundled floor) with placeholder substitution (T007).
    // T036 (FR-004a): Native also surfaces the workflow frontmatter's
    // `tools:` references as a prompt section (Legacy keeps the exact
    // pre-feature prompt — no tools section).
    let mut tools_section = String::new();
    let workflow = match policy {
        PrepPolicy::Legacy => load_skill_workflow(step.skill)?,
        PrepPolicy::Native => {
            let name = step.skill.strip_prefix("speckit-").unwrap_or(step.skill);
            let wf = crate::speckit_bodies::resolve_body(Some(root), name);
            if !wf.frontmatter.tools.is_empty() {
                let mut lines = String::new();
                for tool in &wf.frontmatter.tools {
                    lines.push_str(&format!("- {tool}\n"));
                }
                tools_section = format!(
                    "\n\n## Tools referenced by this workflow\n\n\
{lines}\
These tools are referenced by the upstream workflow; ensure the corresponding MCP servers (e.g. github-mcp-server) are configured before relying on them."
                );
            }
            let script_display = match step.script {
                Some(s) => format!(
                    ".specify/scripts/bash/{} {}",
                    s,
                    step.script_args.join(" ")
                )
                .trim()
                .to_string(),
                None => "-".to_string(),
            };
            let substituted = crate::speckit_bodies::substitute_placeholders(wf.body(), &script_display);
            format!("{substituted}{tools_section}")
        }
    };
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

    // T025 wiring (Native only): when the lifecycle feature dir has
    // tasks.md, derive the orchestration task graph from it and embed it in
    // the prompt — the agent publishes it via the task_graph tool.
    // Same-phase TaskFileCollision pairs with no dependency between them
    // get a sequencing edge (document-earlier → document-later) so the
    // write sets stay exclusive under parallel dispatch.
    let mut prompt = prompt;
    if policy == PrepPolicy::Native {
        let state = crate::speckit_lifecycle::derive_state(root);
        if state.has_tasks {
            let tasks_path = root.join(&state.feature_directory).join("tasks.md");
            if let Ok(tasks_md) = std::fs::read_to_string(&tasks_path) {
                let mut bundle = crate::speckit_lifecycle::tasks_to_task_nodes(&tasks_md);
                let mut collision_notes: Vec<String> = Vec::new();
                for c in &bundle.collisions {
                    let by_id = |id: &str| {
                        let want = crate::speckit_lifecycle::sanitized_task_id_pub(id);
                        bundle.nodes.iter().position(|n| n.id == want)
                    };
                    // Sequence document-earlier → document-later, but only
                    // when no dependency exists between the pair already.
                    if let (Some(ai), Some(bi)) = (by_id(&c.task_a), by_id(&c.task_b)) {
                        let (earlier, later) = if ai < bi { (ai, bi) } else { (bi, ai) };
                        let earlier_id = bundle.nodes[earlier].id.clone();
                        let later_id = bundle.nodes[later].id.clone();
                        if !bundle.nodes[later].dependencies.contains(&earlier_id)
                            && !bundle.nodes[earlier].dependencies.contains(&later_id)
                        {
                            bundle.nodes[later].dependencies.push(earlier_id.clone());
                            collision_notes.push(format!(
                                "- tasks {} and {} both write `{}` — sequenced {} → {} (exclusive write set)",
                                c.task_a, c.task_b, c.file, earlier_id, later_id
                            ));
                        }
                    }
                }
                let nodes_json: Vec<serde_json::Value> = bundle
                    .nodes
                    .iter()
                    .map(|n| {
                        serde_json::json!({
                            "id": n.id.as_str(),
                            "objective": n.objective,
                            "dependencies": n.dependencies.iter().map(|d| d.as_str()).collect::<Vec<_>>(),
                            "read_set": n.read_set.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                            "write_set": n.write_set.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                let mut section = String::from(
                    "ORCHESTRATION TASK GRAPH (derived from tasks.md; publish via the task_graph tool; write_sets are exclusive — never assign two concurrent specialists the same file):\n",
                );
                section.push_str(&serde_json::to_string_pretty(&nodes_json).unwrap_or_default());
                if !collision_notes.is_empty() {
                    section.push_str("\n\nCollision notes:\n");
                    section.push_str(&collision_notes.join("\n"));
                }
                prompt.push_str("\n\n---\n\n");
                prompt.push_str(&section);
            }
        }
    }

    Ok(StepPrep { preflight, prompt, hooks_note })
}

// ---------------------------------------------------------------------------
// Extension-hook note surface (feature 026, T018 / contracts/hooks.md)
// ---------------------------------------------------------------------------

/// Build the resolved `before_<point>` / `after_<point>` hook section for a
/// step prompt, following the block shapes the upstream workflow bodies
/// themselves define (the bodies instruct the agent to emit these, but the
/// model cannot reliably execute native commands — so we surface the
/// RESOLVED state instead):
///
/// - non-executable (conditioned) entry → one skip line:
///   `- Hook '{extension}' skipped: condition '{condition}' is left to the extension runtime`
/// - optional entry → the upstream Optional Pre-Hook block (command in
///   slash-normalized form, description, prompt, how to execute);
/// - mandatory entry → the upstream Automatic Pre-Hook block with the raw
///   dotted `EXECUTE_COMMAND:` id.
///
/// Blocks are joined with blank lines. Empty when the repo declares no
/// (enabled) hooks for the point.
pub fn gather_hook_notes(root: &Path, point: &str) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for entry in crate::speckit_hooks::hooks_for(root, point) {
        if !crate::speckit_hooks::is_executable(&entry) {
            blocks.push(format!(
                "- Hook '{}' skipped: condition '{}' is left to the extension runtime",
                entry.extension,
                entry.condition.clone().unwrap_or_default()
            ));
        } else if entry.optional {
            blocks.push(format!(
                "## Extension Hooks\n\n**Optional Pre-Hook**: {ext}\nCommand: `/{cmd}`\nDescription: {desc}\n\nPrompt: {prompt}\nTo execute: `/{cmd}`",
                ext = entry.extension,
                cmd = crate::speckit_hooks::slash_form(&entry.command),
                desc = entry.description,
                prompt = entry.prompt,
            ));
        } else {
            blocks.push(format!(
                "## Extension Hooks\n\n**Automatic Pre-Hook**: {ext}\nExecuting: `/{cmd}`\nEXECUTE_COMMAND: {command}",
                ext = entry.extension,
                cmd = crate::speckit_hooks::slash_form(&entry.command),
                command = entry.command,
            ));
        }
    }
    blocks.join("\n\n")
}

// ---------------------------------------------------------------------------
// Post-step handoff surface (feature 026, T021 / US5)
// ---------------------------------------------------------------------------

/// The primary (first-listed) handoff of a step's workflow-body frontmatter:
/// `(label, prompt, send, canonical target command)`. The target is derived
/// from the handoff agent id (`speckit.X` → `speckit-X`, dots → hyphens for
/// display; the canonical command is `speckit-<X>`).
pub fn primary_handoff(step: &StepDef, root: &Path) -> Option<(String, String, bool, String)> {
    let name = step.skill.strip_prefix("speckit-").unwrap_or(step.skill);
    let wf = crate::speckit_bodies::resolve_body(Some(root), name);
    let primary = wf.frontmatter.handoffs.first()?;
    let bare = primary
        .agent
        .strip_prefix("speckit.")
        .unwrap_or(&primary.agent);
    let target = format!("speckit-{}", bare.replace('.', "-"));
    Some((
        primary.label.clone(),
        primary.prompt.clone(),
        primary.send,
        target,
    ))
}

/// The primary (first-listed) handoff declared by a step's workflow-body
/// frontmatter: `(label, prompt, send)`. `send=true` means auto-send the
/// handoff prompt to the target command; `send=false` means offer only.
/// `None` when the step's body declares no handoffs (checklist, analyze,
/// implement, converge, taskstoissues upstream).
// Projection asserted by the speckit-native test suite; not yet called
// from production paths.
#[allow(dead_code)]
pub fn handoff_offer(step: &StepDef, root: &Path) -> Option<(String, String, bool)> {
    primary_handoff(step, root).map(|(l, p, s, _)| (l, p, s))
}

// ---------------------------------------------------------------------------
// Pre-flight fallback registry (feature 026, T008 / FR-002)
// ---------------------------------------------------------------------------

/// Internal equivalent for a missing `.specify/scripts/bash/<script>`:
/// `Some(Ok(section))` = pre-flight section (warning prepended),
/// `Some(Err(e))` = hard error, `None` = no internal equivalent (the
/// caller falls back to the pre-feature optional/required behavior).
fn internal_preflight_fallback(
    root: &Path,
    script: &str,
    argv: &[&str],
    user_args: &str,
) -> Option<Result<String, String>> {
    // Every fallback prepends the same warning line, plus a platform-
    // variant note when a powershell/python sibling of the script exists.
    let warn = format!(
        "⚠ pre-flight fallback: `{script}` not present in this .specify scaffold — internal equivalent used{}\n\n",
        platform_variant_note(root, script)
    );
    let body = match script {
        "check-prerequisites.sh" => match internal_check_prerequisites(root, argv) {
            Ok(b) => b,
            Err(e) => return Some(Err(e)),
        },
        "create-new-feature.sh" => match internal_create_feature(root, user_args.trim()) {
            Ok(b) => b,
            Err(e) => return Some(Err(e)),
        },
        "setup-plan.sh" => match internal_ensure_feature(root, "plan-template.md") {
            Ok(b) => b,
            Err(e) => return Some(Err(e)),
        },
        "setup-tasks.sh" => match internal_ensure_feature(root, "tasks-template.md") {
            Ok(b) => b,
            Err(e) => return Some(Err(e)),
        },
        // Older scaffold: the workflow reads the template itself — today's
        // optional-skip behavior, with the fallback warning prepended.
        "resolve-template.sh" => {
            "(script `resolve-template.sh` not present in this .specify scaffold — skipped; the workflow below resolves paths/templates itself)\n".to_string()
        }
        _ => return None,
    };
    Some(Ok(format!(
        "{warn}## Pre-flight (internal equivalent of {script})\n\n{body}\n\n"
    )))
}

/// The ` (platform variant present: …)` note appended to the fallback
/// warning when a powershell or python sibling of the missing bash script
/// exists under `.specify/scripts/` (stems normalized: `-`/`_` equal).
fn platform_variant_note(root: &Path, script: &str) -> String {
    let stem = script.strip_suffix(".sh").unwrap_or(script);
    if let Some(name) = find_platform_variant(root, "powershell", stem, "ps1") {
        return format!(" (platform variant present: powershell/{name})");
    }
    if let Some(name) = find_platform_variant(root, "python", &stem.replace('-', "_"), "py") {
        return format!(" (platform variant present: python/{name})");
    }
    String::new()
}

/// Exact file first, then a listing-based match with `-`/`_`-normalized
/// stems (upstream python scripts are snake_case, e.g.
/// `check_prerequisites.py` for `check-prerequisites.sh`).
fn find_platform_variant(root: &Path, dir: &str, stem: &str, ext: &str) -> Option<String> {
    let exact = format!("{stem}.{ext}");
    let dir_path = root.join(".specify/scripts").join(dir);
    if dir_path.join(&exact).is_file() {
        return Some(exact);
    }
    let norm = |s: &str| s.replace('_', "-");
    let wanted = norm(stem);
    let entries = std::fs::read_dir(&dir_path).ok()?;
    for e in entries.flatten() {
        if let Some(n) = e.file_name().to_str() {
            if let Some(base) = n.strip_suffix(&format!(".{ext}")) {
                if norm(base) == wanted {
                    return Some(n.to_string());
                }
            }
        }
    }
    None
}

/// Read the active feature directory from `.specify/feature.json`.
fn read_feature_dir(root: &Path) -> Result<String, String> {
    let path = root.join(".specify/feature.json");
    let raw = std::fs::read_to_string(&path)
        .map_err(|_| "no active feature — run /speckit-specify <description> first".to_string())?;
    let v: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|_| format!("invalid .specify/feature.json: {}", path.display()))?;
    let dir = v
        .get("feature_directory")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .to_string();
    if dir.is_empty() {
        return Err("no active feature — run /speckit-specify <description> first".to_string());
    }
    Ok(dir)
}

/// Internal equivalent of `check-prerequisites.sh`: feature dir, git branch
/// (short ref name from `.git/HEAD`, `"(none)"` when absent), the feature
/// dir's doc listing, and — with `--include-tasks` — the tasks.md contents
/// (truncated at 8000 chars with a note).
fn internal_check_prerequisites(root: &Path, argv: &[&str]) -> Result<String, String> {
    let feature_dir = read_feature_dir(root)?;
    let branch = read_git_branch(root);
    let feature_path = root.join(&feature_dir);
    let mut docs: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&feature_path) {
        for e in entries.flatten() {
            if let Some(n) = e.file_name().to_str() {
                if !n.starts_with('.') {
                    docs.push(n.to_string());
                }
            }
        }
    }
    docs.sort();
    let docs_json = serde_json::to_string(&docs).unwrap_or_else(|_| "[]".to_string());
    let mut body = format!(
        "```json\n{{\"FEATURE_DIR\":\"{feature_dir}\",\"BRANCH\":\"{branch}\",\"AVAILABLE_DOCS\":{docs_json}}}\n```"
    );
    if argv.contains(&"--include-tasks") {
        let tasks = feature_path.join("tasks.md");
        if tasks.is_file() {
            if let Ok(mut text) = std::fs::read_to_string(&tasks) {
                if text.chars().count() > 8000 {
                    text = text.chars().take(8000).collect::<String>();
                    text.push_str("\n…(truncated at 8000 chars)");
                }
                body.push_str(&format!("\n\ntasks.md:\n\n{text}"));
            }
        }
    }
    Ok(body)
}

/// Short branch name from `.git/HEAD` (`ref: refs/heads/<name>` → `<name>`);
/// `"(none)"` when HEAD is absent or not a symbolic ref.
fn read_git_branch(root: &Path) -> String {
    let head = std::fs::read_to_string(root.join(".git/HEAD")).unwrap_or_default();
    if let Some(rest) = head.trim().strip_prefix("ref:") {
        let full = rest.trim();
        if !full.is_empty() {
            return full.rsplit('/').next().unwrap_or("").to_string();
        }
    }
    "(none)".to_string()
}

/// Internal equivalent of `create-new-feature.sh`: allocate the next
/// `NNN-slug` directory under `specs/` (max NNN + 1, zero-padded to 3),
/// create it, and record it in `.specify/feature.json` (compact JSON).
fn internal_create_feature(root: &Path, description: &str) -> Result<String, String> {
    let specs = root.join("specs");
    let mut max = 0usize;
    if let Ok(entries) = std::fs::read_dir(&specs) {
        for e in entries.flatten() {
            if let Some(n) = e.file_name().to_str() {
                if let Some(num) = n.split('-').next() {
                    if let Ok(v) = num.parse::<usize>() {
                        max = max.max(v);
                    }
                }
            }
        }
    }
    let rel = format!("specs/{:03}-{}", max + 1, slugify(description));
    std::fs::create_dir_all(root.join(&rel))
        .map_err(|e| format!("failed to create {rel}: {e}"))?;
    std::fs::create_dir_all(root.join(".specify"))
        .map_err(|e| format!("failed to create .specify: {e}"))?;
    let json = serde_json::json!({ "feature_directory": rel });
    std::fs::write(
        root.join(".specify/feature.json"),
        serde_json::to_string(&json).unwrap_or_default(),
    )
    .map_err(|e| format!("failed to write .specify/feature.json: {e}"))?;
    Ok(format!("created feature directory {rel} (recorded in .specify/feature.json)"))
}

/// `[^a-z0-9]+` → `-`, trimmed, collapsed, capped at 40 chars.
fn slugify(description: &str) -> String {
    let dashed: String = description
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let mut slug = dashed
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.chars().count() > 40 {
        slug = slug.chars().take(40).collect::<String>().trim_matches('-').to_string();
    }
    if slug.is_empty() {
        slug = "feature".to_string();
    }
    slug
}

/// Internal equivalent of `setup-plan.sh` / `setup-tasks.sh`: require an
/// active feature, then report it plus the relevant template's presence.
fn internal_ensure_feature(root: &Path, template: &str) -> Result<String, String> {
    let feature_dir = read_feature_dir(root)?;
    let template_path = root.join(format!(".specify/templates/{template}"));
    Ok(format!(
        "FEATURE_DIR: {feature_dir}\ntemplate: {} ({})",
        template_path.display(),
        if template_path.is_file() { "present" } else { "absent" }
    ))
}

// ---------------------------------------------------------------------------
// Dotted-form dispatch + unknown-command surface (T011-T014)
// ---------------------------------------------------------------------------

/// The twelve spec-kit command names (ten lifecycle + status + help),
/// canonical order.
pub fn all_command_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = LIFECYCLE.iter().map(|s| s.name).collect();
    names.push("speckit-status");
    names.push("speckit-help");
    names
}

/// Split a dotted `speckit.<name> [args…]` line into `(name, args)`.
/// `None` when the input lacks the literal `speckit.` prefix or the name
/// is empty.
pub fn dotted_parts(input: &str) -> Option<(&str, &str)> {
    let rest = input.strip_prefix("speckit.")?;
    let (name, args) = match rest.find(char::is_whitespace) {
        Some(i) => (&rest[..i], rest[i..].trim()),
        None => (rest, ""),
    };
    if name.is_empty() {
        None
    } else {
        Some((name, args))
    }
}

/// Canonical dotted → slash mapping (T011/T012): `speckit.plan notes` →
/// `("speckit-plan", "notes")`. `Some` only for the twelve commands; an
/// unknown dotted name returns `None` so the caller errors listing the
/// surface (never silently shadowing user input).
pub fn dotted_to_slash(input: &str) -> Option<(String, String)> {
    let (name, args) = dotted_parts(input)?;
    let canon = format!("speckit-{name}");
    if step_by_name(&canon).is_some() || canon == "speckit-status" || canon == "speckit-help" {
        Some((canon, args.to_string()))
    } else {
        None
    }
}

/// The unknown-command error for BOTH forms (T014, contract invariant 2):
/// names the input, lists all twelve commands, and appends the closest by
/// edit distance when reasonably near (distance ≤ 4).
pub fn unknown_command_error(input: &str) -> String {
    let mut out = format!("unknown spec-kit command: {input}\nAvailable commands:");
    for name in all_command_names() {
        out.push_str(&format!("\n/{name}"));
    }
    let typed = input.trim().trim_start_matches('/');
    if !typed.is_empty() {
        let mut best: Option<(usize, &'static str)> = None;
        for name in all_command_names() {
            let d = levenshtein(typed, name);
            if best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, name));
            }
        }
        if let Some((d, name)) = best {
            if d <= 4 {
                out.push_str(&format!("\nClosest: /{name}"));
            }
        }
    }
    out
}

/// Minimal edit distance (insert/delete/substitute), char-based.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur: Vec<usize> = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
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
/// feature branch to exist). JSON fields are read through serde_json (no
/// substring scraping). When the script itself is ABSENT (missing file, or
/// a nonzero exit from a scaffold that has no script on disk), the state
/// is derived from disk instead (`derive_state` + `.git/HEAD`).
pub fn status(cwd: &Path) -> Result<SpeckitStatus, String> {
    let root = find_repo_root(cwd)
        .ok_or_else(|| "not a spec-kit repository (no .specify/ directory found in this or any parent directory)".to_string())?;
    // Disk-derivation fallback (feature 026 parity with the pre-flight
    // fallback registry): same shape as check-prerequisites --paths-only.
    let derive_fallback = |raw: String| {
        let state = crate::speckit_lifecycle::derive_state(&root);
        SpeckitStatus {
            root: root.clone(),
            branch: read_git_branch(&root),
            feature_dir: state.feature_directory,
            has_spec: state.has_spec,
            has_plan: state.has_plan,
            has_tasks: state.has_tasks,
            raw,
        }
    };
    let script_path = root.join(".specify/scripts/bash/check-prerequisites.sh");
    match run_specify_script(&root, "check-prerequisites.sh", &["--json", "--paths-only"]) {
        Ok((stdout, _stderr, 0)) => {
            // Structured field lookup — a VALUE containing a quoted
            // `"FEATURE_DIR":"…"` payload can no longer fool extraction.
            let json = parse_json_object(&stdout)
                .ok_or_else(|| format!("check-prerequisites failed: {stdout}"))?;
            let get = |key: &str| -> String {
                json.get(key).and_then(|f| f.as_str()).unwrap_or("").to_string()
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
        // Nonzero exit with no script on disk → the scaffold never had it;
        // derive from disk rather than failing the whole status surface.
        Ok((stdout, _stderr, _code)) if !script_path.is_file() => Ok(derive_fallback(stdout)),
        Ok((stdout, _stderr, code)) => Err(format!("check-prerequisites failed (exit {code}): {stdout}")),
        Err(_e) if !script_path.is_file() => Ok(derive_fallback(String::new())),
        Err(e) => Err(e),
    }
}

/// Parse the first JSON object in `s` (the script may surround it with
/// progress text): `from_str` on the trimmed whole, else the first `{` …
/// last `}` slice.
fn parse_json_object(s: &str) -> Option<serde_json::Value> {
    let t = s.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
        return Some(v);
    }
    let start = t.find('{')?;
    let end = t.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(&t[start..=end]).ok()
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

/// Feature 026 (T035, FR-007): render `/speckit-status` with the lifecycle
/// context block (Feature/Step/Artifacts/Note) appended after the existing
/// output — but ONLY when the native surface is enabled; disabled config
/// renders EXACTLY as [`render_status`] (pre-feature parity, FR-013).
pub fn render_status_with_config(s: &SpeckitStatus, config: &joey_core::Config) -> String {
    let base = render_status(s);
    if !speckit_enabled(config) {
        return base;
    }
    format!(
        "{}\n{}",
        base,
        crate::speckit_lifecycle::context_block(&crate::speckit_lifecycle::derive_state(&s.root))
    )
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
    out.push_str("  /speckit-help                List the spec-kit workflow commands (this help)\n");
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
