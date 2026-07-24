//! OMO orchestration runtime: the actual routing, delegation, plan execution,
//! and wisdom-accumulation logic that makes the agents behave differently.
//!
//! Port of oh-my-openagent's orchestration layer:
//!   - `delegate-task/` tool routing (category → Junior, subagent_type → agent)
//!   - `start-work` hook (boulder init/resume, Atlas activation)
//!   - Atlas plan execution loop (read, delegate, verify, accumulate wisdom)
//!   - Boulder-push system reminder for Junior todo continuation
//!   - Tool restriction enforcement during delegation
//!
//! This is the RUNTIME layer that sits between the agent loop and the
//! delegation engine (joey-orchestration). It inspects the delegation request,
//! resolves the target agent, applies tool restrictions, injects accumulated
//! wisdom, and returns results.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agents::registry::AgentRegistry;
use crate::agents::OmoAgent;
use crate::boulder::{BoulderState, BoulderWork, BoulderWorkStatus};
use crate::notepad::{NotepadFile, NotepadStore};
use crate::plan_parser::ParsedTask;

// ─── Delegation Request ──────────────────────────────────────────────

/// A delegation request from the OMO orchestration layer.
///
/// This is the resolved form of the `delegate_task` / `call_omo_agent` tool
/// arguments, adapted to OMO's routing semantics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmoDelegationRequest {
    /// The prompt for the delegated agent.
    pub prompt: String,
    /// Short description (3-5 words).
    #[serde(default)]
    pub description: Option<String>,
    /// Category name (routes to Sisyphus-Junior with category model).
    /// Mutually exclusive with `subagent_type`.
    #[serde(default)]
    pub category: Option<String>,
    /// Subagent type (routes to the named agent directly).
    /// Mutually exclusive with `category`.
    #[serde(default)]
    pub subagent_type: Option<String>,
    /// Skill names to load into the subagent's prompt.
    #[serde(default)]
    pub load_skills: Vec<String>,
    /// Whether to run in background (true) or sync (false).
    #[serde(default)]
    pub run_in_background: bool,
}

/// The resolved routing decision for a delegation request.
#[derive(Debug, Clone)]
pub struct DelegationRoute {
    /// The agent name to delegate to (e.g. "sisyphus-junior", "oracle").
    pub agent_name: String,
    /// The model to use (resolved from category or agent fallback chain).
    pub model: Option<String>,
    /// Tool restrictions to apply (from the agent's ToolPermissions).
    pub denied_tools: Vec<String>,
    /// Prompt append from category (if category-routed).
    pub prompt_append: Option<String>,
    /// Temperature override.
    pub temperature: Option<f64>,
    /// Max tokens override.
    pub max_tokens: Option<u32>,
}

/// Validate and route a delegation request.
///
/// This implements OMO's delegation semantics:
/// - `category` → Sisyphus-Junior with category-optimized model
/// - `subagent_type` → the named agent directly
/// - Both are mutually exclusive
pub fn route_delegation(
    request: &OmoDelegationRequest,
    registry: &AgentRegistry,
) -> Result<DelegationRoute, String> {
    // Validate: category and subagent_type are mutually exclusive.
    if request.category.is_some() && request.subagent_type.is_some() {
        return Err(
            "Cannot provide both category and subagent_type. Choose one."
                .to_string(),
        );
    }

    // Category routing → Sisyphus-Junior
    if let Some(ref category_name) = request.category {
        let category = registry
            .categories()
            .iter()
            .find(|c| &c.name == category_name)
            .ok_or_else(|| {
                let available: Vec<&str> =
                    registry.categories().iter().map(|c| c.name.as_str()).collect();
                format!(
                    "Unknown category: '{}'. Available: {}",
                    category_name,
                    available.join(", ")
                )
            })?;

        // Resolve the category's model from the fallback chain.
        let model = category
            .model_requirement
            .fallback_chain
            .first()
            .map(|e| e.model.clone());

        // Junior's tool restrictions + category-specific.
        let junior = registry
            .get("sisyphus-junior")
            .ok_or("sisyphus-junior agent not found")?;
        let denied_tools: Vec<String> = junior
            .tool_permissions
            .denied()
            .iter()
            .cloned()
            .collect();

        return Ok(DelegationRoute {
            agent_name: "sisyphus-junior".to_string(),
            model,
            denied_tools,
            prompt_append: category.prompt_append.clone(),
            temperature: Some(category.temperature.unwrap_or(0.5)),
            max_tokens: junior.max_tokens,
        });
    }

    // Subagent routing → named agent
    if let Some(ref agent_name) = request.subagent_type {
        let agent = registry
            .get(agent_name)
            .ok_or_else(|| format!("Unknown agent: '{}'", agent_name))?;

        let denied_tools: Vec<String> = agent
            .tool_permissions
            .denied()
            .iter()
            .cloned()
            .collect();

        return Ok(DelegationRoute {
            agent_name: agent_name.clone(),
            model: agent.resolved_model.clone(),
            denied_tools,
            prompt_append: None,
            temperature: Some(agent.temperature),
            max_tokens: agent.max_tokens,
        });
    }

    Err("Must provide either category or subagent_type.".to_string())
}

