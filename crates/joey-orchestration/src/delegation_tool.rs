//! The `delegate_task` tool — spawn one or more subagents in isolated contexts.
//!
//! Registered by higher crates (joey-cli) after constructing a SubagentManager.
//! The tool parses single/batch mode from args, calls dispatch_single or
//! dispatch_batch, and formats results per the delegation-tool contract.

use async_trait::async_trait;
use joey_agent_core::{AgentConfig, AgentEvent};
use joey_core::Config;
#[allow(unused_imports)] // Used via the `dyn ModelAllocator` field + trait methods.
use joey_llm_selector::ModelAllocator;
use joey_tools::registry::{Tool, ToolResult};
use joey_tools::{ToolContext, ToolRegistry};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::manager::SubagentManager;
use crate::types::{DelegationRequest, SubagentRole, TaskSpec};
use crate::CategoryResolver;

// ---------------------------------------------------------------------------
// HyperCode role routing (explorer / implementor)
// ---------------------------------------------------------------------------

/// Per-role delegation settings resolved from the `hypercode.*` config
/// tables. Self-contained mirror of joey-cli's RoleConfig so this crate
/// stays independent (the config keys are the contract).
#[derive(Debug, Clone, Default)]
pub(crate) struct HyperRoleSettings {
    pub model: String,
    pub max_tokens: u64,
    pub max_turns: u64,
    pub reasoning_level: String,
}

impl HyperRoleSettings {
    fn from_config_tree(tree: &Config, table: &str, provider: &str) -> Self {
        let mut s = Self::default();
        let table_key = format!("hypercode.{}.{}", table, provider);
        if let Some(node) = tree.get(&table_key) {
            if let Some(map) = node.as_mapping() {
                let get = |k: &str| map.get(serde_yaml::Value::String(k.to_string()));
                if let Some(v) = get("model").and_then(|v| v.as_str()) {
                    s.model = v.to_string();
                }
                if let Some(v) = get("max_tokens").and_then(|v| v.as_u64()) {
                    s.max_tokens = v;
                }
                if let Some(v) = get("max_turns").and_then(|v| v.as_u64()) {
                    s.max_turns = v;
                }
                if let Some(v) = get("reasoning_level").and_then(|v| v.as_str()) {
                    s.reasoning_level = v.to_string();
                }
            }
        }
        s
    }
}

/// The delegation role requested via the tool's `role` parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HyperRole {
    Explorer,
    Implementor,
}

impl HyperRole {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "explorer" | "explore" => Some(HyperRole::Explorer),
            "implementor" | "implement" | "builder" => Some(HyperRole::Implementor),
            _ => None,
        }
    }

    /// Config table name (`hypercode.<table>.<provider>`).
    fn table(self) -> &'static str {
        match self {
            HyperRole::Explorer => "explorer",
            HyperRole::Implementor => "implementor",
        }
    }

    /// Toolsets the role operates with. Explorer is READ-ONLY on files but
    /// has terminal (diagnostic commands for the orchestrator); Implementor
    /// owns the write path plus verification builds/tests.
    fn toolsets(self) -> &'static [&'static str] {
        match self {
            HyperRole::Explorer => &["file-read", "terminal", "web"],
            HyperRole::Implementor => &["file", "terminal", "web"],
        }
    }

    fn prompt_append(self) -> &'static str {
        match self {
            HyperRole::Explorer => EXPLORER_DIRECTIVE,
            HyperRole::Implementor => IMPLEMENTOR_DIRECTIVE,
        }
    }
}

/// Role directives injected as the child's extra instructions. Wording kept
/// in joey-orchestration (not joey-cli) so the tool is self-contained.
pub(crate) const EXPLORER_DIRECTIVE: &str = "\
You are an EXPLORER subagent. Your parent orchestrator has NO tools — you are \
its eyes and hands for everything read-only.\n\
- Investigate code, docs, and the environment; run read-only/diagnostic \
commands (rg, ls, git log/diff, cargo check, --help) and report ACTUAL output.\n\
- NEVER modify, create, or delete files. Never run state-changing commands.\n\
- Your final message is the orchestrator's ONLY source of truth: exact paths, \
symbols, command output, risks. Answer precisely what was asked.\n\
- Keep it under 600 tokens; lead with the answer, then evidence.";

