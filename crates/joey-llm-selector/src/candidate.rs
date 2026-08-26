//! `CandidateModel` and catalog consolidation (T006, T008, T009).
//!
//! Typed view of a model in the active provider's catalog, plus the
//! consolidator that normalizes the three scattered JSON sources into it
//! (research.md §6). No unified catalog struct exists today; this is it.

use serde_json::Value;

// ── Public types ───────────────────────────────────────────────────────────

/// One model in the active provider's live catalog, normalized to a typed view.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateModel {
    /// Provider-internal model id, sent verbatim to the API (e.g. "gpt-4.1").
    pub id: String,
    /// Owning provider key (e.g. "copilot", "openrouter").
    pub provider: String,
    /// Highest configurable context window for this model, in tokens.
    pub context_window: u64,
    /// Tool/function-calling support.
    pub supports_tools: bool,
    /// Vision/image support (derived from id-prefix table + provider hints).
    pub supports_vision: bool,
    /// Capability tier, used for the cost tie-break (FR-006).
    pub tier: CapabilityTier,
    /// Billing cost if known. None when unavailable.
    pub cost: Option<Cost>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CapabilityTier {
    /// Small/cheap/fast (e.g. haiku, flash, mini).
    Flash,
    /// Mid-tier.
    Standard,
    /// Strong general-purpose (default diagnoser tier).
    Versatile,
    /// Top-tier / flagship.
    Frontier,
}