// ─── Boulder-Push System Reminder (Junior todo continuation) ─────────

/// The system reminder injected when Junior has incomplete todos.
///
/// Port of OMO's "boulder pushing" mechanism — Junior is not allowed to
/// respond until all todos are marked complete.
pub fn boulder_push_reminder(incomplete_todos: &[String]) -> Option<String> {
    if incomplete_todos.is_empty() {
        return None;
    }
    let todo_lines: Vec<String> = incomplete_todos
        .iter()
        .map(|t| format!("  - [ ] {}", t))
        .collect();
    Some(format!(
        "[SYSTEM REMINDER - TODO CONTINUATION]\n\n\
         You have incomplete todos! Complete ALL before responding:\n{}\n\n\
         DO NOT respond until all todos are marked completed.",
        todo_lines.join("\n")
    ))
}

// ─── Wisdom Extraction ───────────────────────────────────────────────

/// Extract wisdom from a subagent's response for notepad accumulation.
///
/// Atlas runs this after each delegated task completes, categorizing
/// learnings into: Conventions, Successes, Failures, Gotchas, Commands.
#[derive(Debug, Clone, Default)]
pub struct ExtractedWisdom {
    pub learnings: Vec<String>,
    pub decisions: Vec<String>,
    pub issues: Vec<String>,
    pub verification: Vec<String>,
    pub problems: Vec<String>,
}

/// Categorize a subagent response into wisdom buckets.
///
/// This uses simple keyword/pattern matching to route content into the
/// appropriate notepad file. The subagent's response is scanned for
/// indicators of each wisdom type.
pub fn extract_wisdom(subagent_response: &str, task_description: &str) -> ExtractedWisdom {
    let mut wisdom = ExtractedWisdom::default();

    // Lines that look like learnings (patterns, conventions, approaches).
    for line in subagent_response.lines() {
        let lower = line.to_lowercase();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("```") {
            continue;
        }

        // Verification results.
        if lower.contains("test") && (lower.contains("pass") || lower.contains("fail")) {
            wisdom.verification.push(trimmed.to_string());
            continue;
        }

        // Decisions (architectural choices).
        if lower.contains("decided")
            || lower.contains("chose")
            || lower.contains("architecture")
            || lower.contains("rationale")
        {
            wisdom.decisions.push(trimmed.to_string());
            continue;
        }

        // Issues and gotchas.
        if lower.contains("issue")
            || lower.contains("problem")
            || lower.contains("gotcha")
            || lower.contains("blocker")
            || lower.contains("warning")
            || lower.contains("error:")
        {
            wisdom.issues.push(trimmed.to_string());
            continue;
        }

        // Patterns and conventions.
        if lower.contains("convention")
            || lower.contains("pattern")
            || lower.contains("standard")
            || lower.contains("discovered")
            || lower.contains("found that")
        {
            wisdom.learnings.push(trimmed.to_string());
            continue;
        }

        // Unresolved problems.
        if lower.contains("unresolved")
            || lower.contains("technical debt")
            || lower.contains("todo:")
            || lower.contains("fixme")
        {
            wisdom.problems.push(trimmed.to_string());
            continue;
        }
    }

    // Always record at least one learning entry (the task was done).
    if wisdom.learnings.is_empty() && !subagent_response.trim().is_empty() {
        wisdom
            .learnings
            .push(format!("Task '{}': completed", task_description));
    }

    wisdom
}

