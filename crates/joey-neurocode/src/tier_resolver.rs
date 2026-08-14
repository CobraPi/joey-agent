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
}

impl TierModelResolver {
    pub fn new(config: NeuroCodeConfig, agent_default_model: String) -> Self {
        Self {
            config,
            agent_default_model,
        }
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

        if resolved.is_empty() {
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
                model_id: resolved.clone(),
                fell_back: false,
                reason: format!("tier '{}' resolved from config", tier),
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
}
