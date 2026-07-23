//! Model catalog for the setup wizard (port of the wizard-facing surface of
//! `hermes_cli/models.py`, `agent/models_dev.py`, and
//! `hermes_cli/model_cost_guard.py`).
//!
//! Pieces: curated per-provider model lists (`_PROVIDER_MODELS`), the curated
//! OpenRouter picker list with live tools/free filtering, the models.dev
//! registry (in-memory + disk cache hierarchy), the generic `/models`
//! endpoint probe, OpenRouter-style pricing fetch, and the expensive-model
//! guard.
//!
//! Port notes: the remote catalog manifest (hermes-website
//! `model-catalog.json`) is upstream infrastructure — the in-repo curated
//! snapshots below are the fallbacks upstream uses when it is unreachable.
//! The `usage_pricing` fallback inside the cost guard is unported (models.dev
//! is the pricing source here).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::Lazy;
use serde_json::{json, Value};

/// Curated OpenRouter picker list: `(model_id, description)` shown in menus
/// (models.py `OPENROUTER_MODELS`, verbatim).
pub const OPENROUTER_MODELS: &[(&str, &str)] = &[
    // Anthropic
    ("anthropic/claude-fable-5", ""),
    ("anthropic/claude-opus-4.8", ""),
    ("anthropic/claude-opus-4.8-fast", "2x price, higher output speed"),
    ("anthropic/claude-sonnet-5", ""),
    ("anthropic/claude-haiku-4.5", ""),
    // OpenAI
    ("openai/gpt-5.6-sol", ""),
    ("openai/gpt-5.6-sol-pro", ""),
    ("openai/gpt-5.6-terra", ""),
    ("openai/gpt-5.6-terra-pro", ""),
    ("openai/gpt-5.6-luna", ""),
    ("openai/gpt-5.6-luna-pro", ""),
    ("openai/gpt-5.5", ""),
    ("openai/gpt-5.5-pro", ""),
    ("openai/gpt-5.4-mini", ""),
    // Google
    ("google/gemini-3-pro-preview", ""),
    ("google/gemini-3.1-pro-preview", ""),
    ("google/gemini-3.5-flash", ""),
    // xAI
    ("x-ai/grok-4.5", ""),
    // DeepSeek
    ("deepseek/deepseek-v4-pro", ""),
    ("deepseek/deepseek-v4-flash", ""),
    // Qwen
    ("qwen/qwen3.7-max", ""),
    ("qwen/qwen3.7-plus", ""),
    ("qwen/qwen3.6-35b-a3b", ""),
    // MoonshotAI
    ("moonshotai/kimi-k3", "recommended"),
    // MiniMax
    ("minimax/minimax-m3", ""),
    // Z-AI
    ("z-ai/glm-5.2", "default"),
    ("z-ai/glm-5.1", ""),
    // Xiaomi
    ("xiaomi/mimo-v2.5-pro", ""),
    // Tencent
    ("tencent/hy3", ""),
    // StepFun
    ("stepfun/step-3.7-flash", ""),
    // NVIDIA
    ("nvidia/nemotron-3-super-120b-a12b", ""),
    // Sakana
    ("sakana/fugu-ultra", ""),
    // OpenRouter routers
    (
        "openrouter/pareto-code",
        "auto-routes to cheapest coder meeting openrouter.min_coding_score",
    ),
    // Free tier
    ("openrouter/elephant-alpha", "free"),
    ("poolside/laguna-m.1:free", "free"),
    ("tencent/hy3:free", "free"),
    ("nvidia/nemotron-3-super-120b-a12b:free", "free"),
    ("nvidia/nemotron-3-ultra-550b-a55b:free", "free"),
    ("inclusionai/ring-2.6-1t:free", "free"),
];

/// The silent-default model badge id (models.py
/// `PREFERRED_SILENT_DEFAULT_MODEL`).
pub const PREFERRED_SILENT_DEFAULT_MODEL: &str = "z-ai/glm-5.2";

/// Static xAI fallback when the models.dev disk cache is empty
/// (models.py `_XAI_STATIC_FALLBACK`).
const XAI_STATIC_FALLBACK: &[&str] = &[
    "grok-build-0.1",
    "grok-4.5",
    "grok-4.3",
    "grok-4.20-0309-reasoning",
    "grok-4.20-0309-non-reasoning",
    "grok-4.20-multi-agent-0309",
];

