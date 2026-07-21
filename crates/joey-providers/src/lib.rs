//! `joey-providers` — the LLM provider layer for joey-agent.
//!
//! Port of `providers/`, `agent/transports/`, and the wire adapters. Maps a
//! provider-neutral [`ProviderRequest`] onto either the OpenAI Chat Completions
//! or Anthropic Messages wire protocol, with SSE streaming, and normalizes the
//! result into a [`NormalizedResponse`].

pub mod client;
pub mod error;
pub mod profile;
pub mod request;
pub mod types;

pub use client::ProviderClient;
pub use error::{jittered_backoff, ProviderError};
pub use profile::{resolve_profile, ApiMode, AuthType, ProviderProfile};
pub use request::{ProviderRequest, ReasoningEffort};
pub use types::{
    ContentPart, FinishReason, FunctionCall, ImageUrl, Message, NormalizedResponse, StreamEvent,
    ToolCall, ToolSchema, Usage,
};

/// Build a client from a resolved provider setting, base URL, and model.
/// Convenience wrapper over [`resolve_profile`] + [`ProviderClient::new`].
pub fn build_client(
    provider_setting: &str,
    base_url: &str,
    model: &str,
    api_key: Option<String>,
) -> Result<ProviderClient, ProviderError> {
    let profile = resolve_profile(provider_setting, base_url, model);
    // A custom base_url only overrides when it differs from the profile default
    // (so `provider: anthropic` with the default OpenRouter base_url still hits
    // the Anthropic endpoint).
    let base_override = if base_url.trim().is_empty() || base_url == profile.base_url {
        None
    } else if profile.name == "openrouter" || provider_setting == "custom" {
        Some(base_url.to_string())
    } else {
        None
    };
    ProviderClient::new(profile, base_override, api_key)
}
