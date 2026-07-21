//! The provider-neutral request type.

use crate::types::{Message, ToolSchema};

/// Reasoning effort passed to the provider (already resolved from config).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReasoningEffort {
    Disabled,
    Level(String),
}

/// A single inference request in provider-neutral form. The client maps this
/// onto the active provider's wire protocol.
#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub system: Option<String>,
    pub tools: Vec<ToolSchema>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub reasoning: Option<ReasoningEffort>,
    pub stream: bool,
}

impl ProviderRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            messages,
            system: None,
            tools: Vec::new(),
            max_tokens: None,
            temperature: None,
            reasoning: None,
            stream: false,
        }
    }

    pub fn with_system(mut self, system: Option<String>) -> Self {
        self.system = system;
        self
    }

    pub fn with_tools(mut self, tools: Vec<ToolSchema>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: Option<u32>) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn with_reasoning(mut self, reasoning: Option<ReasoningEffort>) -> Self {
        self.reasoning = reasoning;
        self
    }

    pub fn streaming(mut self, stream: bool) -> Self {
        self.stream = stream;
        self
    }
}
