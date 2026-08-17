//! Oneshot (`-z`) mode: send a prompt, print ONLY the final text, exit
//! (port of `hermes_cli/oneshot.py`).
//!
//! Toolsets = explicit `--toolsets` when provided, otherwise whatever the
//! user has configured for "cli" in `joey tools`. Approvals are auto-bypassed
//! (`JOEY_YOLO_MODE=1`). Model/provider selection mirrors `joey chat`:
//! both optional; `--provider` without a model errors out; an explicit
//! `--model` auto-detects the provider that serves it. `JOEY_INFERENCE_MODEL`
//! is honored here (and ONLY here — oneshot.py:205).

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use joey_agent_core::{Agent, AgentConfig, AgentEvent};
use joey_core::Config;
use joey_tools::{ToolContext, ToolRegistry};

pub struct OneshotOptions {
    pub prompt: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub toolsets: Option<String>,
    pub usage_file: Option<String>,
    pub max_turns: Option<usize>,
}

/// The observable outcome of the agent run, separated from process concerns
/// so the exit-code mapping is a pure, testable function.
pub struct RunOutcome {
    pub response: String,
    /// Set when the turn ended with `AgentEvent::Failed` (upstream
    /// `result["failed"]`).
    pub failed: bool,
}

/// Map a finished run to its exit code (oneshot.py:280-294):
/// failed/partial with no text → 2; no text at all → 1 (+ stderr message);
/// otherwise 0. The caller prints the response before calling this.
pub fn exit_code_for(outcome: &RunOutcome) -> i32 {
    let has_text = !outcome.response.trim().is_empty();
    if outcome.failed && !has_text {
        return 2;
    }
    if !has_text {
        return 1;
    }
    0
}

// ---------------------------------------------------------------------------
// Toolset validation (oneshot.py:35-124)
// ---------------------------------------------------------------------------

/// Result of `--toolsets` validation: `Ok(None)` = use the config default
/// (or `all`), `Ok(Some(list))` = explicit valid toolsets, `Err(msg)` = fatal
/// (exit 2).
pub fn validate_explicit_toolsets(
    config: &Config,
    raw: Option<&str>,
) -> Result<Option<Vec<String>>, String> {
    let Some(raw) = raw else { return Ok(None) };
    let normalized = crate::commands::normalize_toolsets(raw);
    if normalized.is_empty() {
        return Ok(None);
    }

    let known: Vec<&str> = joey_tools::toolsets::names();
    let mut built_in: Vec<String> = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();
    for name in &normalized {
        if name == "all" || name == "*" || known.contains(&name.as_str()) {
            built_in.push(name.clone());
        } else {
            unresolved.push(name.clone());
        }
    }

    if built_in.iter().any(|n| n == "all" || n == "*") {
        let ignored: Vec<&String> =
            normalized.iter().filter(|n| n.as_str() != "all" && n.as_str() != "*").collect();
        if !ignored.is_empty() {
            eprintln!(
                "joey -z: --toolsets all enables every toolset; ignoring additional entries: {}",
                ignored.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            );
        }
        return Ok(None);
    }

    // Unresolved names may be MCP server names from config.yaml.
    let (mcp_enabled, mcp_disabled) = mcp_server_names(config);
    let mut mcp_valid: Vec<String> = Vec::new();
    let mut disabled: Vec<String> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    for name in unresolved {
        if mcp_enabled.contains(&name) {
            mcp_valid.push(name);
        } else if mcp_disabled.contains(&name) {
            disabled.push(name);
        } else {
            unknown.push(name);
        }
    }

    if !unknown.is_empty() {
        eprintln!("joey -z: ignoring unknown --toolsets entries: {}", unknown.join(", "));
    }
    if !disabled.is_empty() {
        eprintln!(
            "joey -z: ignoring disabled MCP servers (set enabled: true in config.yaml to use): {}",
            disabled.join(", ")
        );
    }

    let mut valid = built_in;
    valid.extend(mcp_valid);
    if valid.is_empty() {
        return Err("joey -z: --toolsets did not contain any valid toolsets.\n".to_string());
    }
    Ok(Some(valid))
}

