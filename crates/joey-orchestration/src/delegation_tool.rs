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
use crate::types::{DelegationRequest, DelegationResult, SubagentRole, TaskSpec};
use crate::CategoryResolver;

/// The full OMO agent roster valid as `subagent_type` values (feature 025,
/// FR-003/FR-010). Kept in one place so the schema enum, the parameter
/// descriptions, and the unknown-type error can never drift apart. The
/// runtime resolver stays authoritative — this list is advisory guidance.
pub(crate) const OMO_ROSTER: &[&str] = &[
    "sisyphus",
    "hephaestus",
    "prometheus",
    "atlas",
    "oracle",
    "librarian",
    "explore",
    "multimodal-looker",
    "metis",
    "momus",
    "sisyphus-junior",
];

/// Directive synthesized as the `prompt_append` for named-agent (subagent_type)
/// delegations that carry `load_skills` (feature 025, FR-004). Without a
/// prompt_append the child's skill overlay is silently dropped (subagent.rs
/// gates the overlay on a non-empty append), so named dispatches synthesize
/// one that instructs the child to apply its loaded skills under its agent
/// identity — the same overlay machinery the category path uses.
pub(crate) fn named_agent_skill_directive(sat: &str) -> String {
    format!(
        "You are dispatched as the OMO agent '{sat}'. Load and follow each skill listed above within that agent's identity and constraints."
    )
}

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
    /// owns the write path plus targeted verification (scoped builds/tests).
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

/// OMO agent chains for role model defaults (feature 025, FR-005,
/// contracts/role-defaults.md). Mirrors joey-cli's hypercode derivation —
/// the config keys are the contract; both sides derive identically. The
/// orchestration crate cannot depend on joey-omo, so chain members resolve
/// through the injected CategoryResolver (the same bridge the CLI's
/// OmoCategoryResolver provides): the first member the resolver can serve
/// supplies the role's default model.
pub(crate) fn omo_role_chain_default(
    role: HyperRole,
    resolver: &dyn CategoryResolver,
) -> Option<String> {
    let chain: &[&str] = match role {
        HyperRole::Explorer => &["explore", "librarian"],
        HyperRole::Implementor => &["momus"],
    };
    for name in chain {
        if let Some(r) = resolver.resolve_subagent_type(name) {
            if !r.model.is_empty() {
                return Some(r.model);
            }
        }
    }
    None
}

/// Direct 1:1 tier→agent mapping (hypercode.omo_specialists.enabled,
/// default true): Explorer → "explore", Implementor → "hephaestus".
/// Strict: no chain fallback — an unresolvable agent yields None (the
/// caller warns and inherits, FR-006 precedence 3 shape).
pub(crate) fn omo_specialist_agent(role: HyperRole) -> &'static str {
    match role {
        HyperRole::Explorer => "explore",
        HyperRole::Implementor => "hephaestus",
    }
}

/// Read the specialists toggle (default true).
pub(crate) fn omo_specialists_enabled(tree: &Config) -> bool {
    tree.get_bool("hypercode.omo_specialists.enabled", true)
}

/// Resolve per-task `subagent_type` for a batch (mirrors single-mode
/// precedence: the resolved agent model WINS over spec/batch model;
/// identity prompt rides prompt_append). Unknown name or missing
/// resolver is a hard error naming the roster.
pub(crate) fn resolve_batch_subagent_types(
    requests: &mut [DelegationRequest],
    resolver: Option<&Arc<dyn CategoryResolver>>,
) -> Result<(), String> {
    for req in requests.iter_mut() {
        let Some(name) = req.subagent_type.clone() else {
            continue;
        };
        let Some(resolver) = resolver else {
            return Err(
                "Subagent type delegation requires an OMO category resolver, but none is configured. Use 'model' directly instead."
                    .to_string(),
            );
        };
        let Some(r) = resolver.resolve_subagent_type(&name) else {
            return Err(format!(
                "Subagent type '{}' is unknown or unavailable. Valid OMO agent names: {}.",
                name,
                OMO_ROSTER.join(", ")
            ));
        };
        // Resolved agent model wins over spec/batch model (single-mode
        // parity: resolution first, args model only as fallback).
        req.model = Some(r.model);
        // Identity prompt rides prompt_append (prepended before any
        // existing append); skip when the resolved append is empty.
        match r.prompt_append {
            Some(append) if !append.is_empty() => {
                req.prompt_append = Some(match req.prompt_append.take() {
                    Some(existing) if !existing.is_empty() => {
                        format!("{existing}\n\n{append}")
                    }
                    _ => append,
                });
            }
            _ => {}
        }
    }
    Ok(())
}

