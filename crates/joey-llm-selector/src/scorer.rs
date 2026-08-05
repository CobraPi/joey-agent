//! `ColdStartScorer` — the capability+cost scorer (T011).
//!
//! Built from scratch: the Rust port did not port upstream Python's cost-router
//! (`model_catalog.rs:7-11`, `PORTING.md:392-400`). This scorer is the single
//! source of truth for cold-start allocation (FR-007) and the diagnoser's
//! baseline.

use crate::candidate::{CandidateModel, CandidateModelPool};

/// Requirements a module imposes on its allocated model (FR-005).
#[derive(Debug, Clone, Copy)]
pub struct ModuleRequirements {
    pub needs_tools: bool,
    pub needs_vision: bool,
    /// Minimum context window in tokens.
    pub min_context_window: u64,
}

impl ModuleRequirements {
    pub fn main_turn(turn_has_images: bool, token_budget_hint: u64) -> Self {
        Self {
            needs_tools: true,
            needs_vision: turn_has_images,
            min_context_window: token_budget_hint,
        }
    }

    pub fn compression(context_length: u64) -> Self {
        Self {
            needs_tools: false,
            needs_vision: false,
            min_context_window: context_length,
        }
    }

    pub fn subagent(token_budget_hint: u64) -> Self {
        Self {
            needs_tools: true,
            needs_vision: false,
            min_context_window: token_budget_hint,
        }
    }
}

/// The cold-start scorer: assigns each module the cheapest capable model.
pub struct ColdStartScorer;