/// (enabled, disabled) MCP server names from `mcp_servers` in config.yaml.
fn mcp_server_names(config: &Config) -> (Vec<String>, Vec<String>) {
    let mut enabled = Vec::new();
    let mut disabled = Vec::new();
    if let Some(serde_yaml::Value::Mapping(map)) = config.get("mcp_servers") {
        for (k, v) in map {
            let Some(name) = k.as_str() else { continue };
            let is_enabled = match v.get("enabled") {
                Some(serde_yaml::Value::Bool(b)) => *b,
                Some(serde_yaml::Value::String(s)) => {
                    matches!(s.to_lowercase().as_str(), "true" | "1" | "yes")
                }
                _ => true,
            };
            if is_enabled {
                enabled.push(name.to_string());
            } else {
                disabled.push(name.to_string());
            }
        }
    }
    (enabled, disabled)
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------

pub async fn run_oneshot(opts: OneshotOptions) -> Result<i32> {
    let config = Config::load()?;

    // --provider without --model is ambiguous (oneshot.py:203-211).
    let env_model = std::env::var("JOEY_INFERENCE_MODEL").unwrap_or_default().trim().to_string();
    let arg_model = opts.model.as_deref().unwrap_or("").trim().to_string();
    if opts.provider.is_some() && arg_model.is_empty() && env_model.is_empty() {
        eprintln!(
            "joey -z: --provider requires --model (or JOEY_INFERENCE_MODEL). \
             Pass both explicitly, or neither to use your configured defaults."
        );
        return Ok(2);
    }

    let explicit_toolsets = match validate_explicit_toolsets(&config, opts.toolsets.as_deref()) {
        Ok(t) => t,
        Err(msg) => {
            eprint!("{}", msg);
            return Ok(2);
        }
    };

    // Auto-approve everything: non-interactive by definition (oneshot.py:220-222).
    std::env::set_var("JOEY_YOLO_MODE", "1");

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut agent_cfg = AgentConfig::from_config(&config);

    // Resolve effective model: explicit arg → env var → config (oneshot.py:332-340).
    let model_explicit = !arg_model.is_empty() || !env_model.is_empty();
    if !arg_model.is_empty() {
        agent_cfg.model = arg_model.clone();
        agent_cfg.model_pinned = true;
    } else if !env_model.is_empty() {
        agent_cfg.model = env_model.clone();
        agent_cfg.model_pinned = true;
    }

    // Resolve effective provider: explicit arg → auto-detect from an explicit
    // model's prefix (oneshot.py:350-383; profile aliases resolve inside
    // build_client → get_profile).
    if let Some(p) = &opts.provider {
        agent_cfg.provider = p.trim().to_string();
    } else if model_explicit {
        agent_cfg.provider = "auto".to_string();
    }

    // Toolsets: explicit --toolsets, else the "cli" platform tools config
    // (oneshot.py:391-396).
    agent_cfg.enabled_tools = match &explicit_toolsets {
        Some(names) => joey_tools::resolve_toolsets(names),
        None => crate::commands::platform_tools(&config, "cli"),
    };
    if let Some(n) = opts.max_turns {
        agent_cfg.max_turns = n;
    }

    let model = agent_cfg.model.clone();
    let outcome = run_agent(&config, agent_cfg, cwd, &opts.prompt).await;

    match outcome {
        Err(e) => {
            write_usage_file(opts.usage_file.as_deref(), &UsageReport::failure_only(&model, &e.to_string()));
            eprintln!("joey -z: agent failed: {}", e);
            Ok(1)
        }
        Ok((outcome, report)) => {
            write_usage_file(opts.usage_file.as_deref(), &report);
            if !outcome.response.is_empty() {
                let mut out = std::io::stdout();
                let _ = out.write_all(outcome.response.as_bytes());
                if !outcome.response.ends_with('\n') {
                    let _ = out.write_all(b"\n");
                }
                let _ = out.flush();
            }
            let code = exit_code_for(&outcome);
            if code == 1 {
                eprintln!("joey -z: no final response was produced; treating the run as failed.");
            }
            Ok(code)
        }
    }
}

async fn run_agent(
    config: &Config,
    agent_cfg: AgentConfig,
    cwd: PathBuf,
    prompt: &str,
) -> Result<(RunOutcome, UsageReport)> {
    let ctx = ToolContext::new(cwd.clone(), config.clone(), "oneshot").with_interactive(false);
    let mut registry = ToolRegistry::with_builtins();

    // Wire session_search.
    let session_db = joey_core::SessionDb::open_default()
        .ok()
        .map(|db| std::sync::Arc::new(std::sync::Mutex::new(db)));
    joey_tools::builtins::register_session_tools(&mut registry, session_db);

    // Wire orchestration.
    let mgr_config = joey_orchestration::ManagerConfig::from_config(config);
    let manager = std::sync::Arc::new(joey_orchestration::SubagentManager::new(mgr_config));
    let base_registry = registry.clone();

    // Feature 011: build the dynamic LLM model allocator when enabled or when
    // `auto` is the configured model (FR-002). Off-by-default — returns None
    // and leaves the agent byte-identical to pre-feature-011 (Constitution VII).
    // Built before orchestration registration so it can be threaded into the
    // delegate_task tool (T028 subagent intercept).
    let allocator = crate::llm_selector::try_build_allocator(config);

    // Feature 015 (NeuroCode): build the engine when enabled and register the
    // 4 NeuroCode tools (T056/T057). None when disabled — byte-identical
    // to pre-feature-015 (FR-020).
    let neurocode_engine = crate::neurocode_wiring::try_build_engine(config);
    if let Some(engine) = &neurocode_engine {
        let backend = crate::neurocode_wiring::backend_for_engine(engine);
        joey_tools::builtins::register_neurocode_tools(&mut registry, Some(backend));
    }

    joey_orchestration::register_orchestration_with_allocator(
        &mut registry,
        manager.clone(),
        agent_cfg.clone(),
        config.clone(),
        base_registry,
        None,
        allocator
            .clone()
            .map(|a| a as std::sync::Arc<dyn joey_llm_selector::ModelAllocator>),
    );

    let mut agent = Agent::new(agent_cfg.clone(), registry, ctx)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    agent.set_provider_semaphore(manager.semaphore());

    // Feature 011: install the allocator on the parent agent (main turn +
    // compression intercepts).
    if let Some(allocator) = allocator {
        agent.install_model_allocator(allocator);
    }

    // Feature 015 (NeuroCode): install the engine on the parent agent
    // (turn-loop intercept — T056). None when disabled — no-op.
    if let Some(engine) = neurocode_engine {
        agent.set_neurocode_engine(engine);
    }

    if !agent.client().has_credentials() {
        anyhow::bail!(
            "no API key configured for provider '{}' (set one with `joey config set` or `joey model`)",
            agent.client().profile().name
        );
    }

    // Best-effort session store so the run is recallable (oneshot.py:297-310).
    let session_id = joey_core::SessionDb::open_default().ok().and_then(|db| {
        let sid = db.create_session("cli", Some(&agent_cfg.model), cwd.to_str()).ok()?;
        agent.set_session_store(db, sid.clone());
        Some(sid)
    });

    // Drain events silently; only the Failed message matters here.
    //
    // A bare `rx.recv()`-until-close would hang forever: background-process
    // reapers (process_tool.rs) hold ToolContext clones whose progress_tx
    // keeps the turn's event forwarder — and through it a tx clone — alive
    // for the lifetime of every child process the turn spawned. So the
    // drain task also listens for a stop signal, then flushes whatever is
    // still queued behind it.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (stop_tx, mut stop_rx) = tokio::sync::mpsc::channel::<()>(1);
    let drain = tokio::spawn(async move {
        let mut failed: Option<String> = None;
        loop {
            tokio::select! {
                ev = rx.recv() => {
                    match ev {
                        Some(ev) => {
                            if let AgentEvent::Failed(msg) = ev {
                                failed = Some(msg);
                            }
                        }
                        None => break,
                    }
                }
                _ = stop_rx.recv() => {
                    // Flush events still queued behind the stop signal.
                    while let Ok(ev) = rx.try_recv() {
                        if let AgentEvent::Failed(msg) = ev {
                            failed = Some(msg);
                        }
                    }
                    break;
                }
            }
        }
        failed
    });
    let result = agent.run_turn(prompt, tx).await;
    let _ = stop_tx.send(()).await;
    let failed = drain.await.ok().flatten();

    if let Some(db) = agent.session_db() {
        if let Some(sid) = &session_id {
            let _ = db.end_session(sid, "oneshot_complete");
        }
    }

    let provider_name = agent.client().profile().name.to_string();
    let report = UsageReport {
        input_tokens: result.usage.prompt_tokens,
        output_tokens: result.usage.completion_tokens,
        cache_read_tokens: result.usage.cache_read_tokens,
        cache_write_tokens: result.usage.cache_write_tokens,
        reasoning_tokens: result.usage.reasoning_tokens,
        total_tokens: result.usage.total_tokens,
        api_calls: Some(result.iterations as u64),
        model: agent_cfg.model.clone(),
        provider: Some(provider_name),
        session_id,
        completed: Some(failed.is_none() && !result.interrupted),
        failed: failed.is_some(),
        failure: failed.clone(),
    };
    Ok((
        RunOutcome { response: result.final_text, failed: failed.is_some() },
        report,
    ))
}

// ---------------------------------------------------------------------------
// Usage report (oneshot.py:127-167)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct UsageReport {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub api_calls: Option<u64>,
    pub model: String,
    pub provider: Option<String>,
    pub session_id: Option<String>,
    pub completed: Option<bool>,
    pub failed: bool,
    pub failure: Option<String>,
}