pub(crate) const IMPLEMENTOR_DIRECTIVE: &str = "\
You are an IMPLEMENTOR subagent. You own the write path for your assigned task.\n\
- Edit files AND run the builds/tests/commands that verify your own work; \
report the real verification result.\n\
- Follow the brief exactly; if something is missing, make the smallest \
reasonable decision and note it.\n\
- Touch only the files in your assignment; siblings work in parallel.\n\
- Report exactly what changed, file by file, plus verification. Under 500 tokens.";

/// Resolve a DelegationRequest patch for a HyperCode role: toolsets, role
/// config (model/turns/tokens/reasoning) unless the caller set explicit
/// overrides, and the role directive as prompt_append (appended after any
/// caller-provided append so both survive).
///
/// `explicit_toolsets` = the tool call included its own `toolsets` array
/// (keeps user control; role defaults only fill gaps).
pub(crate) fn apply_hyper_role(
    req: &mut DelegationRequest,
    role: HyperRole,
    tree: &Config,
    provider: &str,
) {
    let settings = HyperRoleSettings::from_config_tree(tree, role.table(), provider);
    if req.toolsets.is_empty() {
        req.toolsets = role.toolsets().iter().map(|s| s.to_string()).collect();
    }
    if req.model.is_none() && !settings.model.is_empty() {
        req.model = Some(settings.model.clone());
    }
    if req.max_turns.is_none() && settings.max_turns > 0 {
        req.max_turns = Some(settings.max_turns as usize);
    }
    if req.max_tokens.is_none() && settings.max_tokens > 0 {
        req.max_tokens = Some(settings.max_tokens as u32);
    }
    if req.reasoning.is_none() && !settings.reasoning_level.is_empty() {
        req.reasoning = parse_role_reasoning(&settings.reasoning_level);
    }
    // Role directive stacks with any existing prompt_append (caller content
    // first, role identity second).
    let directive = role.prompt_append().to_string();
    req.prompt_append = Some(match req.prompt_append.take() {
        Some(existing) if !existing.is_empty() => format!("{existing}\n\n{directive}"),
        _ => directive,
    });
}

/// Parse a HyperCode role name (shared by single + batch paths).
pub(crate) fn hyper_role_parse(s: &str) -> Option<HyperRole> {
    HyperRole::parse(s)
}

/// Parse a reasoning-level string ("none"|"low"|"medium"|"high"|"") the same
/// way joey-cli's hypercode module does.
pub(crate) fn parse_role_reasoning(level: &str) -> Option<joey_providers::ReasoningEffort> {
    match level.trim().to_lowercase().as_str() {
        "" | "inherit" => None,
        "none" | "off" => Some(joey_providers::ReasoningEffort::Disabled),
        other => Some(joey_providers::ReasoningEffort::Level(other.to_string())),
    }
}

/// The delegate_task tool. Holds an Arc<SubagentManager> for dispatching.
pub struct DelegateTask {
    manager: Arc<SubagentManager>,
    parent_config: AgentConfig,
    parent_config_tree: Config,
    base_registry: ToolRegistry,
    /// Event channel for emitting orchestration events to the parent's UI.
    event_tx: Option<mpsc::UnboundedSender<AgentEvent>>,
    /// Optional OMO category resolver (None = raw delegate_task without
    /// category/subagent_type support).
    resolver: Option<Arc<dyn CategoryResolver>>,
    /// Optional dynamic model allocator (feature 011, T028). When the resolved
    /// subagent model is `auto`, the tool consults the allocator's
    /// `resolve(ModuleId::Subagent, …)` to pick a concrete model id before
    /// dispatch. None when the selector is inactive (byte-identical to
    /// pre-feature-011).
    model_allocator: Option<Arc<dyn joey_llm_selector::ModelAllocator>>,
}

impl DelegateTask {
    /// Parse + validate the top-level `budgets` tool arg (T021, FR-011).
    ///
    /// `None` when the caller omitted `budgets` entirely (no caps — byte-
    /// identical to pre-feature dispatch). A present-but-invalid object
    /// (unknown shape, negative value, or any value ≤ 0) is a clean
    /// [`ToolResult::Error`] naming the offending field — caught here so the
    /// serde rejection inside [`crate::types::Budgets`] never surfaces as a
    /// panic, and NOTHING dispatches.
    fn parse_budgets(args: &Value) -> Result<Option<crate::types::Budgets>, ToolResult> {
        let Some(v) = args.get("budgets") else {
            return Ok(None);
        };
        if v.is_null() {
            return Ok(None);
        }
        match serde_json::from_value::<crate::types::Budgets>(v.clone()) {
            Ok(b) => Ok(Some(b)),
            Err(e) => Err(ToolResult::Error(format!("Invalid budgets: {e}"))),
        }
    }

