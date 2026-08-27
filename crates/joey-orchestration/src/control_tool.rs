//! The `subagent_control` tool — steer/stop (US3) + progress inspection
//! (feature 020, User Stories 3 & 4).
//!
//! Action-based (mirrors the background process tool's UX):
//! - `steer` (FR-008): queues a steering message on the child; it is
//!   delivered before the child's next action, at the next action boundary.
//! - `stop`  (FR-009/FR-010): requests a graceful stop with reason
//!   `orchestrator-requested`; the child winds down at its next checkpoint
//!   and yields a partial result whose terminal record keeps the stop reason.
//! - `list` (FR-005/FR-019): one line per child (running + session-lifetime
//!   terminal history): id, truncated goal, state, elapsed, tokens.
//! - `status` (FR-012): single-record detail incl. cumulative usage.
//! - `log` (FR-006): bounded most-recent slice of a child's activity —
//!   NEVER the full transcript.
//! - `wait` (FR-007): blocks until all given ids are terminal or a timeout
//!   expires; on timeout returns partial statuses. Holds no semaphore
//!   permits and is a plain async poll loop, so the normal tool-timeout
//!   machinery cancels it.
//!
//! list/status/log/wait are read-only (registry snapshots + tap drain) and
//! never acquire provider permits — control stays live under total child
//! saturation (SC-007).
//!
//! LOG SOURCE (T018/T029): `new` installs the recorder as the manager's
//! SECONDARY recorder tap (`set_recorder_tap`) — a dedicated channel fed
//! ALONGSIDE the external event tap at every emission site. It never
//! participates in `event_tap()` resolution, so a host tap installed before
//! OR after tool registration (the real TUI startup order: manager →
//! SubagentControl → set_global_tap) receives every delegation event
//! unchanged, while the recorder still fills the per-child log rings
//! (FR-005/FR-006). (Pre-T029 the recorder was chained ONTO the local tap
//! slot, silently shadowing any tap installed later.) The recorder needs no
//! runtime task: events accumulate in an unbounded channel and are drained
//! synchronously (`try_recv`) on every tool call. Per-child activity is
//! kept in a bounded ring ([`LOG_RING_CAP`]) — log output is bounded
//! regardless of transcript length.

use async_trait::async_trait;
use joey_agent_core::AgentEvent;
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::manager::SubagentManager;
use crate::types::{DelegationOverview, DelegationState, StopReason};

/// Per-child activity ring cap: the recorder never stores more than this
/// many lines per child, so `log` is bounded regardless of transcript
/// length (FR-006).
const LOG_RING_CAP: usize = 256;
/// Default `last` for `log` (contract).
const LOG_DEFAULT_LAST: usize = 10;
/// Default `timeout_secs` for `wait` (contract).
const WAIT_DEFAULT_TIMEOUT_SECS: u64 = 60;
/// `wait` poll interval (ms).
const WAIT_POLL_MS: u64 = 50;
/// Max events drained per pump (keeps a single call O(bounded)).
const PUMP_BATCH: usize = 1024;
/// Goal truncation width inside `list` lines.
const LIST_GOAL_CLIP: usize = 40;

/// The subagent_control tool. Holds an `Arc<SubagentManager>` so it acts on
/// the SAME child registry the delegate_task tool dispatches into (blocking
/// and background children alike), plus a bounded per-child activity log
/// fed by the manager's recorder tap (see module docs).
pub struct SubagentControl {
    manager: Arc<SubagentManager>,
    /// Bounded per-child activity lines (tap-derived), keyed by child id.
    logs: Mutex<HashMap<u64, VecDeque<String>>>,
    /// Receiver side of the recorder tap (drained by `pump`).
    tap_rx: Mutex<Option<mpsc::UnboundedReceiver<AgentEvent>>>,
}