impl CapabilityTier {
    /// Numeric weight for cost comparison: lower = cheaper.
    pub fn cost_weight(self) -> u8 {
        match self {
            CapabilityTier::Flash => 0,
            CapabilityTier::Standard => 1,
            CapabilityTier::Versatile => 2,
            CapabilityTier::Frontier => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cost {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

/// The active provider's consolidated candidate pool (FR-003, SC-005).
#[derive(Debug, Clone, Default)]
pub struct CandidateModelPool {
    pub models: Vec<CandidateModel>,
    pub source: CatalogSource,
    pub fetched_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CatalogSource {
    #[default]
    Empty,
    Copilot,
    OpenRouter,
    ModelsDotDev,
    GenericProbe,
}

impl CandidateModelPool {
    /// Number of chat-capable models discovered.
    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// Whether the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// Find a candidate by id.
    pub fn get(&self, id: &str) -> Option<&CandidateModel> {
        self.models.iter().find(|m| m.id == id)
    }

    /// Build a pool from consolidated candidates, stamping `fetched_at` to now
    /// (T074 — data-model.md Entity 4 declares `fetched_at` non-optional; this
    /// is the single construction path used after a catalog fetch, so it is the
    /// right place to set it).
    pub fn from_consolidated(models: Vec<CandidateModel>, source: CatalogSource) -> Self {
        Self {
            models,
            source,
            fetched_at: Some(chrono::Utc::now()),
        }
    }
}

// ── Catalog consolidation ──────────────────────────────────────────────────

/// Consolidate Copilot catalog JSON (`Vec<Value>` from
/// `joey_providers::copilot::fetch_model_catalog`) into typed candidates.
///
/// Chat-type filter `capabilities.type == "chat"` is applied (research.md §6).
/// Context window from `capabilities.limits.max_prompt_tokens` with fallback
/// to the hardcoded table then 8192.
pub fn consolidate_copilot(raw: &[Value]) -> (Vec<CandidateModel>, usize) {
    let mut out = Vec::new();
    let mut dropped = 0usize;
    for entry in raw {
        // Chat-type filter (copilot.rs:576-583).
        let cap_type = entry
            .get("capabilities")
            .and_then(|c| c.get("type"))
            .and_then(|t| t.as_str());
        if cap_type != Some("chat") {
            dropped += 1;
            continue;
        }
        if let Some(m) = parse_copilot_entry(entry) {
            out.push(m);
        } else {
            dropped += 1;
        }
    }
    (out, dropped)
}

fn parse_copilot_entry(entry: &Value) -> Option<CandidateModel> {
    let id = entry.get("id")?.as_str()?.to_string();
    let supports_tools = copilot_supports_tools(entry);
    let supports_vision = copilot_supports_vision(&id, entry);
    let context_window = copilot_context_window(entry, &id);
    let tier = classify_tier(&id);
    Some(CandidateModel {
        id,
        provider: "copilot".to_string(),
        context_window,
        supports_tools,
        supports_vision,
        tier,
        cost: None, // Copilot catalog doesn't expose pricing.
    })
}

fn copilot_context_window(entry: &Value, id: &str) -> u64 {
    // Mirrors copilot.rs catalog_context_window: prefer
    // capabilities.limits.max_context_window_tokens (the full window;
    // max_prompt_tokens is a smaller prompt-only budget), fall back to
    // max_prompt_tokens for older catalogs without the window field.
    let limits = entry
        .get("capabilities")
        .and_then(|c| c.get("limits"));
    let from_cat = limits
        .and_then(|l| l.get("max_context_window_tokens"))
        .and_then(|v| v.as_u64())
        .filter(|v| *v > 0)
        .or_else(|| {
            limits
                .and_then(|l| l.get("max_prompt_tokens"))
                .and_then(|v| v.as_u64())
                .filter(|v| *v > 0)
        });
    from_cat.unwrap_or_else(|| default_context_length(id))
}

fn copilot_supports_tools(entry: &Value) -> bool {
    // copilot.rs infers tool/API mode from supported_endpoints membership.
    if let Some(endpoints) = entry
        .get("capabilities")
        .and_then(|c| c.get("supported_endpoints"))
        .and_then(|e| e.as_array())
    {
        return endpoints.iter().any(|e| {
            e.as_str()
                .map(|s| s.contains("chat/completions") || s.contains("responses"))
                .unwrap_or(false)
        });
    }
    true // permissive default when absent (mirrors OpenRouter policy)
}

fn copilot_supports_vision(id: &str, entry: &Value) -> bool {
    // Provider hint overrides table.
    if let Some(true) = entry
        .get("capabilities")
        .and_then(|c| c.get("supports"))
        .and_then(|s| s.get("vision"))
        .and_then(|v| v.as_bool())
    {
        return true;
    }
    supports_vision_by_id(id)
}

// ── OpenRouter consolidation ───────────────────────────────────────────────

/// Consolidate OpenRouter model-list JSON into typed candidates.
pub fn consolidate_openrouter(raw: &[Value]) -> (Vec<CandidateModel>, usize) {
    let mut out = Vec::new();
    let mut dropped = 0usize;
    for entry in raw {
        if let Some(m) = parse_openrouter_entry(entry) {
            out.push(m);
        } else {
            dropped += 1;
        }
    }
    (out, dropped)
}

fn parse_openrouter_entry(entry: &Value) -> Option<CandidateModel> {
    let id = entry.get("id")?.as_str()?.to_string();

    // tool support from supported_parameters containing "tools"
    let supports_tools = entry
        .get("supported_parameters")
        .and_then(|p| p.as_array())
        .map(|arr| arr.iter().any(|v| v.as_str() == Some("tools")))
        .unwrap_or(true); // permissive when absent

    // vision from architecture.input_modalities containing "image"
    let supports_vision = entry
        .get("architecture")
        .and_then(|a| a.get("input_modalities"))
        .and_then(|m| m.as_array())
        .map(|arr| arr.iter().any(|v| v.as_str() == Some("image")))
        .unwrap_or_else(|| supports_vision_by_id(&id));

    let context_window = entry
        .get("context_length")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| default_context_length(&id));

    let cost = parse_openrouter_cost(entry);

    let tier = classify_tier(&id);
    Some(CandidateModel {
        id,
        provider: "openrouter".to_string(),
        context_window,
        supports_tools,
        supports_vision,
        tier,
        cost,
    })
}

fn parse_openrouter_cost(entry: &Value) -> Option<Cost> {
    let pricing = entry.get("pricing")?;
    let input = pricing.get("prompt")?.as_str()?.parse::<f64>().ok()?;
    let output = pricing.get("completion")?.as_str()?.parse::<f64>().ok()?;
    // OpenRouter pricing is per-token; convert to per-million-tokens.
    if input == 0.0 && output == 0.0 {
        return None;
    }
    Some(Cost {
        input_per_mtok: input * 1_000_000.0,
        output_per_mtok: output * 1_000_000.0,
    })
}

// ── models.dev consolidation ───────────────────────────────────────────────

/// Consolidate a models.dev provider entry list into typed candidates.
pub fn consolidate_models_dev(provider: &str, raw: &[Value]) -> (Vec<CandidateModel>, usize) {
    let mut out = Vec::new();
    let mut dropped = 0usize;
    for entry in raw {
        if let Some(m) = parse_models_dev_entry(provider, entry) {
            out.push(m);
        } else {
            dropped += 1;
        }
    }
    (out, dropped)
}

fn parse_models_dev_entry(provider: &str, entry: &Value) -> Option<CandidateModel> {
    let id = entry.get("id")?.as_str()?.to_string();
    let supports_tools = entry.get("tool_call").and_then(|v| v.as_bool());
    let supports_vision = supports_vision_by_id(&id);
    let context_window = entry
        .get("limit")
        .and_then(|l| l.get("context"))
        .and_then(|v| v.as_i64())
        .map(|i| i.max(0) as u64)
        .unwrap_or_else(|| default_context_length(&id));
    let cost = parse_models_dev_cost(entry);
    let tier = classify_tier(&id);
    Some(CandidateModel {
        id,
        provider: provider.to_string(),
        context_window,
        supports_tools: supports_tools.unwrap_or(true),
        supports_vision,
        tier,
        cost,
    })
}

fn parse_models_dev_cost(entry: &Value) -> Option<Cost> {
    let cost = entry.get("cost")?;
    let input = cost.get("input")?.as_f64()?;
    let output = cost.get("output")?.as_f64()?;
    if input == 0.0 && output == 0.0 {
        return None;
    }
    Some(Cost {
        input_per_mtok: input,
        output_per_mtok: output,
    })
}

// ── Generic probe (OpenAI-compat /models) ──────────────────────────────────

/// Consolidate a generic OpenAI-compat `/models` probe response into typed
/// candidates. Only id is reliable; capabilities inferred from the id table.
pub fn consolidate_generic_probe(provider: &str, raw: &[Value]) -> (Vec<CandidateModel>, usize) {
    let mut out = Vec::new();
    for entry in raw {
        if let Some(id_str) = entry.get("id").and_then(|i| i.as_str()) {
            let id = id_str.to_string();
            out.push(CandidateModel {
                id: id.clone(),
                provider: provider.to_string(),
                context_window: default_context_length(&id),
                supports_tools: true, // conservative default for agentic models
                supports_vision: supports_vision_by_id(&id),
                tier: classify_tier(&id),
                cost: None,
            });
        }
    }
    (out, 0) // dropped count not meaningful for generic
}

// ── Capability derivation helpers ──────────────────────────────────────────

/// Curated id-prefix table for vision support (additive; research.md §6).
const VISION_PREFIXES: &[&str] = &[
    "gpt-4o",
    "gpt-4.1",
    "gpt-4.5",
    "gpt-5",
    "claude-3-7",
    "claude-3-5",
    "claude-sonnet-4",
    "claude-opus-4",
    "claude-haiku-4",
    "gemini-",
    "grok-vision",
    "grok-2-vision",
    "qwen2.5-vl",
    "qwen2-vl",
    "llama-3.2-",
];

/// Whether a model supports vision, derived from its id via the prefix table.
pub fn supports_vision_by_id(id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    VISION_PREFIXES.iter().any(|p| lower.starts_with(p))
}

/// Context-length family table (mirrors the longest-key-first substring match
/// in `compression/catalog.rs:83 DEFAULT_CONTEXT_LENGTHS`). Kept conservative
/// and small — the real source-of-truth is the catalog; this is the fallback.
fn default_context_length(id: &str) -> u64 {
    let lower = id.to_ascii_lowercase();
    // Order: longest-key-first substring match (mirrors upstream).
    if lower.contains("1m") || lower.contains("2m") {
        return 1_000_000;
    }
    if lower.contains("gemini-1.5-pro") {
        return 2_097_152;
    }
    if lower.starts_with("gpt-4") {
        return 128_000;
    }
    if lower.starts_with("claude") {
        return 200_000;
    }
    if lower.starts_with("gemini") {
        return 1_048_576;
    }
    if lower.starts_with("grok") {
        return 128_000;
    }
    8_192 // conservative default
}

/// Classify a model into a capability tier from its id (research.md §6).
fn classify_tier(id: &str) -> CapabilityTier {
    let l = id.to_ascii_lowercase();

    // Frontier: top per-vendor flagships.
    let frontier = ["gpt-5", "claude-opus-4", "gemini-2.5-pro", "grok-4", "o3", "o4"];
    if frontier.iter().any(|p| l.starts_with(p)) {
        return CapabilityTier::Frontier;
    }

    // Flash: cheap/fast suffixes.
    let flash = ["haiku", "flash", "mini", "nano", "micro"];
    if flash.iter().any(|p| l.contains(p)) {
        return CapabilityTier::Flash;
    }

    // Versatile: strong general-purpose.
    let versatile = [
        "gpt-4.1",
        "gpt-4o",
        "gpt-4.5",
        "claude-sonnet-4",
        "claude-3-7",
        "gemini-2.5",
        "grok-3",
        "deepseek-v3",
        "glm-4.6",
        "glm-5",
    ];
    if versatile.iter().any(|p| l.starts_with(p)) {
        return CapabilityTier::Versatile;
    }

    CapabilityTier::Standard
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vision_prefix_table() {
        assert!(supports_vision_by_id("gpt-4o-2024-08-06"));
        assert!(supports_vision_by_id("GPT-4O"));
        assert!(supports_vision_by_id("claude-sonnet-4-20250514"));
        assert!(supports_vision_by_id("gemini-2.5-flash"));
        assert!(!supports_vision_by_id("text-embedding-3"));
        assert!(!supports_vision_by_id("o1-mini"));
    }

    #[test]
    fn test_classify_tier() {
        assert_eq!(classify_tier("gpt-5"), CapabilityTier::Frontier);
        assert_eq!(classify_tier("claude-opus-4-1"), CapabilityTier::Frontier);
        assert_eq!(
            classify_tier("claude-haiku-4-5"),
            CapabilityTier::Flash
        );
        assert_eq!(classify_tier("gemini-2.5-flash"), CapabilityTier::Flash);
        assert_eq!(classify_tier("gpt-4.1"), CapabilityTier::Versatile);
        assert_eq!(
            classify_tier("some-unknown-model"),
            CapabilityTier::Standard
        );
    }

    #[test]
    fn test_default_context_length() {
        assert_eq!(default_context_length("gpt-4o"), 128_000);
        assert_eq!(default_context_length("claude-sonnet-4"), 200_000);
        assert_eq!(default_context_length("random-model"), 8_192);
    }

    #[test]
    fn test_consolidate_copilot_chat_filter() {
        let raw = serde_json::json!([
            {"id": "m1", "capabilities": {"type": "chat", "limits": {"max_prompt_tokens": 64000}, "supported_endpoints": ["chat/completions"]}},
            {"id": "m2", "capabilities": {"type": "embedding", "limits": {"max_prompt_tokens": 8192}}},
        ]);
        let arr = raw.as_array().unwrap();
        let (models, dropped) = consolidate_copilot(arr);
        assert_eq!(models.len(), 1);
        assert_eq!(dropped, 1);
        assert_eq!(models[0].id, "m1");
        assert_eq!(models[0].context_window, 64000);
        assert!(models[0].supports_tools);
    }

    #[test]
    fn test_copilot_context_window_prefers_full_window() {
        let raw = serde_json::json!([
            {"id": "m1", "capabilities": {"type": "chat", "limits": {"max_context_window_tokens": 264000, "max_prompt_tokens": 200000}, "supported_endpoints": ["chat/completions"]}},
            {"id": "m2", "capabilities": {"type": "chat", "limits": {"max_prompt_tokens": 128000}, "supported_endpoints": ["chat/completions"]}},
            {"id": "m3", "capabilities": {"type": "chat", "limits": {"max_context_window_tokens": 0, "max_prompt_tokens": 64000}, "supported_endpoints": ["chat/completions"]}},
        ]);
        let arr = raw.as_array().unwrap();
        let (models, _) = consolidate_copilot(arr);
        assert_eq!(models.len(), 3);
        // Full window wins over the prompt-only budget.
        assert_eq!(models[0].context_window, 264_000);
        // Fallback when the window field is absent.
        assert_eq!(models[1].context_window, 128_000);
        // Zero/invalid window falls back to max_prompt_tokens.
        assert_eq!(models[2].context_window, 64_000);
    }

    #[test]
    fn test_consolidate_openrouter() {
        let raw = serde_json::json!([{
            "id": "gpt-4o",
            "supported_parameters": ["tools"],
            "architecture": {"input_modalities": ["text", "image"]},
            "context_length": 128000,
            "pricing": {"prompt": "0.000005", "completion": "0.000015"}
        }]);
        let (models, _) = consolidate_openrouter(raw.as_array().unwrap());
        assert_eq!(models.len(), 1);
        let m = &models[0];
        assert!(m.supports_tools);
        assert!(m.supports_vision);
        assert_eq!(m.context_window, 128_000);
        let cost = m.cost.expect("cost");
        assert!((cost.input_per_mtok - 5.0).abs() < 0.01);
    }

    #[test]
    fn test_consolidate_openrouter_free_tier_no_cost() {
        let raw = serde_json::json!([{
            "id": "free-model",
            "pricing": {"prompt": "0", "completion": "0"}
        }]);
        let (models, _) = consolidate_openrouter(raw.as_array().unwrap());
        assert!(models[0].cost.is_none());
    }

    #[test]
    fn test_pool_coverage_sc005() {
        // SC-005: every chat-capable model appears; none silently dropped.
        let raw = serde_json::json!([
            {"id": "a", "capabilities": {"type": "chat"}},
            {"id": "b", "capabilities": {"type": "chat"}},
            {"id": "c", "capabilities": {"type": "chat"}},
        ]);
        let arr = raw.as_array().unwrap();
        let (models, dropped) = consolidate_copilot(arr);
        let pool = CandidateModelPool {
            models,
            source: CatalogSource::Copilot,
            fetched_at: None,
        };
        // SC-005: all chat-capable entries are present.
        assert_eq!(pool.len(), 3);
        assert_eq!(dropped, 0);
        assert!(pool.get("a").is_some());
        assert!(pool.get("b").is_some());
        assert!(pool.get("c").is_some());
        assert!(pool.get("missing").is_none());
    }
}