    pub fn new(
        manager: Arc<SubagentManager>,
        parent_config: AgentConfig,
        parent_config_tree: Config,
        base_registry: ToolRegistry,
        event_tx: Option<mpsc::UnboundedSender<AgentEvent>>,
        resolver: Option<Arc<dyn CategoryResolver>>,
    ) -> Self {
        Self {
            manager,
            parent_config,
            parent_config_tree,
            base_registry,
            event_tx,
            resolver,
            model_allocator: None,
        }
    }

    /// Set the dynamic model allocator (feature 011, T028). Called by the CLI
    /// after agent construction when the selector is active.
    pub fn set_model_allocator(&mut self, allocator: Arc<dyn joey_llm_selector::ModelAllocator>) {
        self.model_allocator = Some(allocator);
    }
}

#[async_trait]
impl Tool for DelegateTask {
    fn name(&self) -> &str {
        "delegate_task"
    }

    fn toolset(&self) -> &str {
        "delegation"
    }

    fn emoji(&self) -> &str {
        "🤖"
    }

    fn description(&self) -> &str {
        "Spawn one or more subagents to work on tasks in isolated contexts. Each \
         subagent gets its own conversation history, toolset, and execution budget. \
         The parent receives only a concise summary from each child. By default, \
         subagent traces are ephemeral (discarded after summary); set persist=true \
         to store the child session for later session_search recall. \
         BACKGROUND: set background=true to return a work handle immediately \
         ('[BACKGROUND] id=<child_id> goal=<goal> started') while the child runs; \
         blocking (default) waits for results. \
         PARALLELISM: batch `tasks` all launch simultaneously (bounded only by \
         system capacity) — for codebase exploration or multi-part implementation, \
         ALWAYS fan out one task per concern in a single batch call instead of \
         sequential single-goal calls; this cuts wall-clock time dramatically \
         by parallelizing inference."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "goal": {
                    "type": "string",
                    "description": "The task goal for the subagent. Required for single-task mode."
                },
                "role": {
                    "type": "string",
                    "enum": ["explorer", "implementor"],
                    "description": "HyperCode role routing. 'explorer' = read-only investigation (file-read + terminal + web; runs diagnostic commands on your behalf; NEVER writes). 'implementor' = writes files AND runs builds/tests to verify its own work. When set, the role's configured model/turns/tokens/reasoning (hypercode.explorer / hypercode.implementor in config) apply unless explicitly overridden here."
                },
                "context": {
                    "type": "string",
                    "description": "Additional context to pass to the subagent. Include file paths, error messages, project structure, constraints. The subagent knows nothing about the parent conversation."
                },
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "goal": {"type": "string"},
                            "context": {"type": "string"},
                            "model": {"type": "string"},
                            "toolsets": {"type": "array", "items": {"type": "string"}},
                            "role": {"type": "string", "enum": ["explorer", "implementor"], "description": "HyperCode role routing for this task: 'explorer' (read-only + diagnostic commands) or 'implementor' (writes + verifies). Applies the role's config and directive unless overridden per-task."}
                        },
                        "required": ["goal"]
                    },
                    "description": "Batch mode: array of task specs for parallel dispatch. Each runs concurrently and independently. If provided, 'goal' is ignored. Set role:'explorer'/'implementor' per task to fan out mixed read/write waves in ONE call."
                },
                "model": {
                    "type": "string",
                    "description": "Override model for the subagent(s). If omitted, uses delegation.default_model from config or the parent's model."
                },
                "toolsets": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Restrict the subagent's available tools to these toolsets. If omitted, all enabled tools are available (minus delegate_task for leaf role)."
                },
                "persist": {
                    "type": "boolean",
                    "description": "If true, persist the subagent's full session trace to the session store for later session_search recall. Default: false (ephemeral).",
                    "default": false
                },
                "background": {
                    "type": "boolean",
                    "description": "If true, return immediately with a work handle per task ('[BACKGROUND] id=<child_id> goal=<goal> started') instead of blocking until the subagent finishes; the work runs under the same concurrency limits (excess queues). Check status later via subagent_control. Default: false (blocking).",
                    "default": false
                },
                "budgets": {
                    "type": "object",
                    "description": "Per-child resource budgets (feature 020, FR-011). On breach the child is stopped with reason budget_exceeded (at most one in-flight action completes past detection). BATCH SEMANTICS: a top-level budgets object applies to EVERY child in the batch; per-task budgets overrides are out of scope. BLOCKING PATH: only max_turns is enforced (as the child's turn cap); max_tokens/max_wall_clock_secs are enforced on the background path only. Every present value must be > 0.",
                    "properties": {
                        "max_turns": {"type": "integer", "minimum": 1, "description": "Max child iterations; an iteration beyond this stops the child (BudgetExceeded)."},
                        "max_tokens": {"type": "integer", "minimum": 1, "description": "Max cumulative tokens (prompt + completion). Exceeding stops the child (background path)."},
                        "max_wall_clock_secs": {"type": "integer", "minimum": 1, "description": "Max wall-clock seconds for the child. Exceeding stops the child (background path)."}
                    },
                    "additionalProperties": false
                },
                "category": {
                    "type": "string",
                    "description": "OMO category name (e.g. 'quick', 'visual-engineering', 'deep'). When set, routes through Sisyphus-Junior with the category's resolved model and prompt_append. Mutually exclusive with 'subagent_type'."
                },
                "subagent_type": {
                    "type": "string",
                    "description": "OMO subagent type (e.g. 'oracle', 'explore', 'librarian'). When set, spawns the named agent with its resolved model. Mutually exclusive with 'category'."
                },
                "load_skills": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Skill names to load and prepend to the subagent's system prompt. Only effective when 'category' is also specified."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        // T021: parse + validate budgets BEFORE any dispatch (FR-011): an
        // invalid object (value ≤ 0, negative, unknown shape) errors cleanly
        // naming the field and NOTHING dispatches — single AND batch paths.
        let budgets = match Self::parse_budgets(&args) {
            Ok(b) => b,
            Err(e) => return e,
        };

        // Check if batch mode (tasks array provided).
        let tasks_value = args.get("tasks");
        let is_batch = tasks_value.is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty()));

        if is_batch {
            return self.execute_batch(tasks_value.unwrap(), &args, budgets).await;
        }

        // Single-task mode.
        let goal = match args.get("goal").and_then(|v| v.as_str()) {
            Some(g) => g.to_string(),
            None => {
                return ToolResult::Error(
                    "delegate_task requires 'goal' (single mode) or 'tasks' (batch mode)".to_string(),
                );
            }
        };

        // Extract OMO category/subagent_type (T057/T058/T135).
        let category = args.get("category").and_then(|v| v.as_str()).map(String::from);
        let subagent_type = args.get("subagent_type").and_then(|v| v.as_str()).map(String::from);
        let load_skills: Vec<String> = args
            .get("load_skills")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Validate mutual exclusivity (BC-011).
        if category.is_some() && subagent_type.is_some() {
            return ToolResult::Error(
                "Cannot specify both 'category' and 'subagent_type' — they are mutually exclusive (BC-011).".to_string(),
            );
        }

        // Resolve category or subagent_type to model + prompt_append (T057/T135).
        let mut resolved_model = None;
        let mut prompt_append = None;
        if let Some(ref cat) = category {
            if let Some(ref resolver) = self.resolver {
                match resolver.resolve_category(cat) {
                    Some(r) => {
                        resolved_model = Some(r.model);
                        prompt_append = r.prompt_append;
                    }
                    None => {
                        return ToolResult::Error(format!(
                            "Category '{}' is unknown or its model chain could not resolve against available providers.",
                            cat
                        ));
                    }
                }
            } else {
                return ToolResult::Error(
                    "Category delegation requires an OMO category resolver, but none is configured. Use 'model' directly instead.".to_string(),
                );
            }
        }
        if let Some(ref sat) = subagent_type {
            if let Some(ref resolver) = self.resolver {
                match resolver.resolve_subagent_type(sat) {
                    Some(r) => {
                        resolved_model = Some(r.model);
                        prompt_append = r.prompt_append;
                    }
                    None => {
                        return ToolResult::Error(format!(
                            "Subagent type '{}' is unknown or unavailable.",
                            sat
                        ));
                    }
                }
            } else {
                return ToolResult::Error(
                    "Subagent type delegation requires an OMO category resolver, but none is configured. Use 'model' directly instead.".to_string(),
                );
            }
        }

        // Emit CategoryDelegation event if category was used.
        if let Some(ref cat) = category {
            if let Some(tx) = &self.event_tx {
                let model_for_event = resolved_model
                    .as_deref()
                    .unwrap_or(&self.parent_config.model);
                let _ = tx.send(AgentEvent::CategoryDelegation {
                    category: cat.clone(),
                    model: model_for_event.to_string(),
                });
            }
        }

        // Feature 011 (T028): when the resolved subagent model is `auto` (the
        // activation sentinel) and a dynamic model allocator is wired, resolve
        // a concrete model id for the Subagent module before dispatch. This is
        // the third intercept point (research.md §2). When the allocator is
        // None or inactive, `auto` falls through to the parent model
        // (byte-identical to pre-feature-011).
        let mut effective_model = resolved_model
            .or_else(|| args.get("model").and_then(|v| v.as_str()).map(String::from));
        if effective_model.as_deref().unwrap_or(&self.parent_config.model) == "auto" {
            if let Some(allocator) = &self.model_allocator {
                if allocator.is_active() {
                    let alloc = allocator.resolve(
                        joey_llm_selector::ModuleId::Subagent,
                        false, // subagents don't carry images at dispatch time
                        true,  // subagents need tools
                        0,     // token_budget_hint: no hard gate
                    );
                    // Never send "auto" to the API (FR-020).
                    if alloc.model_id != "auto" {
                        effective_model = Some(alloc.model_id);
                    }
                }
            }
        }

        let req = DelegationRequest {
            goal: goal.clone(),
            context: args.get("context").and_then(|v| v.as_str()).map(String::from),
            tasks: Vec::new(),
            model: effective_model,
            toolsets: args
                .get("toolsets")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            max_turns: None,
            reasoning: None,
            max_tokens: None,
            persist: args.get("persist").and_then(|v| v.as_bool()).unwrap_or(false),
            role: SubagentRole::Leaf,
            workdir: None,
            category,
            subagent_type,
            load_skills,
            prompt_append,
        };

        // HyperCode role routing: `role: "explorer"|"implementor"` fills
        // toolsets/model/turns/tokens/reasoning from the role's config table
        // (gaps only — explicit args win) and injects the role directive.
        let mut req = req;
        if let Some(role_str) = args.get("role").and_then(|v| v.as_str()) {
            match HyperRole::parse(role_str) {
                Some(role) => {
                    apply_hyper_role(&mut req, role, &self.parent_config_tree, &self.parent_config.provider);
                }
                None => {
                    return ToolResult::Error(format!(
                        "Unknown role '{role_str}'. Use 'explorer' or 'implementor'."
                    ));
                }
            }
        }

        // T021 budgets. Background: the whole object rides the budgeted
        // dispatcher — the T020 parent-side watcher enforces turns/tokens/
        // wall-clock and stops the child with BudgetExceeded (FR-016 notice).
        // Do NOT clamp req.max_turns here: the child's own turn cap would end
        // it naturally at the boundary before the watcher ever sees
        // IterationStart(max+1) — the watcher needs that headroom to be the
        // enforcing leg (strict-> breach math, D6).

        // Background mode (feature 020, FR-001): hand the child to the
        // background dispatcher and return a handle line NOW (SC-001).
        // background=false / unset keeps the blocking path below untouched
        // (FR-002 byte parity — pinned by tests/background.rs T007).
        if args.get("background").and_then(|v| v.as_bool()).unwrap_or(false) {
            let handle = crate::background::dispatch_background_with_notices_and_budgets(
                &self.manager,
                &req,
                &self.parent_config,
                &self.parent_config_tree,
                &self.base_registry,
                self.event_tx.as_ref(),
                ctx,
                budgets,
            );
            return ToolResult::Text(format!(
                "[BACKGROUND] id={} goal={} started",
                handle.child_id, handle.goal
            ));
        }

        // Blocking path (T021): no watcher exists here, so max_turns is
        // enforced as the child's turn cap via req.max_turns (the one leg
        // that requires no manager redesign — dispatch_single already
        // honors req.max_turns over the delegation default). The cap ends
        // the child at the boundary (natural agent stop). max_tokens /
        // max_wall_clock_secs are DEFERRED on the blocking path: enforcing
        // them needs a live usage observer, which only the background
        // watcher provides.
        if let Some(mt) = budgets.and_then(|b| b.max_turns) {
            req.max_turns = Some(mt as usize);
        }

        let result = self
            .manager
            .dispatch_single(
                &req,
                &self.parent_config,
                &self.parent_config_tree,
                &self.base_registry,
                self.event_tx.as_ref(),
            )
            .await;

        if result.success {
            ToolResult::Text(result.summary)
        } else {
            ToolResult::Error(format!(
                "Subagent failed: {}",
                result.error.as_deref().unwrap_or("unknown error")
            ))
        }
    }
}