impl UsageReport {
    fn failure_only(model: &str, failure: &str) -> Self {
        Self {
            model: model.to_string(),
            failed: true,
            failure: Some(failure.to_string()),
            ..Default::default()
        }
    }
}

/// Best-effort JSON usage report for pipelines (`-z --usage-file`). Written
/// even on failure; never lets a broken write mask the run's own outcome.
fn write_usage_file(path: Option<&str>, report: &UsageReport) {
    let Some(path) = path else { return };
    let value = serde_json::json!({
        // Cost estimation is not ported: keys stay for pipeline shape parity.
        "estimated_cost_usd": serde_json::Value::Null,
        "cost_status": serde_json::Value::Null,
        "cost_source": serde_json::Value::Null,
        "input_tokens": report.input_tokens,
        "output_tokens": report.output_tokens,
        "cache_read_tokens": report.cache_read_tokens,
        "cache_write_tokens": report.cache_write_tokens,
        "reasoning_tokens": report.reasoning_tokens,
        "total_tokens": report.total_tokens,
        "api_calls": report.api_calls,
        "model": report.model,
        "provider": report.provider,
        "session_id": report.session_id,
        "completed": report.completed,
        "failed": report.failed || report.failure.is_some(),
        "service_tier": serde_json::Value::Null,
    });
    let mut value = value;
    if let Some(f) = &report.failure {
        value["failure"] = serde_json::Value::String(f.clone());
    }
    let expanded = shellexpand_tilde(path);
    let p = PathBuf::from(expanded);
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&p, format!("{}\n", serde_json::to_string_pretty(&value).unwrap_or_default()));
}