/// Role directives injected as the child's extra instructions. Wording kept
/// in joey-orchestration (not joey-cli) so the tool is self-contained.
pub(crate) const EXPLORER_DIRECTIVE: &str = "You are the EXPLORER agent: read-only, facts only.\n\
Answer ONLY the questions in your brief, with evidence: exact file\n\
paths, line numbers, short verbatim quotes, and real command output. Run\n\
read-only/diagnostic commands as needed (rg, ls, git log/diff, cargo\n\
check, --help, version probes). NEVER modify anything. Do not analyze\n\
beyond the questions asked and do not propose solutions, plans, or\n\
recommendations — the orchestrator does all planning and interpretation.\n\
If a question cannot be answered from the code, say so plainly and\n\
report the closest evidence you found. Keep your final summary under\n\
1000 tokens.";

pub(crate) const IMPLEMENTOR_DIRECTIVE: &str = "You are the IMPLEMENTOR agent: execution only.\n\
Follow the brief EXACTLY. It specifies the file paths, the precise\n\
edits to make, and the commands to run; every planning and design\n\
decision was already made by the orchestrator — do not make, revise, or\n\
second-guess decisions. If the brief is ambiguous, incomplete, or\n\
conflicts with what you find (missing file, code differs from the\n\
description), STOP: make no changes beyond what is unambiguous and\n\
report back exactly what is missing or contradictory. Never guess,\n\
infer, or fill gaps with your own judgment. Verify with TARGETED checks\n\
only — build the crates you touched (cargo build -p <crate>) and run\n\
only the scoped tests that cover your changes (cargo test -p <crate>\n\
[filter]). NEVER run the full test suite (cargo test --workspace) or\n\
any broad test run: the orchestrator runs that once, after all\n\
implementors finish. Report exactly what you changed, file by file, and\n\
the real scoped check output. Keep your final summary under 1000 tokens.";