impl DelegateTask {
    async fn execute_batch(
        &self,
        tasks_value: &Value,
        args: &Value,
        budgets: Option<crate::types::Budgets>,
    ) -> ToolResult {
        let task_specs: Vec<TaskSpec> = match serde_json::from_value(tasks_value.clone()) {
            Ok(specs) => specs,
            Err(e) => {
                return ToolResult::Error(format!("Failed to parse tasks array: {}", e));
            }
        };

        // Extract batch-level overrides from the top-level tool args (FR-006/FR-007).
        let batch_model = args.get("model").and_then(|v| v.as_str()).map(String::from);
        let batch_toolsets: Vec<String> = args
            .get("toolsets")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Batch-level `role` applies to tasks that didn't set their own.
        let batch_role = args.get("role").and_then(|v| v.as_str()).map(String::from);
        let mut task_specs = task_specs;
        if let Some(role) = &batch_role {
            if crate::delegation_tool::hyper_role_parse(role).is_none() {
                return ToolResult::Error(format!(
                    "Unknown role '{role}'. Use 'explorer' or 'implementor'."
                ));
            }
            for spec in &mut task_specs {
                if spec.role.is_none() {
                    spec.role = Some(role.clone());
                }
            }
        }

        // Background mode (feature 020, FR-001 + contracts/delegation-tools.md):
        // a top-level background=true applies to EVERY task in the batch —
        // each dispatches as background and the tool returns one handle line
        // per task, in order, immediately (SC-001). FR-013: nothing is
        // rejected; permits are acquired inside the children under the same
        // limits. background=false / unset keeps the blocking path below
        // untouched (FR-002 byte parity — pinned by tests/background.rs T007).
        let background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
            || task_specs.iter().any(|s| s.background);
        if background {
            // Same request construction the blocking batch path uses
            // (model/toolsets/turns/persist defaults + HyperCode role
            // routing), so background children run with identical config.
            let mut requests = crate::subagent::specs_to_requests(
                &task_specs,
                batch_model.as_deref(),
                &batch_toolsets,
                Some(self.manager.config().default_max_turns),
                self.manager.config().default_persist,
                SubagentRole::Leaf,
            );
            if let Err(e) = crate::subagent::apply_batch_hyper_roles(
                &mut requests,
                &task_specs,
                &self.parent_config_tree,
                &self.parent_config.provider,
            ) {
                tracing::warn!("hypercode role routing failed: {e}");
            }
            // T021: a top-level budgets object applies to EVERY child in the
            // wave (contracts/delegation-tools.md; per-task override out of
            // scope). Do NOT bake budgets.max_turns into req.max_turns here:
            // the child's own turn cap would end it naturally at the
            // boundary before the T020 watcher sees the breach — the watcher
            // is the enforcing leg.
            let pairs: Vec<(DelegationRequest, Option<crate::types::Budgets>)> = requests
                .into_iter()
                .map(|r| (r, budgets))
                .collect();
            let handles = crate::background::dispatch_background_wave_budgeted(
                &self.manager,
                pairs,
                &self.parent_config,
                &self.parent_config_tree,
                &self.base_registry,
                self.event_tx.as_ref(),
            );
            let mut output = String::new();
            for (i, handle) in handles.iter().enumerate() {
                if i > 0 {
                    output.push('\n');
                }
                output.push_str(&format!(
                    "[BACKGROUND] id={} goal={} started",
                    handle.child_id, handle.goal
                ));
            }
            return ToolResult::Text(output);
        }

        // Blocking batch (T021): when budgets.max_turns is set, build the
        // requests with the budgeted turn cap (same construction as
        // dispatch_batch_with_roles, which is otherwise left untouched for
        // the no-budgets byte-parity path). tokens/wall-clock are deferred
        // on the blocking path (no watcher exists — see single path).
        let budgeted_turns = budgets
            .and_then(|b| b.max_turns)
            .map(|t| t as usize);
        let results = if let Some(mt) = budgeted_turns {
            let mut requests = crate::subagent::specs_to_requests(
                &task_specs,
                batch_model.as_deref(),
                &batch_toolsets,
                Some(mt),
                self.manager.config().default_persist,
                SubagentRole::Leaf,
            );
            if let Err(e) = crate::subagent::apply_batch_hyper_roles(
                &mut requests,
                &task_specs,
                &self.parent_config_tree,
                &self.parent_config.provider,
            ) {
                tracing::warn!("hypercode role routing failed: {e}");
            }
            self.manager
                .dispatch_requests(
                    &requests,
                    &self.parent_config,
                    &self.parent_config_tree,
                    &self.base_registry,
                    self.event_tx.as_ref(),
                )
                .await
        } else {
            self.manager
                .dispatch_batch_with_roles(
                    &task_specs,
                    batch_model.as_deref(),
                    &batch_toolsets,
                    &self.parent_config,
                    &self.parent_config_tree,
                    &self.base_registry,
                    self.event_tx.as_ref(),
                )
                .await
        };

        // Format results per the delegation-tool contract.
        let total = results.len();
        let mut output = String::new();
        for (i, r) in results.iter().enumerate() {
            output.push_str(&format!(
                "[{}/{}] goal: {:?}\n",
                i + 1,
                total,
                r.goal
            ));
            if r.success {
                output.push_str("      status: success\n");
                output.push_str(&format!("      summary: {}\n", r.summary));
            } else {
                output.push_str("      status: failed\n");
                output.push_str(&format!(
                    "      error: {}\n",
                    r.error.as_deref().unwrap_or("unknown")
                ));
            }
            output.push_str(&format!(
                "      tokens: {} | duration: {:.1}s\n",
                r.token_usage.total_tokens,
                r.wall_clock.as_secs_f64()
            ));
            if i + 1 < total {
                output.push('\n');
            }
        }

        ToolResult::Text(output)
    }
}