/// Callable but omitted from models.dev / `/v1/models` listings
/// (models.py `_XAI_CURATED_EXTRAS`).
const XAI_CURATED_EXTRAS: &[&str] = &["grok-4.5", "grok-composer-2.5-fast"];

const XAI_TOP_MODEL: &str = "grok-build-0.1";

/// Curated static fallback lists shown in the picker when live discovery
/// fails (models.py `_PROVIDER_MODELS`, restricted to the ported providers).
pub fn provider_models(provider: &str) -> Vec<String> {
    let list: &[&str] = match provider {
        "nous" => &[
            // Anthropic
            "anthropic/claude-fable-5",
            "anthropic/claude-opus-4.8",
            "anthropic/claude-sonnet-5",
            "anthropic/claude-haiku-4.5",
            // OpenAI
            "openai/gpt-5.6-sol",
            "openai/gpt-5.6-sol-pro",
            "openai/gpt-5.6-terra",
            "openai/gpt-5.6-terra-pro",
            "openai/gpt-5.6-luna",
            "openai/gpt-5.6-luna-pro",
            "openai/gpt-5.5",
            "openai/gpt-5.5-pro",
            "openai/gpt-5.4-mini",
            // Google
            "google/gemini-3-pro-preview",
            "google/gemini-3.1-pro-preview",
            "google/gemini-3.5-flash",
            // xAI
            "x-ai/grok-4.5",
            // DeepSeek
            "deepseek/deepseek-v4-pro",
            "deepseek/deepseek-v4-flash",
            // Qwen
            "qwen/qwen3.7-max",
            "qwen/qwen3.7-plus",
            "qwen/qwen3.6-35b-a3b",
            // MoonshotAI
            "moonshotai/kimi-k3",
            // MiniMax
            "minimax/minimax-m3",
            // Z-AI
            "z-ai/glm-5.2",
            "z-ai/glm-5.1",
            // Xiaomi
            "xiaomi/mimo-v2.5-pro",
            // Tencent
            "tencent/hy3",
            // StepFun
            "stepfun/step-3.7-flash",
            // NVIDIA
            "nvidia/nemotron-3-super-120b-a12b",
            // Sakana
            "sakana/fugu-ultra",
        ],
        "openai-api" => &[
            "gpt-5.6-sol",
            "gpt-5.6-sol-pro",
            "gpt-5.6-terra",
            "gpt-5.6-terra-pro",
            "gpt-5.6-luna",
            "gpt-5.6-luna-pro",
            "gpt-5.5",
            "gpt-5.5-pro",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.4-nano",
            "gpt-5-mini",
            "gpt-5.3-codex",
            "gpt-4.1",
            "gpt-4o",
            "gpt-4o-mini",
        ],
        "anthropic" => &[
            "claude-fable-5",
            "claude-sonnet-5",
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-opus-4-5-20251101",
            "claude-sonnet-4-5-20250929",
            "claude-opus-4-20250514",
            "claude-sonnet-4-20250514",
            "claude-haiku-4-5-20251001",
        ],
        "gemini" => &[
            "gemini-3.1-pro-preview",
            "gemini-3-pro-preview",
            "gemini-3.5-flash",
            "gemini-3.1-flash-lite-preview",
        ],
        "zai" => &[
            "glm-5.2",
            "glm-5.1",
            "glm-5",
            "glm-5v-turbo",
            "glm-5-turbo",
            "glm-4.7",
            "glm-4.5",
            "glm-4.5-flash",
        ],
        "deepseek" => &[
            "deepseek-v4-pro",
            "deepseek-v4-flash",
            "deepseek-chat",
            "deepseek-reasoner",
        ],
        "xai" => return xai_curated_models(),
        "copilot" => return joey_providers::copilot::fallback_models(),
        _ => &[],
    };
    list.iter().map(|s| s.to_string()).collect()
}

/// Pin the headline xAI model to the top (models.py `_xai_promote_top`).
fn xai_promote_top(ids: Vec<String>) -> Vec<String> {
    if ids.iter().any(|m| m == XAI_TOP_MODEL) {
        let mut out = vec![XAI_TOP_MODEL.to_string()];
        out.extend(ids.into_iter().filter(|m| m != XAI_TOP_MODEL));
        out
    } else {
        ids
    }
}

