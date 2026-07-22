//! OpenAI Chat Completions wire shaping — port of
//! `agent/transports/chat_completions.py` (`build_kwargs` /
//! `_build_kwargs_from_profile`), the per-provider plugin reasoning hooks
//! (`plugins/model-providers/*`), the max-tokens-kwarg switch
//! (`run_agent._max_tokens_param` + `utils.model_forces_max_completion_tokens`),
//! and the reasoning-extra-body allowlist (`run_agent._supports_reasoning_extra_body`).

use serde_json::{json, Map, Value};

use crate::anthropic;
use crate::profile::ProviderProfile;
use crate::request::{ProviderRequest, ReasoningEffort};

/// Model substrings that take the `developer` system role instead of `system`
/// (prompt_builder.DEVELOPER_ROLE_MODELS, chat_completions.py:347-356).
const DEVELOPER_ROLE_MODELS: &[&str] = &["gpt-5", "codex"];

/// True for model families that require `max_completion_tokens` instead of
/// `max_tokens` (`utils.model_forces_max_completion_tokens`, utils.py:493-527).
/// Vendor prefixes (`openai/gpt-5.4`) are stripped to the tail.
fn model_forces_max_completion_tokens(model: &str) -> bool {
    let mut m = model.trim().to_lowercase();
    if m.is_empty() {
        return false;
    }
    if let Some((_, tail)) = m.rsplit_once('/') {
        m = tail.to_string();
    }
    m.starts_with("gpt-4o")
        || m.starts_with("gpt-4.1")
        || m.starts_with("gpt-5")
        || m.starts_with("o1")
        || m.starts_with("o3")
        || m.starts_with("o4")
}

/// Emit the correct max-tokens kwarg key for this route/model
/// (`run_agent._max_tokens_param`, run_agent.py:1457-1479): `max_tokens`
/// normally, `max_completion_tokens` on api.openai.com / Azure / gpt-4o+ /
/// o-series. (Azure detection is host-based; the ported providers never hit
/// Azure, so the model-name rule carries it.)
fn insert_max_tokens(obj: &mut Map<String, Value>, base_url: &str, model: &str, value: u32) {
    let host = joey_core::utils::base_url_hostname(base_url);
    let forces = host == "api.openai.com"
        || host.ends_with(".openai.azure.com")
        || model_forces_max_completion_tokens(model);
    let key = if forces { "max_completion_tokens" } else { "max_tokens" };
    obj.insert(key.into(), json!(value));
}

/// OpenRouter reasoning-extra-body allowlist for models
/// (`run_agent._supports_reasoning_extra_body`, run_agent.py:5821-5867).
/// Only the OpenRouter + Nous portions are ported (GitHub/LM Studio/Ollama
/// routes are unported providers).
fn supports_reasoning_extra_body(profile: &ProviderProfile, base_url: &str, model: &str) -> bool {
    let host = joey_core::utils::base_url_hostname(base_url);
    if host == "nousresearch.com" || host.ends_with(".nousresearch.com") || profile.name == "nous" {
        return true;
    }
    let base_lower = base_url.to_lowercase();
    if !base_lower.contains("openrouter") && profile.name != "openrouter" {
        return false;
    }
    if base_lower.contains("api.mistral.ai") {
        return false;
    }
    let m = model.to_lowercase();
    const PREFIXES: &[&str] = &[
        "deepseek/",
        "anthropic/",
        "openai/",
        "x-ai/",
        "google/gemini-2",
        "google/gemma-4",
        "qwen/qwen3",
        "tencent/hy3",
        "xiaomi/",
    ];
    PREFIXES.iter().any(|p| m.starts_with(p))
}

