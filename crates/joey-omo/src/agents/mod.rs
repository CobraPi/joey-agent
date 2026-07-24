//! The 11 built-in OMO agents: definitions, model requirements, prompt dispatch.
//!
//! 1-to-1 port of oh-my-openagent's agent definitions. Each agent has an
//! identity, a model fallback chain, a mode (Primary/Subagent), and a
//! system-prompt builder that selects the model-family variant.

pub mod prompts;
pub mod registry;

use crate::mode::{AgentMode, ToolPermissions};
use crate::models::{ModelFamily, ModelRequirement};

// ── OmoAgent ────────────────────────────────────────────────────────

/// The identity and configuration of one of the 11 built-in agents.
///
/// Port of data-model.md `OmoAgent`. Fields map 1-to-1 with the OMO source.
#[derive(Debug, Clone)]
pub struct OmoAgent {
    /// Canonical machine name (e.g. "sisyphus", "oracle").
    pub name: String,
    /// Human label (e.g. "Sisyphus", "Sisyphus - ultraworker").
    pub display_name: String,
    /// `Primary` (Tab-selectable) or `Subagent` (delegation-invoked).
    pub mode: AgentMode,
    /// Hex color string (e.g. "#10B981" for Atlas).
    pub color: String,
    /// One-line description for the Tab picker.
    pub description: String,
    /// Fallback chain for model resolution.
    pub model_requirement: ModelRequirement,
    /// Model after fallback resolution (None = skipped/unavailable).
    pub resolved_model: Option<String>,
    /// Effort variant after resolution.
    pub resolved_variant: Option<String>,
    /// Sampling temperature.
    pub temperature: f64,
    /// Output token cap.
    pub max_tokens: Option<u32>,
    /// Allow/deny per-tool map.
    pub tool_permissions: ToolPermissions,
}

impl OmoAgent {
    /// Build the system prompt for this agent, selecting the model-family
    /// variant (BC-004). If no specific variant exists, the default is used.
    pub fn system_prompt(&self, model: &str) -> String {
        prompts::dispatch_system_prompt(&self.name, model)
    }

    /// Whether this agent is available (model resolved) — BC-002.
    pub fn is_available(&self) -> bool {
        self.resolved_model.is_some()
    }

    /// The resolved model, falling back to a default string if unresolved.
    pub fn effective_model(&self) -> &str {
        self.resolved_model.as_deref().unwrap_or("unavailable")
    }
}

// ── Model Requirement Constants (T012) ──────────────────────────────
//
// 1-to-1 port of OMO's `agent-model-requirements.ts`. Each chain is the
// exact ordered list from the source.

use crate::models::FallbackEntry as FE;

/// Sisyphus: requiresAnyModel=true. The default OMO orchestrator.
pub fn sisyphus_requirement() -> ModelRequirement {
    ModelRequirement {
        fallback_chain: vec![
            FE::new("claude-opus-4-8", Some("max"), &["anthropic", "github-copilot", "opencode", "vercel"]),
            FE::new("kimi-k3", None, &["opencode-go", "kimi-for-coding", "moonshotai", "opencode", "vercel", "bailian-coding-plan", "moonshotai-cn", "firmware", "ollama-cloud", "aihubmix"]),
            FE::new("gpt-5.6-sol", Some("medium"), &["openai", "github-copilot", "opencode", "vercel"]),
            FE::new("glm-5", None, &["zai-coding-plan", "opencode", "bailian-coding-plan", "vercel"]),
            FE::new("big-pickle", None, &["opencode"]),
        ],
        requires_any_model: true,
        requires_provider: None,
    }
}

/// Hephaestus: GPT-only; requiresProvider=[openai, github-copilot, opencode, vercel].
pub fn hephaestus_requirement() -> ModelRequirement {
    ModelRequirement {
        fallback_chain: vec![
            FE::new("gpt-5.6-sol", Some("medium"), &["openai", "github-copilot", "vercel", "opencode"]),
        ],
        requires_any_model: true,
        requires_provider: Some(vec![
            "openai".into(),
            "github-copilot".into(),
            "opencode".into(),
            "vercel".into(),
        ]),
    }
}