/// Write extracted wisdom to the notepad store.
pub fn accumulate_wisdom(
    notepad_store: &NotepadStore,
    wisdom: &ExtractedWisdom,
) -> std::io::Result<()> {
    if !wisdom.learnings.is_empty() {
        notepad_store.append(
            NotepadFile::Learnings,
            &wisdom.learnings.join("\n"),
        )?;
    }
    if !wisdom.decisions.is_empty() {
        notepad_store.append(
            NotepadFile::Decisions,
            &wisdom.decisions.join("\n"),
        )?;
    }
    if !wisdom.issues.is_empty() {
        notepad_store.append(NotepadFile::Issues, &wisdom.issues.join("\n"))?;
    }
    if !wisdom.verification.is_empty() {
        notepad_store.append(
            NotepadFile::Verification,
            &wisdom.verification.join("\n"),
        )?;
    }
    if !wisdom.problems.is_empty() {
        notepad_store.append(
            NotepadFile::Problems,
            &wisdom.problems.join("\n"),
        )?;
    }
    Ok(())
}

/// Build the accumulated-wisdom context block to inject into subsequent
/// subagent prompts.
pub fn wisdom_context_block(notepad_store: &NotepadStore) -> String {
    let mut sections = Vec::new();

    for file in NotepadFile::all() {
        let content = notepad_store.read(*file);
        if !content.trim().is_empty() {
            let label = match file {
                NotepadFile::Learnings => "Patterns & Conventions",
                NotepadFile::Decisions => "Decisions",
                NotepadFile::Issues => "Issues & Gotchas",
                NotepadFile::Verification => "Verification Results",
                NotepadFile::Problems => "Unresolved Problems",
            };
            sections.push(format!("### {}\n{}", label, content));
        }
    }

    if sections.is_empty() {
        String::new()
    } else {
        format!(
            "<accumulated-wisdom>\nThis knowledge was gathered from previous tasks in this work session. Apply it.\n\n{}\n</accumulated-wisdom>",
            sections.join("\n\n")
        )
    }
}

// ─── Atlas Plan Execution ────────────────────────────────────────────

/// The result of executing a plan task.
#[derive(Debug, Clone)]
pub struct TaskExecutionResult {
    pub task_number: usize,
    pub task_title: String,
    pub success: bool,
    pub subagent_response: String,
    pub wisdom: ExtractedWisdom,
}

/// Atlas plan execution configuration.
#[derive(Debug, Clone)]
pub struct AtlasPlanConfig {
    /// Path to `.omo/plans/{name}.md`.
    pub plan_path: PathBuf,
    /// Plan name (slug).
    pub plan_name: String,
    /// Directory for notepads (`.omo/notepads/{plan_name}/`).
    pub notepad_dir: PathBuf,
}

/// Process a plan for Atlas execution: parse tasks, determine ordering.
///
/// Returns the implementation tasks (in order) and final verification tasks.
pub fn prepare_plan_execution(
    plan_content: &str,
) -> Result<(Vec<ParsedTask>, Vec<ParsedTask>), String> {
    let plan = crate::plan_parser::parse_plan(plan_content);

    let implementation: Vec<ParsedTask> = plan
        .implementation_tasks()
        .into_iter()
        .cloned()
        .collect();
    let verification: Vec<ParsedTask> = plan
        .final_verification_tasks()
        .into_iter()
        .cloned()
        .collect();

    Ok((implementation, verification))
}