impl SubagentControl {
    pub fn new(manager: Arc<SubagentManager>) -> Self {
        // Install the recorder as the manager's SECONDARY tap (T029): fed
        // alongside the external tap at every emission site, never part of
        // `event_tap()` resolution — so it cannot shadow a host tap. Synchronous
        // (no runtime task), so `new` stays callable from plain sync code.
        let (tx, rx) = mpsc::unbounded_channel();
        manager.set_recorder_tap(Some(tx));
        Self {
            manager,
            logs: Mutex::new(HashMap::new()),
            tap_rx: Mutex::new(Some(rx)),
        }
    }

    /// Drain pending recorder-tap events into the per-child activity rings.
    /// Synchronous and bounded per call ([`PUMP_BATCH`]). No forwarding:
    /// the external tap receives events directly at the emission sites
    /// (T029), never through this recorder.
    fn pump(&self) {
        let drained: Vec<AgentEvent> = {
            let mut guard = self.tap_rx.lock().unwrap_or_else(|p| p.into_inner());
            match guard.as_mut() {
                Some(rx) => {
                    let mut out = Vec::new();
                    while out.len() < PUMP_BATCH {
                        match rx.try_recv() {
                            Ok(ev) => out.push(ev),
                            Err(_) => break,
                        }
                    }
                    out
                }
                None => Vec::new(),
            }
        };
        if drained.is_empty() {
            return;
        }
        let mut logs = self.logs.lock().unwrap_or_else(|p| p.into_inner());
        for ev in drained {
            record_event(&mut logs, &ev);
        }
    }

    /// `list` (FR-005/FR-016/FR-019): one line per child — running children
    /// plus session-lifetime terminal history, oldest first.
    fn action_list(&self) -> ToolResult {
        self.pump();
        let records = self.manager.overview();
        if records.is_empty() {
            return ToolResult::Text(
                "no delegation children this session — start one with \
                 delegate_task background=true"
                    .to_string(),
            );
        }
        let lines: Vec<String> = records
            .iter()
            .map(|r| {
                format!(
                    "id={} goal={} state={} elapsed={}s tokens={}",
                    r.child_id,
                    clip(&r.goal, LIST_GOAL_CLIP),
                    state_str(&r.state),
                    r.elapsed.as_secs(),
                    r.tokens
                )
            })
            .collect();
        ToolResult::Text(lines.join("\n"))
    }

    /// `status` (FR-012): single-record detail incl. cumulative usage.
    fn action_status(&self, id: u64) -> ToolResult {
        self.pump();
        let Some(rec) = self.manager.child_status(id) else {
            return ToolResult::Error(unknown_child_error(id));
        };
        let mut out = format!(
            "[status] id={} goal={}\nstate={} elapsed={}s tokens={}",
            rec.child_id,
            clip(&rec.goal, 80),
            state_str(&rec.state),
            rec.elapsed.as_secs(),
            rec.tokens
        );
        match &rec.state {
            DelegationState::Completed { result } => out.push_str(&format!(
                "\niterations={} model={}\nsummary: {}",
                result.iterations,
                result.model,
                clip(&result.summary, 400)
            )),
            DelegationState::Failed { error } => {
                out.push_str(&format!("\nerror: {}", clip(error, 400)))
            }
            DelegationState::Stopped { reason } => out.push_str(&format!(
                "\nstop reason: {}",
                stop_reason_snake(*reason)
            )),
            DelegationState::Running => out.push_str(
                "\n(still running — steer or stop it via subagent_control)",
            ),
        }
        ToolResult::Text(out)
    }

