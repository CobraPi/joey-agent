//! The `task_graph` tool — publish and maintain the orchestration task
//! graph (the HyperCode orchestrator's planning surface).
//!
//! The orchestrator plans in strict `joey-taskgraph/1` documents
//! (validated by [`TaskGraph::from_strict_json`] — the same validator
//! the execution-graph pipeline uses), then keeps the published graph
//! current as work progresses via legal [`TaskStatus`] transitions.
//! Every successful `plan`/`update` emits
//! [`joey_agent_core::AgentEvent::TaskGraphPublished`] through the
//! manager-local tap first, process-global tap as
//! fallback — see [`SubagentManager::event_tap`]); the TUI feeds that
//! event into its Tasks tab and header badge.
//!
//! State is in-memory only (the tool holds the current graph), so the
//! tool is benign: it writes no files and touches no children. It is
//! registered unconditionally and exposed via the `task-graph`
//! toolset only.

use async_trait::async_trait;
use joey_tools::context::ToolContext;
use joey_tools::registry::{Tool, ToolResult};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

use crate::manager::SubagentManager;
use crate::task_graph::{TaskGraph, TaskId, TaskStatus};

/// The task_graph tool. Holds an `Arc<SubagentManager>` so published
/// graphs flow through the SAME event tap every other delegation event
/// uses, plus the current in-memory graph.
pub struct TaskGraphTool {
    manager: Arc<SubagentManager>,
    /// The current published graph, if any.
    current: Mutex<Option<TaskGraph>>,
}

impl TaskGraphTool {
    pub fn new(manager: Arc<SubagentManager>) -> Self {
        Self {
            manager,
            current: Mutex::new(None),
        }
    }

    /// Emit a `TaskGraphPublished` event carrying a fresh snapshot of
    /// `graph` through the manager's event tap (manager-local tap, else
    /// the process-global tap). Call with the CURRENT graph under the
    /// lock, re-snapshotting after any mutations.
    fn emit_published(&self, graph: &TaskGraph) {
        let ev = joey_agent_core::AgentEvent::TaskGraphPublished {
            graph: graph.snapshot(),
        };
        if let Some(tap) = self.manager.event_tap() {
            let _ = tap.send(ev.clone());
        }
        // T029 mirror: every other emission site feeds the recorder tap
        // alongside the external tap — the subagent_control log ring keeps
        // filling without shadowing any host tap.
        if let Some(rec) = self.manager.recorder_tap() {
            let _ = rec.send(ev);
        }
    }

    /// `plan`: replace the current graph with a strict
    /// `joey-taskgraph/1` document (full validation), then publish.
    fn action_plan(&self, graph: &Value, ctx: &ToolContext) -> ToolResult {
        let serialized =
            serde_json::to_string(graph).expect("serde_json::Value serialization is infallible");
        let parsed = TaskGraph::from_strict_json(&serialized, ctx.cwd());
        let g = match parsed {
            Ok(g) => g,
            Err(errors) => {
                return ToolResult::Error(format!("graph rejected: {errors:?}"));
            }
        };
        let summary = format!(
            "plan accepted: {} tasks, {} ready now",
            g.nodes.len(),
            g.ready_nodes().len()
        );
        let mut guard = self.current.lock().unwrap_or_else(|p| p.into_inner());
        *guard = Some(g);
        if let Some(current) = guard.as_ref() {
            self.emit_published(current);
        }
        ToolResult::Text(summary)
    }

    /// `update`: apply `{id, status}` transitions sequentially,
    /// enforcing the legal edge set (`TaskGraph::transition`).
    /// Re-publishes iff at least one transition succeeded; an
    /// all-failed call is an Error enumerating the failures.
    fn action_update(&self, args: &Value) -> ToolResult {
        let Some(entries) = args.get("transitions").and_then(|v| v.as_array()) else {
            return ToolResult::Error(
                "action=update requires 'transitions' — an array of {id, status} objects"
                    .to_string(),
            );
        };
        let mut guard = self.current.lock().unwrap_or_else(|p| p.into_inner());
        let Some(graph) = guard.as_mut() else {
            return ToolResult::Error(
                "no graph published yet — call action=plan first".to_string(),
            );
        };
        let mut applied: Vec<String> = Vec::new();
        let mut failures: Vec<String> = Vec::new();
        for (i, entry) in entries.iter().enumerate() {
            let id_value = entry.get("id").cloned().unwrap_or(Value::Null);
            let status_value = entry.get("status").cloned().unwrap_or(Value::Null);
            let parsed_id = serde_json::from_value::<TaskId>(id_value);
            let parsed_status = serde_json::from_value::<TaskStatus>(status_value);
            match (parsed_id, parsed_status) {
                (Ok(id), Ok(status)) => match graph.transition(&id, status) {
                    Ok(_) => applied.push(id.to_string()),
                    Err(e) => failures.push(format!("{id}: {e}")),
                },
                (Err(e), _) => failures.push(format!("entry {}: invalid id: {e}", i + 1)),
                (_, Err(e)) => failures.push(format!("entry {}: invalid status: {e}", i + 1)),
            }
        }
        if !applied.is_empty() {
            // Re-snapshot AFTER all mutations and publish.
            self.emit_published(graph);
            let msg = if failures.is_empty() {
                format!("applied {}: {}", applied.len(), applied.join(", "))
            } else {
                format!(
                    "applied {}: {}; rejected {}: {}",
                    applied.len(),
                    applied.join(", "),
                    failures.len(),
                    failures.join("; ")
                )
            };
            ToolResult::Text(msg)
        } else {
            ToolResult::Error(format!(
                "applied 0; rejected {}: {}",
                failures.len(),
                failures.join("; ")
            ))
        }
    }

