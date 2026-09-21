//! Browser explorer (feature 035): orchestrator-steered persistent browser
//! explorer. Registry, wait tool, `[PAGE-REPORT]` reporting, and designated-
//! explorer guidance live here. Scaffolded by T001; populated by
//! T004/T006/T010/T011.

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use joey_agent_core::AgentEvent;
use joey_tools::{Tool, ToolContext, ToolResult};
use serde_json::{json, Value};

/// The single active browser-explorer designation (feature 035, data-model
/// Entity 1).
#[derive(Debug, Clone)]
pub struct ActiveExplorer {
    pub child_id: u64,
    pub goal: String,
    pub designated_at: Instant,
}

/// Orchestrator-side singleton enforcing the single-active rule (FR-001).
/// All state in-memory; no on-disk format (constitution VII).
#[derive(Debug, Default)]
pub struct BrowserExplorerRegistry {
    active: Mutex<Option<ActiveExplorer>>,
}

impl BrowserExplorerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Designate `child_id` as the session's browser explorer. Fails while a
    /// designation is active; the error names the active child id and goal
    /// (clarification Q1-A / FR-001).
    pub fn designate(&self, child_id: u64, goal: &str) -> Result<(), String> {
        let mut guard = self.active.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(current) = guard.as_ref() {
            return Err(format!(
                "browser explorer already active: child {} (goal: {}); steer it via subagent_control or stop it first",
                current.child_id, current.goal
            ));
        }
        *guard = Some(ActiveExplorer {
            child_id,
            goal: goal.to_string(),
            designated_at: Instant::now(),
        });
        Ok(())
    }

    /// Clear the designation iff it currently points at `child_id` (invoked
    /// on every one-way terminal transition: Completed/Failed/Stopped).
    pub fn clear_if_active(&self, child_id: u64) {
        let mut guard = self.active.lock().unwrap_or_else(|p| p.into_inner());
        if guard.as_ref().map(|a| a.child_id) == Some(child_id) {
            *guard = None;
        }
    }

    /// The active designation, if any.
    pub fn active(&self) -> Option<ActiveExplorer> {
        self.active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Whether `child_id` is the currently designated explorer.
    pub fn is_active_for(&self, child_id: u64) -> bool {
        self.active().map(|a| a.child_id == child_id).unwrap_or(false)
    }
}

/// Feature 035 (T011): behavioral guidance for the designated browser
/// explorer, appended to its delegation directive (wired in subagent.rs).
pub const DESIGNATED_EXPLORER_GUIDANCE: &str = "You are the orchestrator's designated BROWSER EXPLORER — its eyes and hands on ONE persistent browser page.\nRules of engagement:\n1. Act ONLY on explicit steering instructions from the orchestrator. Never choose destinations, follow unprompted links, or take corrective actions on your own initiative.\n2. After EVERY instructed action, call browser_explorer_wait with a report ([PAGE-REPORT]): echo the instruction verbatim, the current url, the page title, a COMPLETE snapshot (browser_snapshot payload WITHOUT since_last — never a delta), and outcome success|failure.\n3. On failure: report the failure plus its error AND a snapshot of the unchanged current page. Do NOT try alternatives — wait for the next instruction.\n4. browser_explorer_wait blocks until the next steering instruction arrives; execute exactly that one step, then report again via the next browser_explorer_wait call.\n5. If the open page dies mid-task (browser crash or tab closed): reopen the page, report the interruption (outcome failure, error describing what died) in your next report, and wait for the next instruction.\n6. If an instruction is ambiguous or references something not on the current page: report what you currently see and request a corrected instruction. Do not guess.\n7. The loop ends when the orchestrator steers the literal instruction 'finish': emit one final [PAGE-REPORT] (outcome success, snapshot of the still-open page) and then produce your final summary.\n8. Keep the same browser session and page open across steps: never relaunch the browser, restart yourself, or close the page between instructions.";

/// A validated [PAGE-REPORT] payload (contracts/page-report.md, Entity 4).
pub(crate) struct ValidatedReport {
    pub instruction: String,
    pub url: String,
    pub title: String,
    pub snapshot: String,
    pub outcome: String,
    pub error: Option<String>,
}

