//! Tier context-budget sizing (FR-008, T016).
//!
//! Economical tier gets a focused slice (method + immediate interface + deps
//! to mock). Frontier tier gets fuller graph (class + interface + repository + DTO).

use crate::classifier::ComplexityTier;

/// The context budget for a tier.
#[derive(Debug, Clone, Copy)]
pub struct TierBudget {
    /// Maximum graph expansion depth (edges from the primary node).
    pub max_expansion_depth: usize,
    /// Maximum number of primary nodes to include.
    pub max_primary_nodes: usize,
    /// Maximum number of expanded nodes to include.
    pub max_expanded_nodes: usize,
}

impl TierBudget {
    /// Budget for a given tier (FR-008).
    pub fn for_tier(tier: ComplexityTier) -> Self {
        match tier {
            ComplexityTier::Economical => Self {
                // Focused slice: method + immediate interface + deps to mock.
                max_expansion_depth: 1,
                max_primary_nodes: 1,
                max_expanded_nodes: 5,
            },
            ComplexityTier::Frontier => Self {
                // Fuller graph: class + interface + repository + DTO.
                max_expansion_depth: 2,
                max_primary_nodes: 3,
                max_expanded_nodes: 20,
            },
            // AmbiguousDefault resolves to Economical (data-model.md Entity 1).
            ComplexityTier::AmbiguousDefault => Self::for_tier(ComplexityTier::Economical),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn economical_budget_is_focused() {
        let b = TierBudget::for_tier(ComplexityTier::Economical);
        assert_eq!(b.max_expansion_depth, 1);
        assert!(b.max_expanded_nodes <= 10);
    }

    #[test]
    fn frontier_budget_is_fuller() {
        let e = TierBudget::for_tier(ComplexityTier::Economical);
        let f = TierBudget::for_tier(ComplexityTier::Frontier);
        assert!(f.max_expansion_depth > e.max_expansion_depth);
        assert!(f.max_expanded_nodes > e.max_expanded_nodes);
    }
}