/// Reasoning-mandatory Anthropic models on OpenRouter (Claude 4.6+ / future
/// named models) that reject every disable form
/// (`_anthropic_reasoning_is_mandatory`,
/// plugins/model-providers/openrouter/__init__.py:21-44).
fn anthropic_reasoning_is_mandatory(model: &str) -> bool {
    let m = model.to_lowercase();
    if !m.starts_with("anthropic/") && !m.starts_with("claude") && !m.contains("claude") {
        return false;
    }
    const OPTIONAL: &[&str] = &[
        "claude-3",
        "claude-opus-4-0",
        "claude-opus-4.0",
        "claude-opus-4-1",
        "claude-opus-4.1",
        "claude-sonnet-4-0",
        "claude-sonnet-4.0",
        "claude-opus-4-2025",
        "claude-sonnet-4-2025",
        "claude-opus-4-5",
        "claude-opus-4.5",
        "claude-sonnet-4-5",
        "claude-sonnet-4.5",
        "claude-haiku-4-5",
        "claude-haiku-4.5",
    ];
    !OPTIONAL.iter().any(|s| m.contains(s))
}

/// DeepSeek thinking-capable families
/// (deepseek/__init__.py:28-44): V4+ and legacy `deepseek-reasoner`.
fn deepseek_supports_thinking(model: &str) -> bool {
    let m = model.trim().to_lowercase();
    if m.is_empty() {
        return false;
    }
    if m.starts_with("deepseek-v") && !m.starts_with("deepseek-v3") {
        return true;
    }
    m == "deepseek-reasoner"
}

/// GLM thinking-capable families: glm-4.5 and later (zai/__init__.py:38-46).
fn glm_supports_thinking(model: &str) -> bool {
    let m = model.trim().to_lowercase();
    // Parse ^glm-(\d+)(?:\.(\d+))?
    let Some(rest) = m.strip_prefix("glm-") else {
        return false;
    };
    let mut chars = rest.chars();
    let major: String = chars.by_ref().take_while(|c| c.is_ascii_digit()).collect();
    if major.is_empty() {
        return false;
    }
    let major: u32 = major.parse().unwrap_or(0);
    // The take_while consumed the separator; re-parse minor from the original.
    let after_major = &rest[major.to_string().len()..];
    let minor: u32 = after_major
        .strip_prefix('.')
        .map(|s| s.chars().take_while(|c| c.is_ascii_digit()).collect::<String>())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (major, minor) >= (4, 5)
}

/// Detect GLM-5.2 across alias spellings (zai/__init__.py:49-59).
fn is_glm_5_2(model: &str) -> bool {
    let m = model.trim().to_lowercase();
    ["glm-5.2", "glm-5-2", "glm-5p2"].iter().any(|t| m.contains(t))
}

/// GLM-5.2 native reasoning_effort: high/max only (zai/__init__.py:62-82).
fn glm_5_2_reasoning_effort(reasoning: &ReasoningEffort) -> Option<&'static str> {
    match reasoning {
        ReasoningEffort::Disabled => None,
        ReasoningEffort::Level(effort) => {
            let e = effort.trim().to_lowercase();
            if e.is_empty() || e == "none" {
                None
            } else if matches!(e.as_str(), "xhigh" | "max" | "ultra") {
                Some("max")
            } else {
                Some("high")
            }
        }
    }
}

/// Gemini thinkingConfig from reasoning config, snake-cased for the /openai
/// shim (`_build_gemini_thinking_config` + `_snake_case_gemini_thinking_config`,
/// chat_completions.py:35-103). Only Gemini-family models get it.
fn gemini_thinking_config(model: &str, reasoning: Option<&ReasoningEffort>) -> Option<Value> {
    let reasoning = reasoning?;
    let mut m = model.trim().to_lowercase();
    if let Some(tail) = m.strip_prefix("google/") {
        m = tail.to_string();
    }
    if !m.starts_with("gemini") {
        return None;
    }
    // Build the raw config first.
    let raw: Value = match reasoning {
        ReasoningEffort::Disabled => json!({"includeThoughts": false}),
        ReasoningEffort::Level(effort) => {
            let mut e = effort.trim().to_lowercase();
            if e == "none" {
                return snake_case_gemini(&json!({"includeThoughts": false}));
            }
            let mut cfg = json!({"includeThoughts": true});
            if m.starts_with("gemini-2.5-") {
                // 2.5 accepts thinkingBudget; includeThoughts alone is enough.
                cfg
            } else {
                const VALID: &[&str] =
                    &["minimal", "low", "medium", "high", "xhigh", "max", "ultra"];
                if !VALID.contains(&e.as_str()) {
                    e = "medium".to_string();
                }
                if m.starts_with("gemini-3") || m.starts_with("gemini-3.1") {
                    if m.contains("flash") {
                        let level = if matches!(e.as_str(), "minimal" | "low") {
                            "low"
                        } else if matches!(e.as_str(), "high" | "xhigh" | "max" | "ultra") {
                            "high"
                        } else {
                            "medium"
                        };
                        cfg["thinkingLevel"] = json!(level);
                    } else if m.contains("pro") {
                        let level =
                            if matches!(e.as_str(), "high" | "xhigh" | "max" | "ultra") {
                                "high"
                            } else {
                                "low"
                            };
                        cfg["thinkingLevel"] = json!(level);
                    }
                }
                cfg
            }
        }
    };
    snake_case_gemini(&raw)
}

