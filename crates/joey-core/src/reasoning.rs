//! Reasoning-effort parsing (port of `hermes_constants.parse_reasoning_effort`
//! and `resolve_reasoning_config`).

use serde_yaml::Value;

/// Valid reasoning-effort levels (order = ascending capability).
pub const VALID_EFFORTS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max", "ultra"];

/// Parsed reasoning configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReasoningConfig {
    /// Thinking explicitly disabled.
    Disabled,
    /// Thinking enabled at a named effort level.
    Effort(String),
}

/// Parse a reasoning-effort value into a config.
///
/// - `false` / "none" / "false" / "disabled" → `Disabled`
/// - a valid effort level → `Effort(level)`
/// - `true` / empty / unrecognized → `None` (caller uses provider default)
pub fn parse_effort(value: &Value) -> Option<ReasoningConfig> {
    match value {
        Value::Bool(false) => Some(ReasoningConfig::Disabled),
        Value::Bool(true) | Value::Null => None,
        Value::String(s) => parse_effort_str(s),
        _ => None,
    }
}

/// Parse a string effort level.
pub fn parse_effort_str(s: &str) -> Option<ReasoningConfig> {
    let t = s.trim().to_lowercase();
    if t.is_empty() {
        return None;
    }
    if matches!(t.as_str(), "none" | "false" | "disabled" | "off" | "no") {
        return Some(ReasoningConfig::Disabled);
    }
    if VALID_EFFORTS.contains(&t.as_str()) {
        return Some(ReasoningConfig::Effort(t));
    }
    None
}

/// Resolve the effective reasoning config for `model` from a config tree.
/// Checks `agent.reasoning_overrides.<model>` (spelling-tolerant) first, then
/// the global `agent.reasoning_effort`.
pub fn resolve(agent_cfg: Option<&Value>, model: &str) -> Option<ReasoningConfig> {
    let agent = agent_cfg?;

    if let Some(Value::Mapping(overrides)) = agent.get("reasoning_overrides") {
        for variant in model_variants(model) {
            if let Some(v) = overrides.get(Value::String(variant)) {
                if let Some(parsed) = parse_effort(v) {
                    return Some(parsed);
                }
            }
        }
    }

    agent.get("reasoning_effort").and_then(parse_effort)
}

/// Generate spelling variants for tolerant override matching: the exact name,
/// dots↔dashes swaps, and the bare model name without a provider prefix.
fn model_variants(model: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut add = |s: String, out: &mut Vec<String>| {
        if !s.is_empty() && seen.insert(s.clone()) {
            out.push(s);
        }
    };
    add(model.to_string(), &mut out);
    add(model.replace('.', "-"), &mut out);
    add(model.replace('-', "."), &mut out);
    if let Some(bare) = model.rsplit('/').next() {
        add(bare.to_string(), &mut out);
        add(bare.replace('.', "-"), &mut out);
        add(bare.replace('-', "."), &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_levels() {
        assert_eq!(parse_effort_str("high"), Some(ReasoningConfig::Effort("high".into())));
        assert_eq!(parse_effort_str("none"), Some(ReasoningConfig::Disabled));
        assert_eq!(parse_effort_str("bogus"), None);
    }

    #[test]
    fn resolves_override_then_global() {
        let cfg: Value = serde_yaml::from_str(
            "reasoning_effort: medium\nreasoning_overrides:\n  \"anthropic/claude-opus-4.6\": high\n",
        )
        .unwrap();
        assert_eq!(
            resolve(Some(&cfg), "anthropic/claude-opus-4.6"),
            Some(ReasoningConfig::Effort("high".into()))
        );
        assert_eq!(
            resolve(Some(&cfg), "openai/gpt-5"),
            Some(ReasoningConfig::Effort("medium".into()))
        );
    }
}
