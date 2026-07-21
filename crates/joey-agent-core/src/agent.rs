//! The agent runtime and turn loop (port of `run_agent.py` +
//! `agent/conversation_loop.py`).
//!
//! A turn: assemble messages → call the provider → if the assistant requested
//! tools, run them and loop; otherwise finish. Retries transient provider
//! errors with jittered backoff, up to `api_max_retries`.

use joey_core::Config;
use joey_providers::{
    build_client, FinishReason, Message, ProviderClient, ProviderError, ProviderRequest,
    ReasoningEffort, StreamEvent, ToolCall, ToolSchema, Usage,
};
use joey_tools::{ToolContext, ToolRegistry, ToolResult};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::events::AgentEvent;
use crate::prompt::build_system_prompt;

/// Runtime configuration for the agent.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: String,
    pub provider: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub max_turns: usize,
    pub api_max_retries: usize,
    pub reasoning: Option<ReasoningEffort>,
    pub enabled_tools: Vec<String>,
    pub max_tokens: Option<u32>,
    pub stream: bool,
}

impl AgentConfig {
    /// Build the agent config from a loaded [`Config`], honoring env overrides.
    pub fn from_config(cfg: &Config) -> Self {
        let model = cfg.model();
        let provider = cfg.get_str("model.provider", "auto");
        let base_url = cfg.get_str("model.base_url", "https://openrouter.ai/api/v1");
        let reasoning = resolve_reasoning(cfg, &model);
        let enabled = joey_tools::resolve_toolsets(&cfg.get_str_list("toolsets"));
        Self {
            model,
            provider,
            base_url,
            api_key: None,
            max_turns: cfg.get_i64("agent.max_turns", 60) as usize,
            api_max_retries: cfg.get_i64("agent.api_max_retries", 3) as usize,
            reasoning,
            enabled_tools: enabled,
            max_tokens: None,
            stream: cfg.get_bool("display.streaming", true),
        }
    }
}

fn resolve_reasoning(cfg: &Config, model: &str) -> Option<ReasoningEffort> {
    use joey_core::reasoning::{resolve, ReasoningConfig};
    match resolve(cfg.get("agent"), model) {
        Some(ReasoningConfig::Disabled) => Some(ReasoningEffort::Disabled),
        Some(ReasoningConfig::Effort(level)) => Some(ReasoningEffort::Level(level)),
        None => None,
    }
}

/// The result of a completed turn.
pub struct TurnResult {
    pub final_text: String,
    pub usage: Usage,
    pub iterations: usize,
    pub interrupted: bool,
}

/// The agent runtime.
pub struct Agent {
    config: AgentConfig,
    registry: ToolRegistry,
    ctx: ToolContext,
    client: ProviderClient,
    system_prompt: String,
    /// Running conversation history (excludes the system prompt).
    history: Vec<Message>,
}

impl Agent {
    /// Build an agent from config + tool registry + execution context.
    pub fn new(
        config: AgentConfig,
        registry: ToolRegistry,
        ctx: ToolContext,
    ) -> Result<Self, ProviderError> {
        let client = build_client(
            &config.provider,
            &config.base_url,
            &config.model,
            config.api_key.clone(),
        )?;
        let system_prompt = build_system_prompt(&ctx, &config.model);
        Ok(Self {
            config,
            registry,
            ctx,
            client,
            system_prompt,
            history: Vec::new(),
        })
    }

    pub fn client(&self) -> &ProviderClient {
        &self.client
    }

    pub fn history(&self) -> &[Message] {
        &self.history
    }

    /// Seed the history (e.g. from a resumed session).
    pub fn set_history(&mut self, history: Vec<Message>) {
        self.history = history;
    }

    /// The tool schemas exposed to the model this turn.
    fn tool_schemas(&self) -> Vec<ToolSchema> {
        let defs = self.registry.definitions(&self.config.enabled_tools, &self.ctx);
        defs.into_iter()
            .filter_map(|d| serde_json::from_value::<ToolSchema>(d).ok())
            .collect()
    }

    /// Run one conversational turn from a user message. Streams events on `tx`.
    pub async fn run_turn(
        &mut self,
        user_input: &str,
        tx: mpsc::UnboundedSender<AgentEvent>,
    ) -> TurnResult {
        self.history.push(Message::user(user_input));
        let tools = self.tool_schemas();
        let mut total_usage = Usage::default();
        let mut final_text = String::new();

        for iteration in 0..self.config.max_turns {
            let req = ProviderRequest::new(self.config.model.clone(), self.history.clone())
                .with_system(Some(self.system_prompt.clone()))
                .with_tools(tools.clone())
                .with_reasoning(self.config.reasoning.clone())
                .with_max_tokens(self.config.max_tokens)
                .streaming(self.config.stream);

            let resp = match self.call_with_retries(&req, &tx).await {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(AgentEvent::Failed(e.to_string()));
                    return TurnResult {
                        final_text: final_text.clone(),
                        usage: total_usage,
                        iterations: iteration,
                        interrupted: false,
                    };
                }
            };

            accumulate_usage(&mut total_usage, &resp.usage);

            // Record the assistant message (text + any tool calls).
            let assistant_msg = Message::assistant_with_tools(
                Some(resp.content.clone()),
                resp.tool_calls.clone(),
            );
            self.history.push(assistant_msg);

            if !resp.content.is_empty() {
                let _ = tx.send(AgentEvent::AssistantMessage(resp.content.clone()));
                final_text = resp.content.clone();
            }

            if resp.tool_calls.is_empty() || resp.finish_reason != FinishReason::ToolCalls {
                let _ = tx.send(AgentEvent::Done {
                    final_text: final_text.clone(),
                    usage: total_usage.clone(),
                });
                return TurnResult {
                    final_text,
                    usage: total_usage,
                    iterations: iteration + 1,
                    interrupted: false,
                };
            }

            // Execute the requested tool calls sequentially and append results.
            for call in &resp.tool_calls {
                let result = self.run_tool(call, &tx).await;
                let content = result.to_content_string();
                self.history
                    .push(Message::tool_result(&call.id, &call.function.name, content));
            }
        }