/// Build the delegation prompt for a single plan task (Atlas → Junior).
///
/// Includes:
///   - The task description from the plan
///   - Accumulated wisdom from previous tasks
///   - MUST DO / MUST NOT DO constraints
///   - Verification requirements
pub fn build_task_delegation_prompt(
    task: &ParsedTask,
    wisdom_context: &str,
    plan_context: &str,
) -> String {
    let mut prompt = String::new();

    // Task header.
    prompt.push_str(&format!("## Task {}: {}\n\n", task.number, task.title));

    // Plan context (so Junior understands the big picture).
    if !plan_context.is_empty() {
        prompt.push_str("<plan-context>\n");
        prompt.push_str(plan_context);
        prompt.push_str("\n</plan-context>\n\n");
    }

    // Accumulated wisdom.
    if !wisdom_context.is_empty() {
        prompt.push_str(wisdom_context);
        prompt.push_str("\n\n");
    }

    // Constraints.
    prompt.push_str("<constraints>\n");
    prompt.push_str("MUST DO:\n");
    prompt.push_str("- Complete the task fully before responding.\n");
    prompt.push_str("- Create a todo list with `todo` before starting.\n");
    prompt.push_str("- Mark all todos complete before finishing.\n");
    prompt.push_str("- If you encounter errors, fix them before completing.\n");
    prompt.push_str("\n");
    prompt.push_str("MUST NOT DO:\n");
    prompt.push_str("- Do NOT delegate further (no task/delegate_task calls).\n");
    prompt.push_str("- Do NOT modify .omo/plans/ files.\n");
    prompt.push_str("- Do NOT start work beyond this task's scope.\n");
    prompt.push_str("</constraints>\n\n");

    // Verification requirement.
    prompt.push_str("<verification>\n");
    prompt.push_str("Before marking this task complete, verify your changes:\n");
    prompt.push_str("- Run relevant tests or build commands.\n");
    prompt.push_str("- Use lsp_diagnostics if available to check for errors.\n");
    prompt.push_str("- Report what you verified.\n");
    prompt.push_str("</verification>\n");

    prompt
}

// ─── Start-Work Runtime ──────────────────────────────────────────────

/// The resolved state for a /start-work invocation.
#[derive(Debug, Clone)]
pub struct StartWorkResult {
    /// Whether this is a resume (true) or fresh start (false).
    pub is_resume: bool,
    /// The agent to activate (always "atlas" if available, else "sisyphus").
    pub agent: String,
    /// The plan path to execute.
    pub plan_path: Option<PathBuf>,
    /// The boulder state (created or existing).
    pub boulder: BoulderState,
    /// Context info to inject into the first prompt.
    pub context_injection: String,
}