    /// `log` (FR-006): the last `last` (default 10) recorded activity lines
    /// for the child — bounded regardless of transcript length.
    fn action_log(&self, id: u64, last_val: Option<&Value>) -> ToolResult {
        // Parse `last` before existence so bad values are reported as such.
        let last = match last_val {
            None | Some(Value::Null) => LOG_DEFAULT_LAST,
            Some(v) => match v.as_u64() {
                Some(n) if n > 0 => n as usize,
                _ => {
                    return ToolResult::Error(format!(
                        "'last' must be a positive integer (default {LOG_DEFAULT_LAST})"
                    ))
                }
            },
        };
        self.pump();
        let Some(rec) = self.manager.child_status(id) else {
            return ToolResult::Error(unknown_child_error(id));
        };
        let ring = self
            .logs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&id)
            .cloned()
            .unwrap_or_default();
        let total = ring.len();
        let start = total.saturating_sub(last);
        let slice: Vec<&String> = ring.iter().skip(start).collect();
        let header = format!(
            "[log] child {} goal={} state={} — last {} of {} recorded events",
            rec.child_id,
            clip(&rec.goal, LIST_GOAL_CLIP),
            state_str(&rec.state),
            slice.len(),
            total
        );
        if slice.is_empty() {
            return ToolResult::Text(format!("{header}\n(no recorded activity yet)"));
        }
        let body: Vec<String> = slice
            .iter()
            .enumerate()
            .map(|(i, l)| format!("#{} {}", start + i + 1, l))
            .collect();
        ToolResult::Text(format!("{header}\n{}", body.join("\n")))
    }

    /// `wait` (FR-007): block until every id is terminal or `timeout_secs`
    /// (default 60) expires; timeout returns partial statuses. Pure poll
    /// loop — no semaphore permits, cancellable by the tool-timeout
    /// machinery (the future is simply dropped).
    async fn action_wait(&self, args: &Value) -> ToolResult {
        // Parse ids + timeout BEFORE existence checks so arg errors win.
        let Some(arr) = args.get("ids").and_then(|v| v.as_array()) else {
            return ToolResult::Error(
                "wait requires 'ids' — a non-empty array of child ids".to_string(),
            );
        };
        if arr.is_empty() {
            return ToolResult::Error(
                "wait requires 'ids' — a non-empty array of child ids".to_string(),
            );
        }
        let mut ids: Vec<u64> = Vec::with_capacity(arr.len());
        for v in arr {
            match parse_id(Some(v)) {
                Some(id) => {
                    if !ids.contains(&id) {
                        ids.push(id); // order-preserving dedup
                    }
                }
                None => {
                    return ToolResult::Error(
                        "'ids' must contain numeric child ids (numbers or numeric strings)"
                            .to_string(),
                    )
                }
            }
        }
        let timeout_secs = match args.get("timeout_secs") {
            None | Some(Value::Null) => WAIT_DEFAULT_TIMEOUT_SECS,
            Some(v) => match v.as_u64() {
                Some(n) if n > 0 => n,
                _ => {
                    return ToolResult::Error(format!(
                        "'timeout_secs' must be a positive integer number of seconds \
                         (default {WAIT_DEFAULT_TIMEOUT_SECS})"
                    ))
                }
            },
        };
        for &id in &ids {
            if self.manager.child_status(id).is_none() {
                return ToolResult::Error(unknown_child_error(id));
            }
        }

        let poll = async {
            loop {
                self.pump();
                let snap: Vec<DelegationOverview> = ids
                    .iter()
                    .map(|&i| self.manager.child_status(i).expect("id validated above"))
                    .collect();
                if snap.iter().all(|r| r.state.is_terminal()) {
                    return snap;
                }
                tokio::time::sleep(Duration::from_millis(WAIT_POLL_MS)).await;
            }
        };
        match tokio::time::timeout(Duration::from_secs(timeout_secs), poll).await {
            Ok(snap) => {
                let lines: Vec<String> = snap.iter().map(wait_line).collect();
                ToolResult::Text(format!(
                    "[wait] all {} waited-on children finished:\n{}",
                    snap.len(),
                    lines.join("\n")
                ))
            }
            Err(_) => {
                self.pump();
                let snap: Vec<DelegationOverview> = ids
                    .iter()
                    .map(|&i| self.manager.child_status(i).expect("id validated above"))
                    .collect();
                let lines: Vec<String> = snap.iter().map(wait_line).collect();
                ToolResult::Text(format!(
                    "[wait] timed out after {timeout_secs}s — partial statuses \
                     (still-running children included):\n{}",
                    lines.join("\n")
                ))
            }
        }
    }
}

/// Parse the `id` argument: the handle line prints the child id bare, and
/// models frequently copy ids back as JSON strings — accept both a number
/// and a numeric string. `None` for anything else.
fn parse_id(value: Option<&Value>) -> Option<u64> {
    match value {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.trim().parse::<u64>().ok(),
        _ => None,
    }
}

