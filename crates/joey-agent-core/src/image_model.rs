//! Dedicated image-model resolution (feature 016, US6; FR-015/FR-016;
//! contracts/image-model-routing.md).
//!
//! Pure resolution order (normative):
//! 1. `providers.<id>.image_model`  → explicit_per_provider
//! 2. `model.image_model`           → explicit_global
//! 3. provider default multimodal   → provider_default (catalog vision data)
//! 4. primary model if vision-capable → primary_if_vision
//! 5. else → unavailable(reason) with an actionable message naming the keys.

use joey_core::config::Config;

/// Which resolution step produced the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageModelSource {
    ExplicitPerProvider,
    ExplicitGlobal,
    ProviderDefault,
    PrimaryIfVision,
}

impl ImageModelSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            ImageModelSource::ExplicitPerProvider => "explicit_per_provider",
            ImageModelSource::ExplicitGlobal => "explicit_global",
            ImageModelSource::ProviderDefault => "provider_default",
            ImageModelSource::PrimaryIfVision => "primary_if_vision",
        }
    }
}

/// A successfully resolved image model.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedImageModel {
    pub model_id: String,
    pub source: ImageModelSource,
}

/// Resolution outcome — `Unavailable` carries an actionable message.
#[derive(Debug, Clone, PartialEq)]
pub enum ImageModelResolution {
    Available(ResolvedImageModel),
    Unavailable(String),
}

/// Heuristic vision-capability check used when no catalog entry exists.
/// Mirrors joey-llm-selector's prefix table conservatively (no dependency —
/// agent-core sits above llm-selector in neither direction; the catalog is
/// consulted by the caller when available and passed via `catalog_default`).
pub fn looks_vision_capable(model_id: &str) -> bool {
    let m = model_id.to_ascii_lowercase();
    const PREFIXES: &[&str] = &[
        "gpt-4o", "gpt-4.1", "gpt-5", "chatgpt-4o", "o3", "o4",
        "claude-3", "claude-sonnet", "claude-opus", "claude-haiku",
        "gemini", "glm-4v", "glm-4.5v", "glm-4.6v", "glm-5", "qwen-vl",
        "qwen2-vl", "qwen2.5-vl", "llama-3.2-vision", "pixtral",
        "grok-4", "minimax", "mistral-small-3",
    ];
    PREFIXES.iter().any(|p| m.starts_with(p)) || m.contains("-vl") || m.contains("vision")
}

/// Resolve the image model for the active provider.
///
/// * `provider_id` — active provider (e.g. "zai").
/// * `primary_model` — the currently selected primary model.
/// * `catalog_default` — the provider's default multimodal model from the
///   model catalog, when known (None → step 3 skipped).
/// * `primary_supports_vision` — catalog knowledge about the primary model;
///   when None, the heuristic ([`looks_vision_capable`]) decides.
pub fn resolve_image_model(
    config: &Config,
    provider_id: &str,
    primary_model: &str,
    catalog_default: Option<&str>,
    primary_supports_vision: Option<bool>,
) -> ImageModelResolution {
    // 1. per-provider override (wins over global).
    let per_provider = config.get_str(&format!("providers.{provider_id}.image_model"), "");
    if !per_provider.is_empty() {
        return ImageModelResolution::Available(ResolvedImageModel {
            model_id: per_provider,
            source: ImageModelSource::ExplicitPerProvider,
        });
    }
    // 2. global default.
    let global = config.get_str("model.image_model", "");
    if !global.is_empty() {
        return ImageModelResolution::Available(ResolvedImageModel {
            model_id: global,
            source: ImageModelSource::ExplicitGlobal,
        });
    }
    // 3. provider catalog default.
    if let Some(default) = catalog_default {
        if !default.is_empty() {
            return ImageModelResolution::Available(ResolvedImageModel {
                model_id: default.to_string(),
                source: ImageModelSource::ProviderDefault,
            });
        }
    }
    // 4. primary model if vision-capable.
    let capable = primary_supports_vision
        .unwrap_or_else(|| looks_vision_capable(primary_model));
    if capable {
        return ImageModelResolution::Available(ResolvedImageModel {
            model_id: primary_model.to_string(),
            source: ImageModelSource::PrimaryIfVision,
        });
    }
    // 5. unavailable — actionable message naming the keys.
    ImageModelResolution::Unavailable(format!(
        "no image-capable model available for provider '{provider_id}' (primary '{primary_model}' \
         is not vision-capable); set model.image_model or providers.{provider_id}.image_model \
         in your configuration"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_from(yaml: &str) -> Config {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, yaml).expect("write yaml");
        Config::load_from(path).expect("config load")
    }

    #[test]
    fn order_per_provider_wins() {
        let c = cfg_from_yaml(
            "model:\n  image_model: global-vlm\nproviders:\n  zai:\n    image_model: zai-vlm\n",
        );
        match resolve_image_model(&c, "zai", "text-model", None, None) {
            ImageModelResolution::Available(r) => {
                assert_eq!(r.model_id, "zai-vlm");
                assert_eq!(r.source, ImageModelSource::ExplicitPerProvider);
            }
            other => panic!("expected available, got {other:?}"),
        }
    }

    #[test]
    fn order_global_second() {
        let c = cfg_from_yaml("model:\n  image_model: global-vlm\n");
        match resolve_image_model(&c, "zai", "text-model", Some("cat-vlm"), None) {
            ImageModelResolution::Available(r) => {
                assert_eq!(r.model_id, "global-vlm");
                assert_eq!(r.source, ImageModelSource::ExplicitGlobal);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn order_catalog_third() {
        let c = cfg_from_yaml("");
        match resolve_image_model(&c, "zai", "text-model", Some("cat-vlm"), None) {
            ImageModelResolution::Available(r) => {
                assert_eq!(r.model_id, "cat-vlm");
                assert_eq!(r.source, ImageModelSource::ProviderDefault);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn order_primary_if_vision_fourth() {
        let c = cfg_from_yaml("");
        match resolve_image_model(&c, "zai", "glm-5.2", None, Some(true)) {
            ImageModelResolution::Available(r) => {
                assert_eq!(r.model_id, "glm-5.2");
                assert_eq!(r.source, ImageModelSource::PrimaryIfVision);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unavailable_names_the_keys() {
        let c = cfg_from_yaml("");
        match resolve_image_model(&c, "zai", "text-only-model", None, Some(false)) {
            ImageModelResolution::Unavailable(msg) => {
                assert!(msg.contains("model.image_model"));
                assert!(msg.contains("providers.zai.image_model"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn heuristic_covers_known_families() {
        for m in [
            "gpt-4o", "gpt-4o-mini", "claude-sonnet-4-5", "gemini-2.0-flash",
            "glm-4.6v", "qwen2.5-vl-72b", "pixtral-large",
        ] {
            assert!(looks_vision_capable(m), "{m} should be vision-capable");
        }
        for m in ["text-davinci-003", "llama-3-8b", "deepseek-chat"] {
            assert!(!looks_vision_capable(m), "{m} should not be vision-capable");
        }
    }

    fn cfg_from_yaml(y: &str) -> Config {
        cfg_from(y)
    }
}
