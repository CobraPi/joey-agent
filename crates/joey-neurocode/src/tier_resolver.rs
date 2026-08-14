//! `TierModelResolver` — tier → model id via config lookup (Mode 2) or
//! 011 composition path (Mode 1) — T024, FR-018.

use crate::classifier::ComplexityTier;
use crate::config::NeuroCodeConfig;

/// Resolves a `ComplexityTier` to a concrete model id.
///
/// Mode 2 (NeuroCode ON, 011 OFF): reads the configured model for the tier
/// directly from `config.yaml` (`neurocode.tier.<tier>.model`).
/// Mode 1 (NeuroCode ON, 011 ON): the tier is passed as a constraint hint
/// to 011's `ModelAllocator::resolve()` by the turn-loop intercept — this
/// resolver is only consulted in Mode 2.
pub struct TierModelResolver {
    config: NeuroCodeConfig,
    /// The agent's default model id (fallback when a tier model is missing).
    agent_default_model: String,
    /// The active provider id — selects the per-provider tier override
    /// (`neurocode.tier.providers.<id>`) when present; the flat legacy keys
    /// apply otherwise.
    provider: String,
}

impl TierModelResolver {
    pub fn new(config: NeuroCodeConfig, agent_default_model: String) -> Self {
        Self {
            config,
            agent_default_model,
            provider: String::new(),
        }
    }

    /// Scope resolution to `provider`'s per-provider tier models.
    pub fn with_provider(mut self, provider: &str) -> Self {
        self.provider = provider.trim().to_string();
        self
    }

    /// Resolve the model id for a tier (Mode 2 — direct config lookup).
    ///
    /// Falls back to the agent's default model when the tier model is missing,
    /// and records the fallback reason (contracts/tier-routing-composition.md).
    pub fn resolve(&self, tier: ComplexityTier) -> TierModelResolution {
        let resolved = match tier {
            ComplexityTier::Economical => &self.config.tier.economical_model,
            ComplexityTier::Frontier => &self.config.tier.frontier_model,
            ComplexityTier::AmbiguousDefault => {
                // AmbiguousDefault resolves to the configured default tier.
                let default_tier = self.config.ambiguous_default_tier();
                return self.resolve(default_tier);
            }
        };
        // Per-provider override wins when set for the active provider.
        let per_provider = if self.provider.is_empty() {
            None
        } else {
            let models = self.config.tier.tiers_for_provider(&self.provider);
            match tier {
                ComplexityTier::Frontier if !models.frontier.is_empty() => Some(models.frontier),
                ComplexityTier::Economical if !models.economical.is_empty() => {
                    Some(models.economical)
                }
                _ => None,
            }
        };
        let (model, source) = match per_provider {
            Some(m) => (m, format!("provider-scoped tier '{}'", self.provider)),
            None => (resolved.clone(), "config".to_string()),
        };

        if model.is_empty() {
            TierModelResolution {
                model_id: self.agent_default_model.clone(),
                fell_back: true,
                reason: format!(
                    "tier '{}' model not configured — fell back to agent default",
                    tier
                ),
            }
        } else {
            TierModelResolution {
                model_id: model,
                fell_back: false,
                reason: format!("tier '{}' resolved from {}", tier, source),
            }
        }
    }
}

/// The result of resolving a tier to a model id.
#[derive(Debug, Clone)]
pub struct TierModelResolution {
    /// The resolved model id.
    pub model_id: String,
    /// True when the tier model was missing and the agent default was used.
    pub fell_back: bool,
    /// Human-readable resolution reason.
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_economical_from_config() {
        let mut cfg = NeuroCodeConfig::default();
        cfg.tier.economical_model = "qwen2.5-coder-7b".into();
        let resolver = TierModelResolver::new(cfg, "default-model".into());
        let res = resolver.resolve(ComplexityTier::Economical);
        assert_eq!(res.model_id, "qwen2.5-coder-7b");
        assert!(!res.fell_back);
    }

    #[test]
    fn resolve_falls_back_when_unconfigured() {
        let cfg = NeuroCodeConfig::default();
        let resolver = TierModelResolver::new(cfg, "default-model".into());
        let res = resolver.resolve(ComplexityTier::Frontier);
        assert_eq!(res.model_id, "default-model");
        assert!(res.fell_back);
    }

    #[test]
    fn ambiguous_default_resolves_to_economical() {
        let mut cfg = NeuroCodeConfig::default();
        cfg.tier.economical_model = "economical-model".into();
        let resolver = TierModelResolver::new(cfg, "default".into());
        let res = resolver.resolve(ComplexityTier::AmbiguousDefault);
        assert_eq!(res.model_id, "economical-model");
    }

    #[test]
    fn per_provider_tier_overrides_apply() {
        let mut cfg = NeuroCodeConfig::default();
        cfg.tier.frontier_model = "legacy-frontier".into();
        cfg.tier.provider_tiers.insert(
            "zai".into(),
            crate::config::ProviderTierModels {
                frontier: "glm-5.2".into(),
                economical: "glm-4.5-flash".into(),
            },
        );
        let resolver =
            TierModelResolver::new(cfg, "default-model".into()).with_provider("zai");
        let frontier = resolver.resolve(ComplexityTier::Frontier);
        assert_eq!(frontier.model_id, "glm-5.2");
        assert!(frontier.reason.contains("provider-scoped"));
        let economical = resolver.resolve(ComplexityTier::Economical);
        assert_eq!(economical.model_id, "glm-4.5-flash");
        // A different provider scope keeps the legacy keys.
        let other = TierModelResolver::new(
            NeuroCodeConfig {
                tier: crate::config::TierConfig {
                    frontier_model: "legacy-frontier".into(),
                    ..Default::default()
                },
                ..Default::default()
            },
            "default".into(),
        )
        .with_provider("deepseek");
        assert_eq!(
            other.resolve(ComplexityTier::Frontier).model_id,
            "legacy-frontier"
        );
    }

    /// Without with_provider (legacy callers), behavior is unchanged: the
    /// flat keys are the single source.
    #[test]
    fn unscoped_resolver_keeps_flat_keys() {
        let mut cfg = NeuroCodeConfig::default();
        cfg.tier.frontier_model = "flat-frontier".into();
        cfg.tier.provider_tiers.insert(
            "zai".into(),
            crate::config::ProviderTierModels {
                frontier: "glm-5.2".into(),
                economical: String::new(),
            },
        );
        let resolver = TierModelResolver::new(cfg, "default".into());
        assert_eq!(
            resolver.resolve(ComplexityTier::Frontier).model_id,
            "flat-frontier"
        );
    }
}