/// Append curated xAI models missing from models.dev
/// (models.py `_xai_merge_curated_extras`).
fn xai_merge_curated_extras(ids: Vec<String>) -> Vec<String> {
    let mut out = ids;
    for extra in XAI_CURATED_EXTRAS {
        if out.iter().any(|m| m == extra) {
            continue;
        }
        let insert_at = if out.first().map(|m| m == XAI_TOP_MODEL).unwrap_or(false) {
            1
        } else {
            out.len()
        };
        out.insert(insert_at, extra.to_string());
    }
    out
}

/// Derive the xAI-direct curated list from the models.dev disk cache
/// (models.py `_xai_curated_models`) — no network; static fallback otherwise.
fn xai_curated_models() -> Vec<String> {
    if let Some(data) = load_models_dev_disk_cache() {
        if let Some(models) = data
            .get("xai")
            .and_then(|p| p.get("models"))
            .and_then(Value::as_object)
        {
            if !models.is_empty() {
                let mut ids: Vec<String> = models.keys().cloned().collect();
                ids.sort();
                return xai_merge_curated_extras(xai_promote_top(ids));
            }
        }
    }
    xai_merge_curated_extras(XAI_STATIC_FALLBACK.iter().map(|s| s.to_string()).collect())
}

// ---------------------------------------------------------------------------
// models.dev registry (port of agent/models_dev.py)
// ---------------------------------------------------------------------------

pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const MODELS_DEV_CACHE_TTL: Duration = Duration::from_secs(3600);

/// In-memory cache: (data, anchored-instant) — the instant is back-dated by
/// the disk cache's age so a stale file doesn't gain a fresh hour.
static MODELS_DEV_CACHE: Lazy<Mutex<Option<(Value, Instant)>>> = Lazy::new(|| Mutex::new(None));

/// Hermes→models.dev provider-id mapping (models_dev.py
/// `PROVIDER_TO_MODELS_DEV`, ported providers only; nous has no entry).
fn models_dev_provider_id(provider: &str) -> Option<&'static str> {
    match provider {
        "openrouter" => Some("openrouter"),
        "anthropic" => Some("anthropic"),
        "openai" | "openai-api" => Some("openai"),
        "zai" => Some("zai"),
        "deepseek" => Some("deepseek"),
        "gemini" | "google" => Some("google"),
        "xai" => Some("xai"),
        _ => None,
    }
}

fn models_dev_cache_path() -> PathBuf {
    joey_core::constants::joey_home().join("models_dev_cache.json")
}

fn load_models_dev_disk_cache() -> Option<Value> {
    let raw = std::fs::read_to_string(models_dev_cache_path()).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    v.is_object().then_some(v)
}

fn models_dev_disk_cache_age() -> Option<Duration> {
    let meta = std::fs::metadata(models_dev_cache_path()).ok()?;
    let mtime = meta.modified().ok()?;
    // Future mtime (clock skew) → unknown freshness (models_dev.py:219-224).
    std::time::SystemTime::now().duration_since(mtime).ok()
}

fn save_models_dev_disk_cache(data: &Value) {
    let path = models_dev_cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = joey_core::utils::atomic_json_write(&path, data);
}

/// Fetch the models.dev registry with the upstream cache hierarchy
/// (models_dev.py `fetch_models_dev`): in-mem → fresh disk → network →
/// stale-disk fallback with a 5-minute retry grace. Returns an empty object
/// on total failure.
pub fn fetch_models_dev(force_refresh: bool) -> Value {
    let mut guard = MODELS_DEV_CACHE.lock().unwrap_or_else(|e| e.into_inner());

    // Stage 1: fresh in-memory cache.
    if !force_refresh {
        if let Some((data, at)) = guard.as_ref() {
            if at.elapsed() < MODELS_DEV_CACHE_TTL {
                return data.clone();
            }
        }
        // Stage 2: fresh-by-mtime disk cache short-circuits the network.
        if let Some(age) = models_dev_disk_cache_age() {
            if age < MODELS_DEV_CACHE_TTL {
                if let Some(data) = load_models_dev_disk_cache() {
                    // Anchor the in-mem TTL to the file's age (checked: on
                    // macOS Instant is since-boot and can't go negative).
                    let anchored = Instant::now().checked_sub(age).unwrap_or_else(Instant::now);
                    *guard = Some((data.clone(), anchored));
                    return data;
                }
            }
        }
    }

    // Stage 3: network fetch.
    if let Some(data) = http_get_json(MODELS_DEV_URL, &[], 15.0) {
        if data.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
            save_models_dev_disk_cache(&data);
            *guard = Some((data.clone(), Instant::now()));
            return data;
        }
    }

    // Stage 4: network failed — fall back to any disk cache, even stale,
    // with a short retry grace instead of a full fresh hour.
    if guard.is_none() {
        if let Some(data) = load_models_dev_disk_cache() {
            let anchored = Instant::now()
                .checked_sub(MODELS_DEV_CACHE_TTL - Duration::from_secs(300))
                .unwrap_or_else(Instant::now);
            *guard = Some((data.clone(), anchored));
            return data;
        }
    }
    guard
        .as_ref()
        .map(|(d, _)| d.clone())
        .unwrap_or_else(|| json!({}))
}