fn snake_case_gemini(config: &Value) -> Option<Value> {
    let obj = config.as_object()?;
    let mut out = Map::new();
    if let Some(v) = obj.get("includeThoughts").and_then(|v| v.as_bool()) {
        out.insert("include_thoughts".into(), json!(v));
    }
    if let Some(v) = obj.get("thinkingLevel").and_then(|v| v.as_str()) {
        if !v.trim().is_empty() {
            out.insert("thinking_level".into(), json!(v.trim().to_lowercase()));
        }
    }
    if let Some(v) = obj.get("thinkingBudget").and_then(|v| v.as_i64()) {
        out.insert("thinking_budget".into(), json!(v));
    }
    (!out.is_empty()).then_some(Value::Object(out))
}

/// A conservative vision allowlist for the non-vision image-stripping rule
/// (M17). Upstream derives vision-ness from the live model catalog, which is
/// unported; this approximates it by model-name family. Reported.
fn model_supports_vision(model: &str) -> bool {
    let m = model.to_lowercase();
    m.contains("claude")
        || m.contains("gpt-4o")
        || m.contains("gpt-4.1")
        || m.contains("gpt-5")
        || m.contains("gemini")
        || m.contains("grok")
        || m.contains("pixtral")
        || m.contains("llava")
        || m.contains("qwen-vl")
        || m.contains("qwen2-vl")
        || m.contains("qwen2.5-vl")
        || m.contains("-vl")
        || m.contains("vision")
        || m.contains("minimax")
}