/// Validate a report object: instruction/url/snapshot non-empty strings, title
/// present (may be empty), outcome ∈ {success, failure}, `error` present IFF
/// outcome = failure. Returns a precise error telling the child what is
/// missing — no partial reports (FR-004/FR-005).
pub(crate) fn validate_report(report: &Value) -> Result<ValidatedReport, String> {
    let get_str = |k: &str| report.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let instruction = get_str("instruction")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing non-empty 'instruction' (echo the steering instruction verbatim)".to_string())?;
    let url = get_str("url")
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "missing non-empty 'url'".to_string())?;
    let title = report
        .get("title")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "missing 'title'".to_string())?;
    let snapshot = get_str("snapshot")
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "missing non-empty 'snapshot' (complete browser_snapshot payload, never a delta)".to_string())?;
    let outcome = get_str("outcome").ok_or_else(|| "missing 'outcome'".to_string())?;
    if outcome != "success" && outcome != "failure" {
        return Err(format!("'outcome' must be 'success' or 'failure' (got '{outcome}')"));
    }
    let error = get_str("error").filter(|s| !s.trim().is_empty());
    match (outcome.as_str(), error.as_ref()) {
        ("failure", None) => {
            return Err("outcome=failure must carry 'error' (what failed + unchanged-page state)".to_string())
        }
        ("success", Some(_)) => return Err("'error' must be omitted when outcome=success".to_string()),
        _ => {}
    }
    Ok(ValidatedReport { instruction, url, title, snapshot, outcome, error })
}

/// Render the recorder-ring line for one report (contracts/page-report.md
/// block format; consumed verbatim by describe_event's [PAGE-REPORT] arm).
pub(crate) fn format_page_report(child_id: u64, seq: u64, v: &ValidatedReport) -> String {
    let mut block = format!(
        "[PAGE-REPORT] child={} seq={}\ninstruction: {}\noutcome: {}\nurl: {}\ntitle: {}",
        child_id, seq, v.instruction, v.outcome, v.url, v.title
    );
    if let Some(e) = &v.error {
        block.push_str(&format!("\nerror: {e}"));
    }
    block.push_str(&format!("\n{}", v.snapshot));
    block
}

/// The designated explorer's report channel + keep-alive (feature 035,
/// contracts/page-report.md). Constructed per-child at dispatch time (never
/// registered in any base registry or toolset — it is visible ONLY to the
/// designated explorer, FR-001) and bound to THIS child's steer slot: the
/// manager's steer bridge skips designated explorers, so this tool is the
/// sole consumer of steering text, returned at the wait boundary (FR-006).
pub struct BrowserExplorerWaitTool {
    child_id: u64,
    registry: Arc<BrowserExplorerRegistry>,
    steer: Arc<Mutex<String>>,
    interrupt: Arc<AtomicBool>,
    recorder: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
    seq: AtomicU64,
}

impl BrowserExplorerWaitTool {
    pub fn new(
        child_id: u64,
        goal: String,
        registry: Arc<BrowserExplorerRegistry>,
        interrupt: Arc<AtomicBool>,
        steer: Arc<Mutex<String>>,
        recorder: Option<tokio::sync::mpsc::UnboundedSender<AgentEvent>>,
    ) -> Self {
        let _ = goal; // reserved for diagnostics; designation carries the goal
        Self { child_id, registry, steer, interrupt, recorder, seq: AtomicU64::new(0) }
    }
}

#[async_trait]
impl Tool for BrowserExplorerWaitTool {
    fn name(&self) -> &str {
        "browser_explorer_wait"
    }

    fn toolset(&self) -> &str {
        "browser"
    }