impl ColdStartScorer {
    /// Score all candidates against the requirements, returning them sorted by
    /// suitability (cheapest capable first). Capability hard-gates (FR-005)
    /// are applied first — incapable models are excluded entirely.
    pub fn rank<'a>(
        pool: &'a CandidateModelPool,
        reqs: &ModuleRequirements,
    ) -> Vec<&'a CandidateModel> {
        // FR-005: filter to capable candidates first.
        let capable: Vec<&CandidateModel> = pool
            .models
            .iter()
            .filter(|m| Self::satisfies(m, reqs))
            .collect();
        // Then sort by cost: capability tier (cheaper first), then billing cost.
        let mut ranked = capable;
        ranked.sort_by(|a, b| {
            // Lower tier (cheaper) wins first.
            a.tier
                .cost_weight()
                .cmp(&b.tier.cost_weight())
                .then_with(|| {
                    let ca = a.cost.map(|c| c.input_per_mtok + c.output_per_mtok);
                    let cb = b.cost.map(|c| c.input_per_mtok + c.output_per_mtok);
                    ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        ranked
    }

    /// Pick the single best (cheapest capable) model for the module.
    /// Returns None if no candidate satisfies the requirements.
    pub fn pick<'a>(
        pool: &'a CandidateModelPool,
        reqs: &ModuleRequirements,
    ) -> Option<&'a CandidateModel> {
        Self::rank(pool, reqs).into_iter().next()
    }

    /// Whether a candidate satisfies the module's hard requirements (FR-005).
    /// FR-005: never assign an incapable model just because it scored well.
    pub fn satisfies(m: &CandidateModel, reqs: &ModuleRequirements) -> bool {
        if reqs.needs_tools && !m.supports_tools {
            return false;
        }
        if reqs.needs_vision && !m.supports_vision {
            return false;
        }
        if m.context_window < reqs.min_context_window {
            return false;
        }
        true
    }

    /// Build a human-readable reason string for a cold-start pick.
    pub fn reason_for(pick: &CandidateModel, reqs: &ModuleRequirements) -> String {
        let mut bits = Vec::new();
        bits.push(format!("{:?}", pick.tier).to_ascii_lowercase());
        if reqs.needs_tools {
            bits.push("tool-capable".to_string());
        }
        if reqs.needs_vision {
            bits.push("vision-capable".to_string());
        }
        bits.push(format!("ctx>={}k", pick.context_window / 1000));
        format!("cold-start: cheapest capable ({})", bits.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{CatalogSource, CapabilityTier, Cost};

    fn model(id: &str, tools: bool, vision: bool, ctx: u64, tier: CapabilityTier) -> CandidateModel {
        CandidateModel {
            id: id.to_string(),
            provider: "test".to_string(),
            context_window: ctx,
            supports_tools: tools,
            supports_vision: vision,
            tier,
            cost: None,
        }
    }

    fn pool(models: Vec<CandidateModel>) -> CandidateModelPool {
        CandidateModelPool {
            models,
            source: CatalogSource::Copilot,
            fetched_at: None,
        }
    }

    #[test]
    fn test_satisfies_capability_gates() {
        let m = model("x", false, false, 1000, CapabilityTier::Standard);
        assert!(ColdStartScorer::satisfies(
            &m,
            &ModuleRequirements {
                needs_tools: false,
                needs_vision: false,
                min_context_window: 500,
            }
        ));
        assert!(!ColdStartScorer::satisfies(
            &m,
            &ModuleRequirements {
                needs_tools: true,
                needs_vision: false,
                min_context_window: 500,
            }
        ));
        assert!(!ColdStartScorer::satisfies(
            &m,
            &ModuleRequirements {
                needs_tools: false,
                needs_vision: true,
                min_context_window: 500,
            }
        ));
        assert!(!ColdStartScorer::satisfies(
            &m,
            &ModuleRequirements {
                needs_tools: false,
                needs_vision: false,
                min_context_window: 2000,
            }
        ));
    }

    #[test]
    fn test_never_assigns_incapable() {
        // A frontier model that lacks vision should not be picked for a vision turn.
        let m = model("opus", true, false, 200_000, CapabilityTier::Frontier);
        let p = pool(vec![m]);
        let reqs = ModuleRequirements {
            needs_tools: true,
            needs_vision: true,
            min_context_window: 1000,
        };
        assert!(ColdStartScorer::pick(&p, &reqs).is_none());
    }

    #[test]
    fn test_picks_cheapest_capable() {
        let p = pool(vec![
            model("frontier", true, true, 200_000, CapabilityTier::Frontier),
            model("flash", true, true, 128_000, CapabilityTier::Flash),
            model("versatile", true, true, 128_000, CapabilityTier::Versatile),
        ]);
        let reqs = ModuleRequirements {
            needs_tools: true,
            needs_vision: true,
            min_context_window: 1000,
        };
        let pick = ColdStartScorer::pick(&p, &reqs).unwrap();
        assert_eq!(pick.id, "flash"); // cheapest tier wins
    }

    #[test]
    fn test_cost_tiebreak_within_tier() {
        let p = pool(vec![
            CandidateModel {
                id: "expensive".to_string(),
                provider: "test".to_string(),
                context_window: 128_000,
                supports_tools: true,
                supports_vision: true,
                tier: CapabilityTier::Versatile,
                cost: Some(Cost {
                    input_per_mtok: 10.0,
                    output_per_mtok: 30.0,
                }),
            },
            CandidateModel {
                id: "cheap".to_string(),
                provider: "test".to_string(),
                context_window: 128_000,
                supports_tools: true,
                supports_vision: true,
                tier: CapabilityTier::Versatile,
                cost: Some(Cost {
                    input_per_mtok: 1.0,
                    output_per_mtok: 3.0,
                }),
            },
        ]);
        let reqs = ModuleRequirements {
            needs_tools: true,
            needs_vision: true,
            min_context_window: 1000,
        };
        let pick = ColdStartScorer::pick(&p, &reqs).unwrap();
        assert_eq!(pick.id, "cheap");
    }

    #[test]
    fn test_empty_pool() {
        let p = CandidateModelPool::default();
        let reqs = ModuleRequirements {
            needs_tools: false,
            needs_vision: false,
            min_context_window: 100,
        };
        assert!(ColdStartScorer::pick(&p, &reqs).is_none());
    }

    #[test]
    fn test_single_model_pool() {
        let p = pool(vec![model("only", true, true, 128_000, CapabilityTier::Versatile)]);
        let reqs = ModuleRequirements {
            needs_tools: true,
            needs_vision: true,
            min_context_window: 1000,
        };
        let pick = ColdStartScorer::pick(&p, &reqs).unwrap();
        assert_eq!(pick.id, "only");
    }
}
