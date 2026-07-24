//! System-prompt dispatch for the 11 built-in OMO agents + ultrawork mode.
//!
//! Each agent has a model-family-variant prompt selector. The dispatch
//! function `dispatch_system_prompt(agent_name, model)` routes by canonical
//! agent name, and each module's `for_model(model)` selects the variant by
//! [`ModelFamily::detect`].
//!
//! Port fidelity note: the OMO source builds prompts dynamically with tool
//! tables, category skill guides, and agent availability tables injected at
//! runtime. This module captures the **identity-core** of each agent — its
//! role, behavioral directives, key constraints, and model-family
//! calibration — as compile-time `&str` constants. Runtime injection layers
//! (tool tables, skills) are layered on top by the harness, not baked in here.

use crate::models::ModelFamily;

// ── Agent prompt modules ────────────────────────────────────────────

pub mod atlas;
pub mod explore;
pub mod hephaestus;
pub mod junior;
pub mod librarian;
pub mod metis;
pub mod momus;
pub mod multimodal;
pub mod oracle;
pub mod prometheus;
pub mod sisyphus;
pub mod ultrawork;

// ── Dispatch ────────────────────────────────────────────────────────

/// Resolve the system prompt for a named agent + model combination.
///
/// `agent_name` is the canonical machine name (`"sisyphus"`, `"oracle"`,
/// `"multimodal-looker"`, etc.). `model` is the resolved model ID used to
/// select the model-family variant (Anthropic/GPT/Kimi/Glm/Gemini).
///
/// Unknown agent names fall back to the Sisyphus default prompt, since
/// Sisyphus is the canonical orchestrator and the safest default identity.
///
/// Port of OMO's `dynamic-agent-prompt-builder.ts` →
/// `buildPromptForAgent(model, agentName)`.
pub fn dispatch_system_prompt(agent_name: &str, model: &str) -> String {
    let normalized = agent_name.to_ascii_lowercase();
    let family = ModelFamily::detect(model);
    let _ = family; // family is used by each submodule's for_model()

    let prompt: &str = match normalized.as_str() {
        "sisyphus" => sisyphus::for_model(model),
        "atlas" => atlas::for_model(model),
        "hephaestus" => hephaestus::for_model(model),
        "prometheus" => prometheus::for_model(model),
        "oracle" => oracle::for_model(model),
        "librarian" => librarian::for_model(model),
        "explore" => explore::for_model(model),
        "multimodal-looker" | "multimodal" => multimodal::for_model(model),
        "metis" => metis::for_model(model),
        "momus" => momus::for_model(model),
        "sisyphus-junior" | "junior" => junior::for_model(model),
        // Unknown → Sisyphus default (safest orchestrator identity).
        _ => sisyphus::default(),
    };

    prompt.to_string()
}

/// Return the ultrawork-mode system prompt variant for the given model.
///
/// Separate from `dispatch_system_prompt` because ultrawork is an
/// overlay mode activated on the active primary agent (typically Sisyphus),
/// not its own agent identity.
pub fn ultrawork_prompt(model: &str) -> String {
    ultrawork::for_model(model).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_returns_nonempty_prompt_for_known_agents() {
        for agent in [
            "sisyphus",
            "atlas",
            "hephaestus",
            "prometheus",
            "oracle",
            "librarian",
            "explore",
            "multimodal-looker",
            "metis",
            "momus",
            "sisyphus-junior",
        ] {
            let prompt = dispatch_system_prompt(agent, "claude-opus-4-8");
            assert!(
                !prompt.is_empty(),
                "{} prompt must not be empty",
                agent
            );
        }
    }

    #[test]
    fn dispatch_selects_glm_variant_for_glm_model() {
        let prompt = dispatch_system_prompt("sisyphus", "glm-5.2");
        assert!(prompt.contains("GLM"), "GLM sisyphus variant must mention GLM");
    }

    #[test]
    fn dispatch_selects_gpt_variant_for_gpt_model() {
        let prompt = dispatch_system_prompt("sisyphus", "gpt-5.6-sol");
        assert!(
            prompt.contains("orchestrat") || prompt.contains("Orchestrat"),
            "GPT sisyphus variant must mention orchestration"
        );
    }

    #[test]
    fn dispatch_falls_back_to_sisyphus_for_unknown_agent() {
        let prompt = dispatch_system_prompt("nonexistent-agent", "claude-opus-4-8");
        assert!(
            prompt.contains("Sisyphus"),
            "Unknown agent should fall back to Sisyphus default"
        );
    }

    #[test]
    fn ultrawork_prompt_includes_mandatory_announcement() {
        let prompt = ultrawork_prompt("claude-opus-4-8");
        assert!(
            prompt.contains("ULTRAWORK MODE ENABLED!"),
            "Ultrawork prompt must include mandatory first-response announcement"
        );
    }
}