/// Execute the /start-work runtime logic.
///
/// Port of OMO's start-work hook:
/// 1. Check for existing boulder.json → resume mode
/// 2. If no boulder, find latest plan in .omo/plans/ → init mode
/// 3. Create/update boulder state
/// 4. Return context injection for Atlas
pub fn start_work(
    omo_dir: &Path,
    session_id: &str,
    explicit_plan_name: Option<&str>,
) -> Result<StartWorkResult, String> {
    let boulder_path = omo_dir.join("boulder.json");
    let plans_dir = omo_dir.join("plans");

    // Check for existing boulder state (resume mode).
    if boulder_path.exists() {
        let existing = BoulderState::read(omo_dir);
        // Only resume if there are works.
        if !existing.works.is_empty() {
            // Find active work for this session or the most recent active work.
            let active_work = existing
                .works
                .iter()
                .find(|w| w.session_id == session_id && w.status == BoulderWorkStatus::Active)
                .or_else(|| {
                    existing
                        .works
                        .iter()
                        .filter(|w| w.status == BoulderWorkStatus::Active)
                        .last()
                });

            if let Some(work) = active_work {
                let plan_path = PathBuf::from(&work.plan_path);
                let progress = calculate_plan_progress(&plan_path);

                let context = format!(
                    "<session-context>\n\
                     Resuming work on plan: {}\n\
                     Plan path: {}\n\
                     Progress: {} of {} tasks complete\n\
                     \n\
                     You are Atlas. Continue executing the plan from where it was left off.\n\
                     Read the plan file, identify incomplete tasks, and delegate them.\n\
                     </session-context>",
                    work.plan_name,
                    work.plan_path,
                    progress.completed,
                    progress.total,
                );

                return Ok(StartWorkResult {
                    is_resume: true,
                    agent: "atlas".to_string(),
                    plan_path: Some(plan_path),
                    boulder: existing,
                    context_injection: context,
                });
            }
        }
    }

    // Init mode: find the most recent plan (or use explicit name).
    let plan_path = if let Some(name) = explicit_plan_name {
        let slug = name.trim().replace(' ', "-");
        plans_dir.join(format!("{}.md", slug))
    } else {
        find_latest_plan(&plans_dir)?
    };

    if !plan_path.exists() {
        let hint = if explicit_plan_name.is_some() {
            format!("Plan file not found: {}", plan_path.display())
        } else {
            "No plans found in .omo/plans/. Create one with Prometheus (@plan) first.".to_string()
        };
        return Err(hint);
    }

    let plan_name = plan_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unnamed")
        .to_string();

    // Create boulder state.
    let work = BoulderWork {
        id: format!("work_{}", chrono::Utc::now().timestamp()),
        plan_path: plan_path.to_string_lossy().to_string(),
        plan_name: plan_name.clone(),
        session_id: session_id.to_string(),
        agent: "atlas".to_string(),
        worktree_path: None,
        status: BoulderWorkStatus::Active,
        started_at: chrono::Utc::now().to_rfc3339(),
    };

    let mut boulder = BoulderState::read(omo_dir);
    boulder.works.push(work);
    let _ = boulder.write(omo_dir);

    let context = format!(
        "<session-context>\n\
         Starting new work on plan: {}\n\
         Plan path: {}\n\
         Session ID: {}\n\
         Timestamp: {}\n\
         \n\
         You are Atlas. Read the plan file and begin executing from Task 1.\n\
         Delegate each task to Sisyphus-Junior via delegate_task(category=\"deep\").\n\
         Accumulate wisdom in .omo/notepads/{}/ after each task.\n\
         Verify all tasks are complete before reporting done.\n\
         </session-context>",
        plan_name,
        plan_path.display(),
        session_id,
        chrono::Utc::now().to_rfc3339(),
        plan_name,
    );

    Ok(StartWorkResult {
        is_resume: false,
        agent: "atlas".to_string(),
        plan_path: Some(plan_path),
        boulder,
        context_injection: context,
    })
}

/// Plan progress (completed/total tasks).
#[derive(Debug, Clone)]
pub struct PlanProgress {
    pub completed: usize,
    pub total: usize,
}

/// Calculate progress from a plan file by counting checked vs unchecked boxes.
fn calculate_plan_progress(plan_path: &Path) -> PlanProgress {
    let content = std::fs::read_to_string(plan_path).unwrap_or_default();
    let total = content.lines().filter(|l| l.starts_with("- [")).count();
    let completed = content
        .lines()
        .filter(|l| l.starts_with("- [x]") || l.starts_with("- [X]"))
        .count();
    PlanProgress { completed, total }
}

/// Find the most recently modified .md file in .omo/plans/.
fn find_latest_plan(plans_dir: &Path) -> Result<PathBuf, String> {
    if !plans_dir.exists() {
        return Err("No .omo/plans/ directory found.".to_string());
    }
    let entries = std::fs::read_dir(plans_dir)
        .map_err(|e| format!("Failed to read plans directory: {}", e))?;

    let mut plans: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) == Some("md") {
                let mtime = e.metadata().ok()?.modified().ok()?;
                Some((mtime, path))
            } else {
                None
            }
        })
        .collect();

    if plans.is_empty() {
        return Err("No plan files found in .omo/plans/.".to_string());
    }

    // Sort by modification time, newest first.
    plans.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(plans[0].1.clone())
}

// ─── Prometheus Write Restriction ────────────────────────────────────