/// The `call_omo_agent` tool — research-only delegation for Sisyphus-Junior.
///
/// Junior's tool permissions allow this tool (but block `delegate_task`),
/// enabling Junior to call explore/librarian/oracle for research while still
/// doing all implementation itself (BC-005, FR-014, T153).
///
/// Semantically identical to delegate_task with subagent_type, but presented
/// under a distinct name so Junior's permission system can gate it separately.
pub struct CallOmoAgent {
    inner: DelegateTask,
}

impl CallOmoAgent {
    pub fn new(
        manager: Arc<SubagentManager>,
        parent_config: AgentConfig,
        parent_config_tree: Config,
        base_registry: ToolRegistry,
        event_tx: Option<mpsc::UnboundedSender<AgentEvent>>,
        resolver: Option<Arc<dyn CategoryResolver>>,
    ) -> Self {
        Self {
            inner: DelegateTask::new(
                manager,
                parent_config,
                parent_config_tree,
                base_registry,
                event_tx,
                resolver,
            ),
        }
    }
}

#[async_trait]
impl Tool for CallOmoAgent {
    fn name(&self) -> &str {
        "call_omo_agent"
    }

    fn toolset(&self) -> &str {
        "delegation"
    }

    fn emoji(&self) -> &str {
        "📞"
    }