/// Resolve a DelegationRequest patch for a HyperCode role: toolsets, role
/// config (model/turns/tokens/reasoning) unless the caller set explicit
/// overrides, and the role directive as prompt_append (appended after any
/// caller-provided append so both survive).
///
/// `explicit_toolsets` = the tool call included its own `toolsets` array
/// (keeps user control; role defaults only fill gaps).
///
/// Returns whether the FR-006 warning condition hit: the request's model was
/// left to the existing inherit default despite an applicable OMO chain
/// (feature 025, FR-005 precedence 3).
///
/// OMO specialists toggle: when hypercode.omo_specialists.enabled (default
/// true) the call site swaps the chain default for the direct tier→agent
/// mapping (Explorer→explore, Implementor→hephaestus).
pub(crate) fn apply_hyper_role(
    req: &mut DelegationRequest,
    role: HyperRole,
    tree: &Config,
    provider: &str,
    omo_default: Option<String>,
    chain_applicable: bool,
) -> bool {
    let settings = HyperRoleSettings::from_config_tree(tree, role.table(), provider);
    if req.toolsets.is_empty() {
        req.toolsets = role.toolsets().iter().map(|s| s.to_string()).collect();
    }
    let mut warned = false;
    if req.model.is_none() {
        if !settings.model.is_empty() {
            req.model = Some(settings.model.clone()); // explicit config wins (FR-005 precedence 1)
        } else if let Some(m) = omo_default {
            req.model = Some(m); // OMO-chain default (precedence 2)
        } else if chain_applicable {
            warned = true; // unresolvable chain → inherit existing default + warn (precedence 3, FR-006)
        }
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
    warned
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
                    "description": "HyperCode role routing. 'explorer' = read-only investigation (file-read + terminal + web; runs diagnostic commands on your behalf; NEVER writes). 'implementor' = writes files and runs ONLY the targeted checks specified in its brief (scoped builds/tests); the single full-suite run belongs to the orchestrator after all implementors finish. When set, the role's configured model/turns/tokens/reasoning (hypercode.explorer / hypercode.implementor in config) apply unless explicitly overridden here. When hypercode.omo_specialists.enabled (default true), the role's default model maps directly to its OMO counterpart agent (explorer→explore, implementor→hephaestus)."
                },
                "context": {
                    "type": "string",
                    "description": "Additional context to pass to the subagent. Include file paths, error messages, project structure, constraints. The subagent knows nothing about the parent conversation. Briefs must be fully self-contained execution orders — the orchestrator has already made every planning decision; include exact paths, exact changes, and exact commands so the subagent needs no inference."
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
                            "role": {"type": "string", "enum": ["explorer", "implementor"], "description": "HyperCode role routing for this task: 'explorer' (read-only + diagnostic commands) or 'implementor' (writes + targeted checks only). Applies the role's config and directive unless overridden per-task."},
                            "subagent_type": {"type": "string", "description": "OMO agent name for this task (e.g. 'oracle', 'hephaestus', 'momus'). Resolved model + identity prompt apply; the resolved model wins over per-task/batch model. Composes with 'role' (role still fills toolsets/turns and appends its directive)."}
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
                    "description": "OMO subagent type — any registered OMO agent by name (sisyphus, hephaestus, prometheus, atlas, oracle, librarian, explore, multimodal-looker, metis, momus, sisyphus-junior). When set, spawns the named agent with its resolved model and identity prompt. Mutually exclusive with 'category'."
                },
                "load_skills": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Skill names to load and prepend to the subagent's system prompt. Effective with 'category' or 'subagent_type' routing."
                },
                "team": {
                    "type": "string",
                    "description": "Team mode (feature 022): team name. Registers the spawned child as a member of that team; the first reference lazily creates the team (that child is the lead). Errors `team mode is disabled` when hypercode.team.enabled is false."
                },
                "name": {
                    "type": "string",
                    "description": "Member name (mailbox identity) when `team` is set. Must be unique within the team. Defaults to 'lead' for a new team; required for later members."
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

        // Feature 022 (agent teams): `team`/`name` args (single mode only).
        let team_arg = args.get("team").and_then(|v| v.as_str()).map(|s| s.to_string());
        let name_arg = args.get("name").and_then(|v| v.as_str()).map(|s| s.to_string());

        // Check if batch mode (tasks array provided).
        let tasks_value = args.get("tasks");
        let is_batch = tasks_value.is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty()));

        if is_batch {
            if team_arg.is_some() {
                return ToolResult::Error("team spawns do not support batch tasks".to_string());
            }
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
                        if !load_skills.is_empty() && prompt_append.is_none() {
                            prompt_append = Some(named_agent_skill_directive(sat));
                        }
                    }
                    None => {
                        return ToolResult::Error(format!(
                            "Subagent type '{}' is unknown or unavailable. Valid OMO agent names: {}.",
                            sat,
                            OMO_ROSTER.join(", ")
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
            team: None,
            name: None,
        };

        // HyperCode role routing: `role: "explorer"|"implementor"` fills
        // toolsets/model/turns/tokens/reasoning from the role's config table
        // (gaps only — explicit args win) and injects the role directive.
        let mut req = req;
        let mut hyper_role: Option<HyperRole> = None;
        if let Some(role_str) = args.get("role").and_then(|v| v.as_str()) {
            match HyperRole::parse(role_str) {
                Some(role) => {
                    hyper_role = Some(role);
                    // Feature 025 (FR-005/FR-006): role model default. With
                    // OMO specialists enabled (default true) the tier maps
                    // DIRECTLY to its counterpart agent (Explorer→explore,
                    // Implementor→hephaestus); toggle OFF keeps the legacy
                    // chain default byte-identically.
                    let specialists = omo_specialists_enabled(&self.parent_config_tree);
                    let chain_default = if specialists {
                        self.resolver
                            .as_ref()
                            .and_then(|r| r.resolve_subagent_type(omo_specialist_agent(role)))
                            .filter(|r| !r.model.is_empty())
                            .map(|r| r.model)
                    } else {
                        self.resolver
                            .as_ref()
                            .and_then(|r| omo_role_chain_default(role, r.as_ref()))
                    };
                    let chain_applicable = self.resolver.is_some();
                    let warned = apply_hyper_role(
                        &mut req,
                        role,
                        &self.parent_config_tree,
                        &self.parent_config.provider,
                        chain_default,
                        chain_applicable,
                    );
                    if warned {
                        // FR-006: user-visible warning through the agent-notice channel —
                        // the same mechanism other delegation notices use.
                        if let Some(tx) = &self.event_tx {
                            let notice = if specialists {
                                format!(
                                    "hypercode {} role: OMO specialist agent '{}' unresolved against available providers; inheriting the default model (FR-006).",
                                    role.table(),
                                    omo_specialist_agent(role)
                                )
                            } else {
                                format!(
                                    "hypercode {} role: no OMO chain member resolved against available providers; inheriting the default model (FR-006).",
                                    if matches!(role, HyperRole::Explorer) { "explorer" } else { "implementor" }
                                )
                            };
                            let _ = tx.send(AgentEvent::Notice(notice));
                        }
                    }
                }
                None => {
                    return ToolResult::Error(format!(
                        "Unknown role '{role_str}'. Use 'explorer' or 'implementor'."
                    ));
                }
            }
        }

        // Feature 022 (agent teams): gate + lazy-create / member-register
        // BEFORE dispatch so failures reject the call outright (FR-009/FR-010).
        if let Some(team_name) = team_arg {
            // Prefer the named agent (subagent_type) as the member role
            // label; fall back to the hypercode role table, then explorer.
            let role_str = hyper_role
                .map(|r| r.table().to_string())
                .or_else(|| req.subagent_type.clone())
                .unwrap_or_else(|| "explorer".to_string());
            match crate::team::register_spawn(
                &self.parent_config_tree,
                &team_name,
                name_arg.as_deref(),
                &req.goal,
                &role_str,
            ) {
                Ok(spawn) => {
                    req.team = Some(team_name.clone());
                    req.name = Some(spawn.member.clone());
                    let tree = &self.parent_config_tree;
                    let max_members = tree.get_i64("hypercode.team.max_members", 8).max(1) as usize;
                    let max_parallel = tree.get_i64("hypercode.team.max_parallel_members", 4).max(1) as usize;
                    let poll_ms = tree.get_i64("hypercode.team.poll_interval_ms", 500).max(0) as u64;
                    let directive = if spawn.is_lead {
                        // The lead coordinates (Orchestrator role) with the
                        // delegation + team toolsets; teammates keep their
                        // role profile (FR-017) plus the team toolset.
                        req.role = crate::types::SubagentRole::Orchestrator;
                        req.toolsets = vec![
                            "delegation".to_string(),
                            "terminal".to_string(),
                            "file-read".to_string(),
                            "web".to_string(),
                            "team".to_string(),
                        ];
                        crate::team::team_lead_directive_with_specialists(
                            max_members,
                            max_parallel,
                            omo_specialists_enabled(tree),
                        )
                    } else {
                        if !req.toolsets.iter().any(|t| t == "team") {
                            req.toolsets.push("team".to_string());
                        }
                        crate::team::teammate_directive(poll_ms)
                    };
                    req.prompt_append = Some(match req.prompt_append.take() {
                        Some(p) => format!("{p}\n\n{directive}"),
                        None => directive,
                    });
                }
                Err(e) => return ToolResult::Error(e),
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
        // per task, in order, immediately (SC-001). A PER-SPEC
        // background=true applies to that task only: the batch is SPLIT so
        // blocking siblings keep their blocking results (one background
        // spec must not reflag the whole batch). FR-013: nothing is
        // rejected; permits are acquired inside the children under the same
        // limits. background=false / unset keeps the blocking path below
        // untouched (FR-002 byte parity — pinned by tests/background.rs T007).
        let top_background = args
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let is_background: Vec<bool> = task_specs
            .iter()
            .map(|s| top_background || s.background)
            .collect();
        if is_background.iter().any(|b| *b) {
            // Split preserving original task order: background specs
            // dispatch through the background wave (handles now), blocking
            // specs through the blocking path (results below).
            let bg_specs: Vec<TaskSpec> = task_specs
                .iter()
                .zip(is_background.iter())
                .filter(|(_, b)| **b)
                .map(|(s, _)| s.clone())
                .collect();
            let blocking_specs: Vec<TaskSpec> = task_specs
                .iter()
                .zip(is_background.iter())
                .filter(|(_, b)| !**b)
                .map(|(s, _)| s.clone())
                .collect();

            // Background subwave: same request construction the blocking
            // batch path uses (model/toolsets/turns/persist defaults +
            // HyperCode role routing), so background children run with
            // identical config.
            let mut requests = crate::subagent::specs_to_requests(
                &bg_specs,
                batch_model.as_deref(),
                &batch_toolsets,
                Some(self.manager.config().default_max_turns),
                self.manager.config().default_persist,
                SubagentRole::Leaf,
            );
            if let Err(e) = crate::delegation_tool::resolve_batch_subagent_types(
                &mut requests,
                self.resolver.as_ref(),
            ) {
                return ToolResult::Error(e);
            }
            if let Err(e) = crate::subagent::apply_batch_hyper_roles(
                &mut requests,
                &bg_specs,
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

            // Blocking subwave (empty when the whole batch is background):
            // starts after the background handles exist but still blocks
            // until every blocking child finishes.
            let blocking_results: Vec<DelegationResult> = if blocking_specs.is_empty() {
                Vec::new()
            } else {
                match self
                    .dispatch_blocking_batch(
                        &blocking_specs,
                        batch_model.as_deref(),
                        &batch_toolsets,
                        budgets,
                    )
                    .await
                {
                    Ok(results) => results,
                    // The background subwave is already dispatched; surface
                    // the resolution failure per blocking spec instead of
                    // dropping the already-returned handles.
                    Err(e) => {
                        tracing::warn!("blocking subwave resolution failed: {e}");
                        blocking_specs
                            .iter()
                            .map(|spec| DelegationResult {
                                goal: spec.goal.clone(),
                                summary: String::new(),
                                success: false,
                                error: Some(e.clone()),
                                token_usage: Default::default(),
                                wall_clock: std::time::Duration::ZERO,
                                model: String::new(),
                                iterations: 0,
                                persisted_session_id: None,
                                stop_reason: None,
                            })
                            .collect()
                    }
                }
            };

            // Merge the result lines in ORIGINAL task order: blocking
            // entries keep their [i/total] report blocks, background
            // entries are the immediate handle lines.
            let total = task_specs.len();
            let mut bg_handles = handles.into_iter();
            let mut blocking_iter = blocking_results.into_iter();
            let mut segments: Vec<String> = Vec::with_capacity(total);
            for (position, background) in is_background.iter().enumerate() {
                if *background {
                    let handle = bg_handles
                        .next()
                        .expect("one handle per background spec");
                    segments.push(format!(
                        "[BACKGROUND] id={} goal={} started",
                        handle.child_id, handle.goal
                    ));
                } else {
                    let r = blocking_iter
                        .next()
                        .expect("one result per blocking spec");
                    segments.push(format_result_block(position + 1, total, &r));
                }
            }
            return ToolResult::Text(segments.join("\n"));
        }

        // Blocking batch (T021): when budgets.max_turns is set, build the
        // requests with the budgeted turn cap (same construction as
        // dispatch_batch_with_roles, which is otherwise left untouched for
        // the no-budgets byte-parity path). tokens/wall-clock are deferred
        // on the blocking path (no watcher exists — see single path).
        let results = match self
            .dispatch_blocking_batch(&task_specs, batch_model.as_deref(), &batch_toolsets, budgets)
            .await
        {
            Ok(results) => results,
            Err(e) => return ToolResult::Error(e),
        };

        // Format results per the delegation-tool contract.
        let total = results.len();
        let blocks: Vec<String> = results
            .iter()
            .enumerate()
            .map(|(i, r)| format_result_block(i + 1, total, r))
            .collect();
        if blocks.is_empty() {
            return ToolResult::Text(String::new());
        }
        ToolResult::Text(format!("{}\n", blocks.join("\n\n")))
    }

    /// The blocking batch dispatch paths (T021), factored out so the mixed
    /// background/blocking split can send the blocking subwave through the
    /// SAME code: budgeted turn cap → named subagent types → default
    /// role-routed batch. `Err` carries the resolver failure the inline
    /// paths surfaced as `ToolResult::Error`.
    async fn dispatch_blocking_batch(
        &self,
        task_specs: &[TaskSpec],
        batch_model: Option<&str>,
        batch_toolsets: &[String],
        budgets: Option<crate::types::Budgets>,
    ) -> Result<Vec<DelegationResult>, String> {
        let budgeted_turns = budgets
            .and_then(|b| b.max_turns)
            .map(|t| t as usize);
        if let Some(mt) = budgeted_turns {
            let mut requests = crate::subagent::specs_to_requests(
                task_specs,
                batch_model,
                batch_toolsets,
                Some(mt),
                self.manager.config().default_persist,
                SubagentRole::Leaf,
            );
            if let Err(e) = crate::delegation_tool::resolve_batch_subagent_types(
                &mut requests,
                self.resolver.as_ref(),
            ) {
                return Err(e);
            }
            if let Err(e) = crate::subagent::apply_batch_hyper_roles(
                &mut requests,
                task_specs,
                &self.parent_config_tree,
                &self.parent_config.provider,
            ) {
                tracing::warn!("hypercode role routing failed: {e}");
            }
            Ok(self
                .manager
                .dispatch_requests(
                    &requests,
                    &self.parent_config,
                    &self.parent_config_tree,
                    &self.base_registry,
                    self.event_tx.as_ref(),
                )
                .await)
        } else if task_specs.iter().any(|s| s.subagent_type.is_some()) {
            // Per-task subagent_type present but no budget cap: the manager's
            // dispatch_batch_with_roles builds requests without a resolver, so
            // build them here (same construction, default turn cap, no budget)
            // to run named-agent resolution first.
            let mut requests = crate::subagent::specs_to_requests(
                task_specs,
                batch_model,
                batch_toolsets,
                Some(self.manager.config().default_max_turns),
                self.manager.config().default_persist,
                SubagentRole::Leaf,
            );
            if let Err(e) = crate::delegation_tool::resolve_batch_subagent_types(
                &mut requests,
                self.resolver.as_ref(),
            ) {
                return Err(e);
            }
            if let Err(e) = crate::subagent::apply_batch_hyper_roles(
                &mut requests,
                task_specs,
                &self.parent_config_tree,
                &self.parent_config.provider,
            ) {
                tracing::warn!("hypercode role routing failed: {e}");
            }
            Ok(self
                .manager
                .dispatch_requests(
                    &requests,
                    &self.parent_config,
                    &self.parent_config_tree,
                    &self.base_registry,
                    self.event_tx.as_ref(),
                )
                .await)
        } else {
            Ok(self
                .manager
                .dispatch_batch_with_roles(
                    task_specs,
                    batch_model,
                    batch_toolsets,
                    &self.parent_config,
                    &self.parent_config_tree,
                    &self.base_registry,
                    self.event_tx.as_ref(),
                )
                .await)
        }
    }
}

/// One `[i/total]` blocking-result report block (no trailing newline — the
/// caller inserts separators). The block lines are byte-identical to the
/// pinned contract format (tests/background.rs t007_blocking_batch_exact_format).
fn format_result_block(index: usize, total: usize, r: &DelegationResult) -> String {
    let mut block = format!("[{}/{}] goal: {:?}\n", index, total, r.goal);
    if r.success {
        block.push_str("      status: success\n");
        block.push_str(&format!("      summary: {}\n", r.summary));
    } else {
        block.push_str("      status: failed\n");
        block.push_str(&format!(
            "      error: {}\n",
            r.error.as_deref().unwrap_or("unknown")
        ));
    }
    block.push_str(&format!(
        "      tokens: {} | duration: {:.1}s",
        r.token_usage.total_tokens,
        r.wall_clock.as_secs_f64()
    ));
    block
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
        "Delegate a research task to another OMO agent (any registered OMO agent by name) \
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
                    "description": "The OMO agent to consult, by name — any registered agent: sisyphus, hephaestus, prometheus, atlas, oracle, librarian, explore, multimodal-looker, metis, momus, sisyphus-junior. Required.",
                    "enum": OMO_ROSTER.to_vec()
                }
            },
            "required": ["goal", "subagent_type"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        // Force subagent_type to be present (BC-012).
        if args.get("subagent_type").and_then(|v| v.as_str()).is_none() {
            return ToolResult::Error(
                "call_omo_agent requires 'subagent_type' (any registered OMO agent name)".to_string(),
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
            team: None,
            name: None,
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
        apply_hyper_role(&mut req, HyperRole::Explorer, &tree, "prov", None, false);
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
        apply_hyper_role(&mut req, HyperRole::Implementor, &tree, "prov", None, false);
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
        apply_hyper_role(&mut req, HyperRole::Explorer, &tree, "prov", None, false);
        assert_eq!(req.model.as_deref(), Some("cheap-model"));
        assert_eq!(req.max_turns, Some(7));
        assert_eq!(req.max_tokens, Some(9000));
        assert!(matches!(req.reasoning, Some(joey_providers::ReasoningEffort::Level(l)) if l == "low"));

        // Explicit args win over role config.
        let mut req = base_req();
        req.model = Some("explicit".into());
        req.max_turns = Some(3);
        apply_hyper_role(&mut req, HyperRole::Explorer, &tree, "prov", None, false);
        assert_eq!(req.model.as_deref(), Some("explicit"));
        assert_eq!(req.max_turns, Some(3));
    }

    #[test]
    fn role_directive_stacks_with_existing_append() {
        let tree = tree_with("");
        let mut req = base_req();
        req.prompt_append = Some("caller content".into());
        apply_hyper_role(&mut req, HyperRole::Explorer, &tree, "p", None, false);
        let append = req.prompt_append.unwrap();
        assert!(append.starts_with("caller content"));
        assert!(append.contains("EXPLORER"));
    }

    #[test]
    fn batch_role_routing_applies_per_task() {
        let tree = tree_with("");
        let tasks = vec![
            TaskSpec { goal: "read thing".into(), context: None, model: None, toolsets: Vec::new(), role: Some("explorer".into()), background: false, subagent_type: None, budgets: None },
            TaskSpec { goal: "build thing".into(), context: None, model: None, toolsets: Vec::new(), role: Some("implementor".into()), background: false, subagent_type: None, budgets: None },
            TaskSpec { goal: "plain".into(), context: None, model: None, toolsets: Vec::new(), role: None, background: false, subagent_type: None, budgets: None },
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
        let tasks = vec![TaskSpec { goal: "x".into(), context: None, model: None, toolsets: Vec::new(), role: Some("wat".into()), background: false, subagent_type: None, budgets: None }];
        let mut reqs: Vec<DelegationRequest> = vec![base_req()];
        assert!(crate::subagent::apply_batch_hyper_roles(&mut reqs, &tasks, &tree, "p").is_err());
    }

    /// Feature 022 (T007): delegate_task advertises the `team`/`name`
    /// parameters so models can discover team mode from the schema alone.
    #[test]
    fn parameters_advertise_team_and_name() {
        let mgr = std::sync::Arc::new(crate::manager::SubagentManager::new(
            crate::manager::ManagerConfig::default(),
        ));
        let tree = tree_with("");
        let parent_config = joey_agent_core::AgentConfig {
            model: "test-model".into(),
            provider: "openai".into(),
            base_url: "http://127.0.0.1:9".into(),
            api_key: None,
            max_turns: 5,
            api_max_retries: 1,
            tool_delay: 0.0,
            reasoning: None,
            enabled_tools: Vec::new(),
            max_tokens: None,
            stream: false,
            pass_session_id: false,
            model_pinned: false,
        };
        let tool = DelegateTask::new(
            mgr,
            parent_config,
            tree,
            ToolRegistry::new(),
            None,
            None,
        );
        let params = tool.parameters();
        assert_eq!(params["properties"]["team"]["type"], json!("string"));
        assert!(params["properties"]["team"]["description"]
            .as_str()
            .unwrap()
            .contains("team mode is disabled"));
        assert_eq!(params["properties"]["name"]["type"], json!("string"));
        assert!(params["properties"]["name"]["description"]
            .as_str()
            .unwrap()
            .contains("unique"));
    }
}

#[cfg(test)]
mod roster_tests {
    use super::*;

    fn tree_with(yaml: &str) -> Config {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), yaml).unwrap();
        Config::load_from(tmp.path().to_path_buf()).unwrap()
    }

    fn base_req() -> DelegationRequest {
        DelegationRequest::single("g")
    }

    /// FR-004: the named-agent skill directive names the agent and instructs
    /// skill application.
    #[test]
    fn named_agent_skill_directive_names_agent_and_skills() {
        let d = named_agent_skill_directive("oracle");
        assert!(d.contains("oracle"));
        assert!(d.contains("skill"));
    }

    /// T014 / FR-005: the OMO-chain role default picks the FIRST chain member
    /// the resolver can serve, per role.
    struct ChainMockResolver {
        explore: bool,
    }

    impl crate::CategoryResolver for ChainMockResolver {
        fn resolve_category(&self, _name: &str) -> Option<crate::ResolvedDelegation> {
            None
        }
        fn resolve_subagent_type(&self, name: &str) -> Option<crate::ResolvedDelegation> {
            match name {
                "explore" if self.explore => Some(crate::ResolvedDelegation {
                    model: "explore-model".to_string(),
                    prompt_append: None,
                }),
                "librarian" => Some(crate::ResolvedDelegation {
                    model: "lib-model".to_string(),
                    prompt_append: None,
                }),
                "momus" => Some(crate::ResolvedDelegation {
                    model: "momus-model".to_string(),
                    prompt_append: None,
                }),
                _ => None,
            }
        }
    }

    #[test]
    fn omo_role_chain_default_prefers_first_member() {
        // Full mock: first member of each chain wins.
        let mock = ChainMockResolver { explore: true };
        assert_eq!(
            omo_role_chain_default(HyperRole::Explorer, &mock),
            Some("explore-model".to_string())
        );
        assert_eq!(
            omo_role_chain_default(HyperRole::Implementor, &mock),
            Some("momus-model".to_string())
        );
        // First member unresolvable → falls to the next chain member.
        let mock_no_explore = ChainMockResolver { explore: false };
        assert_eq!(
            omo_role_chain_default(HyperRole::Explorer, &mock_no_explore),
            Some("lib-model".to_string())
        );
    }

    #[test]
    fn omo_specialist_agent_maps_directly() {
        assert_eq!(omo_specialist_agent(HyperRole::Explorer), "explore");
        assert_eq!(omo_specialist_agent(HyperRole::Implementor), "hephaestus");
    }

    #[test]
    fn omo_specialists_enabled_default_true_and_off() {
        let tree = tree_with("");
        assert!(omo_specialists_enabled(&tree), "default is true");
        let tree_off = tree_with("hypercode:\n  omo_specialists:\n    enabled: false\n");
        assert!(!omo_specialists_enabled(&tree_off));
    }

    /// Mock resolver for batch subagent_type resolution tests (mirrors
    /// ChainMockResolver's style).
    struct OracleMockResolver;

    impl crate::CategoryResolver for OracleMockResolver {
        fn resolve_category(&self, _name: &str) -> Option<crate::ResolvedDelegation> {
            None
        }
        fn resolve_subagent_type(&self, name: &str) -> Option<crate::ResolvedDelegation> {
            match name {
                "oracle" => Some(crate::ResolvedDelegation {
                    model: "oracle-model".to_string(),
                    prompt_append: Some("You are Oracle.".to_string()),
                }),
                _ => None,
            }
        }
    }

    #[test]
    fn resolve_batch_subagent_types_precedence_and_errors() {
        // Resolver None → hard error.
        let mut reqs = vec![{
            let mut r = base_req();
            r.subagent_type = Some("oracle".into());
            r
        }];
        let err = resolve_batch_subagent_types(&mut reqs, None).unwrap_err();
        assert!(err.contains("requires an OMO category resolver"), "err: {err}");

        // Unknown name → hard error listing the roster.
        let resolver: Arc<dyn CategoryResolver> = Arc::new(OracleMockResolver);
        let mut reqs = vec![{
            let mut r = base_req();
            r.subagent_type = Some("nope".into());
            r
        }];
        let err =
            resolve_batch_subagent_types(&mut reqs, Some(&resolver)).unwrap_err();
        assert!(err.contains("'nope' is unknown"), "err: {err}");
        assert!(err.contains("sisyphus, hephaestus"), "err: {err}");

        // Success: resolved model WINS over spec model; prompt_appends
        // combine; requests without subagent_type untouched.
        let mut r0 = base_req();
        r0.subagent_type = Some("oracle".into());
        r0.model = Some("spec-model".into());
        r0.prompt_append = Some("existing content".into());
        let r1 = base_req();
        let mut reqs = vec![r0, r1];
        resolve_batch_subagent_types(&mut reqs, Some(&resolver)).unwrap();
        assert_eq!(reqs[0].model.as_deref(), Some("oracle-model"));
        let append = reqs[0].prompt_append.as_deref().unwrap();
        assert!(append.contains("existing content"), "append: {append}");
        assert!(append.contains("You are Oracle."), "append: {append}");
        assert_eq!(reqs[1].model, None);
        assert_eq!(reqs[1].prompt_append, None);
    }
}