/// Google catalog entries hidden from setup/model selection
/// (models_dev.py `_GOOGLE_HIDDEN_MODELS`).
const GOOGLE_HIDDEN_MODELS: &[&str] = &[
    "gemma-4-31b-it",
    "gemma-4-26b-it",
    "gemma-4-26b-a4b-it",
    "gemma-3-1b",
    "gemma-3-1b-it",
    "gemma-3-2b",
    "gemma-3-2b-it",
    "gemma-3-4b",
    "gemma-3-4b-it",
    "gemma-3-12b",
    "gemma-3-12b-it",
    "gemma-3-27b",
    "gemma-3-27b-it",
    "gemini-1.5-flash",
    "gemini-1.5-pro",
    "gemini-1.5-flash-8b",
    "gemini-2.0-flash",
    "gemini-2.0-flash-lite",
];

/// Noise patterns: TTS, embedding, dated preview snapshots, live/streaming,
/// image-only (models_dev.py `_NOISE_PATTERNS`).
static NOISE_PATTERNS: Lazy<regex::Regex> = Lazy::new(|| {
    regex::RegexBuilder::new(
        r"-tts\b|embedding|live-|-(preview|exp)-\d{2,4}[-_]|-image\b|-image-preview\b|-customtools\b",
    )
    .case_insensitive(true)
    .build()
    .expect("noise pattern must compile")
});

fn hidden_from_provider_catalog(provider: &str, model_id: &str) -> bool {
    let p = provider.trim().to_lowercase();
    let m = model_id.trim().to_lowercase();
    (p == "gemini" || p == "google") && GOOGLE_HIDDEN_MODELS.contains(&m.as_str())
}

