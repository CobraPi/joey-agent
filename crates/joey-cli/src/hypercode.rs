//! HyperCode: parallel task optimization that decomposes work into the maximum
//! number of independent workstreams the system can support.
//!
//! HyperCode executes a multi-phase pipeline natively on the SAME
//! orchestration machinery as the `delegate_task` tool (`SubagentManager`):
//!
//! 1. **Plan** — a planner subagent decomposes the goal into independent
//!    workstreams (or the caller supplies them explicitly).
//! 2. **Explore** — parallel Explorer subagents (read-only toolsets) gather
//!    context for each workstream.
//! 3. **Build** — parallel Implementor subagents implement each workstream,
//!    fed with the matching explorer's findings.
//! 4. **Synthesize** — a merge of all results into a cohesive summary.
//!
//! Because every child is dispatched through `SubagentManager`, the TUI
//! gets full native visibility for free (per-subagent panes on the right
//! rail, live streaming, job board) via the process-global orchestration
//! event tap — exactly like `delegate_task` batches.
//!
//! Roles are configured per provider (model / max tokens / max turns /
//! reasoning level) via `/hypercode configure`.

use std::collections::HashMap;
use std::sync::Arc;

use joey_agent_core::AgentConfig;
use joey_orchestration::evaluator::{
    GateOutcome, VerificationGate as GateTrait, VerificationPlanView,
};
use joey_orchestration::evidence::{run_root, RunHandle};
use joey_orchestration::scheduler::{
    RunStats, Scheduler as GraphScheduler, SchedulerConfig as GraphSchedulerConfig, TaskDispatcher,
};
use joey_orchestration::task_graph::{LegacyWorkstream, TaskGraph, TaskNode, TaskStatus};
use joey_orchestration::workspace::baseline_revision;
use joey_orchestration::{DelegationRequest, SubagentManager, SubagentRole};
use joey_providers::ReasoningEffort;
use joey_tools::ToolRegistry;

/// Cap on workstreams per phase (config: `hypercode.max_workstreams`).
pub const DEFAULT_MAX_WORKSTREAMS: usize = 5;

/// Parse a reasoning level string ("none"|"low"|"medium"|"high"|"" ) into a
/// `ReasoningEffort`. `None` level (empty/inherit) maps to `None`.
pub fn parse_reasoning_level(level: &str) -> Option<ReasoningEffort> {
    match level.trim().to_lowercase().as_str() {
        "" | "inherit" => None,
        "none" | "off" => Some(ReasoningEffort::Disabled),
        other => Some(ReasoningEffort::Level(other.to_string())),
    }
}

/// Configuration for HyperCode parallel optimization.
#[derive(Debug, Clone)]
pub struct HyperCodeConfig {
    /// Whether HyperCode mode is enabled (visual indicator in TUI).
    pub enabled: bool,
    /// Provider-specific model and settings for Explorer subagents.
    pub explorer_configs: HashMap<String, RoleConfig>,
    /// Provider-specific model and settings for Implementor subagents.
    pub implementor_configs: HashMap<String, RoleConfig>,
    /// Max parallel workstreams per phase (0 = default).
    pub max_workstreams: usize,
    /// When HyperCode is enabled, run the MAIN agent as an orchestrator:
    /// file WRITES and build/test commands are delegated to children; the
    /// main agent keeps delegate_task + process monitoring + read-only file
    /// peeks + web (default true).
    pub orchestrator_mode: bool,
    /// Feature 022 (agent teams): `hypercode.team.*` settings.
    pub team: TeamConfig,
}

/// Configuration for a HyperCode role (Explorer or Implementor).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleConfig {
    /// Model to use for this provider (e.g., "gpt-4o", "claude-sonnet-4-20250514").
    pub model: String,
    /// Max context window in tokens (0 = use model default).
    pub max_tokens: usize,
    /// Max turns per subagent before summary.
    pub max_turns: usize,
    /// Reasoning level: "none", "low", "medium", "high", or "" (inherit).
    pub reasoning_level: String,
}

/// Legacy aliases (ExplorerConfig/ImplementorConfig were structurally
/// identical; one type now serves both roles).
#[allow(dead_code)]
pub type ExplorerConfig = RoleConfig;
#[allow(dead_code)]
pub type ImplementorConfig = RoleConfig;

/// Feature 022 (agent teams): `hypercode.team.*` configuration
/// (contracts/team-tools.md §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamConfig {
    pub enabled: bool,
    pub lead_model: String,
    pub max_members: usize,
    pub max_parallel_members: usize,
    pub message_limit: usize,
    pub poll_interval_ms: u64,
    pub cleanup_days: i64,
}

impl Default for TeamConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            lead_model: String::new(),
            max_members: 8,
            max_parallel_members: 4,
            message_limit: 10,
            poll_interval_ms: 500,
            cleanup_days: 7,
        }
    }
}

impl TeamConfig {
    pub fn from_config(config: &joey_core::Config) -> Self {
        Self {
            enabled: config.get_bool("hypercode.team.enabled", false),
            lead_model: config.get_str("hypercode.team.lead_model", ""),
            max_members: config.get_i64("hypercode.team.max_members", 8).max(1) as usize,
            max_parallel_members: config
                .get_i64("hypercode.team.max_parallel_members", 4)
                .max(1) as usize,
            message_limit: config.get_i64("hypercode.team.message_limit", 10).max(1) as usize,
            poll_interval_ms: config.get_i64("hypercode.team.poll_interval_ms", 500).max(0) as u64,
            cleanup_days: config.get_i64("hypercode.team.cleanup_days", 7),
        }
    }
}

impl Default for HyperCodeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            explorer_configs: HashMap::new(),
            implementor_configs: HashMap::new(),
            max_workstreams: 0,
            orchestrator_mode: true,
            team: TeamConfig::default(),
        }
    }
}

impl HyperCodeConfig {
    /// Get the explorer config for the given provider, or a sensible default.
    pub fn get_explorer_config(&self, provider: &str) -> RoleConfig {
        self.explorer_configs
            .get(provider)
            .cloned()
            .unwrap_or_else(|| RoleConfig {
                model: String::new(), // empty = inherit the parent's model
                max_tokens: 0,
                max_turns: 8,
                reasoning_level: String::new(),
            })
    }

    /// Get the implementor config for the given provider, or a sensible default.
    pub fn get_implementor_config(&self, provider: &str) -> RoleConfig {
        self.implementor_configs
            .get(provider)
            .cloned()
            .unwrap_or_else(|| RoleConfig {
                model: String::new(),
                max_tokens: 0,
                max_turns: 12,
                reasoning_level: String::new(),
            })
    }

    /// Effective workstream cap for a run.
    pub fn effective_max_workstreams(&self) -> usize {
        if self.max_workstreams == 0 {
            DEFAULT_MAX_WORKSTREAMS
        } else {
            self.max_workstreams
        }
    }


    /// Set the explorer config for a specific provider.
    /// (Currently exercised by tests and kept for future `configure` flows —
    /// CLI persistence goes through `save_explorer_config`.)
    #[allow(dead_code)]
    pub fn set_explorer_config(&mut self, provider: String, config: RoleConfig) {
        self.explorer_configs.insert(provider, config);
    }

    /// Set the implementor config for a specific provider.
    /// (Currently exercised by tests and kept for future `configure` flows —
    /// CLI persistence goes through `save_implementor_config`.)
    #[allow(dead_code)]
    pub fn set_implementor_config(&mut self, provider: String, config: RoleConfig) {
        self.implementor_configs.insert(provider, config);
    }

    /// Load HyperCode configuration from the joey Config.
    pub fn from_config(config: &joey_core::Config) -> Self {
        let mut hc = Self::default();

        // Load enabled state
        hc.enabled = config.get_bool("hypercode.enabled", false);
        hc.max_workstreams = config.get_i64("hypercode.max_workstreams", 0).max(0) as usize;
        hc.orchestrator_mode = config.get_bool("hypercode.orchestrator_mode", true);
        hc.team = TeamConfig::from_config(config);

        // Load role configs per provider (explorer + implementor tables).
        for (table_key, target) in [
            ("hypercode.explorer", 0),
            ("hypercode.implementor", 1),
        ] {
            if let Some(table) = config.get(table_key) {
                if let Some(mapping) = table.as_mapping() {
                    for (provider, value) in mapping {
                        if let (Some(provider_str), Some(map)) =
                            (provider.as_str(), value.as_mapping())
                        {
                            let rc = role_config_from_mapping(map);
                            match target {
                                0 => hc.explorer_configs.insert(provider_str.to_string(), rc),
                                _ => hc.implementor_configs.insert(provider_str.to_string(), rc),
                            };
                        }
                    }
                }
            }
        }

        hc
    }

    /// Persist the enabled state to config.
    pub fn save_enabled(enabled: bool) -> Result<(), String> {
        let mut config = joey_core::Config::load()
            .map_err(|e| format!("Failed to load config: {}", e))?;
        config
            .set_and_save("hypercode.enabled", if enabled { "true" } else { "false" })
            .map_err(|e| format!("Failed to save config: {}", e))
    }

    /// Save explorer config for a provider to config.
    pub fn save_explorer_config(provider: &str, config: &RoleConfig) -> Result<(), String> {
        save_role_config("hypercode.explorer", provider, config)
    }

    /// Save implementor config for a provider to config.
    pub fn save_implementor_config(provider: &str, config: &RoleConfig) -> Result<(), String> {
        save_role_config("hypercode.implementor", provider, config)
    }