/// Unknown-id tool error, matching the manager's wording so callers see one
/// consistent message across all actions.
fn unknown_child_error(id: u64) -> String {
    format!("No subagent with id {id} is running or has finished in this session")
}

/// snake_case string form of a [`StopReason`] (matches its serde name).
fn stop_reason_snake(reason: StopReason) -> &'static str {
    match reason {
        StopReason::OrchestratorRequested => "orchestrator_requested",
        StopReason::OperatorRequested => "operator_requested",
        StopReason::BudgetExceeded => "budget_exceeded",
        StopReason::SessionEnd => "session_end",
    }
}

/// Compact state string for list/status/wait lines (FR-016):
/// running | completed | failed | stopped:<reason_snake>.
fn state_str(state: &DelegationState) -> String {
    match state {
        DelegationState::Running => "running".to_string(),
        DelegationState::Completed { .. } => "completed".to_string(),
        DelegationState::Failed { .. } => "failed".to_string(),
        DelegationState::Stopped { reason } => {
            format!("stopped:{}", stop_reason_snake(*reason))
        }
    }
}

/// One status line for a `wait` result (terminal or, on timeout, running).
fn wait_line(r: &DelegationOverview) -> String {
    let base = format!(
        "id={} state={} elapsed={}s tokens={}",
        r.child_id,
        state_str(&r.state),
        r.elapsed.as_secs(),
        r.tokens
    );
    match &r.state {
        DelegationState::Completed { result } => {
            format!("{base} summary: {}", clip(&result.summary, 200))
        }
        DelegationState::Failed { error } => format!("{base} error: {}", clip(error, 200)),
        DelegationState::Stopped { reason } => {
            format!("{base} reason: {}", stop_reason_snake(*reason))
        }
        DelegationState::Running => format!("{base} (still running)"),
    }
}

/// Collapse to a single line and char-clip with an ellipsis marker.
fn clip(s: &str, max: usize) -> String {
    let flat: String = if s.contains('\n') {
        s.lines().collect::<Vec<_>>().join(" ")
    } else {
        s.to_string()
    };
    if flat.chars().count() <= max {
        return flat;
    }
    let mut out: String = flat.chars().take(max).collect();
    out.push('…');
    out
}

/// First line of a string (for one-line event summaries).
fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}

/// Record one tap event into the per-child rings (bounded, FR-006). Child
/// activity arrives wrapped as `SubagentEvent { id, event }`; the
/// orchestration-level lifecycle events carry the id directly.
fn record_event(logs: &mut HashMap<u64, VecDeque<String>>, ev: &AgentEvent) {
    fn push(logs: &mut HashMap<u64, VecDeque<String>>, id: u64, line: String) {
        let ring = logs.entry(id).or_default();
        ring.push_back(line);
        while ring.len() > LOG_RING_CAP {
            ring.pop_front();
        }
    }
    match ev {
        AgentEvent::SubagentEvent { id, event } => {
            if let Some(line) = describe_event(event) {
                push(logs, *id, line);
            }
        }
        AgentEvent::SubagentSpawn { id, goal, .. } => {
            push(logs, *id, format!("spawned (goal: {})", clip(goal, 80)));
        }
        AgentEvent::SubagentComplete {
            id, summary_preview, ..
        } => push(logs, *id, format!("completed: {}", clip(summary_preview, 120))),
        AgentEvent::SubagentFailed { id, error, .. } => {
            push(logs, *id, format!("failed: {}", clip(error, 120)))
        }
        AgentEvent::SubagentStopped { id, reason, .. } => {
            push(logs, *id, format!("stopped ({reason})"))
        }
        _ => {}
    }
}

