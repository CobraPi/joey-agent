//! Reasoning-effort parsing (port of `hermes_constants.parse_reasoning_effort`,
//! `_canonical_model_variants`, and `resolve_reasoning_config`).

use once_cell::sync::Lazy;
use regex::Regex;
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
/// - YAML `false` / "none" / "false" / "disabled" → `Disabled`
/// - a valid effort level → `Effort(level)`
/// - `true` / null / empty / unrecognized → `None` (caller uses provider default)
pub fn parse_effort(value: &Value) -> Option<ReasoningConfig> {
    match value {
        Value::Bool(false) => Some(ReasoningConfig::Disabled),
        Value::Bool(true) | Value::Null => None,
        other => {
            // Upstream coerces with str(effort) — numbers etc. stringify then
            // fail the membership test, returning None.
            let s = match other {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => return None,
            };
            parse_effort_str(&s)
        }
    }
}

/// Parse a string effort level. The disabled-string set is exactly
/// `{"none", "false", "disabled"}` (upstream) — "off"/"no" are NOT strings
/// upstream accepts; they only disable via YAML booleans.
pub fn parse_effort_str(s: &str) -> Option<ReasoningConfig> {
    let t = s.trim().to_lowercase();
    if t.is_empty() {
        return None;
    }
    if matches!(t.as_str(), "none" | "false" | "disabled") {
        return Some(ReasoningConfig::Disabled);
    }
    if VALID_EFFORTS.contains(&t.as_str()) {
        return Some(ReasoningConfig::Effort(t));
    }
    None
}

static DASH_TO_DOT: Lazy<Regex> = Lazy::new(|| Regex::new(r"(\d)-(\d)").unwrap());
static DOT_TO_DASH: Lazy<Regex> = Lazy::new(|| Regex::new(r"(\d)\.(\d)").unwrap());

fn dash_to_dot(s: &str) -> String {
    DASH_TO_DOT.replace_all(s, "${1}.${2}").into_owned()
}

fn dot_to_dash(s: &str) -> String {
    DOT_TO_DASH.replace_all(s, "${1}-${2}").into_owned()
}

/// The 12 known provider prefixes prepended to bare model variants.
const KNOWN_PROVIDERS: &[&str] = &[
    "anthropic", "openai", "google", "openrouter", "groq", "mistral",
    "xai", "cohere", "perplexity", "together", "fireworks", "deepseek",
];

/// The 5 known aggregator prefixes prepended to single-slash variants.
const KNOWN_AGGREGATORS: &[&str] = &["openrouter", "opencode", "fireworks", "groq", "together"];

/// Generate bounded spelling variants for tolerant override matching.
///
/// Port of upstream `_canonical_model_variants`:
/// 1. Exact input
/// 2. Dots/dashes cross-substitution on the entire string
/// 3. Version-dot recovery (digit-sep-digit) applied to ALL derivatives
/// 4. Strip provider/aggregator prefix → bare model variants (+ recovery)
/// 5. Prepend known provider prefixes to bare variants
/// 6. Prepend known aggregator prefixes to single-slash variants
///
/// Duplicates removed in insertion order (exact always wins).
pub fn model_variants(model: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut variants: Vec<String> = Vec::new();

    fn add(v: String, seen: &mut std::collections::HashSet<String>, out: &mut Vec<String>) {
        if !v.is_empty() && seen.insert(v.clone()) {
            out.push(v);
        }
    }

    fn add_with_derivatives(
        s: &str,
        seen: &mut std::collections::HashSet<String>,
        out: &mut Vec<String>,
    ) {
        add(s.to_string(), seen, out);
        let all_dashed = s.replace('.', "-");
        add(all_dashed.clone(), seen, out);
        let all_dotted = s.replace('-', ".");
        add(all_dotted.clone(), seen, out);
        // Version-dot recovery on each base form
        add(dash_to_dot(s), seen, out);
        add(dot_to_dash(s), seen, out);
        add(dash_to_dot(&all_dashed), seen, out);
        add(dot_to_dash(&all_dotted), seen, out);
    }

    // 1-3. Base variants for the full string
    add_with_derivatives(model, &mut seen, &mut variants);

    let parts: Vec<&str> = model.split('/').collect();

    // 4. Bare model variants (strip provider/aggregator prefix)
    if parts.len() >= 2 {
        let bare = parts[parts.len() - 1];
        add_with_derivatives(bare, &mut seen, &mut variants);
    }

    // Strip aggregator only (3+ parts)
    if parts.len() >= 3 {
        let stripped = parts[1..].join("/");
        add_with_derivatives(&stripped, &mut seen, &mut variants);
    }

    // 5. Prepend known provider prefixes to bare variants
    let bare_variants: Vec<String> = variants
        .iter()
        .filter(|v| !v.contains('/'))
        .cloned()
        .collect();
    for v in &bare_variants {
        for provider in KNOWN_PROVIDERS {
            add(format!("{}/{}", provider, v), &mut seen, &mut variants);
        }
    }

    // 6. Prepend aggregator to single-slash variants
    let single_slash: Vec<String> = variants
        .iter()
        .filter(|v| v.matches('/').count() == 1)
        .cloned()
        .collect();
    for v in &single_slash {
        for agg in KNOWN_AGGREGATORS {
            add(format!("{}/{}", agg, v), &mut seen, &mut variants);
        }
    }

    variants
}

