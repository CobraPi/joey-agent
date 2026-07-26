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
use crate::CategoryResolver;

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
}

impl DelegateTask {
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

        let req = DelegationRequest {
            goal: goal.clone(),
            context: args.get("context").and_then(|v| v.as_str()).map(String::from),
            tasks: Vec::new(),
            model: resolved_model.or_else(|| args.get("model").and_then(|v| v.as_str()).map(String::from)),
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
            category,
            subagent_type,
            load_skills,
            prompt_append,
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