    fn description(&self) -> &str {
        "Report the outcome of the last steering instruction (one [PAGE-REPORT]: instruction echo, url, title, COMPLETE snapshot, outcome success|failure, error iff failure) and BLOCK until the orchestrator delivers the next steering instruction (timeout_secs, default 300). The returned text IS the next instruction. On timeout, re-report current state and call again."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "report": {
                    "type": "object",
                    "required": ["instruction", "url", "title", "snapshot", "outcome"],
                    "properties": {
                        "instruction": { "type": "string", "description": "The steering instruction this report answers (echo verbatim)" },
                        "url": { "type": "string" },
                        "title": { "type": "string" },
                        "snapshot": { "type": "string", "description": "Complete structural snapshot of the current page (browser_snapshot payload without since_last)" },
                        "outcome": { "type": "string", "enum": ["success", "failure"] },
                        "error": { "type": "string", "description": "Required when outcome=failure; description plus unchanged-page state" }
                    }
                },
                "timeout_secs": { "type": "integer", "minimum": 1, "default": 300 }
            },
            "required": ["report"]
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        if !self.registry.is_active_for(self.child_id) {
            return ToolResult::Error(
                "browser_explorer_wait: this child is no longer the designated browser explorer; produce your final summary now".to_string(),
            );
        }
        let Some(report) = args.get("report") else {
            return ToolResult::Error(
                "browser_explorer_wait requires 'report' { instruction, url, title, snapshot, outcome, error? } — exactly one report per call".to_string(),
            );
        };
        let validated = match validate_report(report) {
            Ok(v) => v,
            Err(e) => return ToolResult::Error(format!("invalid [PAGE-REPORT]: {e}")),
        };
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let block = format_page_report(self.child_id, seq, &validated);
        if let Some(tx) = &self.recorder {
            let _ = tx.send(AgentEvent::SubagentEvent {
                id: self.child_id,
                event: Box::new(AgentEvent::Notice(block)),
            });
        }
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .filter(|t| *t >= 1)
            .unwrap_or(300);
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            {
                let mut slot = self.steer.lock().unwrap_or_else(|p| p.into_inner());
                if !slot.is_empty() {
                    let text = std::mem::take(&mut *slot);
                    drop(slot);
                    return ToolResult::Text(text);
                }
            }
            if self.interrupt.load(Ordering::SeqCst) {
                return ToolResult::Text(
                    "[browser-explorer-wait] interrupted: a stop was requested; finish immediately with a one-line summary".to_string(),
                );
            }
            if Instant::now() >= deadline {
                return ToolResult::Text(
                    "[browser-explorer-wait] timeout: no steering instruction arrived; re-report the current page state via your next browser_explorer_wait call".to_string(),
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn designate_then_active() {
        let r = BrowserExplorerRegistry::new();
        assert!(r.active().is_none());
        r.designate(7, "open example.com").unwrap();
        let a = r.active().unwrap();
        assert_eq!(a.child_id, 7);
        assert_eq!(a.goal, "open example.com");
    }

    #[test]
    fn second_designation_rejected_naming_active_child_and_goal() {
        let r = BrowserExplorerRegistry::new();
        r.designate(7, "open example.com").unwrap();
        let err = r.designate(9, "other goal").unwrap_err();
        assert_eq!(
            err,
            "browser explorer already active: child 7 (goal: open example.com); steer it via subagent_control or stop it first"
        );
        assert_eq!(r.active().unwrap().child_id, 7);
    }

    #[test]
    fn clear_if_active_only_clears_matching_child() {
        let r = BrowserExplorerRegistry::new();
        r.designate(7, "g").unwrap();
        r.clear_if_active(9);
        assert!(r.active().is_some());
        r.clear_if_active(7);
        assert!(r.active().is_none());
        r.designate(9, "g2").unwrap();
        assert_eq!(r.active().unwrap().child_id, 9);
    }

    #[test]
    fn is_active_for_matches_only_designated_child() {
        let r = BrowserExplorerRegistry::new();
        assert!(!r.is_active_for(7));
        r.designate(7, "g").unwrap();
        assert!(r.is_active_for(7));
        assert!(!r.is_active_for(8));
    }

    mod wait_tool_tests {
        use super::super::*;
        use joey_core::Config;
        use joey_tools::{Tool, ToolContext};
        use std::path::PathBuf;

        fn harness() -> (BrowserExplorerWaitTool, Arc<Mutex<String>>, Arc<AtomicBool>) {
            let registry = Arc::new(BrowserExplorerRegistry::new());
            registry.designate(42, "open example.com").unwrap();
            let steer = Arc::new(Mutex::new(String::new()));
            let interrupt = Arc::new(AtomicBool::new(false));
            (
                BrowserExplorerWaitTool::new(
                    42,
                    "open example.com".into(),
                    registry,
                    interrupt.clone(),
                    steer.clone(),
                    None,
                ),
                steer,
                interrupt,
            )
        }

        fn valid_report() -> Value {
            json!({
                "instruction": "navigate to https://example.com",
                "url": "https://example.com",
                "title": "Example Domain",
                "snapshot": "- heading: Example Domain\n- link: More information...",
                "outcome": "success"
            })
        }

        fn ctx() -> ToolContext {
            ToolContext::new(PathBuf::from("/"), Config::defaults(), "wait-test")
        }

        #[tokio::test]
        async fn wait_returns_pre_delivered_steer_text() {
            let (tool, steer, _i) = harness();
            steer
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push_str("click the Docs link");
            let out = tool
                .execute(json!({ "report": valid_report() }), &ctx())
                .await;
            assert_eq!(out.to_content_string(), "click the Docs link");
        }

        #[tokio::test]
        async fn wait_times_out_with_notice_when_no_steer() {
            let (tool, _s, _i) = harness();
            let out = tool
                .execute(json!({ "report": valid_report(), "timeout_secs": 1 }), &ctx())
                .await;
            let s = out.to_content_string();
            assert!(s.starts_with("[browser-explorer-wait] timeout"), "{s}");
        }

        #[tokio::test]
        async fn wait_rejects_when_not_designated() {
            let registry = Arc::new(BrowserExplorerRegistry::new());
            let tool = BrowserExplorerWaitTool::new(
                7,
                "g".into(),
                registry,
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(String::new())),
                None,
            );
            let out = tool.execute(json!({ "report": valid_report() }), &ctx()).await;
            assert!(out.is_error());
            assert!(
                out.to_content_string().contains("designated browser explorer"),
                "{}",
                out.to_content_string()
            );
        }

        #[tokio::test]
        async fn report_validation_rejects_missing_and_mismatched_fields() {
            let (tool, _s, _i) = harness();
            // missing report object
            let out = tool.execute(json!({}), &ctx()).await;
            assert!(out.is_error());
            // missing instruction
            let mut bad = valid_report();
            bad.as_object_mut().unwrap().remove("instruction");
            let out = tool.execute(json!({ "report": bad }), &ctx()).await;
            assert!(out.to_content_string().contains("instruction"));
            // failure without error
            let mut bad = valid_report();
            bad["outcome"] = json!("failure");
            let out = tool.execute(json!({ "report": bad }), &ctx()).await;
            assert!(out.is_error(), "failure without error must be rejected");
            // success with error
            let mut bad = valid_report();
            bad["error"] = json!("boom");
            let out = tool.execute(json!({ "report": bad }), &ctx()).await;
            assert!(out.is_error(), "success with error must be rejected");
        }

        #[tokio::test]
        async fn pushes_page_report_block_to_recorder_with_seq() {
            let registry = Arc::new(BrowserExplorerRegistry::new());
            registry.designate(42, "g").unwrap();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let tool = BrowserExplorerWaitTool::new(
                42,
                "g".into(),
                registry,
                Arc::new(AtomicBool::new(false)),
                Arc::new(Mutex::new(String::new())),
                Some(tx),
            );
            let out = tool
                .execute(json!({ "report": valid_report(), "timeout_secs": 1 }), &ctx())
                .await;
            assert!(out.to_content_string().starts_with("[browser-explorer-wait] timeout"));
            let ev = rx.try_recv().expect("recorder event");
            match ev {
                AgentEvent::SubagentEvent { id, event } => {
                    assert_eq!(id, 42);
                    match *event {
                        AgentEvent::Notice(block) => {
                            assert!(block.starts_with("[PAGE-REPORT] child=42 seq=1\n"), "{block}");
                            assert!(block.contains("instruction: navigate to https://example.com\n"));
                            assert!(block.contains("outcome: success\n"));
                            assert!(block.contains("url: https://example.com\n"));
                            assert!(block.contains("title: Example Domain\n"));
                            assert!(block.contains("\n- heading: Example Domain"));
                        }
                        other => panic!("expected Notice, got {other:?}"),
                    }
                }
                other => panic!("expected SubagentEvent, got {other:?}"),
            }
            // seq increments on the second report
            let _ = tool
                .execute(json!({ "report": valid_report(), "timeout_secs": 1 }), &ctx())
                .await;
            match rx.try_recv().expect("second event") {
                AgentEvent::SubagentEvent { event, .. } => match *event {
                    AgentEvent::Notice(block) => {
                        assert!(block.starts_with("[PAGE-REPORT] child=42 seq=2\n"), "{block}")
                    }
                    other => panic!("expected Notice, got {other:?}"),
                },
                other => panic!("expected SubagentEvent, got {other:?}"),
            }
        }

        #[test]
        fn failure_report_block_carries_error_line() {
            let v = validate_report(
                &json!({
                    "instruction": "click missing link",
                    "url": "https://example.com",
                    "title": "Example Domain",
                    "snapshot": "- link: More information...",
                    "outcome": "failure",
                    "error": "no element matching 'missing link'"
                }),
            )
            .expect("valid");
            let block = format_page_report(9, 1, &v);
            assert!(block.contains("outcome: failure\n"));
            let idx_err = block.find("error: no element matching").expect("error line");
            let idx_snap = block.find("- link: More information...").expect("snapshot");
            assert!(idx_err < idx_snap, "error line precedes snapshot payload");
        }

        #[test]
        fn guidance_pins_steering_discipline() {
            let g = DESIGNATED_EXPLORER_GUIDANCE;
            for needle in [
                "browser_explorer_wait",
                "ONLY on explicit steering instructions",
                "COMPLETE snapshot",
                "unchanged current page",
                "'finish'",
                "reopen the page",
            ] {
                assert!(g.contains(needle), "guidance missing: {needle}");
            }
        }
    }
}