/// Lookup a per-model reasoning_effort override with spelling tolerance.
/// First variant whose parse is non-None wins.
pub fn resolve_per_model(overrides: &Value, model: &str) -> Option<ReasoningConfig> {
    let map = overrides.as_mapping()?;
    if map.is_empty() || model.is_empty() {
        return None;
    }
    for variant in model_variants(model) {
        if let Some(v) = map.get(Value::String(variant)) {
            if let Some(parsed) = parse_effort(v) {
                return Some(parsed);
            }
        }
    }
    None
}

/// Resolve the effective reasoning config for `model` from the ROOT config
/// tree (port of upstream `resolve_reasoning_config`).
///
/// Priority:
/// 1. Per-model override from `agent.reasoning_overrides` (spelling-tolerant)
/// 2. Global `agent.reasoning_effort` — the raw value passes through so a
///    YAML `false` means "disabled", never silently re-enabled.
///
/// When `model` is empty it is derived from the config's `model` section
/// (string form, or a mapping's `default`/`model` keys). An unrecognized
/// non-empty global value logs the upstream warning and returns `None`.
pub fn resolve(cfg: Option<&Value>, model: &str) -> Option<ReasoningConfig> {
    let empty = Value::Mapping(Default::default());
    let cfg = cfg.unwrap_or(&empty);
    let agent_cfg = match cfg.get("agent") {
        Some(v @ Value::Mapping(_)) => v.clone(),
        _ => Value::Mapping(Default::default()),
    };

    let mut model = model.trim().to_string();
    if model.is_empty() {
        model = match cfg.get("model") {
            Some(Value::String(s)) => s.trim().to_string(),
            Some(Value::Mapping(m)) => {
                let get_str = |k: &str| -> String {
                    m.get(Value::String(k.to_string()))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string()
                };
                let default = get_str("default");
                if !default.is_empty() {
                    default
                } else {
                    get_str("model")
                }
            }
            _ => String::new(),
        };
    }

    if let Some(overrides) = agent_cfg.get("reasoning_overrides") {
        if let Some(per_model) = resolve_per_model(overrides, &model) {
            return Some(per_model);
        }
    }

    // Global fallback — keep the raw value; a YAML boolean False must mean
    // "disabled", never "".
    let effort = agent_cfg.get("reasoning_effort").cloned().unwrap_or(Value::Null);
    let result = parse_effort(&effort);
    let effort_display = match &effort {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    if !effort_display.trim().is_empty() && result.is_none() {
        tracing::warn!("Unknown reasoning_effort '{}', using default (medium)", effort_display);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_levels() {
        assert_eq!(parse_effort_str("high"), Some(ReasoningConfig::Effort("high".into())));
        assert_eq!(parse_effort_str("none"), Some(ReasoningConfig::Disabled));
        assert_eq!(parse_effort_str("false"), Some(ReasoningConfig::Disabled));
        assert_eq!(parse_effort_str("disabled"), Some(ReasoningConfig::Disabled));
        // Upstream's disabled-set does NOT include the strings "off"/"no".
        assert_eq!(parse_effort_str("off"), None);
        assert_eq!(parse_effort_str("no"), None);
        assert_eq!(parse_effort_str("bogus"), None);
        assert_eq!(parse_effort_str(""), None);
        // Bools handled as bools.
        assert_eq!(parse_effort(&Value::Bool(false)), Some(ReasoningConfig::Disabled));
        assert_eq!(parse_effort(&Value::Bool(true)), None);
        assert_eq!(parse_effort(&Value::Null), None);
    }

    #[test]
    fn variant_table() {
        // Version-dot symmetry: all three spellings share the canonical form.
        for spelling in ["claude-opus-4.5", "claude-opus-4-5", "claude-opus.4.5"] {
            let v = model_variants(spelling);
            assert!(
                v.contains(&"claude-opus-4.5".to_string()),
                "{spelling} → {v:?} should contain claude-opus-4.5"
            );
            assert!(v.contains(&"claude-opus-4-5".to_string()), "{spelling}");
        }

        // Exact input always first.
        let v = model_variants("anthropic/claude-opus-4.5");
        assert_eq!(v[0], "anthropic/claude-opus-4.5");
        // Bare variant present.
        assert!(v.contains(&"claude-opus-4.5".to_string()));
        // Provider prefixes prepended to bare variants.
        assert!(v.contains(&"openrouter/claude-opus-4.5".to_string()));
        assert!(v.contains(&"deepseek/claude-opus-4.5".to_string()));
        // Aggregator prefixes prepended to single-slash variants.
        assert!(v.contains(&"openrouter/anthropic/claude-opus-4.5".to_string()));

        // Aggregator stripping for 3-part names.
        let v = model_variants("openrouter/anthropic/claude-opus-4.5");
        assert!(v.contains(&"anthropic/claude-opus-4.5".to_string()));
        assert!(v.contains(&"claude-opus-4.5".to_string()));

        // No duplicates.
        let v = model_variants("openai/gpt-5.5");
        let set: std::collections::HashSet<_> = v.iter().collect();
        assert_eq!(set.len(), v.len());
    }

    #[test]
    fn resolves_override_then_global() {
        let cfg: Value = serde_yaml::from_str(
            "agent:\n  reasoning_effort: medium\n  reasoning_overrides:\n    \"anthropic/claude-opus-4.6\": high\n",
        )
        .unwrap();
        assert_eq!(
            resolve(Some(&cfg), "anthropic/claude-opus-4.6"),
            Some(ReasoningConfig::Effort("high".into()))
        );
        // Spelling-tolerant: dashed version matches the dotted override.
        assert_eq!(
            resolve(Some(&cfg), "anthropic/claude-opus-4-6"),
            Some(ReasoningConfig::Effort("high".into()))
        );
        assert_eq!(
            resolve(Some(&cfg), "openai/gpt-5"),
            Some(ReasoningConfig::Effort("medium".into()))
        );
    }

    #[test]
    fn derives_model_from_config_when_empty() {
        let cfg: Value = serde_yaml::from_str(
            "model:\n  default: \"anthropic/claude-opus-4.6\"\nagent:\n  reasoning_overrides:\n    \"claude-opus-4.6\": low\n",
        )
        .unwrap();
        assert_eq!(resolve(Some(&cfg), ""), Some(ReasoningConfig::Effort("low".into())));
    }

    #[test]
    fn global_false_disables() {
        let cfg: Value = serde_yaml::from_str("agent:\n  reasoning_effort: false\n").unwrap();
        assert_eq!(resolve(Some(&cfg), "m"), Some(ReasoningConfig::Disabled));
        // Absent → None (provider default; no reasoning field sent).
        let cfg: Value = serde_yaml::from_str("agent: {}\n").unwrap();
        assert_eq!(resolve(Some(&cfg), "m"), None);
    }
}
