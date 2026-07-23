//! The `delegate_task` tool — spawn one or more subagents in isolated contexts.
//!
//! Registered by higher crates (joey-cli) after constructing a SubagentManager.
//! The tool parses single/batch mode from args, calls dispatch_single or
//! dispatch_batch, and formats results per the delegation-tool contract.

use async_trait::async_trait;
use joey_agent_core::{AgentConfig, AgentEvent};
use joey_core::Config;
use joey_tools::registry::{Tool, ToolResult};
use joey_tools::{ToolContext, ToolRegistry};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::manager::SubagentManager;
use crate::types::{DelegationRequest, SubagentRole, TaskSpec};

/// The delegate_task tool. Holds an Arc<SubagentManager> for dispatching.
pub struct DelegateTask {
    manager: Arc<SubagentManager>,
    parent_config: AgentConfig,
    parent_config_tree: Config,
    base_registry: ToolRegistry,
    /// Event channel for emitting orchestration events to the parent's UI.
    event_tx: Option<mpsc::UnboundedSender<AgentEvent>>,
}

impl DelegateTask {
    pub fn new(
        manager: Arc<SubagentManager>,
        parent_config: AgentConfig,
        parent_config_tree: Config,
        base_registry: ToolRegistry,
        event_tx: Option<mpsc::UnboundedSender<AgentEvent>>,
    ) -> Self {
        Self {
            manager,
            parent_config,
            parent_config_tree,
            base_registry,
            event_tx,
        }
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
         to store the child session for later session_search recall."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "goal": {
                    "type": "string",
                    "description": "The task goal for the subagent. Required for single-task mode."
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
                            "toolsets": {"type": "array", "items": {"type": "string"}}
                        },
                        "required": ["goal"]
                    },
                    "description": "Batch mode: array of task specs for parallel dispatch. Each runs concurrently and independently. If provided, 'goal' is ignored."
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
                }
            }
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        // Check if batch mode (tasks array provided).
        let tasks_value = args.get("tasks");
        let is_batch = tasks_value.is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty()));

        if is_batch {
            return self.execute_batch(tasks_value.unwrap(), &args).await;
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

        let req = DelegationRequest {
            goal: goal.clone(),
            context: args.get("context").and_then(|v| v.as_str()).map(String::from),
            tasks: Vec::new(),
            model: args.get("model").and_then(|v| v.as_str()).map(String::from),
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
            persist: args.get("persist").and_then(|v| v.as_bool()).unwrap_or(false),
            role: SubagentRole::Leaf,
            workdir: None,
        };

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
    async fn execute_batch(&self, tasks_value: &Value, args: &Value) -> ToolResult {
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

        let results = self
            .manager
            .dispatch_batch(
                &task_specs,
                batch_model.as_deref(),
                &batch_toolsets,
                &self.parent_config,
                &self.parent_config_tree,
                &self.base_registry,
                self.event_tx.as_ref(),
            )
            .await;

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
                output.push_str(&format!("      status: success\n"));
                output.push_str(&format!("      summary: {}\n", r.summary));
            } else {
                output.push_str(&format!("      status: failed\n"));
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