    /// Persist the orchestrator-mode flag.
    pub fn save_orchestrator_mode(on: bool) -> Result<(), String> {
        let mut config = joey_core::Config::load()
            .map_err(|e| format!("Failed to load config: {e}"))?;
        config
            .set_and_save("hypercode.orchestrator_mode", if on { "true" } else { "false" })
            .map_err(|e| format!("Failed to save config: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Orchestrator mode: the main agent delegates EVERYTHING
// ---------------------------------------------------------------------------

/// The orchestrator's effective toolsets: delegation + terminal (process
/// monitoring/management) + read-only files + web research. It still never
/// WRITES files or runs build/edit commands itself — those belong to the
/// Implementor children.
pub const ORCHESTRATOR_TOOLSET: &[&str] = &["delegation", "terminal", "file-read", "web"];

/// True when the main agent should run as a pure orchestrator right now:
/// HyperCode enabled AND orchestrator_mode on.
pub fn orchestrator_active(config: &joey_core::Config) -> bool {
    let hc = HyperCodeConfig::from_config(config);
    hc.enabled && hc.orchestrator_mode
}

/// Apply orchestrator mode to a freshly-built [`AgentConfig`]: restrict the
/// enabled tools to `delegate_task` (the `delegation` toolset resolved to
/// tool names — `enabled_tools` holds flat tool names). Call BEFORE
/// `Agent::new` so the system prompt's tool section reflects the restricted
/// surface.
///
/// Returns false (and changes nothing) when orchestrator mode is off.
pub fn apply_orchestrator_to_agent_config(
    config: &joey_core::Config,
    agent_cfg: &mut AgentConfig,
) -> bool {
    if !orchestrator_active(config) {
        return false;
    }
    agent_cfg.enabled_tools = orchestrator_tool_names();
    true
}

/// The resolved tool-name list for orchestrator mode (delegate_task only).
/// `enabled_tools` holds flat TOOL names, so the `delegation` toolset must be
/// RESOLVED — returning the toolset name verbatim would leave the agent with
/// zero valid tools (the registry gate matches tool names, not set names).
pub fn orchestrator_tool_names() -> Vec<String> {
    let names: Vec<String> = ORCHESTRATOR_TOOLSET.iter().map(|s| s.to_string()).collect();
    let resolved = joey_tools::resolve_toolsets(&names);
    if resolved.is_empty() {
        // Defensive: never hand the agent an empty toolset (that would be a
        // silent no-tool agent). Fall back to the canonical tool name.
        return vec!["delegate_task".to_string()];
    }
    resolved
}

/// The overlay appended to the system prompt when orchestrator mode is on.
/// Applied via `Agent::set_extra_instructions` — a runtime toggle never
/// needs an agent rebuild.
pub fn orchestrator_overlay() -> String {
    ORCHESTRATOR_PROMPT.to_string()
}

/// Read one RoleConfig from a YAML mapping (provider table row).
fn role_config_from_mapping(map: &serde_yaml::Mapping) -> RoleConfig {
    let get_str = |key: &str| -> String {
        map.get(&serde_yaml::Value::String(key.to_string()))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let get_num = |key: &str| -> usize {
        map.get(&serde_yaml::Value::String(key.to_string()))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize
    };
    RoleConfig {
        model: get_str("model"),
        max_tokens: get_num("max_tokens"),
        max_turns: get_num("max_turns"),
        reasoning_level: get_str("reasoning_level"),
    }
}

/// Persist a role config (4 dotted keys).
fn save_role_config(
    table: &str,
    provider: &str,
    config: &RoleConfig,
) -> Result<(), String> {
    let mut cfg = joey_core::Config::load()
        .map_err(|e| format!("Failed to load config: {}", e))?;
    let base_key = format!("{}.{}", table, provider);
    cfg.set_and_save(&format!("{}.model", base_key), &config.model)
        .map_err(|e| format!("Failed to save config: {}", e))?;
    cfg.set_and_save(&format!("{}.max_tokens", base_key), &config.max_tokens.to_string())
        .map_err(|e| format!("Failed to save config: {}", e))?;
    cfg.set_and_save(&format!("{}.max_turns", base_key), &config.max_turns.to_string())
        .map_err(|e| format!("Failed to save config: {}", e))?;
    cfg.set_and_save(&format!("{}.reasoning_level", base_key), &config.reasoning_level)
        .map_err(|e| format!("Failed to save config: {}", e))?;
    Ok(())
}

/// Result type for HyperCode operations (shared between CLI and TUI).
#[derive(Debug, Clone)]
pub enum HyperCodeOutput {
    /// Multi-line output (status displays, errors, etc.)
    Text(Vec<String>),
    /// Toggle operation that returns the new enabled state.
    Toggle(bool),
    /// Configuration operation that returns success message.
    Configured(String),
    /// Run subcommand: execute the parallel pipeline on the engine.
    Run { goal: String },
}

/// A workstream: an independent unit of work discovered by the planner.
#[derive(Debug, Clone)]
pub struct Workstream {
    pub id: usize,
    pub focus: String,
}

/// Pipeline phase (progress reporting for the TUI badge + transcript).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Planning,
    Exploring,
    Building,
    Synthesizing,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Planning => "planning",
            Phase::Exploring => "exploring",
            Phase::Building => "building",
            Phase::Synthesizing => "synthesizing",
        }
    }
}

/// Everything needed to drive the HyperCode pipeline through the
/// SubagentManager (the same machinery delegate_task uses).
#[derive(Clone)]
pub struct HypercodeContext {
    /// Parent AgentConfig (provider credentials, retries, defaults).
    pub agent_config: AgentConfig,
    /// The joey Config tree.
    pub config: joey_core::Config,
    /// Base tool registry (pre-orchestration snapshot) for children.
    pub base_registry: ToolRegistry,
    /// Manager to dispatch through — SHARE the engine agent's manager so
    /// hypercode children and delegate_task children share one provider
    /// semaphore and interrupt handle.
    pub manager: Arc<SubagentManager>,
    /// Working directory for child agents.
    pub cwd: std::path::PathBuf,
    /// The LIVE main-turn model the parent agent is actually dispatching
    /// with (tier-routed / allocator / image-routed — NOT the raw config
    /// default), captured from `Agent::effective_main_turn_model()` when
    /// the context is built. Children inherit this when the role table
    /// has no model entry for the active provider, so they never silently
    /// run on a WORSE model than the parent (e.g. raw config glm-5.2 on a
    /// copilot-wire provider while the parent's turns are tier-routed to
    /// a servable model). None = legacy behavior (children inherit
    /// `agent_config.model`).
    pub parent_effective_model: Option<String>,
    /// Spec 023 (US2/T013): typed execution graph converted from the
    /// planner's legacy `<workstreams>` decomposition (FR-007), gated by
    /// `hypercode.execution_graph.enabled` (default false ⇒ SC-001 pure
    /// no-op parity). `None` until a converted plan passes
    /// [`TaskGraph::validate`]. `Arc<Mutex<…>>` because
    /// [`run_hypercode`] receives `&HypercodeContext` (interior
    /// mutability) while the context is freely cloned/shared by the
    /// engine + REPL callers (Clone) — clones observe the same slot.
    pub execution_graph: std::sync::Arc<std::sync::Mutex<Option<TaskGraph>>>,
}

impl std::fmt::Debug for HypercodeContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HypercodeContext")
            .field("agent_config", &self.agent_config)
            .field("base_registry_len", &self.base_registry.names().len())
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

/// Progress callback: invoked on each phase transition with the phase and a
/// human-readable detail line.
pub type ProgressFn<'a> = dyn Fn(Phase, &str) + Send + Sync + 'a;

/// Outcome of a HyperCode run.
#[derive(Debug, Clone, Default)]
pub struct HypercodeReport {
    /// Workstreams executed (id + focus).
    pub workstreams: Vec<Workstream>,
    /// Per-workstream final summaries (aligned with workstreams).
    pub build_summaries: Vec<String>,
    /// Per-workstream success flags (aligned with workstreams).
    pub successes: Vec<bool>,
    /// Total wall-clock of the whole pipeline.
    pub total_secs: f64,
    /// True when the run was interrupted before finishing.
    pub interrupted: bool,
    /// FR-016: per-run mode decisions (format_mode_decision strings).
    pub mode_decisions: Vec<String>,
}

impl HypercodeReport {
    pub fn succeeded(&self) -> usize {
        self.successes.iter().filter(|s| **s).count()
    }

    /// Render the final multi-line report for the transcript.
    pub fn render(&self) -> Vec<String> {
        use std::fmt::Write as _;
        let mut out = Vec::new();
        let mut head = String::new();
        let _ = write!(
            &mut head,
            "⚡ HyperCode run {} — {}/{} workstream(s) succeeded in {:.1}s",
            if self.interrupted { "INTERRUPTED" } else { "complete" },
            self.succeeded(),
            self.workstreams.len(),
            self.total_secs
        );
        out.push(head);
        for (i, ws) in self.workstreams.iter().enumerate() {
            let ok = self.successes.get(i).copied().unwrap_or(false);
            let summary = self.build_summaries.get(i).map(String::as_str).unwrap_or("");
            out.push(format!(
                "{} [{}] {}",
                if ok { "✓" } else { "✗" },
                ws.id,
                ws.focus
            ));
            if !summary.is_empty() {
                // First meaningful line of the implementor's summary.
                if let Some(first) = summary.lines().map(str::trim).find(|l| !l.is_empty()) {
                    let preview: String = first.chars().take(160).collect();
                    out.push(format!("    {}", preview));
                }
            }
        }
        if !self.mode_decisions.is_empty() {
            out.push(String::from("Mode decisions:"));
            for d in &self.mode_decisions {
                out.push(format!("  {d}"));
            }
        }
        out
    }
}

/// FR-016: recorded per-run mode decision, format pinned by
/// contracts/team-tools.md §5.
pub fn format_mode_decision(mode: &str, task: &str, rationale: &str) -> String {
    format!("mode={mode} task={task} rationale={rationale}")
}

/// Feature 022 (FR-019 + lead shape): build the lead child's delegation
/// request. `model` stays None when `hypercode.team.lead_model` is empty so
/// the lead inherits the orchestrator's effective model at dispatch.
pub(crate) fn lead_request(
    goal: &str,
    team_name: &str,
    member: &str,
    cfg: &TeamConfig,
) -> DelegationRequest {
    let mut lead_req = DelegationRequest::single(goal.to_string());
    if !cfg.lead_model.is_empty() {
        lead_req.model = Some(cfg.lead_model.clone());
    }
    lead_req.role = joey_orchestration::SubagentRole::Orchestrator;
    lead_req.toolsets = vec![
        "delegation".to_string(),
        "terminal".to_string(),
        "file-read".to_string(),
        "web".to_string(),
        "team".to_string(),
    ];
    lead_req.team = Some(team_name.to_string());
    lead_req.name = Some(member.to_string());
    lead_req.prompt_append = Some(joey_orchestration::team::team_lead_directive(
        cfg.max_members,
        cfg.max_parallel_members,
    ));
    lead_req
}

/// Feature 022 (US1/US2 + FR-018): route decision + team-start attempt.
/// Ok(Some((team, lead_member))) = spawn the lead; Ok(None) = subagents;
/// Err(reason) = team start refused (e.g. one active team per session),
/// fall back to subagents and record the refusal.
pub(crate) fn try_team_run(
    config: &joey_core::Config,
    goal: &str,
    team_enabled: bool,
    explicit_workstreams: bool,
    workstream_count: usize,
) -> Result<Option<(String, String)>, String> {
    if route_mode(team_enabled, explicit_workstreams, workstream_count) != ModeRoute::Team {
        return Ok(None);
    }
    let team_name = team_slug(goal);
    match joey_orchestration::team::register_spawn(config, &team_name, None, goal, "lead") {
        Ok(spawn) => Ok(Some((team_name, spawn.member))),
        Err(e) => Err(e),
    }
}

/// Deterministic team-vs-subagent route for the /hypercode run pipeline
/// (research.md D5 — no keyword classifier; the planner's decomposition is
/// the independence signal). Disabled => always subagents (SC-005 parity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeRoute {
    Subagent,
    Team,
}

pub fn route_mode(team_enabled: bool, explicit_workstreams: bool, workstream_count: usize) -> ModeRoute {
    if team_enabled && !explicit_workstreams && workstream_count >= 2 {
        ModeRoute::Team
    } else {
        ModeRoute::Subagent
    }
}

/// Deterministic team name for a /hypercode team run: `hc-<slug>` where
/// slug is the first 24 goal chars, lowercased, team-filesystem-safe.
pub fn team_slug(goal: &str) -> String {
    let head: String = goal.chars().take(24).collect();
    let slug = joey_orchestration::team::sanitize_name(&head.to_lowercase());
    let slug = slug.trim_matches('_');
    format!("hc-{slug}")
}

/// Explorer system prompt (read-only context gathering — including running
/// read-only/diagnostic commands on the orchestrator's behalf).
pub const EXPLORER_PROMPT: &str = "\
You are the Explorer agent: READ-ONLY, facts only.\n\
\n\
1. Answer ONLY the questions in your brief, with evidence: exact file\n\
paths, line numbers, short verbatim quotes, and real command output.\n\
2. Run read-only/diagnostic commands as needed (rg, ls, git log/diff,\n\
cargo check, --help, version probes). NEVER modify anything.\n\
3. Do not analyze beyond the questions asked and do not propose\n\
solutions, plans, or recommendations — the orchestrator does all\n\
planning and interpretation. If a question cannot be answered from the\n\
code, say so plainly and report the closest evidence you found.\n\
\n\
Keep your final summary under 500 tokens.";

/// Implementor system prompt (execution only — the orchestrator owns all
/// planning and design decisions; the implementor applies fully-specified
/// briefs verbatim and verifies with targeted checks).
pub const IMPLEMENTOR_PROMPT: &str = "\
You are the Implementor agent: execution only.\n\
\n\
1. Follow the brief EXACTLY. It specifies the file paths, the precise\n\
edits to make, and the commands to run. Every planning and design\n\
decision was already made by the orchestrator — do not make, revise, or\n\
second-guess decisions.\n\
2. If the brief is ambiguous, incomplete, or conflicts with what you\n\
find (missing file, code differs from the description), STOP. Make no\n\
changes beyond what is unambiguous and report back exactly what is\n\
missing or contradictory. Never guess, infer, or fill gaps with your\n\
own judgment.\n\
3. Verify with TARGETED checks only: build the crates you touched\n\
(cargo build -p <crate>) and run only the scoped tests that cover your\n\
changes (cargo test -p <crate> [filter]). NEVER run the full test suite\n\
(cargo test --workspace) or any broad test run — the orchestrator runs\n\
that once, after all implementors finish.\n\
4. Report exactly what you changed, file by file, and the real scoped\n\
check output (command + outcome).\n\
\n\
Keep your final summary under 500 tokens.";

/// Orchestrator system prompt (delegation-first; no direct file writes or
/// code-manipulation commands).
///
/// The orchestrator runs on a powerful LLM while keeping its context lean:
/// code READING, file WRITING, and build/test execution happen in children.
/// The orchestrator keeps narrow supervision powers: read-only file peeks,
/// the `process` tool (list/poll/kill subagent processes), and web research.
pub const ORCHESTRATOR_PROMPT: &str = "\
You are the ORCHESTRATOR of a HyperCode pipeline. You coordinate the work;\n\
your subagents do the hands-on implementation.\n\
\n\
HARD RULES:\n\
- NEVER open your response with a tool call. Your first move is ALWAYS a\n\
  short written plan to the user — goal, task breakdown, which subagent\n\
  roles you will dispatch and why — BEFORE your first delegate_task.\n\
- NEVER write, patch, or delete files yourself — that is the Implementors' job.\n\
- NEVER run build/edit/test commands yourself while implementation waves\n\
  are in flight (cargo build/test, npm, git commit, formatters…) —\n\
  Implementors verify their own work with targeted checks. Your only test\n\
  run is the FINAL GATE: after the last Implementor wave completes, run\n\
  the project's full test suite (e.g. cargo test --workspace) exactly\n\
  once. If it fails, triage the output yourself and dispatch ONE final\n\
  fix round of Implementors (they verify fixes with targeted checks\n\
  only); if that round changed code you may run the full suite once more\n\
  to confirm, then stop.\n\
- NEVER claim to have done either. If a fact about the code or a command's\n\
  output matters, delegate for it; do not guess.\n\
\n\
WHAT YOU KEEP (supervision only):\n\
- delegate_task — your primary tool (see below).\n\
- read_file/search_files — PEEKING only: spot-check a specific file or\n\
  confirm a subagent's claim. Bulk code comprehension belongs to Explorers;\n\
  do not read whole files into your context.\n\
- terminal with process actions — monitor and manage subagent processes\n\
  (process list/poll/log/kill) and run trivial read-only probes (ls, pwd).\n\
- web tools — research docs, APIs, and context for your decisions.\n\
\n\
YOUR SUBAGENTS (via delegate_task):\n\
- role:\"explorer\" — read-only investigator. Give it focused FACTUAL\n\
  questions ('which file defines X', 'what does command Y print'). It\n\
  returns exact file paths, symbols, short quotes, and real command\n\
  output — facts only, never analysis, plans, or recommendations.\n\
  Interpreting its findings and deciding what to do is entirely your job.\n\
- role:\"implementor\" — execution only. Give it a fully-specified brief:\n\
  exact file paths, the precise edits to make (down to function/line\n\
  level wherever you know them), the exact commands to run, and the\n\
  expected result. It applies the brief verbatim, runs only the TARGETED\n\
  checks you list (e.g. cargo build -p <crate>, cargo test -p <crate>\n\
  [filter]) — never the full test suite — and reports what changed plus\n\
  the real check output.\n\
\n\
BRIEF QUALITY (execution orders, not problem statements):\n\
- Every brief must be complete enough that the subagent never needs to\n\
  think, infer, choose, or 'use judgment'. You already made every\n\
  decision: approach, file paths, exact edits, commands, expected\n\
  outcomes.\n\
- If you catch yourself writing 'investigate', 'consider', 'decide', 'as\n\
  appropriate', or 'the best approach' inside a brief — stop. Do that\n\
  thinking yourself first and put the conclusion in the brief instead.\n\
- If a subagent reports a brief was ambiguous or incomplete, that is a\n\
  planning failure on your side. Resolve it yourself (an Explorer may\n\
  fetch missing facts) and re-dispatch a corrected, fully-specified\n\
  brief. Never answer ambiguity with 'use your judgment'.\n\
\n\
WORK LOOP:\n\
1. Present a short written plan to the user FIRST, in a few concise bullets:\n\
   the goal, the task breakdown, and which subagent roles you will dispatch\n\
   and why. Then dispatch in the SAME turn — do not wait for the user to\n\
   confirm the plan unless the request is genuinely ambiguous.\n\
2. Fan out Explorers IN ONE delegate_task batch (tasks:[...]) whenever the\n\
   questions are independent — parallel dispatch is dramatically faster.\n\
3. Turn explorer findings into Implementor briefs YOU fully specify: the\n\
   approach, file paths, exact edits, and the targeted check commands\n\
   each implementor must run (scoped builds/tests of what it touched —\n\
   never the full suite). Parallelize implementors the same way, but\n\
   NEVER let two implementors edit the same file.\n\
4. When an implementor reports failure or an ambiguous brief, do the\n\
   diagnosis thinking yourself; delegate a focused Explorer only to\n\
   fetch missing facts, then dispatch a corrected Implementor brief.\n\
   Iterate.\n\
5. After the LAST Implementor wave completes, run the project's full\n\
   test suite ONCE yourself (the FINAL GATE, e.g. cargo test\n\
   --workspace). Green → synthesize and finish. Failures → triage the\n\
   output yourself, dispatch ONE final fix round of Implementors\n\
   (targeted checks only), optionally confirm once with the full suite,\n\
   then stop.\n\
6. Delegate as MANY subagents as the work genuinely needs — there is no\n\
   fixed cap; batch independent ones together.\n\
7. Monitor long-running work with the process tool; kill and re-delegate\n\
   when a subagent is stuck or off-track.\n\
\n\
ECONOMY (why this mode exists):\n\
- Your context stays small: summaries in, decisions out. Ask subagents for\n\
  exactly the facts you need to decide — never file dumps.\n\
- Prefer one batched delegate_task call over N sequential ones.\n\
\n\
FINAL ANSWER: synthesize the subagent reports for the user: what was done,\n\
files touched, verification results, and anything left open. Be honest\n\
about failures — your own verification is the single full-suite final\n\
gate; everything else you report comes from Implementors' targeted\n\
checks, so attribute it as such.\n\
\n\
## Execution Modes (feature 022: agent teams)\n\
\n\
Choose the optimum execution mode per task. ALWAYS state your chosen mode with a one-to-two-sentence rationale before dispatching:\n\
\n\
- TEAMS (delegate_task with `team` + `name`, requires hypercode.team.enabled): for independent, parallelizable work — for example research in one area while implementation proceeds in another, or debugging with competing hypotheses across separate areas. The first `team` reference lazily starts the team: start it without asking the user for confirmation.\n\
- SUBAGENTS or a single session: for sequential work, same-file edits, or heavily interdependent steps.\n\
- When ambiguous, prefer the cheaper mode (subagents) and say that you did.\n\
- While a team is active, keep routing new, unrelated tasks to subagents in parallel with the team — never queue work behind the team (one active team per session).\n\
- If team mode is disabled in configuration, route the work to subagents and inform the user that team mode is disabled.";

/// Planner prompt (decomposition into parallel workstreams).
pub const PLANNER_PROMPT: &str = "\
You are the Planner agent for a HyperCode parallel pipeline. Decompose the\n\
user's goal into INDEPENDENT workstreams that sibling agents can implement\n\
in parallel without colliding.\n\
\n\
Output STRICTLY this format (it is machine-parsed):\n\
<workstreams>\n\
1. <one-line description of workstream 1, naming the files/areas it owns>\n\
2. <workstream 2>\n\
...\n\
</workstreams>\n\
\n\
Rules:\n\
- 1 to N workstreams where N <= the stated cap; prefer the FEWEST streams\n\
  that still parallelize the goal meaningfully.\n\
- Each workstream must own a disjoint set of files/modules — sibling agents\n\
  implement them concurrently and cannot see each other's writes.\n\
- If the goal cannot be safely split (one tightly-coupled change), output\n\
  exactly ONE workstream covering the whole goal.\n\
- Investigate the repository first (read-only) so the split is real, not\n\
  guessed: list actual file paths in each stream's description.";

/// Options for a run.
#[derive(Debug, Clone)]
pub struct HypercodeOptions {
    /// Explicit workstreams (skips the planner phase when non-empty).
    pub workstreams: Vec<String>,
    /// Cap on workstreams (0 = config default).
    pub max_workstreams: usize,
    /// Provider name (for per-role config lookup).
    pub provider: String,
}

impl Default for HypercodeOptions {
    fn default() -> Self {
        Self {
            workstreams: Vec::new(),
            max_workstreams: 0,
            provider: String::new(),
        }
    }
}

/// Extract workstreams from a planner response's `<workstreams>` block.
/// Falls back to plain numbered/bulleted-line parsing when the tags are
/// missing (in fallback mode ONLY lines with a list prefix count — prose
/// is rejected; inside the tags every non-empty line counts).
pub fn parse_workstreams(planner_output: &str, cap: usize) -> Vec<Workstream> {
    let text = planner_output.trim();
    let tagged = match (text.find("<workstreams>"), text.find("</workstreams>")) {
        (Some(a), Some(b)) if b > a => Some(&text[a + "<workstreams>".len()..b]),
        _ => None,
    };
    let (inner, lenient) = match tagged {
        Some(inner) => (inner, true),
        None => (text, false),
    };
    let mut streams = Vec::new();
    for line in inner.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Strip "1." / "1)" / "-" / "*" / "•" prefixes.
        let (focus, had_prefix) = strip_list_prefix(line);
        if focus.is_empty() {
            continue;
        }
        // Ignore echoes of the tags themselves.
        if focus.starts_with('<') {
            continue;
        }
        // Fallback (no tags): only true list items count as streams.
        if !lenient && !had_prefix {
            continue;
        }
        streams.push(Workstream {
            id: streams.len(),
            focus: focus.to_string(),
        });
        if streams.len() >= cap {
            break;
        }
    }
    streams
}

