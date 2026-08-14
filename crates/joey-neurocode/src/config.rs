//! NeuroCodeConfig — load from `config.yaml` dotted keys (T012).
//!
//! Config keys (contracts/neurocode-command.md):
//! ```yaml
//! neurocode:
//!   enabled: false
//!   tier:
//!     economical:
//!       model: ""
//!     frontier:
//!       model: ""
//!     ambiguous_default: economical
//!   verify:
//!     steps: []
//!     max_fix_iterations: 3
//!   classifier:
//!     scope_fanout_frontier_threshold: 4
//!     economical_keywords: []
//!     frontier_keywords: []
//!   pega:
//!     version: ""
//! ```

use crate::classifier::ComplexityTier;

/// NeuroCode configuration loaded from `config.yaml`.
#[derive(Debug, Clone)]
pub struct NeuroCodeConfig {
    /// Whether NeuroCode is enabled (default-off, FR-003).
    pub enabled: bool,
    pub tier: TierConfig,
    pub verify: VerifyConfig,
    pub classifier: ClassifierConfig,
    pub pega: PegaConfig,
}

impl Default for NeuroCodeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tier: TierConfig::default(),
            verify: VerifyConfig::default(),
            classifier: ClassifierConfig::default(),
            pega: PegaConfig::default(),
        }
    }
}

impl NeuroCodeConfig {
    /// Load from a joey-core Config (dotted-key API).
    pub fn from_config(cfg: &joey_core::Config) -> Self {
        Self {
            enabled: cfg.get_bool("neurocode.enabled", false),
            tier: TierConfig::from_config(cfg),
            verify: VerifyConfig::from_config(cfg),
            classifier: ClassifierConfig::from_config(cfg),
            pega: PegaConfig::from_config(cfg),
        }
    }

    /// Resolve which tier AmbiguousDefault maps to.
    pub fn ambiguous_default_tier(&self) -> ComplexityTier {
        match self.tier.ambiguous_default.as_str() {
            "frontier" => ComplexityTier::Frontier,
            _ => ComplexityTier::Economical,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TierConfig {
    pub economical_model: String,
    pub frontier_model: String,
    /// Which tier AmbiguousDefault resolves to ("economical" or "frontier").
    pub ambiguous_default: String,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            economical_model: String::new(),
            frontier_model: String::new(),
            ambiguous_default: "economical".into(),
        }
    }
}

impl TierConfig {
    fn from_config(cfg: &joey_core::Config) -> Self {
        Self {
            economical_model: cfg.get_str("neurocode.tier.economical.model", ""),
            frontier_model: cfg.get_str("neurocode.tier.frontier.model", ""),
            ambiguous_default: cfg.get_str("neurocode.tier.ambiguous_default", "economical"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct VerifyConfig {
    pub steps: Vec<VerifyStepConfig>,
    pub max_fix_iterations: u32,
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self {
            steps: Vec::new(),
            max_fix_iterations: 3,
        }
    }
}

impl VerifyConfig {
    fn from_config(cfg: &joey_core::Config) -> Self {
        let max_fix = cfg.get_i64("neurocode.verify.max_fix_iterations", 3) as u32;
        let steps = parse_verify_steps(cfg);
        Self {
            steps,
            max_fix_iterations: max_fix,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VerifyStepConfig {
    pub name: String,
    pub command: String,
    pub parse: String,
    pub timeout_sec: u64,
}

fn parse_verify_steps(cfg: &joey_core::Config) -> Vec<VerifyStepConfig> {
    let Some(serde_yaml::Value::Sequence(seq)) =
        cfg.get("neurocode.verify.steps").cloned()
    else {
        return Vec::new();
    };
    seq.iter()
        .filter_map(|v| v.as_mapping())
        .filter_map(|m| {
            let name = m
                .get(serde_yaml::Value::String("name".into()))
                .and_then(|v| v.as_str())?
                .to_string();
            let command = m
                .get(serde_yaml::Value::String("command".into()))
                .and_then(|v| v.as_str())?
                .to_string();
            let parse = m
                .get(serde_yaml::Value::String("parse".into()))
                .and_then(|v| v.as_str())
                .unwrap_or("plain")
                .to_string();
            let timeout_sec = m
                .get(serde_yaml::Value::String("timeout_sec".into()))
                .and_then(|v| v.as_u64())
                .unwrap_or(120);
            Some(VerifyStepConfig {
                name,
                command,
                parse,
                timeout_sec,
            })
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct ClassifierConfig {
    pub scope_fanout_frontier_threshold: usize,
    pub economical_keywords: Vec<String>,
    pub frontier_keywords: Vec<String>,
}

impl Default for ClassifierConfig {
    fn default() -> Self {
        Self {
            scope_fanout_frontier_threshold: 4,
            economical_keywords: Vec::new(),
            frontier_keywords: Vec::new(),
        }
    }
}

impl ClassifierConfig {
    fn from_config(cfg: &joey_core::Config) -> Self {
        Self {
            scope_fanout_frontier_threshold: cfg.get_i64(
                "neurocode.classifier.scope_fanout_frontier_threshold",
                4,
            ) as usize,
            economical_keywords: cfg.get_str_list("neurocode.classifier.economical_keywords"),
            frontier_keywords: cfg.get_str_list("neurocode.classifier.frontier_keywords"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PegaConfig {
    /// Explicit version override (empty = auto-detect, FR-009, Q4).
    pub version: String,
}

impl Default for PegaConfig {
    fn default() -> Self {
        Self {
            version: String::new(),
        }
    }
}

impl PegaConfig {
    fn from_config(cfg: &joey_core::Config) -> Self {
        Self {
            version: cfg.get_str("neurocode.pega.version", ""),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_off() {
        let cfg = NeuroCodeConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.verify.max_fix_iterations, 3);
        assert_eq!(cfg.classifier.scope_fanout_frontier_threshold, 4);
    }

    #[test]
    fn ambiguous_default_tier_resolution() {
        let mut cfg = NeuroCodeConfig::default();
        assert_eq!(
            cfg.ambiguous_default_tier(),
            ComplexityTier::Economical
        );
        cfg.tier.ambiguous_default = "frontier".into();
        assert_eq!(cfg.ambiguous_default_tier(), ComplexityTier::Frontier);
    }
}