        // Hit the iteration cap.
        let _ = tx.send(AgentEvent::Notice(format!(
            "Reached the {}-turn limit.",
            self.config.max_turns
        )));
        let _ = tx.send(AgentEvent::Done {
            final_text: final_text.clone(),
            usage: total_usage.clone(),
        });
        TurnResult {
            final_text,
            usage: total_usage,
            iterations: self.config.max_turns,
            interrupted: false,
        }
    }

    async fn run_tool(&self, call: &ToolCall, tx: &mpsc::UnboundedSender<AgentEvent>) -> ToolResult {
        let args = call.parsed_args();
        let emoji = self
            .registry
            .get(&call.function.name)
            .map(|t| t.emoji().to_string())
            .unwrap_or_default();
        let _ = tx.send(AgentEvent::ToolStart {
            name: call.function.name.clone(),
            emoji,
            summary: summarize_args(&call.function.name, &args),
        });
        let result = self.registry.dispatch(&call.function.name, args, &self.ctx).await;
        let _ = tx.send(AgentEvent::ToolEnd {
            name: call.function.name.clone(),
            is_error: result.is_error(),
        });
        result
    }

    /// Call the provider, retrying transient errors with jittered backoff.
    async fn call_with_retries(
        &self,
        req: &ProviderRequest,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<joey_providers::NormalizedResponse, ProviderError> {
        let mut attempt = 0;
        loop {
            let result = if req.stream {
                self.call_streaming(req, tx).await
            } else {
                self.client.complete(req).await
            };
            match result {
                Ok(resp) => return Ok(resp),
                Err(e) if e.is_retryable() && attempt < self.config.api_max_retries => {
                    attempt += 1;
                    let delay = e
                        .retry_after()
                        .unwrap_or_else(|| joey_providers::jittered_backoff(attempt as u32));
                    let _ = tx.send(AgentEvent::Notice(format!(
                        "Provider error ({}); retrying in {:.0}s (attempt {}/{})",
                        e,
                        delay.as_secs_f64(),
                        attempt,
                        self.config.api_max_retries
                    )));
                    tokio::time::sleep(delay).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Streaming provider call: forwards content/reasoning deltas as events.
    async fn call_streaming(
        &self,
        req: &ProviderRequest,
        tx: &mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<joey_providers::NormalizedResponse, ProviderError> {
        let (ptx, mut prx) = mpsc::unbounded_channel::<StreamEvent>();
        let agent_tx = tx.clone();
        let forwarder = tokio::spawn(async move {
            while let Some(ev) = prx.recv().await {
                match ev {
                    StreamEvent::ContentDelta(d) => {
                        let _ = agent_tx.send(AgentEvent::ContentDelta(d));
                    }
                    StreamEvent::ReasoningDelta(d) => {
                        let _ = agent_tx.send(AgentEvent::ReasoningDelta(d));
                    }
                    StreamEvent::Done(_) => break,
                }
            }
        });
        let resp = self.client.stream(req, ptx).await;
        let _ = forwarder.await;
        resp
    }
}

fn accumulate_usage(total: &mut Usage, add: &Usage) {
    total.prompt_tokens += add.prompt_tokens;
    total.completion_tokens += add.completion_tokens;
    total.total_tokens += add.total_tokens;
    total.cache_read_tokens += add.cache_read_tokens;
    total.cache_write_tokens += add.cache_write_tokens;
    total.reasoning_tokens += add.reasoning_tokens;
}

/// A short human summary of a tool call's arguments for progress display.
fn summarize_args(name: &str, args: &Value) -> String {
    let pick = |keys: &[&str]| -> Option<String> {
        for k in keys {
            if let Some(v) = args.get(*k).and_then(|v| v.as_str()) {
                return Some(v.chars().take(80).collect());
            }
        }
        None
    };
    match name {
        "read_file" | "write_file" | "patch" => pick(&["path"]).unwrap_or_default(),
        "terminal" => pick(&["command"]).unwrap_or_default(),
        "search_files" => pick(&["pattern"]).unwrap_or_default(),
        "web_search" => pick(&["query"]).unwrap_or_default(),
        "skill_view" => pick(&["name"]).unwrap_or_default(),
        _ => String::new(),
    }
}