/// Strip a leading list marker ("1.", "1)", "-", "*", "•") from a line.
/// Returns the remainder and whether a marker was present.
fn strip_list_prefix(line: &str) -> (&str, bool) {
    let rest = line.trim_start();
    // numbered prefix
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let after = &rest[digits..];
        if let Some(stripped) = after.strip_prefix('.') {
            return (stripped.trim(), true);
        }
        if let Some(stripped) = after.strip_prefix(')') {
            return (stripped.trim(), true);
        }
    }
    for marker in ["-", "*", "•"] {
        if let Some(stripped) = rest.strip_prefix(marker) {
            return (stripped.trim(), true);
        }
    }
    (rest, false)
}

/// Build the planner DelegationRequest.
fn planner_request(
    goal: &str,
    cfg: &HyperCodeConfig,
    opts: &HypercodeOptions,
    parent_model: &str,
) -> DelegationRequest {
    // The planner uses the IMPLEMENTOR config (it needs to reason about the
    // codebase but produces a tiny output). Toolsets: read-only + terminal so
    // it can inspect the repo (rg/cargo metadata) without write access.
    let rc = cfg.get_implementor_config(&opts.provider);
    DelegationRequest {
        goal: format!(
            "{PLANNER_PROMPT}\n\n=== GOAL ===\n{goal}\n\nMax workstreams: {}",
            effective_cap(cfg, opts)
        ),
        context: None,
        tasks: Vec::new(),
        model: model_override(&rc, parent_model, &opts.provider),
        toolsets: vec![
            "file-read".to_string(),
            "terminal".to_string(),
            "web".to_string(),
        ],
        max_turns: Some(rc.max_turns.max(4)),
        reasoning: parse_reasoning_level(&rc.reasoning_level),
        max_tokens: nonzero(rc.max_tokens),
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

/// Build an explorer request for one workstream.
///
/// Explorer is the orchestrator's read-only proxy INCLUDING terminal access
/// (diagnostic commands: grep, ls, git log, cargo check, --help probes) —
/// the orchestrator itself never runs commands.
fn explorer_request(
    ws: &Workstream,
    goal: &str,
    cfg: &HyperCodeConfig,
    opts: &HypercodeOptions,
    parent_model: &str,
    workdir: &std::path::Path,
) -> DelegationRequest {
    let rc = cfg.get_explorer_config(&opts.provider);
    DelegationRequest {
        goal: format!(
            "Explore the repository for HyperCode workstream #{}:\n{}\n\n(Overall goal: {})",
            ws.id, ws.focus, goal
        ),
        context: None,
        tasks: Vec::new(),
        model: model_override(&rc, parent_model, &opts.provider),
        toolsets: vec![
            "file-read".to_string(),
            "terminal".to_string(),
            "web".to_string(),
        ],
        max_turns: Some(rc.max_turns.max(4)),
        reasoning: parse_reasoning_level(&rc.reasoning_level),
        max_tokens: nonzero(rc.max_tokens),
        persist: false,
        role: SubagentRole::Leaf,
        workdir: Some(workdir.to_path_buf()),
        category: None,
        subagent_type: None,
        load_skills: Vec::new(),
        prompt_append: Some(EXPLORER_PROMPT.to_string()),
        team: None,
        name: None,
    }
}

/// Build an implementor request for one workstream.
///
/// Implementor owns the write path: edits plus the targeted checks that
/// verify them. The single full-suite run is the orchestrator's final gate.
fn implementor_request(
    ws: &Workstream,
    goal: &str,
    explorer_summary: &str,
    cfg: &HyperCodeConfig,
    opts: &HypercodeOptions,
    parent_model: &str,
    workdir: &std::path::Path,
) -> DelegationRequest {
    let rc = cfg.get_implementor_config(&opts.provider);
    DelegationRequest {
        goal: format!(
            "Implement HyperCode workstream #{}:\n{}\n\n(Overall goal: {})",
            ws.id, ws.focus, goal
        ),
        context: Some(format!(
            "--- Explorer brief for workstream #{} ---\n{}\n--- End brief ---",
            ws.id, explorer_summary
        )),
        tasks: Vec::new(),
        model: model_override(&rc, parent_model, &opts.provider),
        toolsets: vec![
            "file".to_string(),
            "terminal".to_string(),
            "web".to_string(),
        ],
        max_turns: Some(rc.max_turns.max(4)),
        reasoning: parse_reasoning_level(&rc.reasoning_level),
        max_tokens: nonzero(rc.max_tokens),
        persist: false,
        role: SubagentRole::Leaf,
        workdir: Some(workdir.to_path_buf()),
        category: None,
        subagent_type: None,
        load_skills: Vec::new(),
        prompt_append: Some(IMPLEMENTOR_PROMPT.to_string()),
        team: None,
        name: None,
    }
}

fn effective_cap(cfg: &HyperCodeConfig, opts: &HypercodeOptions) -> usize {
    if opts.max_workstreams > 0 {
        opts.max_workstreams
    } else {
        cfg.effective_max_workstreams()
    }
}

/// The model hypercode children inherit when a role table has no explicit
/// entry: the LIVE effective main-turn model when the caller captured it,
/// else the raw config default (legacy behavior, back-compatible).
fn parent_model_for(ctx: &HypercodeContext) -> String {
    ctx.parent_effective_model
        .clone()
        .unwrap_or_else(|| ctx.agent_config.model.clone())
}

/// Resolve a child's model: the role table's explicit model wins; an empty
/// entry inherits the parent's (effective) model explicitly so the config
/// `delegation.default_model` doesn't silently shadow the live agent.
///
/// Copilot-wire visibility: when the FINAL model is one the copilot-wire
/// provider cannot serve, the real Copilot backend 400s (ModelNotFound) and
/// proxies like ai-usage-hud silently substitute their default via mapModel
/// with HTTP 200. Warn so the substitution is visible in logs. Warn-only —
/// the returned model is unchanged.
fn model_override(rc: &RoleConfig, parent_model: &str, provider: &str) -> Option<String> {
    let model = if rc.model.is_empty() {
        parent_model.to_string()
    } else {
        rc.model.clone()
    };
    if copilot_wire_unservable(&model, provider) {
        tracing::warn!(
            provider = %provider,
            model = %model,
            "hypercode child model is not servable by the copilot-wire provider; the backend/proxy may 400 or silently substitute its default model (mapModel)"
        );
    }
    Some(model)
}

/// True when `model` cannot be served by a copilot-wire `provider`
/// (canonical name or alias — aliases are canonicalized via the profile
/// registry). Pure predicate for the warn-only visibility guard above.
fn copilot_wire_unservable(model: &str, provider: &str) -> bool {
    let canonical = joey_providers::profile::get_profile(provider)
        .map(|p| p.name.to_string())
        .unwrap_or_else(|| provider.to_string());
    joey_providers::profile::is_copilot_wire(&canonical)
        && !joey_providers::profile::copilot_servable(model)
}

fn nonzero(n: usize) -> Option<u32> {
    if n == 0 {
        None
    } else {
        Some(n as u32)
    }
}

/// AgentConfig the hypercode children run with: identical to the parent's
/// except `tool_delay`. Children are short-lived purpose-built workers on
/// curated toolsets; the parent's `agent.tool_delay` pacing (default 1s
/// between sequential tool calls) is pure added latency for them.
/// `hypercode.child_tool_delay` (default 0.0) disables it; set it to e.g.
/// 1.0 to restore parent-style pacing.
fn child_agent_config(ctx: &HypercodeContext) -> AgentConfig {
    let mut cfg = ctx.agent_config.clone();
    cfg.tool_delay = ctx
        .config
        .get_f64("hypercode.child_tool_delay", 0.0)
        .max(0.0);
    cfg
}

// ---------------------------------------------------------------------------
// Spec 023 (US3/T016): scheduler-driven execution of the typed graph
// ---------------------------------------------------------------------------

/// Single source of truth for mapping a worker's [`DelegationResult`] to
/// the boolean the scheduler's [`TaskDispatcher`] contract expects: the
/// SAME `success` field the legacy path treats as "worker did its job"
/// (see the build-results mapping in [`run_hypercode`]: `report.successes
/// = build_results.iter().map(|r| r.success)`) and the team path before
/// it (`report.successes.push(r.success)`). Extracted so the scheduler
/// adapter can never drift from the legacy semantics. Unit-testable.
pub(crate) fn delegation_succeeded(result: &joey_orchestration::DelegationResult) -> bool {
    result.success
}

/// PLACEHOLDER verification gate (Spec 023 T016): unconditionally passes.
/// Keeps US3 (scheduler-driven execution) runnable end-to-end without
/// US5 — the real VerifyLoop adapter lands in T023 and replaces this type
/// at the [`execute_graph_run`] / [`resume_execution_run`] call sites.
struct AlwaysPassGate;

#[async_trait::async_trait]
impl GateTrait for AlwaysPassGate {
    async fn run(&self, _plan: &VerificationPlanView, _workdir: &std::path::Path) -> GateOutcome {
        GateOutcome::Passed
    }
}

/// [`TaskDispatcher`] adapter (Spec 023 T016): hands one graph task to an
/// Implementor Leaf child through the SAME `SubagentManager` the legacy
/// phases (and `delegate_task`) use — one `DelegationRequest` per task,
/// mirroring [`implementor_request`]'s construction (model resolution via
/// the role table + `parent_model_for` inheritance, `file`/`terminal`/
/// `web` toolsets, `max_turns.max(4)`, `IMPLEMENTOR_PROMPT`).
struct HypercodeDispatcher<'a> {
    ctx: &'a HypercodeContext,
}

#[async_trait::async_trait]
impl TaskDispatcher for HypercodeDispatcher<'_> {
    async fn dispatch(&self, task: &TaskNode, workdir: &std::path::Path) -> bool {
        let cfg = HyperCodeConfig::from_config(&self.ctx.config);
        let provider = self.ctx.agent_config.provider.clone();
        let opts = HypercodeOptions {
            provider,
            ..Default::default()
        };
        let parent_model = parent_model_for(self.ctx);
        let rc = cfg.get_implementor_config(&opts.provider);
        let req = DelegationRequest {
            goal: format!(
                "Implement HyperCode task {}:\n{}\n(Project root: {})",
                task.id.as_str(),
                task.objective,
                workdir.display()
            ),
            context: None,
            tasks: Vec::new(),
            model: model_override(&rc, &parent_model, &opts.provider),
            toolsets: vec![
                "file".to_string(),
                "terminal".to_string(),
                "web".to_string(),
            ],
            max_turns: Some(rc.max_turns.max(4)),
            reasoning: parse_reasoning_level(&rc.reasoning_level),
            max_tokens: nonzero(rc.max_tokens),
            persist: false,
            role: SubagentRole::Leaf,
            workdir: Some(workdir.to_path_buf()),
            category: None,
            subagent_type: None,
            load_skills: Vec::new(),
            prompt_append: Some(IMPLEMENTOR_PROMPT.to_string()),
            team: None,
            name: None,
        };
        eprintln!(
            "hypercode: graph dispatching task {} ({} worker)",
            task.id.as_str(),
            match task.role {
                joey_orchestration::task_graph::WorkerRole::Explorer => "explorer",
                joey_orchestration::task_graph::WorkerRole::Implementor => "implementor",
                joey_orchestration::task_graph::WorkerRole::Orchestrator => "orchestrator",
            }
        );
        let child_agent_cfg = child_agent_config(self.ctx);
        let results = self
            .ctx
            .manager
            .dispatch_requests(
                &[req],
                &child_agent_cfg,
                &self.ctx.config,
                &self.ctx.base_registry,
                None,
            )
            .await;
        match results.first() {
            Some(r) => delegation_succeeded(r),
            None => false,
        }
    }
}