/// One bounded activity line for a child event, or `None` for events too
/// chatty/large to log (streaming deltas, context snapshots, …).
fn describe_event(ev: &AgentEvent) -> Option<String> {
    Some(match ev {
        AgentEvent::TurnStart { max_iterations } => {
            format!("turn start (max {max_iterations} iterations)")
        }
        AgentEvent::IterationStart { iteration, .. } => format!("iteration {iteration}"),
        AgentEvent::AssistantMessage(text) => {
            format!("assistant: {}", clip(first_line(text), 120))
        }
        AgentEvent::ToolStart { name, summary, .. } => {
            format!("tool {name}: {}", clip(summary, 120))
        }
        AgentEvent::ToolEnd {
            name,
            is_error,
            result_preview,
            ..
        } => format!(
            "tool {name} {}: {}",
            if *is_error { "error" } else { "ok" },
            clip(first_line(result_preview), 120)
        ),
        AgentEvent::Done { final_text, .. } => format!("done: {}", clip(first_line(final_text), 120)),
        AgentEvent::Failed(e) => format!("failed: {}", clip(first_line(e), 120)),
        AgentEvent::Notice(n) => format!("notice: {}", clip(first_line(n), 120)),
        AgentEvent::RetryAttempt {
            attempt,
            max_retries,
            error,
            ..
        } => format!("retry {attempt}/{max_retries}: {}", clip(first_line(error), 80)),
        AgentEvent::CompressionStart { reason, .. } => {
            format!("context compression started ({reason})")
        }
        AgentEvent::CompressionEnd {
            original_msgs,
            new_msgs,
        } => format!("context compression done ({original_msgs}→{new_msgs} messages)"),
        _ => return None,
    })
}

#[async_trait]
impl Tool for SubagentControl {
    fn name(&self) -> &str {
        "subagent_control"
    }

    fn toolset(&self) -> &str {
        "delegation"
    }

    fn emoji(&self) -> &str {
        "🎛️"
    }