fn shellexpand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        return joey_core::constants::user_home_dir().join(rest).to_string_lossy().into_owned();
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_map_like_upstream() {
        // success with text → 0
        assert_eq!(exit_code_for(&RunOutcome { response: "hi".into(), failed: false }), 0);
        // failed but produced text → text printed, exit 0 (oneshot.py:280-294)
        assert_eq!(exit_code_for(&RunOutcome { response: "partial".into(), failed: true }), 0);
        // failed with no text → 2
        assert_eq!(exit_code_for(&RunOutcome { response: "  ".into(), failed: true }), 2);
        // no failure flag but empty text → 1
        assert_eq!(exit_code_for(&RunOutcome { response: String::new(), failed: false }), 1);
    }

    #[test]
    fn toolset_validation() {
        let cfg = Config::defaults();
        // No arg → config default path.
        assert_eq!(validate_explicit_toolsets(&cfg, None).unwrap(), None);
        // "all" → None (all toolsets).
        assert_eq!(validate_explicit_toolsets(&cfg, Some("all")).unwrap(), None);
        // Valid names pass through in user order.
        assert_eq!(
            validate_explicit_toolsets(&cfg, Some("web, file")).unwrap(),
            Some(vec!["web".to_string(), "file".to_string()])
        );
        // All-invalid → fatal error (exit 2 at the call site).
        let err = validate_explicit_toolsets(&cfg, Some("nope,unknown")).unwrap_err();
        assert_eq!(err, "joey -z: --toolsets did not contain any valid toolsets.\n");
        // Mixed: unknown entries are dropped with a warning, valid remain.
        assert_eq!(
            validate_explicit_toolsets(&cfg, Some("web,nope")).unwrap(),
            Some(vec!["web".to_string()])
        );
    }
}