/// Execute the converted graph (T013's `ctx.execution_graph` slot) with
/// the deterministic wave scheduler (Spec 023 US3): takes the graph OUT of
/// the slot, runs `run_to_completion` against a fresh evidence run
/// directory, puts the mutated graph BACK (terminal statuses visible to
/// callers/tests), and folds a summary into `report`. Returns the
/// scheduler's counters.
async fn execute_graph_run(ctx: &HypercodeContext, report: &mut HypercodeReport) -> RunStats {
    let mut stats = RunStats::default();
    // Take the graph out of the slot (clone — graph is Clone).
    let graph = match ctx.execution_graph.lock() {
        Ok(mut slot) => slot.take(),
        Err(_) => {
            eprintln!("hypercode: execution graph mutex poisoned");
            return stats;
        }
    };
    let mut graph = match graph {
        Some(g) => g,
        None => return stats,
    };

    let baseline = baseline_revision(&ctx.cwd).unwrap_or_default();
    let run_id = format!("run-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
    let root = run_root(&ctx.cwd, &run_id);
    let mut run = match RunHandle::create_at(&root, &run_id, &baseline) {
        Ok(run) => run,
        Err(e) => {
            eprintln!("hypercode: failed to create run dir {}: {e}", root.display());
            // Put the untouched graph back before bailing.
            if let Ok(mut slot) = ctx.execution_graph.lock() {
                *slot = Some(graph);
            }
            return stats;
        }
    };

    let config = GraphSchedulerConfig {
        max_concurrent_workers: ctx
            .config
            .get_i64("hypercode.execution_graph.max_concurrent_workers", 16)
            .max(0) as usize,
        max_repair_attempts: ctx
            .config
            .get_i64("hypercode.execution_graph.max_repair_attempts", 3)
            .max(0) as u32,
    };
    let dispatcher = HypercodeDispatcher { ctx };
    stats = GraphScheduler::new(config)
        .run_to_completion(&mut graph, &mut run, &dispatcher, &AlwaysPassGate, &ctx.cwd)
        .await;

    let graph_for_report = graph.clone();
    // Put the mutated graph back (terminal statuses + attempts persist).
    if let Ok(mut slot) = ctx.execution_graph.lock() {
        *slot = Some(graph);
    }

    // Fold a summary into the report, mirroring the legacy fill shape
    // (successes/build_summaries aligned with the already-populated
    // workstreams — the conversion filled report.workstreams 1:1 with
    // graph nodes). Defensive: only align when the counts match.
    if report.workstreams.len() == graph_for_report.nodes.len() {
        for (_id, node) in &graph_for_report.nodes {
            let ok = matches!(node.status, TaskStatus::Completed);
            report.successes.push(ok);
            report.build_summaries.push(format!(
                "task {} → {:?}",
                node.id.as_str(),
                node.status
            ));
        }
    }
    report.mode_decisions.push(format_mode_decision(
        "execution-graph",
        "scheduler wave run",
        &format!(
            "{} completed, {} failed, {} degraded, {} blocked",
            stats.completed, stats.failed, stats.degraded, stats.blocked_remaining
        ),
    ));
    eprintln!(
        "hypercode: graph run {} — {} completed, {} failed, {} degraded, {} blocked",
        run_id, stats.completed, stats.failed, stats.degraded, stats.blocked_remaining
    );
    stats
}

/// Spec 023 (FR-013/FR-030) resume entry point: re-open a persisted run
/// directory, re-load its `graph.json`, refuse on baseline mismatch (via
/// [`RunHandle::resume_at`]), and drive the remaining `Pending` tasks to
/// completion with the same scheduler/dispatcher/gate wiring as
/// [`execute_graph_run`]. Returns `None` (with a stderr reason) when the
/// run cannot be resumed, and `Some(stats)` after the resumed wave run.
// Not yet reachable from the CLI tree (subcommand wiring is a later
// spec-023 task); the tests exercise it via the free functions above.
#[allow(dead_code)]
pub async fn resume_execution_run(ctx: &HypercodeContext, run_id: &str) -> Option<RunStats> {
    let root = run_root(&ctx.cwd, run_id);
    let current = baseline_revision(&ctx.cwd).unwrap_or_default();
    let mut run = match RunHandle::resume_at(&root, run_id, &current) {
        Err(e) => {
            eprintln!("hypercode: refusing to resume run {run_id} — {e}");
            return None;
        }
        Ok(run) => run,
    };
    let graph_raw = match std::fs::read_to_string(root.join("graph.json")) {
        Ok(raw) => raw,
        Err(e) => {
            eprintln!("hypercode: cannot read {} — {e}", root.join("graph.json").display());
            return None;
        }
    };
    let mut graph = match serde_json::from_str::<TaskGraph>(&graph_raw) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("hypercode: failed to parse graph.json for run {run_id} — {e}");
            return None;
        }
    };
    if !graph
        .nodes
        .values()
        .any(|n| n.status == TaskStatus::Pending)
    {
        eprintln!("hypercode: run {run_id} has no pending tasks — nothing to resume");
        return None;
    }
    let config = GraphSchedulerConfig {
        max_concurrent_workers: ctx
            .config
            .get_i64("hypercode.execution_graph.max_concurrent_workers", 16)
            .max(0) as usize,
        max_repair_attempts: ctx
            .config
            .get_i64("hypercode.execution_graph.max_repair_attempts", 3)
            .max(0) as u32,
    };
    let dispatcher = HypercodeDispatcher { ctx };
    let stats = GraphScheduler::new(config)
        .run_to_completion(&mut graph, &mut run, &dispatcher, &AlwaysPassGate, &ctx.cwd)
        .await;
    eprintln!(
        "hypercode: resumed run {run_id} — {} completed, {} failed, {} degraded, {} blocked",
        stats.completed, stats.failed, stats.degraded, stats.blocked_remaining
    );
    Some(stats)
}