    /// `status`: render the current graph — one line per node (sorted by
    /// id, BTreeMap order) plus the ready set.
    fn action_status(&self) -> ToolResult {
        let guard = self.current.lock().unwrap_or_else(|p| p.into_inner());
        let Some(g) = guard.as_ref() else {
            return ToolResult::Text("no task graph published yet".to_string());
        };
        let mut lines: Vec<String> = g
            .nodes
            .iter()
            .map(|(id, node)| {
                let deps = node
                    .dependencies
                    .iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                let blocked = if g.is_blocked(id) { "; blocked" } else { "" };
                format!(
                    "{id}: {status} — {objective} (deps: {deps}{blocked})",
                    status = node.status,
                    objective = node.objective
                )
            })
            .collect();
        let ready = g
            .ready_nodes()
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("ready now: {ready}"));
        ToolResult::Text(lines.join("\n"))
    }
}

#[async_trait]
impl Tool for TaskGraphTool {
    fn name(&self) -> &str {
        "task_graph"
    }

    fn toolset(&self) -> &str {
        "task-graph"
    }

    fn emoji(&self) -> &str {
        "⚑"
    }

    fn description(&self) -> &str {
        "Publish and maintain the orchestration task graph (visible live in the TUI \
         Tasks view and header badge). action=plan: replace the graph with a strict \
         joey-taskgraph/1 document: {\"format\":\"joey-taskgraph/1\",\"tasks\":[<task>,...]}. \
         EVERY task object must carry ALL of these keys: \
         - id: string, lowercase [a-z0-9-] only (no underscores/uppercase) \
         - objective: string \
         - dependencies: array of task ids ([] for none) \
         - read_set, write_set: arrays of RELATIVE paths ([] allowed) \
         - artifact_ids: array of INTEGERS (never strings; [] when unknown) \
         - role: exactly \"explorer\" | \"implementor\" | \"orchestrator\" \
         - model_tier: exactly \"economical\" | \"frontier\" (no \"light\"/\"heavy\") \
         - risk: \"low\" | \"medium\" | \"high\" \
         - acceptance: NON-EMPTY array of {\"criterion\": string, \"kind\": string} \
         - verification: {\"steps\":[{\"name\",\"command\",\"parse\",\"timeout_sec\",\"required\"}], \
         \"risk_triggered_review\": bool} — the key is REQUIRED even when empty (use \
         {\"steps\":[],\"risk_triggered_review\":false}); risk \"high\" demands a step \
         with required:true or risk_triggered_review:true \
         Optional: isolation (auto-injected: isolated_worktree when write_set is \
         non-empty), status/attempts (defaulted), baseline_revision (top-level). \
         Validation rejects: unknown enum strings, missing keys, string artifact_ids, \
         absolute or parent-dir paths, empty acceptance, high risk without required \
         verification, dependency cycles, and two dependency-unrelated tasks writing \
         the same path. action=update: apply status transitions (array of {id, \
         status}; legal edges: pending->ready->dispatched->evaluating->{completed|\
         failed|degraded|blocked|dispatched}, degraded->evaluating|blocked, \
         blocked->ready, any non-terminal->skipped). action=status: render the \
         current graph with the ready set. Publish right after planning and keep it \
         current as work progresses — it is what keeps a long orchestration on track."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["plan", "update", "status"],
                    "description": "Graph action: 'plan' replaces the graph with a strict joey-taskgraph/1 document (requires 'graph'); 'update' applies status transitions (requires 'transitions'); 'status' renders the current graph with the ready set."
                },
                "graph": {
                    "type": "object",
                    "description": "Strict joey-taskgraph/1 document {\"format\":\"joey-taskgraph/1\",\"tasks\":[...]} — every task carries id, objective, dependencies, read_set, write_set, artifact_ids (integers), role (explorer|implementor|orchestrator), model_tier (economical|frontier), risk (low|medium|high), acceptance (non-empty), verification (steps + risk_triggered_review). Required when action=plan."
                },
                "transitions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "status": {
                                "type": "string",
                                "enum": ["pending","ready","dispatched","evaluating","completed","failed","degraded","blocked","skipped"]
                            }
                        },
                        "required": ["id", "status"]
                    },
                    "description": "Status transitions to apply. Required when action=update."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a.trim().to_lowercase(),
            None => {
                return ToolResult::Error(
                    "task_graph requires 'action' (plan, update, or status)".to_string(),
                );
            }
        };
        match action.as_str() {
            "plan" => {
                let graph = match args.get("graph") {
                    Some(v) if !v.is_null() => v,
                    _ => {
                        return ToolResult::Error("action=plan requires 'graph'".to_string());
                    }
                };
                self.action_plan(graph, ctx)
            }
            "update" => self.action_update(&args),
            "status" => self.action_status(),
            other => ToolResult::Error(format!(
                "Unknown action '{other}'. Implemented actions: plan, update, status."
            )),
        }
    }
}