/// Check if a path is within the `.omo/` directory (Prometheus constraint).
///
/// Prometheus is restricted to writing markdown files under `.omo/`.
/// This function validates that a write target is within that scope.
pub fn is_prometheus_write_allowed(path: &str, omo_dir: &Path) -> bool {
    let resolved = std::path::PathBuf::from(path);
    let abs = if resolved.is_absolute() {
        resolved
    } else {
        std::env::current_dir()
            .unwrap_or_default()
            .join(resolved)
    };

    // Must be inside .omo/
    if !abs.starts_with(omo_dir) {
        return false;
    }

    // Must be a .md file
    abs.extension()
        .and_then(|e| e.to_str())
        .map(|e| e == "md")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AvailableModelSet;

    fn test_registry() -> AgentRegistry {
        let mut available = AvailableModelSet::new();
        available.add_model("claude-sonnet-4-6".into());
        available.add_model("claude-opus-4-8".into());
        available.add_provider("anthropic".into());
        AgentRegistry::build(available, &Default::default())
    }

    #[test]
    fn route_category_to_junior() {
        let registry = test_registry();
        let req = OmoDelegationRequest {
            prompt: "test".into(),
            description: None,
            category: Some("deep".into()),
            subagent_type: None,
            load_skills: vec![],
            run_in_background: false,
        };
        let route = route_delegation(&req, &registry).unwrap();
        assert_eq!(route.agent_name, "sisyphus-junior");
    }

    #[test]
    fn route_subagent_to_named_agent() {
        let registry = test_registry();
        let req = OmoDelegationRequest {
            prompt: "test".into(),
            description: None,
            category: None,
            subagent_type: Some("oracle".into()),
            load_skills: vec![],
            run_in_background: false,
        };
        let route = route_delegation(&req, &registry).unwrap();
        assert_eq!(route.agent_name, "oracle");
        assert!(route.denied_tools.contains(&"write_file".to_string()));
    }

    #[test]
    fn route_rejects_both_category_and_subagent() {
        let registry = test_registry();
        let req = OmoDelegationRequest {
            prompt: "test".into(),
            description: None,
            category: Some("deep".into()),
            subagent_type: Some("oracle".into()),
            load_skills: vec![],
            run_in_background: false,
        };
        assert!(route_delegation(&req, &registry).is_err());
    }

    #[test]
    fn route_rejects_neither() {
        let registry = test_registry();
        let req = OmoDelegationRequest {
            prompt: "test".into(),
            description: None,
            category: None,
            subagent_type: None,
            load_skills: vec![],
            run_in_background: false,
        };
        assert!(route_delegation(&req, &registry).is_err());
    }

    #[test]
    fn oracle_denies_write_tools() {
        let registry = test_registry();
        let oracle = registry.get("oracle").unwrap();
        assert!(!oracle.tool_permissions.is_allowed("write_file"));
        assert!(!oracle.tool_permissions.is_allowed("patch"));
        assert!(!oracle.tool_permissions.is_allowed("delegate_task"));
        assert!(oracle.tool_permissions.is_allowed("read_file"));
        assert!(oracle.tool_permissions.is_allowed("search_files"));
    }

    #[test]
    fn atlas_denies_write_but_allows_terminal() {
        let registry = test_registry();
        let atlas = registry.get("atlas").unwrap();
        assert!(!atlas.tool_permissions.is_allowed("write_file"));
        assert!(!atlas.tool_permissions.is_allowed("patch"));
        assert!(atlas.tool_permissions.is_allowed("terminal"));
        assert!(atlas.tool_permissions.is_allowed("delegate_task"));
    }

    #[test]
    fn prometheus_denies_terminal_and_delegate() {
        let registry = test_registry();
        let prometheus = registry.get("prometheus").unwrap();
        assert!(!prometheus.tool_permissions.is_allowed("terminal"));
        assert!(!prometheus.tool_permissions.is_allowed("delegate_task"));
        assert!(prometheus.tool_permissions.is_allowed("read_file"));
    }

    #[test]
    fn junior_denies_delegate_task() {
        let registry = test_registry();
        let junior = registry.get("sisyphus-junior").unwrap();
        assert!(!junior.tool_permissions.is_allowed("delegate_task"));
        assert!(!junior.tool_permissions.is_allowed("task"));
        assert!(junior.tool_permissions.is_allowed("read_file"));
        assert!(junior.tool_permissions.is_allowed("write_file"));
    }

    #[test]
    fn boulder_push_when_incomplete_todos() {
        let reminder = boulder_push_reminder(&[
            "Implement user service".into(),
            "Add validation".into(),
        ]);
        assert!(reminder.is_some());
        let msg = reminder.unwrap();
        assert!(msg.contains("SYSTEM REMINDER"));
        assert!(msg.contains("Implement user service"));
        assert!(msg.contains("DO NOT respond"));
    }

    #[test]
    fn boulder_push_empty_no_reminder() {
        let reminder = boulder_push_reminder(&[]);
        assert!(reminder.is_none());
    }

    #[test]
    fn extract_wisdom_categorizes() {
        let response = r#"
        I discovered that the project uses a factory pattern for services.
        Decided to use dependency injection for the new module.
        Issue: the database connection pool was too small.
        Tests passed: 15/15.
        Unresolved: need to add rate limiting later.
        "#;
        let wisdom = extract_wisdom(response, "Add service module");
        assert!(!wisdom.learnings.is_empty());
        assert!(!wisdom.decisions.is_empty());
        assert!(!wisdom.issues.is_empty());
        assert!(!wisdom.verification.is_empty());
        assert!(!wisdom.problems.is_empty());
    }

    #[test]
    fn extract_wisdom_empty_response_no_learning() {
        let wisdom = extract_wisdom("", "test task");
        assert!(wisdom.learnings.is_empty());
    }

    #[test]
    fn build_task_prompt_includes_constraints() {
        let task = ParsedTask {
            number: 1,
            title: "Implement user service".into(),
            is_final_verification: false,
            dependencies: vec![],
            completed: false,
        };
        let prompt = build_task_delegation_prompt(&task, "", "");
        assert!(prompt.contains("Task 1"));
        assert!(prompt.contains("Implement user service"));
        assert!(prompt.contains("MUST DO"));
        assert!(prompt.contains("MUST NOT DO"));
        assert!(prompt.contains("verification"));
    }

    #[test]
    fn build_task_prompt_includes_wisdom() {
        let task = ParsedTask {
            number: 1,
            title: "Test".into(),
            is_final_verification: false,
            dependencies: vec![],
            completed: false,
        };
        let wisdom = "<accumulated-wisdom>\nUse pattern X\n</accumulated-wisdom>";
        let prompt = build_task_delegation_prompt(&task, wisdom, "");
        assert!(prompt.contains("accumulated-wisdom"));
        assert!(prompt.contains("Use pattern X"));
    }

    #[test]
    fn prometheus_write_restriction() {
        let omo_dir = std::path::PathBuf::from("/project/.omo");
        assert!(is_prometheus_write_allowed("/project/.omo/plans/test.md", &omo_dir));
        assert!(is_prometheus_write_allowed("/project/.omo/notepads/x/learnings.md", &omo_dir));
        assert!(!is_prometheus_write_allowed("/project/src/main.rs", &omo_dir));
        assert!(!is_prometheus_write_allowed("/project/.omo/config.json", &omo_dir));
        assert!(!is_prometheus_write_allowed("/project/.omo/plans/test.txt", &omo_dir));
    }

    #[test]
    fn prepare_plan_parses_tasks() {
        let plan = r#"
# Test Plan

- [ ] 1. First task
- [ ] 2. Second task
- [x] 3. Completed task
- [ ] F1. Final verification
"#;
        let (impl_tasks, verify_tasks) = prepare_plan_execution(plan).unwrap();
        assert_eq!(impl_tasks.len(), 3);
        assert_eq!(verify_tasks.len(), 1);
        assert_eq!(impl_tasks[0].number, 1);
        assert_eq!(verify_tasks[0].number, 1);
        assert!(verify_tasks[0].is_final_verification);
    }
}