/// Model IDs suitable for agentic use from models.dev (models_dev.py
/// `list_agentic_models`): tool_call=true minus noise. Empty on any failure.
pub fn list_agentic_models(provider: &str) -> Vec<String> {
    let Some(mdev_id) = models_dev_provider_id(provider) else {
        return Vec::new();
    };
    let data = fetch_models_dev(false);
    let Some(models) = data
        .get(mdev_id)
        .and_then(|p| p.get("models"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for (mid, entry) in models {
        if !entry.is_object() {
            continue;
        }
        if hidden_from_provider_catalog(provider, mid) {
            continue;
        }
        if !entry.get("tool_call").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        if NOISE_PATTERNS.is_match(mid) {
            continue;
        }
        result.push(mid.clone());
    }
    result
}

/// Per-million cost pair from models.dev for the expensive-model guard
/// (models_dev.py `get_model_info` cost fields, exact + case-insensitive
/// model match).
fn models_dev_cost(provider: &str, model: &str) -> Option<(f64, f64)> {
    let mdev_id = models_dev_provider_id(provider)?;
    let data = fetch_models_dev(false);
    let models = data.get(mdev_id)?.get("models")?.as_object()?;
    let entry = models.get(model).or_else(|| {
        let lower = model.to_lowercase();
        models
            .iter()
            .find(|(mid, _)| mid.to_lowercase() == lower)
            .map(|(_, v)| v)
    })?;
    let cost = entry.get("cost")?.as_object()?;
    let input = cost.get("input").and_then(Value::as_f64).unwrap_or(0.0);
    let output = cost.get("output").and_then(Value::as_f64).unwrap_or(0.0);
    (input > 0.0 || output > 0.0).then_some((input, output))
}

// ---------------------------------------------------------------------------
// Generic /models endpoint probe (models.py `probe_api_models`)
// ---------------------------------------------------------------------------

/// Result of probing a `/models` endpoint.
#[derive(Debug, Clone, Default)]
pub struct ModelsProbe {
    pub models: Option<Vec<String>>,
    pub probed_url: Option<String>,
    pub resolved_base_url: String,
    pub suggested_base_url: Option<String>,
    pub used_fallback: bool,
}

/// Probe a `/models` endpoint with light URL heuristics: try the URL as
/// given, then with `/v1` toggled. Anthropic-mode endpoints authenticate via
/// `x-api-key` + `anthropic-version` instead of Bearer.
pub fn probe_api_models(api_key: &str, base_url: &str, api_mode: &str) -> ModelsProbe {
    let normalized = base_url.trim().trim_end_matches('/').to_string();
    if normalized.is_empty() {
        return ModelsProbe::default();
    }
    let alternate = if normalized.ends_with("/v1") {
        normalized[..normalized.len() - 3].trim_end_matches('/').to_string()
    } else {
        format!("{}/v1", normalized)
    };
    let mut candidates = vec![(normalized.clone(), false)];
    if !alternate.is_empty() && alternate != normalized {
        candidates.push((alternate.clone(), true));
    }

    let mut headers: Vec<(String, String)> = Vec::new();
    if !api_key.is_empty() && api_mode == "anthropic_messages" {
        headers.push(("x-api-key".into(), api_key.to_string()));
        headers.push(("anthropic-version".into(), "2023-06-01".into()));
    } else if !api_key.is_empty() {
        headers.push(("Authorization".into(), format!("Bearer {}", api_key)));
    }

    let mut first_url = None;
    for (candidate, is_fallback) in &candidates {
        let url = format!("{}/models", candidate.trim_end_matches('/'));
        if first_url.is_none() {
            first_url = Some(url.clone());
        }
        let hdrs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        if let Some(data) = http_get_json(&url, &hdrs, 5.0) {
            let ids = data
                .get("data")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|m| m.get("id").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            return ModelsProbe {
                models: Some(ids),
                probed_url: Some(url),
                resolved_base_url: candidate.trim_end_matches('/').to_string(),
                suggested_base_url: if *candidate != alternate {
                    Some(alternate.clone())
                } else {
                    Some(normalized.clone())
                },
                used_fallback: *is_fallback,
            };
        }
    }

    ModelsProbe {
        models: None,
        probed_url: first_url,
        resolved_base_url: normalized.clone(),
        suggested_base_url: (alternate != normalized).then_some(alternate),
        used_fallback: false,
    }
}

/// Just the model-id list from [`probe_api_models`] (models.py
/// `fetch_api_models`).
pub fn fetch_api_models(api_key: &str, base_url: &str) -> Option<Vec<String>> {
    probe_api_models(api_key, base_url, "").models
}

// ---------------------------------------------------------------------------
// OpenRouter picker catalog (models.py `fetch_openrouter_models`)
// ---------------------------------------------------------------------------

static OPENROUTER_CATALOG_CACHE: Lazy<Mutex<Option<Vec<(String, String)>>>> =
    Lazy::new(|| Mutex::new(None));

/// True when the model's `supported_parameters` advertise tool calling —
/// permissive when the field is absent (models.py
/// `_openrouter_model_supports_tools`, ported from Kilo-Org/kilocode#9068).
fn openrouter_model_supports_tools(item: &Value) -> bool {
    let Some(params) = item.get("supported_parameters") else {
        return true;
    };
    let Some(list) = params.as_array() else {
        return true; // absent / malformed → be permissive
    };
    list.iter().any(|p| p.as_str() == Some("tools"))
}

/// True when both prompt and completion pricing are zero
/// (models.py `_openrouter_model_is_free`).
fn openrouter_model_is_free(pricing: Option<&Value>) -> bool {
    let Some(p) = pricing.and_then(Value::as_object) else {
        return false;
    };
    let num = |k: &str| {
        p.get(k)
            .map(|v| match v {
                Value::String(s) => s.parse::<f64>().ok(),
                other => other.as_f64(),
            })
            .unwrap_or(Some(0.0))
    };
    matches!((num("prompt"), num("completion")), (Some(a), Some(b)) if a == 0.0 && b == 0.0)
}

/// The curated OpenRouter picker list, refreshed from the live catalog when
/// possible (models.py `fetch_openrouter_models`): curated order, restricted
/// to live models that advertise tool support, with free/default badges.
pub fn fetch_openrouter_models(force_refresh: bool) -> Vec<(String, String)> {
    let mut guard = OPENROUTER_CATALOG_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cached) = guard.as_ref() {
        if !force_refresh {
            return cached.clone();
        }
    }
    let fallback: Vec<(String, String)> = OPENROUTER_MODELS
        .iter()
        .map(|(m, d)| (m.to_string(), d.to_string()))
        .collect();

    let Some(payload) = http_get_json("https://openrouter.ai/api/v1/models", &[], 8.0) else {
        return guard.clone().unwrap_or(fallback);
    };
    let Some(live_items) = payload.get("data").and_then(Value::as_array) else {
        return guard.clone().unwrap_or(fallback);
    };
    let mut live_by_id: HashMap<&str, &Value> = HashMap::new();
    for item in live_items {
        if let Some(mid) = item.get("id").and_then(Value::as_str) {
            if !mid.is_empty() {
                live_by_id.insert(mid, item);
            }
        }
    }

    let mut curated: Vec<(String, String)> = Vec::new();
    for (preferred_id, _) in &fallback {
        let Some(live_item) = live_by_id.get(preferred_id.as_str()) else {
            continue;
        };
        if !openrouter_model_supports_tools(live_item) {
            continue;
        }
        let desc = if preferred_id == PREFERRED_SILENT_DEFAULT_MODEL {
            "default".to_string()
        } else if openrouter_model_is_free(live_item.get("pricing")) {
            "free".to_string()
        } else {
            String::new()
        };
        curated.push((preferred_id.clone(), desc));
    }
    if curated.is_empty() {
        return guard.clone().unwrap_or(fallback);
    }
    if curated[0].1.is_empty() {
        curated[0].1 = "recommended".to_string();
    }
    *guard = Some(curated.clone());
    curated
}

/// Just the OpenRouter model-id strings (models.py `model_ids`).
pub fn openrouter_model_ids(force_refresh: bool) -> Vec<String> {
    fetch_openrouter_models(force_refresh)
        .into_iter()
        .map(|(mid, _)| mid)
        .collect()
}

// ---------------------------------------------------------------------------
// Pricing (models.py `fetch_models_with_pricing` / `get_pricing_for_provider`)
// ---------------------------------------------------------------------------

/// Per-model pricing strings as served by OpenRouter-compatible `/v1/models`.
#[derive(Debug, Clone, Default)]
pub struct ModelPricing {
    pub prompt: String,
    pub completion: String,
    pub input_cache_read: Option<String>,
}

static PRICING_CACHE: Lazy<Mutex<HashMap<String, HashMap<String, ModelPricing>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Fetch `{base_url}/v1/models` pricing, cached per base URL
/// (models.py `fetch_models_with_pricing`). Empty map on failure.
pub fn fetch_models_with_pricing(api_key: &str, base_url: &str) -> HashMap<String, ModelPricing> {
    let cache_key = base_url.trim_end_matches('/').to_string();
    {
        let cache = PRICING_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = cache.get(&cache_key) {
            return hit.clone();
        }
    }
    let url = format!("{}/v1/models", cache_key);
    let auth = format!("Bearer {}", api_key);
    let mut headers: Vec<(&str, &str)> = vec![("Accept", "application/json")];
    if !api_key.is_empty() {
        headers.push(("Authorization", &auth));
    }
    let mut result = HashMap::new();
    if let Some(payload) = http_get_json(&url, &headers, 8.0) {
        if let Some(items) = payload.get("data").and_then(Value::as_array) {
            for item in items {
                let Some(mid) = item.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let Some(pricing) = item.get("pricing").and_then(Value::as_object) else {
                    continue;
                };
                let s = |k: &str| {
                    pricing
                        .get(k)
                        .map(|v| match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .unwrap_or_default()
                };
                let cache_read = s("input_cache_read");
                result.insert(
                    mid.to_string(),
                    ModelPricing {
                        prompt: s("prompt"),
                        completion: s("completion"),
                        input_cache_read: (!cache_read.is_empty() && cache_read != "null")
                            .then_some(cache_read),
                    },
                );
            }
        }
    }
    PRICING_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(cache_key, result.clone());
    result
}

/// Live pricing for providers that support it (models.py
/// `get_pricing_for_provider`): openrouter and nous here; everyone else
/// (including zai) gets an empty map.
pub fn get_pricing_for_provider(provider: &str) -> HashMap<String, ModelPricing> {
    match provider {
        "openrouter" => {
            let key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
            fetch_models_with_pricing(key.trim(), "https://openrouter.ai/api")
        }
        "nous" => {
            // The Nous /v1/models endpoint exposes pricing without auth; the
            // key is best-effort (models.py `_resolve_nous_pricing_credentials`,
            // minus the unported OAuth credential store).
            let key = std::env::var("NOUS_API_KEY").unwrap_or_default();
            fetch_models_with_pricing(key.trim(), "https://inference-api.nousresearch.com")
        }
        _ => HashMap::new(),
    }
}

/// Convert a per-token price string to `$/Mtok`
/// (models.py `_format_price_per_mtok`): "0.000003" → "$3.00", "0" → "free".
pub fn format_price_per_mtok(per_token: &str) -> String {
    let Ok(val) = per_token.trim().parse::<f64>() else {
        return "?".to_string();
    };
    if val == 0.0 {
        return "free".to_string();
    }
    format!("${:.2}", val * 1_000_000.0)
}

// ---------------------------------------------------------------------------
// Expensive-model guard (hermes_cli/model_cost_guard.py)
// ---------------------------------------------------------------------------

const INPUT_COST_WARNING_THRESHOLD: f64 = 20.0;
const OUTPUT_COST_WARNING_THRESHOLD: f64 = 100.0;
const GPT55_PRO_OPENROUTER_ID: &str = "openai/gpt-5.5-pro";
const GPT55_SUGGESTION: &str = "did you mean to select openai/gpt-5.5?";

fn format_money(value: Option<f64>) -> String {
    match value {
        Some(v) => format!("${:.2}/M", v),
        None => "unknown".to_string(),
    }
}

/// Warning message when known pricing exceeds the guardrails
/// (model_cost_guard.py `expensive_model_warning`; pricing source is
/// models.dev — the `usage_pricing` fallback is unported). None = no warning.
pub fn expensive_model_warning(model: &str, provider: &str) -> Option<String> {
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    let (input_cost, output_cost) = match models_dev_cost(provider, model) {
        Some((i, o)) => (Some(i), Some(o)),
        None => (None, None),
    };
    let over_input = input_cost.map(|c| c > INPUT_COST_WARNING_THRESHOLD).unwrap_or(false);
    let over_output = output_cost.map(|c| c > OUTPUT_COST_WARNING_THRESHOLD).unwrap_or(false);
    if !over_input && !over_output {
        return None;
    }
    let mut lines = vec![
        "!!! EXPENSIVE MODEL WARNING !!!".to_string(),
        String::new(),
        format!("{} has known pricing above Joey's safety threshold.", model),
        format!("Input tokens: {}", format_money(input_cost)),
        format!("Output tokens: {}", format_money(output_cost)),
        "Threshold: more than $20/M input tokens or more than $100/M output tokens.".to_string(),
        "Pricing source: models.dev.".to_string(),
    ];
    if model.to_lowercase() == GPT55_PRO_OPENROUTER_ID {
        lines.push(GPT55_SUGGESTION.to_string());
    }
    lines.push("Confirm only if you intend to use this model.".to_string());
    Some(lines.join("\n"))
}

// ---------------------------------------------------------------------------
// Picker cache management (models.py `clear_provider_models_cache`)
// ---------------------------------------------------------------------------

fn provider_models_cache_path() -> PathBuf {
    joey_core::constants::joey_home().join("provider_models_cache.json")
}

/// Wipe the on-disk provider-models cache (`joey model --refresh`).
pub fn clear_provider_models_cache() {
    let path = provider_models_cache_path();
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
}

// ---------------------------------------------------------------------------
// Blocking HTTP helper
// ---------------------------------------------------------------------------

/// GET a JSON document on a dedicated thread with its own runtime — safe from
/// both sync and async contexts (the wizard is synchronous console I/O, like
/// upstream's urllib calls). Sends the joey-cli User-Agent so WAF-fronted
/// catalogs don't 403 the default client UA (providers/base.py
/// `_profile_user_agent`).
pub fn http_get_json(url: &str, headers: &[(&str, &str)], timeout_secs: f64) -> Option<Value> {
    let url = url.to_string();
    let headers: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        rt.block_on(async move {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs_f64(timeout_secs))
                .user_agent(format!(
                    "{}/{}",
                    joey_core::branding::CLI_NAME,
                    joey_core::branding::VERSION
                ))
                .build()
                .ok()?;
            let mut req = client.get(&url).header("Accept", "application/json");
            for (k, v) in &headers {
                req = req.header(k.as_str(), v.as_str());
            }
            let resp = req.send().await.ok()?;
            if !resp.status().is_success() {
                return None;
            }
            resp.json::<Value>().await.ok()
        })
    })
    .join()
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zai_curated_list_matches_upstream() {
        assert_eq!(
            provider_models("zai"),
            vec![
                "glm-5.2",
                "glm-5.1",
                "glm-5",
                "glm-5v-turbo",
                "glm-5-turbo",
                "glm-4.7",
                "glm-4.5",
                "glm-4.5-flash",
            ]
        );
        // >= 8 entries → the wizard uses the curated list directly when
        // models.dev is unavailable (model_setup_flows.py:2830-2835).
        assert!(provider_models("zai").len() >= 8);
    }

    #[test]
    fn openrouter_curated_has_default_badge_on_glm() {
        let entry = OPENROUTER_MODELS
            .iter()
            .find(|(m, _)| *m == "z-ai/glm-5.2")
            .unwrap();
        assert_eq!(entry.1, "default");
        assert_eq!(PREFERRED_SILENT_DEFAULT_MODEL, "z-ai/glm-5.2");
    }

    #[test]
    fn price_formatting_matches_upstream() {
        assert_eq!(format_price_per_mtok("0.000003"), "$3.00");
        assert_eq!(format_price_per_mtok("0.00003"), "$30.00");
        assert_eq!(format_price_per_mtok("0.00000015"), "$0.15");
        assert_eq!(format_price_per_mtok("0.0000001"), "$0.10");
        assert_eq!(format_price_per_mtok("0.00018"), "$180.00");
        assert_eq!(format_price_per_mtok("0"), "free");
        assert_eq!(format_price_per_mtok("garbage"), "?");
    }

    #[test]
    fn noise_patterns_filter_matches_upstream() {
        for noisy in [
            "gemini-tts",
            "text-embedding-3",
            "live-audio-model",
            "gemini-preview-0325-x",
            "model-exp-1206_v",
            "gpt-image",
            "gemini-image-preview",
            "model-customtools",
        ] {
            assert!(NOISE_PATTERNS.is_match(noisy), "{} should be noise", noisy);
        }
        for clean in ["glm-5.2", "claude-sonnet-5", "gemini-3-pro-preview", "deepseek-chat"] {
            assert!(!NOISE_PATTERNS.is_match(clean), "{} should pass", clean);
        }
    }

    #[test]
    fn openrouter_tools_filter_permissive_when_absent() {
        assert!(openrouter_model_supports_tools(&json!({"id": "m"})));
        assert!(openrouter_model_supports_tools(
            &json!({"supported_parameters": ["tools", "temperature"]})
        ));
        assert!(!openrouter_model_supports_tools(
            &json!({"supported_parameters": ["temperature"]})
        ));
        assert!(openrouter_model_is_free(Some(&json!({"prompt": "0", "completion": "0"}))));
        assert!(!openrouter_model_is_free(Some(
            &json!({"prompt": "0.000003", "completion": "0"})
        )));
    }

    #[test]
    fn xai_static_fallback_promotes_and_merges() {
        // With no models.dev disk cache the static list flows through the
        // same promote/merge pipeline as live data.
        let ids = xai_merge_curated_extras(xai_promote_top(
            XAI_STATIC_FALLBACK.iter().map(|s| s.to_string()).collect(),
        ));
        assert_eq!(ids[0], "grok-build-0.1");
        assert!(ids.contains(&"grok-composer-2.5-fast".to_string()));
        // No duplicate grok-4.5 (already in the static list).
        assert_eq!(ids.iter().filter(|m| *m == "grok-4.5").count(), 1);
    }

    #[test]
    fn expensive_warning_skips_unknown_pricing() {
        // No models.dev data in a temp home → no warning (guard only fires
        // on KNOWN pricing).
        let _lock = joey_core::constants::TEST_HOME_OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let _guard = joey_core::constants::HomeOverrideGuard::new(dir.path().to_path_buf());
        // Seed an empty disk cache so fetch_models_dev doesn't hit the network.
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("models_dev_cache.json"), "{\"_\":{}}").unwrap();
        assert!(expensive_model_warning("glm-5.2", "zai").is_none());
    }

    #[test]
    fn models_probe_suggests_v1_toggle() {
        // No network in tests: empty base short-circuits.
        let probe = probe_api_models("", "", "");
        assert!(probe.models.is_none());
        assert!(probe.probed_url.is_none());
    }
}