/// Oracle: read-only architecture consultant.
pub fn oracle_requirement() -> ModelRequirement {
    ModelRequirement {
        fallback_chain: vec![
            FE::new("gpt-5.6-sol", Some("xhigh"), &["openai", "opencode", "vercel"]),
            FE::new("gpt-5.6-sol", Some("high"), &["github-copilot"]),
            FE::new("gemini-3.1-pro", Some("high"), &["google", "github-copilot", "opencode", "vercel"]),
            FE::new("claude-opus-4-8", Some("max"), &["anthropic", "github-copilot", "opencode", "vercel"]),
            FE::new("glm-5.2", None, &["opencode-go", "vercel"]),
        ],
        requires_any_model: false,
        requires_provider: None,
    }
}

/// Librarian: docs/OSS search. Same chain as explore.
pub fn librarian_requirement() -> ModelRequirement {
    let chain = vec![
        FE::new("gpt-5.4-mini-fast", None, &["openai"]),
        FE::new("qwen3.5-plus", None, &["opencode-go", "bailian-coding-plan"]),
        FE::new("minimax-m2.7-highspeed", None, &["vercel"]),
        FE::new("minimax-m3", None, &["opencode-go", "vercel"]),
        FE::new("MiniMax-M3", None, &["minimax-coding-plan", "minimax-cn-coding-plan"]),
        FE::new("minimax-m2.7", None, &["opencode-go", "vercel"]),
        FE::new("claude-haiku-4-5", None, &["anthropic", "github-copilot", "vercel"]),
        FE::new("gpt-5.4-nano", None, &["openai", "vercel"]),
    ];
    ModelRequirement {
        fallback_chain: chain,
        requires_any_model: false,
        requires_provider: None,
    }
}

/// Explore: fast codebase grep. Same chain as librarian.
pub fn explore_requirement() -> ModelRequirement {
    librarian_requirement()
}

/// Multimodal-Looker: vision/screenshot analysis.
pub fn multimodal_requirement() -> ModelRequirement {
    ModelRequirement {
        fallback_chain: vec![
            FE::new("gpt-5.6-sol", Some("low"), &["openai", "opencode", "vercel"]),
            FE::new("kimi-k3", None, &["opencode-go", "vercel"]),
            FE::new("glm-4.6v", None, &["zai-coding-plan", "vercel"]),
            FE::new("gpt-5-nano", None, &["openai", "github-copilot", "opencode", "vercel"]),
        ],
        requires_any_model: false,
        requires_provider: None,
    }
}

/// Prometheus: read-only planner.
pub fn prometheus_requirement() -> ModelRequirement {
    ModelRequirement {
        fallback_chain: vec![
            FE::new("claude-opus-4-8", Some("max"), &["anthropic", "github-copilot", "opencode", "vercel"]),
            FE::new("gpt-5.6-sol", Some("high"), &["openai", "github-copilot", "opencode", "vercel"]),
            FE::new("glm-5.2", None, &["opencode-go", "vercel"]),
            FE::new("gemini-3.1-pro", None, &["google", "github-copilot", "opencode", "vercel"]),
        ],
        requires_any_model: false,
        requires_provider: None,
    }
}

/// Metis: gap analyzer.
pub fn metis_requirement() -> ModelRequirement {
    ModelRequirement {
        fallback_chain: vec![
            FE::new("claude-sonnet-4-6", None, &["anthropic", "github-copilot", "opencode", "vercel"]),
            FE::new("claude-opus-4-8", Some("max"), &["anthropic", "github-copilot", "opencode", "vercel"]),
            FE::new("gpt-5.6-sol", Some("medium"), &["openai", "github-copilot", "opencode", "vercel"]),
            FE::new("glm-5.2", None, &["opencode-go", "vercel"]),
            FE::new("kimi-k3", None, &["kimi-for-coding"]),
        ],
        requires_any_model: false,
        requires_provider: None,
    }
}