    fn description(&self) -> &str {
        "Control and inspect subagents you spawned with delegate_task (works for \
         blocking-batch and background children alike; ids come from the \
         delegate_task report or the '[BACKGROUND] id=<child_id> goal=<goal> \
         started' handle). \
         Action-based: \
         steer (id, message) queues a steering message on that child — it is \
         delivered BEFORE the child's next action, at its next action boundary; \
         use it to correct off-track work early. \
         stop (id) requests a graceful stop (reason: orchestrator-requested) — \
         only that child stops; it winds down at its next checkpoint, yields a \
         partial result including the stop reason, and its completion notice \
         reports it as stopped. \
         list shows every child this session (running and finished): id, goal, \
         state, elapsed, tokens. \
         status (id) shows one child's record in detail, including cumulative \
         token usage. \
         log (id, last=N, default 10) shows the child's most recent activity — \
         a bounded slice, never the full transcript. \
         wait (ids, timeout_secs default 60) blocks until all the given \
         children finish or the timeout expires; on timeout it returns their \
         current (partial) statuses. \
         Steering or stopping a child that already finished returns an \
         'already finished' error; unknown ids return a clear error. \
         Default: none — 'action' is required ('steer' also requires 'message'; \
         'status'/'log' require 'id'; 'wait' requires 'ids')."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["steer", "stop", "list", "status", "log", "wait"],
                    "description": "Control/inspection action: 'steer' queues a steering message delivered before the child's next action (requires 'message'); 'stop' requests a graceful stop with reason orchestrator-requested; 'list' shows all children (running + finished this session); 'status' shows one child's detailed record; 'log' shows a child's last N activity lines; 'wait' blocks until the given children finish or timeout."
                },
                "id": {
                    "type": "integer",
                    "description": "The child id to act on (from the delegate_task report or the '[BACKGROUND] id=…' handle line). Required for steer, stop, status and log."
                },
                "message": {
                    "type": "string",
                    "description": "Steering text for action=steer. Delivered to the child at its next action boundary, wrapped as a direct orchestrator instruction. Required for steer; ignored otherwise."
                },
                "last": {
                    "type": "integer",
                    "description": "For action=log: return the last N activity lines (default 10, must be positive). The output is bounded regardless of transcript length."
                },
                "ids": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "For action=wait: the child ids to wait on. Returns when all are finished or 'timeout_secs' expires (partial statuses on timeout)."
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "For action=wait: max seconds to block (default 60, must be positive). On timeout the current statuses are returned, not an error."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        self.pump();
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a.trim().to_lowercase(),
            None => {
                return ToolResult::Error(
                    "subagent_control requires 'action' (steer, stop, list, status, log, \
                     or wait)"
                        .to_string(),
                );
            }
        };
        // Reject unknown actions BEFORE any id check so e.g. action='pause'
        // reports the real problem, not a missing id.
        if !matches!(
            action.as_str(),
            "steer" | "stop" | "list" | "status" | "log" | "wait"
        ) {
            return ToolResult::Error(format!(
                "Unknown action '{action}'. Implemented actions: \
                 steer, stop, list, status, log, wait."
            ));
        }
        // list takes no id; wait takes `ids` instead.
        match action.as_str() {
            "list" => return self.action_list(),
            "wait" => return self.action_wait(&args).await,
            _ => {}
        }

        let id = match parse_id(args.get("id")) {
            Some(id) => id,
            None => {
                return ToolResult::Error(
                    "subagent_control requires a numeric 'id' — the child id from the \
                     delegate_task report or '[BACKGROUND] id=…' handle line"
                        .to_string(),
                );
            }
        };

        match action.as_str() {
            "steer" => {
                let message = args
                    .get("message")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .unwrap_or("");
                if message.is_empty() {
                    return ToolResult::Error(
                        "steer requires a non-empty 'message'".to_string(),
                    );
                }
                match self.manager.steer_child(id, message) {
                    Ok(()) => ToolResult::Text(format!(
                        "steer queued for child {id}: delivered at next action boundary"
                    )),
                    // Manager error strings already carry the tool-facing
                    // semantics ("already finished" / unknown-id, edge cases).
                    Err(e) => ToolResult::Error(e),
                }
            }
            "stop" => match self.manager.stop_child(id, StopReason::OrchestratorRequested) {
                Ok(()) => ToolResult::Text(format!(
                    "stop requested for child {id} (reason: orchestrator-requested)"
                )),
                Err(e) => ToolResult::Error(e),
            },
            "status" => self.action_status(id),
            "log" => self.action_log(id, args.get("last")),
            // unreachable: the whitelist above filters everything else
            other => ToolResult::Error(format!(
                "Unknown action '{other}'. Implemented actions: \
                 steer, stop, list, status, log, wait."
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit coverage for the formatting/parsing helpers (integration paths
    //! are driven by tests/control_tool.rs).

    use super::*;

    #[test]
    fn state_str_covers_all_states() {
        assert_eq!(state_str(&DelegationState::Running), "running");
        assert_eq!(
            state_str(&DelegationState::Stopped {
                reason: StopReason::OrchestratorRequested
            }),
            "stopped:orchestrator_requested"
        );
        assert_eq!(state_str(&DelegationState::Failed { error: String::new() }), "failed");
        let result = crate::types::DelegationResult {
            goal: "g".into(),
            summary: "s".into(),
            success: true,
            error: None,
            token_usage: Default::default(),
            wall_clock: Duration::ZERO,
            model: "m".into(),
            iterations: 1,
            persisted_session_id: None,
            stop_reason: None,
        };
        assert_eq!(
            state_str(&DelegationState::Completed { result }),
            "completed"
        );
    }

    #[test]
    fn clip_collapses_lines_and_marks_truncation() {
        assert_eq!(clip("a\nb\nc", 10), "a b c");
        let long = "x".repeat(50);
        let clipped = clip(&long, 40);
        assert_eq!(clipped.chars().count(), 41); // 40 + ellipsis
        assert!(clipped.ends_with('…'));
    }

    #[test]
    fn parse_id_accepts_number_and_numeric_string() {
        assert_eq!(parse_id(Some(&json!(7))), Some(7));
        assert_eq!(parse_id(Some(&json!(" 8 "))), Some(8));
        assert_eq!(parse_id(Some(&json!("x"))), None);
        assert_eq!(parse_id(None), None);
        assert_eq!(parse_id(Some(&json!(-1))), None);
    }
}