/// Run the full HyperCode pipeline. Every child dispatch flows through the
/// manager (SubagentSpawn/SubagentEvent/SubagentComplete events hit the
/// global tap → TUI panes + rail + job board natively).
///
/// `progress` is invoked at each phase transition (thread-safe, may send
/// engine events). Returns the final report.
pub async fn run_hypercode(
    ctx: &HypercodeContext,
    goal: &str,
    opts: &HypercodeOptions,
    progress: Option<&ProgressFn<'_>>,
) -> HypercodeReport {
    let started = std::time::Instant::now();
    let cfg = HyperCodeConfig::from_config(&ctx.config);
    let provider = if opts.provider.is_empty() {
        ctx.agent_config.provider.clone()
    } else {
        opts.provider.clone()
    };
    let opts = &HypercodeOptions {
        workstreams: opts.workstreams.clone(),
        max_workstreams: opts.max_workstreams,
        provider,
    };
    // Parent-model inheritance: children inherit the LIVE effective
    // main-turn model (tier-routed / allocator-resolved — what the parent
    // actually dispatches with) when available, falling back to the raw
    // config default (legacy behavior) when the caller didn't capture it.
    let parent_model = parent_model_for(ctx);
    let cap = effective_cap(&cfg, opts);

    // Time-to-completion: children skip the parent's inter-tool pacing
    // (see child_agent_config).
    let child_agent_cfg = child_agent_config(ctx);

    let mut report = HypercodeReport::default();

    // ── Phase 1: Plan ─────────────────────────────────────────────────
    // Feature 022: explicit user-supplied workstreams pin the plan→explore→
    // build pipeline shape (never routed to team mode).
    let mut explicit_workstreams = false;
    let workstreams: Vec<Workstream> = if !opts.workstreams.is_empty() {
        explicit_workstreams = true;
        opts.workstreams
            .iter()
            .take(cap)
            .enumerate()
            .map(|(i, f)| Workstream {
                id: i,
                focus: f.clone(),
            })
            .collect()
    } else {
        if let Some(cb) = progress {
            cb(Phase::Planning, "decomposing the goal into workstreams");
        }
        let req = planner_request(goal, &cfg, opts, &parent_model);
        let results = ctx
            .manager
            .dispatch_requests(
                &[req],
                &child_agent_cfg,
                &ctx.config,
                &ctx.base_registry,
                None,
            )
            .await;
        let planned = results
            .first()
            .map(|r| parse_workstreams(&r.summary, cap))
            .unwrap_or_default();
        if planned.is_empty() {
            // Planner produced nothing parseable — degrade to a single
            // workstream covering the whole goal (the run still works).
            vec![Workstream {
                id: 0,
                focus: goal.to_string(),
            }]
        } else {
            planned
        }
    };
    report.workstreams = workstreams.clone();

    // Spec 023 (US2/T013, FR-007): convert the legacy `<workstreams>`
    // decomposition into a typed TaskGraph behind the
    // `hypercode.execution_graph.enabled` flag. Flag off (default) ⇒
    // pure no-op (SC-001); the pipeline below is untouched.
    if ctx.config.get_bool("hypercode.execution_graph.enabled", false) {
        let baseline = std::process::Command::new("git")
            .arg("rev-parse")
            .arg("HEAD")
            .current_dir(&ctx.cwd)
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    String::from_utf8(o.stdout).ok().map(|s| s.trim().to_string())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        let legacy: Vec<LegacyWorkstream> = workstreams
            .iter()
            .map(|w| LegacyWorkstream {
                id: w.id.to_string(),
                focus: w.focus.clone(),
            })
            .collect();
        let graph = TaskGraph::from_workstreams(&legacy, &baseline);
        match graph.validate() {
            Ok(()) => {
                if let Ok(mut slot) = ctx.execution_graph.lock() {
                    *slot = Some(graph);
                }
            }
            Err(errors) => {
                let report: Vec<String> = errors
                    .iter()
                    .map(|e| format!("  {:?}: {} ({})", e.task_ids, e.rule, e.detail))
                    .collect();
                eprintln!(
                    "hypercode: converted workstream plan failed validation (FR-009):\n{}",
                    report.join("\n")
                );
            }
        }
    }

    // Spec 023 (US3/T016): when the flag produced a validated graph, hand
    // the WHOLE run to the deterministic wave scheduler and return — the
    // legacy explorer/implementor phases never fire. Flag off (default)
    // ⇒ slot is None ⇒ this early return never taken (SC-001: the legacy
    // path below is byte-identical; the only flag-off cost is one mutex
    // lock + is_some check).
    if ctx
        .execution_graph
        .lock()
        .expect("execution graph mutex")
        .is_some()
    {
        let stats = execute_graph_run(ctx, &mut report).await;
        eprintln!(
            "hypercode: graph run complete — {} completed, {} failed, {} degraded, {} blocked",
            stats.completed, stats.failed, stats.degraded, stats.blocked_remaining
        );
        report.total_secs = started.elapsed().as_secs_f64();
        return report;
    }

    if ctx.manager.is_interrupted() {
        report.interrupted = true;
        report.total_secs = started.elapsed().as_secs_f64();
        return report;
    }

    // Feature 022 (US1/US2): route to team mode when the planner's
    // decomposition shows >=2 independent workstreams, team mode is
    // enabled, and the user did not pin explicit workstreams. The lead
    // child (Orchestrator role) coordinates teammates via the shared
    // task list + mailboxes; we block on its final synthesis (FR-019:
    // hypercode.team.lead_model, empty = inherit orchestrator model).
    let mut team_run: Option<(String, String)> = None;
    match try_team_run(
        &ctx.config,
        goal,
        cfg.team.enabled,
        explicit_workstreams,
        workstreams.len(),
    ) {
        Ok(Some(run)) => team_run = Some(run),
        Ok(None) => {}
        Err(e) => report.mode_decisions.push(format_mode_decision(
            "subagent",
            goal.lines()
                .next()
                .unwrap_or(goal)
                .chars()
                .take(60)
                .collect::<String>()
                .trim(),
            &format!("team start refused ({e}); ran via subagents"),
        )),
    }
    if let Some((team_name, member)) = team_run {
        let lead_req = lead_request(goal, &team_name, &member, &cfg.team);
        let results = ctx
            .manager
            .dispatch_requests(&[lead_req], &ctx.agent_config, &ctx.config, &ctx.base_registry, None)
            .await;
        report.mode_decisions.push(format_mode_decision(
            "team",
            goal.lines().next().unwrap_or(goal).chars().take(60).collect::<String>().trim(),
            &format!(
                "{} independent workstreams; lead coordinates teammates",
                workstreams.len()
            ),
        ));
        // Wind the team down (stop stragglers, keep tasks.json for
        // resumption) before reporting (US3 scenario 4).
        let _ = joey_orchestration::team::global_teams().stop_team(&team_name, &ctx.manager);
        if let Some(r) = results.first() {
            report.build_summaries.push(r.summary.clone());
            report.successes.push(r.success);
        }
        report.total_secs = started.elapsed().as_secs_f64();
        return report;
    }

    // ── Phase 2: Explore (parallel) ───────────────────────────────────
    if let Some(cb) = progress {
        cb(
            Phase::Exploring,
            &format!("{} explorer agent(s) gathering context", workstreams.len()),
        );
    }
    let explorer_requests: Vec<DelegationRequest> = workstreams
        .iter()
        .map(|ws| explorer_request(ws, goal, &cfg, opts, &parent_model, &ctx.cwd))
        .collect();
    let explorer_results = ctx
        .manager
        .dispatch_requests(
            &explorer_requests,
            &child_agent_cfg,
            &ctx.config,
            &ctx.base_registry,
            None,
        )
        .await;
    let explorer_summaries: Vec<String> = explorer_results
        .iter()
        .map(|r| {
            if r.success {
                r.summary.clone()
            } else {
                format!(
                    "(explorer failed: {}) Proceed using your own investigation.",
                    r.error.as_deref().unwrap_or("unknown error")
                )
            }
        })
        .collect();

    if ctx.manager.is_interrupted() {
        report.interrupted = true;
        report.total_secs = started.elapsed().as_secs_f64();
        return report;
    }

    // ── Phase 3: Build (parallel) ─────────────────────────────────────
    if let Some(cb) = progress {
        cb(
            Phase::Building,
            &format!("{} implementor agent(s) working", workstreams.len()),
        );
    }
    let build_requests: Vec<DelegationRequest> = workstreams
        .iter()
        .zip(explorer_summaries.iter())
        .map(|(ws, brief)| {
            implementor_request(ws, goal, brief, &cfg, opts, &parent_model, &ctx.cwd)
        })
        .collect();
    let build_results = ctx
        .manager
        .dispatch_requests(
            &build_requests,
            &child_agent_cfg,
            &ctx.config,
            &ctx.base_registry,
            None,
        )
        .await;
    report.build_summaries = build_results.iter().map(|r| r.summary.clone()).collect();
    report.successes = build_results.iter().map(|r| r.success).collect();
    report.total_secs = started.elapsed().as_secs_f64();

    // ── Phase 4: Synthesize (in-memory merge; no extra LLM call) ──────
    if let Some(cb) = progress {
        cb(Phase::Synthesizing, "merging workstream reports");
    }

    // Feature 022 (FR-016): record the pipeline's mode decision. The
    // /hypercode pipeline itself always runs via subagents — its stages
    // (plan → explore → build) are strictly interdependent, so team mode
    // is never routed here even when enabled. Skipped when a decision was
    // already recorded (team run above or a refused team start).
    if report.mode_decisions.is_empty() {
        let rationale = if !cfg.team.enabled {
            "team mode is disabled; work ran via subagents"
        } else if explicit_workstreams {
            "explicit workstreams pin the plan→explore→build pipeline"
        } else {
            "pipeline stages are interdependent; team mode is reserved for independent parallel work"
        };
        report.mode_decisions.push(format_mode_decision(
            "subagent",
            goal
                .lines()
                .next()
                .unwrap_or(goal)
                .chars()
                .take(60)
                .collect::<String>()
                .trim(),
            rationale,
        ));
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hypercode_config_defaults() {
        let config = HyperCodeConfig::default();
        let explorer = config.get_explorer_config("unknown-provider");
        assert_eq!(explorer.model, "");
        assert_eq!(explorer.max_turns, 8);
        assert_eq!(explorer.reasoning_level, "");

        let impl_cfg = config.get_implementor_config("unknown-provider");
        assert_eq!(impl_cfg.model, "");
        assert_eq!(impl_cfg.max_turns, 12);
    }

    #[test]
    fn child_agent_config_zeroes_tool_delay_by_default() {
        let parent = AgentConfig {
            model: "test-model".to_string(),
            provider: "zai".to_string(),
            base_url: String::new(),
            api_key: None,
            max_turns: 10,
            api_max_retries: 3,
            tool_delay: 1.0,
            reasoning: None,
            enabled_tools: vec![],
            max_tokens: None,
            stream: false,
            pass_session_id: false,
            model_pinned: false,
        };
        let ctx = HypercodeContext {
            agent_config: parent.clone(),
            config: joey_core::Config::defaults(),
            base_registry: ToolRegistry::new(),
            manager: std::sync::Arc::new(
                joey_orchestration::SubagentManager::new(
                    joey_orchestration::ManagerConfig::default(),
                ),
            ),
            cwd: std::env::temp_dir(),
            parent_effective_model: None,
            execution_graph: std::sync::Arc::new(std::sync::Mutex::new(None)),
        };
        assert_eq!(ctx.agent_config.tool_delay, 1.0);
        let child = child_agent_config(&ctx);
        assert_eq!(
            child.tool_delay, 0.0,
            "hypercode children skip the parent's inter-tool pacing by default"
        );
    }

    #[test]
    fn test_config_per_provider() {
        let mut config = HyperCodeConfig::default();
        config.set_explorer_config(
            "test-provider".to_string(),
            RoleConfig {
                model: "custom-explorer".to_string(),
                max_tokens: 8000,
                max_turns: 5,
                reasoning_level: "high".to_string(),
            },
        );

        let explorer = config.get_explorer_config("test-provider");
        assert_eq!(explorer.model, "custom-explorer");
        assert_eq!(explorer.max_tokens, 8000);
        assert_eq!(explorer.reasoning_level, "high");
    }

    #[test]
    fn test_parse_workstreams_tagged() {
        let out = "Here's the split:\n<workstreams>\n1. Add X to crates/foo/src/lib.rs\n2. Extend bar: crates/bar/src/m.rs\n</workstreams>\nGood luck.";
        let ws = parse_workstreams(out, 5);
        assert_eq!(ws.len(), 2);
        assert_eq!(ws[0].focus, "Add X to crates/foo/src/lib.rs");
        assert_eq!(ws[1].focus, "Extend bar: crates/bar/src/m.rs");
    }

    #[test]
    fn test_parse_workstreams_plain_numbered() {
        let out = "1. First stream\n2) Second stream\n- third stream\n\nrandom prose line";
        let ws = parse_workstreams(out, 5);
        assert_eq!(ws.len(), 3);
        assert_eq!(ws[0].focus, "First stream");
        assert_eq!(ws[1].focus, "Second stream");
        assert_eq!(ws[2].focus, "third stream");
    }

    #[test]
    fn test_parse_workstreams_cap() {
        let out = "<workstreams>\n1. one\n2. two\n3. three\n</workstreams>";
        let ws = parse_workstreams(out, 2);
        assert_eq!(ws.len(), 2);
    }

    #[test]
    fn test_parse_workstreams_garbage_yields_empty() {
        assert!(parse_workstreams("", 5).is_empty());
        assert!(parse_workstreams("no list here\nat all", 5).is_empty());
    }

    #[test]
    fn test_parse_reasoning_level() {
        assert_eq!(parse_reasoning_level(""), None);
        assert_eq!(parse_reasoning_level("inherit"), None);
        assert_eq!(
            parse_reasoning_level("none"),
            Some(ReasoningEffort::Disabled)
        );
        assert_eq!(
            parse_reasoning_level("High"),
            Some(ReasoningEffort::Level("high".to_string()))
        );
    }

    #[test]
    fn test_report_render() {
        let report = HypercodeReport {
            workstreams: vec![
                Workstream {
                    id: 0,
                    focus: "stream A".into(),
                },
                Workstream {
                    id: 1,
                    focus: "stream B".into(),
                },
            ],
            build_summaries: vec![
                "Edited crates/a/src/lib.rs\n- added foo".to_string(),
                String::new(),
            ],
            successes: vec![true, false],
            total_secs: 12.3,
            interrupted: false,
            mode_decisions: Vec::new(),
        };
        let lines = report.render();
        assert!(lines[0].contains("1/2 workstream(s) succeeded"));
        assert!(lines[1].contains("✓ [0] stream A"));
        assert!(lines[1].contains("✗") || lines[3].contains("✗ [1] stream B"));
        assert!(lines[2].contains("Edited crates/a/src/lib.rs"));
    }

    #[test]
    fn test_request_builders_use_role_config() {
        let mut cfg = HyperCodeConfig::default();
        cfg.set_explorer_config(
            "prov".into(),
            RoleConfig {
                model: "explorer-model".into(),
                max_tokens: 4000,
                max_turns: 6,
                reasoning_level: "high".into(),
            },
        );
        let opts = HypercodeOptions {
            provider: "prov".into(),
            ..Default::default()
        };
        let ws = Workstream {
            id: 0,
            focus: "do things".into(),
        };
        let req = explorer_request(&ws, "goal", &cfg, &opts, "parent-model", std::path::Path::new("/tmp"));
        assert_eq!(req.model.as_deref(), Some("explorer-model"));
        assert_eq!(req.max_turns, Some(6));
        assert_eq!(req.max_tokens, Some(4000));
        assert_eq!(req.reasoning, Some(ReasoningEffort::Level("high".into())));
        assert_eq!(req.toolsets, vec!["file-read".to_string(), "terminal".to_string(), "web".to_string()]);
        assert_eq!(req.prompt_append.as_deref(), Some(EXPLORER_PROMPT));
    }

    #[test]
    fn test_request_builders_inherit_model_when_unset() {
        let cfg = HyperCodeConfig::default();
        let opts = HypercodeOptions {
            provider: "any".into(),
            ..Default::default()
        };
        let ws = Workstream {
            id: 1,
            focus: "f".into(),
        };
        let req = explorer_request(&ws, "g", &cfg, &opts, "live-model", std::path::Path::new("/tmp"));
        // Empty role model → inherit the live parent model (not delegation.default_model).
        assert_eq!(req.model.as_deref(), Some("live-model"));
        assert_eq!(req.max_tokens, None);
    }

    // ── Parent-model inheritance (parent_effective_model) ─────────────

    /// Minimal context with only what parent_model_for / model_override
    /// need (full construction is heavy — no registry/manager required).
    fn model_ctx(provider: &str, effective: Option<&str>) -> HypercodeContext {
        HypercodeContext {
            agent_config: AgentConfig {
                model: "config-raw-model".to_string(),
                provider: provider.to_string(),
                base_url: String::new(),
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
            },
            config: joey_core::Config::defaults(),
            base_registry: ToolRegistry::new(),
            manager: Arc::new(SubagentManager::new(
                joey_orchestration::ManagerConfig::default(),
            )),
            cwd: std::path::PathBuf::from("/tmp"),
            parent_effective_model: effective.map(|s| s.to_string()),
            execution_graph: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Children inherit the LIVE effective model (parent_effective_model)
    /// when the role table is EMPTY for the provider — not the raw config
    /// default. This is the regression the field exists for: on a
    /// copilot-wire provider the parent's turns are tier-routed to a
    /// servable model while agent_config.model (glm-5.2) is unservable.
    #[test]
    fn run_hypercode_children_use_parent_effective_model_when_role_table_empty() {
        // Legacy context (None) keeps the raw config model — back-compat.
        assert_eq!(
            parent_model_for(&model_ctx("zai", None)),
            "config-raw-model",
            "None parent_effective_model falls back to agent_config.model (legacy)"
        );

        // Live capture: children inherit the tier-routed model the parent
        // actually dispatches with. Verified through the same path
        // run_hypercode uses (parent_model_for → model_override) with an
        // EMPTY role table for the provider.
        let ctx = model_ctx("github-copilot", Some("gpt-5.6-sol"));
        let cfg = HyperCodeConfig::default(); // no entries for this provider
        let opts = HypercodeOptions {
            provider: "github-copilot".into(),
            ..Default::default()
        };
        let rc = cfg.get_explorer_config(&opts.provider);
        assert!(rc.model.is_empty(), "role table is empty for this provider");
        let parent_model = parent_model_for(&ctx);
        assert_eq!(parent_model, "gpt-5.6-sol");
        assert_eq!(
            model_override(&rc, &parent_model, &opts.provider).as_deref(),
            Some("gpt-5.6-sol"),
            "children must inherit the parent's EFFECTIVE model when no role entry exists"
        );

        // Explicit role-table entries still win over the parent model.
        let mut cfg = HyperCodeConfig::default();
        cfg.set_explorer_config(
            "github-copilot".into(),
            RoleConfig {
                model: "role-table-model".into(),
                ..Default::default()
            },
        );
        let rc = cfg.get_explorer_config(&opts.provider);
        assert_eq!(
            model_override(&rc, &parent_model, &opts.provider).as_deref(),
            Some("role-table-model")
        );
    }

    /// The copilot-wire unservable-model guard: warn-only visibility, the
    /// returned model is unchanged (glm-5.2 is not copilot-servable).
    /// Non-copilot providers never trip the guard.
    #[test]
    fn model_override_flags_unservable_copilot_wire_models() {
        // Order-dependence guard: drain the process-global copilot catalog
        // cache (other tests warm it via real HTTP) and SEED it with a catalog
        // that contains "gpt-5.4", so copilot_servable is deterministic here.
        let saved_catalog = joey_providers::copilot::take_catalog_cache_for_tests();
        joey_providers::copilot::restore_catalog_cache_for_tests(Some((
            vec![serde_json::json!({ "id": "gpt-5.4" })],
            std::time::Instant::now(),
        )));
        assert!(joey_providers::profile::copilot_servable("gpt-5.4"));
        let rc = RoleConfig::default(); // empty → inherit parent model

        // copilot-wire + unservable inherited model: still returned as-is.
        assert_eq!(
            model_override(&rc, "glm-5.2", "github-copilot").as_deref(),
            Some("glm-5.2")
        );
        assert!(copilot_wire_unservable("glm-5.2", "github-copilot"));
        assert!(copilot_wire_unservable("glm-5.2", "copilot"));

        // copilot-wire + servable model: no guard.
        assert!(!copilot_wire_unservable("gpt-5.4", "github-copilot"));

        // Non-copilot provider: never guarded, whatever the model.
        assert!(!copilot_wire_unservable("glm-5.2", "zai"));
        assert_eq!(
            model_override(&rc, "glm-5.2", "zai").as_deref(),
            Some("glm-5.2")
        );
        joey_providers::copilot::restore_catalog_cache_for_tests(saved_catalog);
    }

    #[test]
    fn test_planner_request_carries_prompt_and_cap() {
        let cfg = HyperCodeConfig::default();
        let opts = HypercodeOptions {
            provider: "p".into(),
            ..Default::default()
        };
        let req = planner_request("my goal", &cfg, &opts, "m");
        assert!(req.goal.starts_with("You are the Planner agent"));
        assert!(req.goal.contains("my goal"));
        assert!(req.goal.contains(&format!("Max workstreams: {}", DEFAULT_MAX_WORKSTREAMS)));
    }

    // ── Orchestrator mode ─────────────────────────────────────────────

    fn config_with_yaml(yaml: &str) -> joey_core::Config {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), yaml).unwrap();
        joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap()
    }

    #[test]
    fn orchestrator_mode_defaults_on_and_loads_from_config() {
        // Default: ON.
        let cfg = HyperCodeConfig::default();
        assert!(cfg.orchestrator_mode);

        // from_config: absent key → true (default).
        let tree = config_with_yaml("model:\n  default: m\n");
        assert!(HyperCodeConfig::from_config(&tree).orchestrator_mode);

        // Explicit off.
        let tree = config_with_yaml("hypercode:\n  orchestrator_mode: false\n");
        assert!(!HyperCodeConfig::from_config(&tree).orchestrator_mode);

        // Explicit on.
        let tree = config_with_yaml("hypercode:\n  enabled: true\n  orchestrator_mode: true\n");
        assert!(HyperCodeConfig::from_config(&tree).orchestrator_mode);
    }

    #[test]
    fn orchestrator_active_requires_both_flags() {
        let off_off = config_with_yaml("");
        assert!(!orchestrator_active(&off_off));

        let enabled_orch = config_with_yaml("hypercode:\n  enabled: true\n");
        assert!(orchestrator_active(&enabled_orch));

        let enabled_no_orch = config_with_yaml("hypercode:\n  enabled: true\n  orchestrator_mode: false\n");
        assert!(!orchestrator_active(&enabled_no_orch));
    }

    #[test]
    fn apply_orchestrator_restricts_tools_to_delegate_task() {
        // Off → untouched, returns false.
        let tree = config_with_yaml("");
        let mut ac = joey_agent_core::AgentConfig::from_config(&tree);
        ac.enabled_tools = vec!["read_file".into(), "write_file".into(), "delegate_task".into()];
        assert!(!apply_orchestrator_to_agent_config(&tree, &mut ac));
        assert_eq!(ac.enabled_tools.len(), 3);

        // On → delegation + supervision surface (terminal/process, read-only
        // files, web) — but NO write_file/patch.
        let tree = config_with_yaml("hypercode:\n  enabled: true\n");
        assert!(apply_orchestrator_to_agent_config(&tree, &mut ac));
        assert!(ac.enabled_tools.contains(&"delegate_task".to_string()));
        assert!(ac.enabled_tools.contains(&"terminal".to_string()), "process monitoring");
        assert!(ac.enabled_tools.contains(&"process".to_string()), "subagent process mgmt");
        assert!(ac.enabled_tools.contains(&"read_file".to_string()), "read-only peeking");
        assert!(ac.enabled_tools.contains(&"web_search".to_string()), "web research");
        assert!(!ac.enabled_tools.contains(&"write_file".to_string()), "no direct writes");
        assert!(!ac.enabled_tools.contains(&"patch".to_string()), "no direct patches");
    }

    #[test]
    fn orchestrator_overlay_mentions_roles_and_guardrails() {
        let o = orchestrator_overlay();
        assert!(o.contains("role:\"explorer\""));
        assert!(o.contains("role:\"implementor\""));
        assert!(o.contains("NEVER write, patch, or delete files"));
        assert!(o.contains("NEVER run build/edit/test commands"));
        assert!(o.contains("process tool"));
        assert!(o.contains("web tools"));
    }

    #[test]
    fn orchestrator_overlay_mandates_plan_first_response() {
        let o = orchestrator_overlay();
        // Hard rule: the orchestrator must never open with a tool call —
        // a written plan always comes before the first delegate_task.
        assert!(o.contains("NEVER open your response with a tool call"));
        assert!(o.contains("BEFORE your first delegate_task"));
        // Work loop step 1 is presenting the plan, then dispatching in the
        // same turn (no waiting on user confirmation unless ambiguous).
        let work_loop = o.split("WORK LOOP:").nth(1).expect("WORK LOOP section");
        assert!(
            work_loop.trim_start().starts_with("1. Present a short written plan"),
            "step 1 of the work loop must be presenting the plan"
        );
        assert!(o.contains("the goal, the task breakdown, and which subagent roles you will dispatch"));
        assert!(o.contains("dispatch in the SAME turn"));
        assert!(o.contains("genuinely ambiguous"));
        // The fan-out guidance survives as a later step.
        assert!(o.contains("Fan out Explorers IN ONE delegate_task batch"));
        // ECONOMY and FINAL ANSWER sections unchanged.
        assert!(o.contains("ECONOMY (why this mode exists):"));
        assert!(o.contains("FINAL ANSWER:"));
    }

    #[test]
    fn explorer_and_implementor_requests_match_roles() {
        let cfg = HyperCodeConfig::default();
        let opts = HypercodeOptions { provider: "p".into(), ..Default::default() };
        let ws = Workstream { id: 0, focus: "f".into() };

        // Explorer: READ-ONLY files + terminal + web.
        let ex = explorer_request(&ws, "g", &cfg, &opts, "m", std::path::Path::new("/tmp"));
        assert!(ex.toolsets.contains(&"file-read".to_string()));
        assert!(!ex.toolsets.contains(&"file".to_string()), "explorer must NOT have write access");
        assert!(ex.toolsets.contains(&"terminal".to_string()), "explorer runs diagnostic commands");
        assert!(ex.toolsets.contains(&"web".to_string()));
        assert!(ex.prompt_append.as_deref().unwrap_or("").contains("Explorer agent"));
        assert!(ex.prompt_append.as_deref().unwrap_or("").contains("READ-ONLY"));

        // Implementor: write access + terminal + web.
        let im = implementor_request(&ws, "g", "brief", &cfg, &opts, "m", std::path::Path::new("/tmp"));
        assert!(im.toolsets.contains(&"file".to_string()), "implementor owns the write path");
        assert!(im.toolsets.contains(&"terminal".to_string()));
        assert!(im.prompt_append.as_deref().unwrap_or("").contains("Implementor agent"));
    }

    // ── Spec 023 T013: execution_graph conversion wiring ──────────────

    /// SC-001 parity: the flag defaults OFF and a freshly built context
    /// carries no execution graph — with the flag off the conversion
    /// block in run_hypercode is a pure no-op (same gate expression the
    /// block uses: `config.get_bool("hypercode.execution_graph.enabled", false)`).
    #[test]
    fn execution_graph_flag_gating_defaults_off_and_slot_empty() {
        // Default config: flag off.
        let defaults = joey_core::Config::defaults();
        assert!(!defaults.get_bool("hypercode.execution_graph.enabled", false));

        // Explicit on/off round-trips through the same accessor the
        // run_hypercode block uses.
        let on = config_with_yaml("hypercode:\n  execution_graph:\n    enabled: true\n");
        assert!(on.get_bool("hypercode.execution_graph.enabled", false));
        let off = config_with_yaml("hypercode:\n  execution_graph:\n    enabled: false\n");
        assert!(!off.get_bool("hypercode.execution_graph.enabled", false));

        // A context built the way every construction site builds it
        // starts with NO graph — flag-off runs leave it None.
        let ctx = model_ctx("zai", None);
        assert!(
            ctx.execution_graph.lock().unwrap().is_none(),
            "execution_graph must start None (SC-001 flag-off parity)"
        );
    }

    /// FR-007 equivalence, checked inline against the real parse path:
    /// parse_workstreams output → LegacyWorkstream mapping (the exact
    /// mapping run_hypercode uses) → TaskGraph nodes 1:1, focus preserved
    /// as objective, validates Ok. Full-pipeline construction is heavy
    /// (real subagent dispatch), so the conversion is exercised at the
    /// same seam the flag-gated block sits on.
    #[test]
    fn execution_graph_from_workstreams_equivalence() {
        let planner_out = "<workstreams>\n1. Add X to crates/foo/src/lib.rs\n2. Extend bar: crates/bar/src/m.rs\n</workstreams>";
        let workstreams = parse_workstreams(planner_out, 5);
        assert_eq!(workstreams.len(), 2);

        let legacy: Vec<LegacyWorkstream> = workstreams
            .iter()
            .map(|w| LegacyWorkstream {
                id: w.id.to_string(),
                focus: w.focus.clone(),
            })
            .collect();
        let graph = TaskGraph::from_workstreams(&legacy, "deadbeef");
        assert_eq!(graph.nodes.len(), workstreams.len(), "1 workstream = 1 node");
        for w in &workstreams {
            let key = joey_orchestration::task_graph::TaskId::new(&format!(
                "workstream-{}",
                w.id
            ))
            .expect("numeric id is valid");
            let node = graph.node(&key).expect("node exists per workstream");
            assert_eq!(node.objective, w.focus, "focus preserved as objective");
        }
        assert_eq!(graph.baseline_revision, "deadbeef");
        assert_eq!(graph.validate(), Ok(()), "converted plan must validate");
    }

    // ── Spec 023 T016: scheduler wiring ───────────────────────────────

    /// Minimal DelegationResult literal for the success-mapping test.
    fn del_result(success: bool) -> joey_orchestration::DelegationResult {
        joey_orchestration::DelegationResult {
            goal: "g".to_string(),
            summary: "s".to_string(),
            success,
            error: if success { None } else { Some("boom".to_string()) },
            token_usage: Default::default(),
            wall_clock: std::time::Duration::from_secs(1),
            model: "m".to_string(),
            iterations: 1,
            persisted_session_id: None,
            stop_reason: None,
        }
    }

    /// delegation_succeeded mirrors the legacy predicate exactly: the
    /// `success` field the build/team paths already map into
    /// report.successes.
    #[test]
    fn delegation_succeeded_maps_both_outcomes() {
        assert!(delegation_succeeded(&del_result(true)));
        assert!(!delegation_succeeded(&del_result(false)));
    }

    /// The placeholder gate passes unconditionally (keeps US3 runnable
    /// end-to-end until T023's VerifyLoop adapter replaces it).
    #[tokio::test]
    async fn always_pass_gate_returns_passed() {
        let gate = AlwaysPassGate;
        let outcome = GateTrait::run(&gate, &VerificationPlanView::default(), std::path::Path::new("/tmp")).await;
        assert_eq!(outcome, GateOutcome::Passed);
    }

    /// Run directories live under hypercode/projects/<hash>/runs/<run_id>
    /// (evidence.rs run_root shape).
    #[test]
    fn run_id_and_root_shape() {
        let root = run_root(std::path::Path::new("/tmp/proj"), "run-20990101-000000");
        let s = root.display().to_string();
        assert!(s.contains("hypercode"), "root must sit under hypercode/: {s}");
        assert!(s.contains("projects"), "root must sit under projects/: {s}");
        assert!(s.contains("runs"), "root must sit under runs/: {s}");
        assert!(s.ends_with("run-20990101-000000"), "root must end with the run id: {s}");
    }
}