/// Momus: plan reviewer.
pub fn momus_requirement() -> ModelRequirement {
    ModelRequirement {
        fallback_chain: vec![
            FE::new("gpt-5.6-terra", Some("high"), &["openai", "vercel"]),
            FE::new("gpt-5.6-terra", Some("high"), &["github-copilot"]),
            FE::new("gpt-5.6-sol", Some("xhigh"), &["openai", "opencode", "vercel"]),
            FE::new("gpt-5.6-sol", Some("high"), &["github-copilot"]),
            FE::new("claude-opus-4-8", Some("max"), &["anthropic", "github-copilot", "opencode", "vercel"]),
            FE::new("gemini-3.1-pro", Some("high"), &["google", "github-copilot", "opencode", "vercel"]),
            FE::new("glm-5.2", None, &["opencode-go", "vercel"]),
        ],
        requires_any_model: false,
        requires_provider: None,
    }
}

/// Atlas: conductor; never writes code.
pub fn atlas_requirement() -> ModelRequirement {
    ModelRequirement {
        fallback_chain: vec![
            FE::new("claude-sonnet-4-6", None, &["anthropic", "github-copilot", "opencode", "vercel"]),
            FE::new("kimi-k3", None, &["opencode-go", "vercel"]),
            FE::new("gpt-5.6-sol", Some("medium"), &["openai", "github-copilot", "opencode", "vercel"]),
            FE::new("minimax-m3", None, &["opencode-go", "vercel"]),
            FE::new("MiniMax-M3", None, &["minimax-coding-plan", "minimax-cn-coding-plan"]),
            FE::new("minimax-m2.7", None, &["opencode-go", "vercel"]),
        ],
        requires_any_model: false,
        requires_provider: None,
    }
}

/// Sisyphus-Junior: task executor; blocks `task` tool.
pub fn sisyphus_junior_requirement() -> ModelRequirement {
    ModelRequirement {
        fallback_chain: vec![
            FE::new("claude-sonnet-4-6", None, &["anthropic", "github-copilot", "opencode", "vercel"]),
            FE::new("kimi-k3", None, &["opencode-go", "vercel"]),
            FE::new("gpt-5.6-sol", Some("medium"), &["openai", "github-copilot", "opencode", "vercel"]),
            FE::new("minimax-m3", None, &["opencode-go", "vercel"]),
            FE::new("MiniMax-M3", None, &["minimax-coding-plan", "minimax-cn-coding-plan"]),
            FE::new("minimax-m2.7", None, &["opencode-go", "vercel"]),
            FE::new("big-pickle", None, &["opencode"]),
        ],
        requires_any_model: false,
        requires_provider: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_agent_requirements_have_chains() {
        let reqs = [
            ("sisyphus", sisyphus_requirement()),
            ("hephaestus", hephaestus_requirement()),
            ("oracle", oracle_requirement()),
            ("librarian", librarian_requirement()),
            ("explore", explore_requirement()),
            ("multimodal-looker", multimodal_requirement()),
            ("prometheus", prometheus_requirement()),
            ("metis", metis_requirement()),
            ("momus", momus_requirement()),
            ("atlas", atlas_requirement()),
            ("sisyphus-junior", sisyphus_junior_requirement()),
        ];
        for (name, req) in &reqs {
            assert!(
                !req.fallback_chain.is_empty(),
                "{} must have a non-empty fallback chain",
                name
            );
        }
        assert_eq!(reqs.len(), 11, "exactly 11 agent requirements");
    }

    #[test]
    fn hephaestus_requires_provider() {
        let req = hephaestus_requirement();
        assert!(req.requires_provider.is_some(), "Hephaestus must require a provider");
        assert!(req.requires_provider.as_ref().unwrap().contains(&"openai".to_string()));
    }

    #[test]
    fn sisyphus_requires_any_model() {
        let req = sisyphus_requirement();
        assert!(req.requires_any_model, "Sisyphus must have requires_any_model=true");
    }
}
