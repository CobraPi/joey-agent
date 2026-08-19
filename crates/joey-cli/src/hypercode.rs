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

impl Default for HyperCodeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            explorer_configs: HashMap::new(),
            implementor_configs: HashMap::new(),
            max_workstreams: 0,
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
        out
    }
}

/// Explorer system prompt (read-only context gathering).
pub const EXPLORER_PROMPT: &str = "\
You are an Explorer agent in a HyperCode parallel pipeline. Your job is to:\n\
1. Locate the relevant code, tests, and documentation for your assigned workstream\n\
2. Identify dependencies, relationships, and integration points\n\
3. Surface gotchas, edge cases, and risks\n\
4. Produce a concise, actionable brief for an Implementor agent\n\
\n\
Rules:\n\
- READ-ONLY: do not modify, create, or delete any files. Exploration only.\n\
- Your final message is handed verbatim to the Implementor agent for this\n\
  workstream — make it a self-contained implementation brief: exact file\n\
  paths, function/type names, relevant snippets, and a suggested approach.\n\
- Keep the brief under 600 tokens.";

/// Implementor system prompt (focused implementation).
pub const IMPLEMENTOR_PROMPT: &str = "\
You are an Implementor agent in a HyperCode parallel pipeline. Your job is to:\n\
1. Implement your assigned workstream following the Explorer's brief\n\
2. Write clean, correct code that follows the project's conventions\n\
3. Build and run the project's tests for what you touched (cargo build / cargo test scoped)\n\
4. Report exactly what you changed, file by file, and the verification result\n\
\n\
Rules:\n\
- Touch ONLY the files in scope for your workstream; other workstreams are\n\
  being implemented in parallel by sibling agents — writing to their files\n\
  will collide.\n\
- If the brief is insufficient, make the smallest reasonable decision and\n\
  note it in your report rather than expanding scope.\n\
- Keep your final report under 500 tokens.";

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
    // codebase but produces a tiny output).
    let rc = cfg.get_implementor_config(&opts.provider);
    DelegationRequest {
        goal: format!(
            "{PLANNER_PROMPT}\n\n=== GOAL ===\n{goal}\n\nMax workstreams: {}",
            effective_cap(cfg, opts)
        ),
        context: None,
        tasks: Vec::new(),
        model: model_override(&rc, parent_model),
        toolsets: vec!["file".to_string()],
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
    }
}

/// Build an explorer request for one workstream.
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
        model: model_override(&rc, parent_model),
        toolsets: vec!["file".to_string()],
        max_turns: Some(rc.max_turns.max(2)),
        reasoning: parse_reasoning_level(&rc.reasoning_level),
        max_tokens: nonzero(rc.max_tokens),
        persist: false,
        role: SubagentRole::Leaf,
        workdir: Some(workdir.to_path_buf()),
        category: None,
        subagent_type: None,
        load_skills: Vec::new(),
        prompt_append: Some(EXPLORER_PROMPT.to_string()),
    }
}

/// Build an implementor request for one workstream.
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
        model: model_override(&rc, parent_model),
        toolsets: vec![
            "file".to_string(),
            "terminal".to_string(),
        ],
        max_turns: Some(rc.max_turns.max(2)),
        reasoning: parse_reasoning_level(&rc.reasoning_level),
        max_tokens: nonzero(rc.max_tokens),
        persist: false,
        role: SubagentRole::Leaf,
        workdir: Some(workdir.to_path_buf()),
        category: None,
        subagent_type: None,
        load_skills: Vec::new(),
        prompt_append: Some(IMPLEMENTOR_PROMPT.to_string()),
    }
}

fn effective_cap(cfg: &HyperCodeConfig, opts: &HypercodeOptions) -> usize {
    if opts.max_workstreams > 0 {
        opts.max_workstreams
    } else {
        cfg.effective_max_workstreams()
    }
}

fn model_override(rc: &RoleConfig, parent_model: &str) -> Option<String> {
    if rc.model.is_empty() {
        // Inherit the parent's model explicitly so the config
        // `delegation.default_model` doesn't silently shadow the live agent.
        Some(parent_model.to_string())
    } else {
        Some(rc.model.clone())
    }
}

fn nonzero(n: usize) -> Option<u32> {
    if n == 0 {
        None
    } else {
        Some(n as u32)
    }
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
    let parent_model = ctx.agent_config.model.clone();
    let cap = effective_cap(&cfg, opts);

    let mut report = HypercodeReport::default();

    // ── Phase 1: Plan ─────────────────────────────────────────────────
    let workstreams: Vec<Workstream> = if !opts.workstreams.is_empty() {
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
                &ctx.agent_config,
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

    if ctx.manager.is_interrupted() {
        report.interrupted = true;
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
            &ctx.agent_config,
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
            &ctx.agent_config,
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
        assert_eq!(req.toolsets, vec!["file".to_string()]);
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
}