/// Serialize one message to the OpenAI wire shape, stripping image parts when
/// the model is non-vision (chat_completion_helpers.py:1171, run_agent
/// `_prepare_messages_for_non_vision_model`).
fn openai_message_json(m: &crate::types::Message, strip_images: bool) -> Value {
    let mut obj = Map::new();
    obj.insert("role".into(), json!(m.role));

    if let Some(parts) = &m.content_parts {
        if strip_images {
            // Replace the multimodal content with the concatenated text parts.
            let text: Vec<String> = parts
                .iter()
                .filter_map(|p| match p {
                    crate::types::ContentPart::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect();
            obj.insert("content".into(), json!(text.join("\n")));
        } else {
            obj.insert("content".into(), serde_json::to_value(parts).unwrap_or(Value::Null));
        }
    } else {
        obj.insert("content".into(), json!(m.content.clone().unwrap_or_default()));
    }
    if !m.tool_calls.is_empty() {
        obj.insert("tool_calls".into(), serde_json::to_value(&m.tool_calls).unwrap_or(Value::Null));
    }
    if let Some(id) = &m.tool_call_id {
        obj.insert("tool_call_id".into(), json!(id));
    }
    if m.role == "tool" {
        if let Some(name) = &m.name {
            obj.insert("name".into(), json!(name));
        }
    }
    Value::Object(obj)
}

/// Build the OpenAI Chat Completions request body for `profile`. The `stream`
/// flag / `stream_options` are added by the client.
pub(crate) fn build_openai_body(
    profile: &ProviderProfile,
    base_url: &str,
    req: &ProviderRequest,
) -> Value {
    let wire_model = crate::profile::wire_model_name(profile, &req.model);
    let model_lower = req.model.to_lowercase();
    let strip_images = !model_supports_vision(&req.model);

    let mut messages: Vec<Value> = Vec::new();
    if let Some(sys) = &req.system {
        if !sys.is_empty() {
            // Developer-role swap for gpt-5/codex (chat_completions.py:347-356,
            // 528-534). Applies to the leading system message.
            let role = if DEVELOPER_ROLE_MODELS.iter().any(|p| model_lower.contains(p)) {
                "developer"
            } else {
                "system"
            };
            messages.push(json!({"role": role, "content": sys}));
        }
    }
    for m in &req.messages {
        messages.push(openai_message_json(m, strip_images));
    }

    let mut body = json!({ "model": wire_model, "messages": messages });
    let obj = body.as_object_mut().unwrap();

    if !req.tools.is_empty() {
        obj.insert("tools".into(), serde_json::to_value(&req.tools).unwrap_or(Value::Null));
        // The OpenAI wire sends NO tool_choice ever (chat_completions.py:374,561).
    }

    // max_tokens resolution: caller > profile default > Anthropic-family
    // model-table fallback (chat_completions.py:563-580,
    // chat_completion_helpers.py:1134-1144).
    let max_tokens = req
        .max_tokens
        .or(profile.default_max_tokens)
        .or_else(|| {
            anthropic::model_in_anthropic_output_table(&req.model)
                .then(|| anthropic::get_anthropic_max_output(&req.model))
        });
    if let Some(mt) = max_tokens {
        insert_max_tokens(obj, base_url, &req.model, mt);
    }

    if let Some(t) = req.temperature {
        obj.insert("temperature".into(), json!(t));
    }

    apply_reasoning_shape(profile, base_url, req, obj);

    body
}

/// Apply the per-provider reasoning wire shape (H6/H7). Mirrors each plugin's
/// `build_api_kwargs_extras` / `build_extra_body`.
fn apply_reasoning_shape(
    profile: &ProviderProfile,
    base_url: &str,
    req: &ProviderRequest,
    obj: &mut Map<String, Value>,
) {
    let reasoning = req.reasoning.as_ref();
    let mut extra_body = Map::new();

    match profile.name {
        "openrouter" => {
            if supports_reasoning_extra_body(profile, base_url, &req.model) {
                if anthropic_reasoning_is_mandatory(&req.model) {
                    // Send NO `reasoning`; route effort onto top-level `verbosity`
                    // (openrouter/__init__.py:149-156). Omit when disabled / no effort.
                    if let Some(ReasoningEffort::Level(effort)) = reasoning {
                        let e = effort.trim();
                        if !e.is_empty() && e.to_lowercase() != "none" {
                            obj.insert("verbosity".into(), json!(e));
                        }
                    }
                } else {
                    match reasoning {
                        Some(ReasoningEffort::Level(effort)) => {
                            extra_body.insert(
                                "reasoning".into(),
                                json!({"enabled": true, "effort": effort}),
                            );
                        }
                        Some(ReasoningEffort::Disabled) => {
                            extra_body.insert("reasoning".into(), json!({"enabled": false}));
                        }
                        None => {
                            extra_body.insert(
                                "reasoning".into(),
                                json!({"enabled": true, "effort": "medium"}),
                            );
                        }
                    }
                }
            }
        }
        "nous" => {
            // Full reasoning_config dict, omitted when disabled (nous/__init__.py:22-40).
            match reasoning {
                Some(ReasoningEffort::Level(effort)) => {
                    extra_body.insert(
                        "reasoning".into(),
                        json!({"enabled": true, "effort": effort}),
                    );
                }
                Some(ReasoningEffort::Disabled) => {} // omitted
                None => {
                    extra_body.insert(
                        "reasoning".into(),
                        json!({"enabled": true, "effort": "medium"}),
                    );
                }
            }
        }
        "deepseek" => {
            // thinking + reasoning_effort only for thinking-capable models
            // (deepseek/__init__.py:47-83).
            if deepseek_supports_thinking(&req.model) {
                let enabled = !matches!(reasoning, Some(ReasoningEffort::Disabled));
                extra_body.insert(
                    "thinking".into(),
                    json!({"type": if enabled { "enabled" } else { "disabled" }}),
                );
                if enabled {
                    if let Some(ReasoningEffort::Level(effort)) = reasoning {
                        let e = effort.trim().to_lowercase();
                        if matches!(e.as_str(), "xhigh" | "max" | "ultra") {
                            obj.insert("reasoning_effort".into(), json!("max"));
                        } else if matches!(e.as_str(), "low" | "medium" | "high") {
                            obj.insert("reasoning_effort".into(), json!(e));
                        }
                    }
                }
            }
        }
        "zai" => {
            // extra_body.thinking on/off + GLM-5.2 reasoning_effort
            // (zai/__init__.py:88-108).
            if glm_supports_thinking(&req.model) || is_glm_5_2(&req.model) {
                match reasoning {
                    Some(ReasoningEffort::Level(_)) => {
                        extra_body.insert("thinking".into(), json!({"type": "enabled"}));
                    }
                    Some(ReasoningEffort::Disabled) => {
                        extra_body.insert("thinking".into(), json!({"type": "disabled"}));
                    }
                    None => {} // omit — keep server default
                }
                if is_glm_5_2(&req.model) {
                    if let Some(r) = reasoning {
                        if let Some(effort) = glm_5_2_reasoning_effort(r) {
                            obj.insert("reasoning_effort".into(), json!(effort));
                        }
                    }
                }
            }
        }
        "gemini" => {
            // Gemini /openai shim: extra_body.extra_body.google.thinking_config
            // snake-cased (gemini/__init__.py:21-49, chat_completions.py:486-497).
            if let Some(tc) = gemini_thinking_config(&req.model, reasoning) {
                extra_body.insert("extra_body".into(), json!({"google": {"thinking_config": tc}}));
            }
        }
        // openai-api / openai-codex: send NOTHING reasoning-related.
        _ => {}
    }

    if !extra_body.is_empty() {
        obj.insert("extra_body".into(), Value::Object(extra_body));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::get_profile;
    use crate::types::Message;

    fn req(model: &str, reasoning: Option<ReasoningEffort>) -> ProviderRequest {
        ProviderRequest::new(model, vec![Message::user("hi")])
            .with_system(Some("sys".into()))
            .with_reasoning(reasoning)
    }

    #[test]
    fn openrouter_claude_uses_verbosity_no_reasoning() {
        let p = get_profile("openrouter").unwrap();
        let body = build_openai_body(
            &p,
            p.base_url,
            &req("anthropic/claude-opus-4.6", Some(ReasoningEffort::Level("high".into()))),
        );
        assert_eq!(body["verbosity"], json!("high"));
        assert!(body.get("extra_body").is_none(), "no reasoning field for mandatory Claude");
    }

    #[test]
    fn openrouter_non_claude_uses_reasoning_effort() {
        let p = get_profile("openrouter").unwrap();
        let body = build_openai_body(
            &p,
            p.base_url,
            &req("openai/gpt-4.1", Some(ReasoningEffort::Level("high".into()))),
        );
        assert_eq!(body["extra_body"]["reasoning"], json!({"enabled": true, "effort": "high"}));
        assert!(body.get("verbosity").is_none());
    }

    #[test]
    fn openrouter_disabled_reasoning() {
        let p = get_profile("openrouter").unwrap();
        let body = build_openai_body(
            &p,
            p.base_url,
            &req("openai/gpt-4.1", Some(ReasoningEffort::Disabled)),
        );
        assert_eq!(body["extra_body"]["reasoning"], json!({"enabled": false}));
    }

    #[test]
    fn openrouter_unlisted_model_no_reasoning() {
        // A model not on the allowlist gets no reasoning extra_body.
        let p = get_profile("openrouter").unwrap();
        let body = build_openai_body(
            &p,
            p.base_url,
            &req("cohere/command-r", Some(ReasoningEffort::Level("high".into()))),
        );
        assert!(body.get("extra_body").is_none());
    }

    #[test]
    fn openai_direct_sends_no_reasoning_and_no_tool_choice() {
        let p = get_profile("openai-api").unwrap();
        let mut r = req("gpt-4.1", Some(ReasoningEffort::Level("high".into())));
        r.tools = vec![crate::types::ToolSchema::new("t", "d", json!({"type": "object"}))];
        let body = build_openai_body(&p, p.base_url, &r);
        assert!(body.get("extra_body").is_none());
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("tools").is_some());
        assert!(body.get("tool_choice").is_none(), "OpenAI wire never sends tool_choice");
    }

    #[test]
    fn deepseek_thinking_shape() {
        let p = get_profile("deepseek").unwrap();
        let body = build_openai_body(
            &p,
            p.base_url,
            &req("deepseek-v4-pro", Some(ReasoningEffort::Level("xhigh".into()))),
        );
        assert_eq!(body["extra_body"]["thinking"], json!({"type": "enabled"}));
        assert_eq!(body["reasoning_effort"], json!("max"));
        // deepseek-chat (V3) → no thinking.
        let body = build_openai_body(&p, p.base_url, &req("deepseek-chat", Some(ReasoningEffort::Level("high".into()))));
        assert!(body.get("extra_body").is_none());
    }

    #[test]
    fn zai_thinking_and_glm52_effort() {
        let p = get_profile("zai").unwrap();
        let body = build_openai_body(&p, p.base_url, &req("glm-5.2", Some(ReasoningEffort::Level("low".into()))));
        assert_eq!(body["extra_body"]["thinking"], json!({"type": "enabled"}));
        assert_eq!(body["reasoning_effort"], json!("high"), "low clamps to GLM-5.2 minimum");
        // glm-4-9b (pre-4.5) → nothing.
        let body = build_openai_body(&p, p.base_url, &req("glm-4-9b", Some(ReasoningEffort::Level("high".into()))));
        assert!(body.get("extra_body").is_none());
    }

    #[test]
    fn gemini_shim_thinking_config() {
        let p = get_profile("gemini").unwrap();
        let body = build_openai_body(
            &p,
            p.base_url,
            &req("gemini-3-flash", Some(ReasoningEffort::Level("high".into()))),
        );
        assert_eq!(
            body["extra_body"]["extra_body"]["google"]["thinking_config"],
            json!({"include_thoughts": true, "thinking_level": "high"})
        );
    }

    #[test]
    fn nous_omits_reasoning_when_disabled() {
        let p = get_profile("nous").unwrap();
        let body = build_openai_body(&p, p.base_url, &req("hermes-4", Some(ReasoningEffort::Disabled)));
        assert!(body.get("extra_body").is_none());
        let body = build_openai_body(&p, p.base_url, &req("hermes-4", Some(ReasoningEffort::Level("high".into()))));
        assert_eq!(body["extra_body"]["reasoning"], json!({"enabled": true, "effort": "high"}));
    }

    #[test]
    fn max_tokens_priority_and_completion_switch() {
        let p = get_profile("openai-api").unwrap();
        // Caller value wins; gpt-4.1 forces max_completion_tokens.
        let mut r = req("gpt-4.1", None);
        r.max_tokens = Some(1234);
        let body = build_openai_body(&p, p.base_url, &r);
        assert_eq!(body["max_completion_tokens"], json!(1234));
        assert!(body.get("max_tokens").is_none());

        // Non-openai model on openrouter with a Claude table entry, no caller
        // value → Anthropic-family fallback via plain max_tokens.
        let orp = get_profile("openrouter").unwrap();
        let body = build_openai_body(&orp, orp.base_url, &req("anthropic/claude-opus-4.6", None));
        assert_eq!(body["max_tokens"], json!(128_000));

        // A non-openai, non-anthropic model with no caller value → no cap.
        let body = build_openai_body(&orp, orp.base_url, &req("mistralai/mistral-large", None));
        assert!(body.get("max_tokens").is_none() && body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn developer_role_swap() {
        let p = get_profile("openai-api").unwrap();
        let body = build_openai_body(&p, p.base_url, &req("gpt-5.1", None));
        assert_eq!(body["messages"][0]["role"], json!("developer"));
        let body = build_openai_body(&p, p.base_url, &req("gpt-4.1", None));
        assert_eq!(body["messages"][0]["role"], json!("system"));
    }
}