    fn description(&self) -> &str {
        "Delegate a research task to another OMO agent (explore, librarian, oracle) \
         for read-only consultation. Use this when you need research, codebase \
         exploration, or architectural guidance — NOT for implementation delegation. \
         The called agent returns a concise summary."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "goal": {
                    "type": "string",
                    "description": "The research goal for the consulted agent."
                },
                "context": {
                    "type": "string",
                    "description": "Additional context to pass to the consulted agent."
                },
                "subagent_type": {
                    "type": "string",
                    "description": "The agent to consult: 'explore' (codebase search), 'librarian' (documentation), or 'oracle' (architecture). Required.",
                    "enum": ["explore", "librarian", "oracle"]
                }
            },
            "required": ["goal", "subagent_type"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        // Force subagent_type to be present (BC-012).
        if args.get("subagent_type").and_then(|v| v.as_str()).is_none() {
            return ToolResult::Error(
                "call_omo_agent requires 'subagent_type' (explore, librarian, or oracle)".to_string(),
            );
        }
        // Delegate to the inner DelegateTask which handles resolution + dispatch.
        self.inner.execute(args, ctx).await
    }
}

#[cfg(test)]
mod role_tests {
    use super::*;
    use crate::types::SubagentRole;

    fn tree_with(yaml: &str) -> Config {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), yaml).unwrap();
        Config::load_from(tmp.path().to_path_buf()).unwrap()
    }

    fn base_req() -> DelegationRequest {
        DelegationRequest {
            goal: "g".into(),
            context: None,
            tasks: Vec::new(),
            model: None,
            toolsets: Vec::new(),
            max_turns: None,
            reasoning: None,
            max_tokens: None,
            persist: false,
            role: SubagentRole::Leaf,
            workdir: None,
            category: None,
            subagent_type: None,
            load_skills: Vec::new(),
            prompt_append: None,
        }
    }

    #[test]
    fn role_parse_variants() {
        assert_eq!(HyperRole::parse("explorer"), Some(HyperRole::Explorer));
        assert_eq!(HyperRole::parse("Explore"), Some(HyperRole::Explorer));
        assert_eq!(HyperRole::parse("implementor"), Some(HyperRole::Implementor));
        assert_eq!(HyperRole::parse("builder"), Some(HyperRole::Implementor));
        assert_eq!(HyperRole::parse("other"), None);
    }

    #[test]
    fn explorer_role_gives_readonly_files_plus_terminal() {
        let tree = tree_with("");
        let mut req = base_req();
        apply_hyper_role(&mut req, HyperRole::Explorer, &tree, "prov");
        assert!(req.toolsets.contains(&"file-read".to_string()));
        assert!(!req.toolsets.contains(&"file".to_string()));
        assert!(req.toolsets.contains(&"terminal".to_string()));
        assert!(req.prompt_append.as_deref().unwrap_or("").contains("EXPLORER"));
        assert!(req.prompt_append.as_deref().unwrap_or("").contains("NEVER modify"));
    }

    #[test]
    fn implementor_role_gives_write_access() {
        let tree = tree_with("");
        let mut req = base_req();
        apply_hyper_role(&mut req, HyperRole::Implementor, &tree, "prov");
        assert!(req.toolsets.contains(&"file".to_string()));
        assert!(req.toolsets.contains(&"terminal".to_string()));
        assert!(req.prompt_append.as_deref().unwrap_or("").contains("IMPLEMENTOR"));
    }

    #[test]
    fn role_settings_fill_gaps_but_explicit_wins() {
        let tree = tree_with(
            "hypercode:\n  explorer:\n    prov:\n      model: cheap-model\n      max_turns: 7\n      max_tokens: 9000\n      reasoning_level: low\n",
        );
        // Gaps filled from config.
        let mut req = base_req();
        apply_hyper_role(&mut req, HyperRole::Explorer, &tree, "prov");
        assert_eq!(req.model.as_deref(), Some("cheap-model"));
        assert_eq!(req.max_turns, Some(7));
        assert_eq!(req.max_tokens, Some(9000));
        assert!(matches!(req.reasoning, Some(joey_providers::ReasoningEffort::Level(l)) if l == "low"));

        // Explicit args win over role config.
        let mut req = base_req();
        req.model = Some("explicit".into());
        req.max_turns = Some(3);
        apply_hyper_role(&mut req, HyperRole::Explorer, &tree, "prov");
        assert_eq!(req.model.as_deref(), Some("explicit"));
        assert_eq!(req.max_turns, Some(3));
    }

    #[test]
    fn role_directive_stacks_with_existing_append() {
        let tree = tree_with("");
        let mut req = base_req();
        req.prompt_append = Some("caller content".into());
        apply_hyper_role(&mut req, HyperRole::Explorer, &tree, "p");
        let append = req.prompt_append.unwrap();
        assert!(append.starts_with("caller content"));
        assert!(append.contains("EXPLORER"));
    }

    #[test]
    fn batch_role_routing_applies_per_task() {
        let tree = tree_with("");
        let tasks = vec![
            TaskSpec { goal: "read thing".into(), context: None, model: None, toolsets: Vec::new(), role: Some("explorer".into()), background: false, budgets: None },
            TaskSpec { goal: "build thing".into(), context: None, model: None, toolsets: Vec::new(), role: Some("implementor".into()), background: false, budgets: None },
            TaskSpec { goal: "plain".into(), context: None, model: None, toolsets: Vec::new(), role: None, background: false, budgets: None },
        ];
        let mut reqs: Vec<DelegationRequest> = tasks.iter().map(|_| base_req()).collect();
        crate::subagent::apply_batch_hyper_roles(&mut reqs, &tasks, &tree, "p").unwrap();
        assert!(reqs[0].toolsets.contains(&"file-read".to_string()));
        assert!(reqs[1].toolsets.contains(&"file".to_string()));
        assert!(reqs[2].toolsets.is_empty(), "no role → untouched");
    }

    #[test]
    fn batch_role_routing_rejects_unknown_role() {
        let tree = tree_with("");
        let tasks = vec![TaskSpec { goal: "x".into(), context: None, model: None, toolsets: Vec::new(), role: Some("wat".into()), background: false, budgets: None }];
        let mut reqs: Vec<DelegationRequest> = vec![base_req()];
        assert!(crate::subagent::apply_batch_hyper_roles(&mut reqs, &tasks, &tree, "p").is_err());
    }
}
