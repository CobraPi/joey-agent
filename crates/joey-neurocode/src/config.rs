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
//!   auto_index:
//!     # Re-index the structural graph after large edits so context stays
//!     # current across turns (feature 015 follow-up: dynamic context).
//!     enabled: true
//!     # Re-index once this many source files have been edited since the
//!     # last index build ("large edits").
//!     file_threshold: 3
//!     # Also trigger when cumulative edited lines (added+removed) cross
//!     # this bound — a single huge rewrite counts as "large".
//!     line_threshold: 200
//!     # Minimum seconds between automatic re-index passes (debounce so a
//!     # tool-loop burst of small patches doesn't re-index every turn).
//!     min_interval_secs: 30.0
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
    /// Automatic re-indexing after large edits (feature 015 follow-up:
    /// dynamic context across turns). Default-on when NeuroCode itself is
    /// enabled.
    pub auto_index: AutoIndexConfig,
}
impl Default for NeuroCodeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tier: TierConfig::default(),
            verify: VerifyConfig::default(),
            classifier: ClassifierConfig::default(),
            pega: PegaConfig::default(),
            auto_index: AutoIndexConfig::default(),
        }
    }
}

/// Thresholds controlling automatic re-indexing of the structural graph.
#[derive(Debug, Clone)]
pub struct AutoIndexConfig {
    pub enabled: bool,
    /// Distinct edited source files needed to trigger a re-index.
    pub file_threshold: usize,
    /// Cumulative edited lines (added+removed) that trigger a re-index
    /// even below the file threshold.
    pub line_threshold: usize,
    /// Minimum seconds between automatic passes (debounce).
    pub min_interval_secs: f64,
}

impl Default for AutoIndexConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            file_threshold: 3,
            line_threshold: 200,
            min_interval_secs: 30.0,
        }
    }
}

impl AutoIndexConfig {
    pub fn from_config(cfg: &joey_core::Config) -> Self {
        Self {
            enabled: cfg.get_bool("neurocode.auto_index.enabled", true),
            file_threshold: cfg.get_f64("neurocode.auto_index.file_threshold", 3.0).max(1.0) as usize,
            line_threshold: cfg
                .get_f64("neurocode.auto_index.line_threshold", 200.0)
                .max(1.0) as usize,
            min_interval_secs: cfg
                .get_f64("neurocode.auto_index.min_interval_secs", 30.0)
                .max(0.0),
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
            auto_index: AutoIndexConfig::from_config(cfg),
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
    /// Per-provider tier models (`neurocode.tier.providers.<provider>.model`):
    /// frontier/economical overrides scoped to one provider, so switching
    /// providers keeps each backend's tier models from drifting into another
    /// provider's catalog. The flat `frontier_model`/`economical_model` keys
    /// remain the fallback for unlisted providers (backward compatible).
    pub provider_tiers: std::collections::HashMap<String, ProviderTierModels>,
}

/// Per-provider tier model pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderTierModels {
    pub frontier: String,
    pub economical: String,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            economical_model: String::new(),
            frontier_model: String::new(),
            ambiguous_default: "economical".into(),
            provider_tiers: std::collections::HashMap::new(),
        }
    }
}

impl TierConfig {
    fn from_config(cfg: &joey_core::Config) -> Self {
        let mut provider_tiers = std::collections::HashMap::new();
        if let Some(serde_yaml::Value::Mapping(map)) =
            cfg.get("neurocode.tier.providers").cloned()
        {
            for (key, value) in map.iter() {
                let Some(provider) = key.as_str() else { continue };
                let Some(inner) = value.as_mapping() else { continue };
                let get = |field: &str| {
                    inner
                        .get(serde_yaml::Value::String(field.to_string()))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string()
                };
                let models = ProviderTierModels {
                    frontier: get("frontier"),
                    economical: get("economical"),
                };
                if !models.frontier.is_empty() || !models.economical.is_empty() {
                    provider_tiers.insert(provider.to_string(), models);
                }
            }
        }
        Self {
            economical_model: cfg.get_str("neurocode.tier.economical.model", ""),
            frontier_model: cfg.get_str("neurocode.tier.frontier.model", ""),
            ambiguous_default: cfg.get_str("neurocode.tier.ambiguous_default", "economical"),
            provider_tiers,
        }
    }

    /// The tier model pair for `provider`: per-provider values win per-field,
    /// with the flat legacy keys filling any gap (so a provider entry that
    /// only pins `frontier` still inherits the flat `economical`).
    pub fn tiers_for_provider(&self, provider: &str) -> ProviderTierModels {
        let entry = self.provider_tiers.get(provider.trim());
        ProviderTierModels {
            frontier: entry
                .filter(|m| !m.frontier.is_empty())
                .map(|m| m.frontier.clone())
                .unwrap_or_else(|| self.frontier_model.clone()),
            economical: entry
                .filter(|m| !m.economical.is_empty())
                .map(|m| m.economical.clone())
                .unwrap_or_else(|| self.economical_model.clone()),
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

    /// Per-provider tier models parse from `neurocode.tier.providers.<id>`.
    #[test]
    fn provider_tiers_parse_from_config() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            "neurocode:\n  enabled: true\n  tier:\n    providers:\n      zai:\n        frontier: glm-5.2\n        economical: glm-4.5-flash\n      copilot:\n        frontier: gpt-5.4\n",
        )
        .unwrap();
        let cfg = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let nc = NeuroCodeConfig::from_config(&cfg);
        assert_eq!(nc.tier.provider_tiers.len(), 2);
        let zai = nc.tier.tiers_for_provider("zai");
        assert_eq!(zai.frontier, "glm-5.2");
        assert_eq!(zai.economical, "glm-4.5-flash");
        // Partial entries are kept: copilot has frontier only.
        let copilot = nc.tier.tiers_for_provider("copilot");
        assert_eq!(copilot.frontier, "gpt-5.4");
        assert_eq!(copilot.economical, "");
        // Unlisted providers fall back to the flat legacy keys (empty here).
        let other = nc.tier.tiers_for_provider("openrouter");
        assert_eq!(other.frontier, "");
        assert_eq!(other.economical, "");
    }

    /// Per-provider entries override the flat legacy keys; unlisted providers
    /// still resolve through them (backward compatibility).
    #[test]
    fn provider_tiers_override_legacy_flat_keys() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            "neurocode:\n  tier:\n    frontier:\n      model: legacy-frontier\n    economical:\n      model: legacy-economical\n    providers:\n      zai:\n        frontier: glm-5.2\n",
        )
        .unwrap();
        let cfg = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let nc = NeuroCodeConfig::from_config(&cfg);
        // Listed provider: per-provider value wins.
        assert_eq!(nc.tier.tiers_for_provider("zai").frontier, "glm-5.2");
        // Economical unset for zai → flat key fallback applies within a listed
        // provider too (per-field fallback).
        assert_eq!(nc.tier.tiers_for_provider("zai").economical, "legacy-economical");
        // Unlisted provider: full flat-key fallback.
        let deepseek = nc.tier.tiers_for_provider("deepseek");
        assert_eq!(deepseek.frontier, "legacy-frontier");
        assert_eq!(deepseek.economical, "legacy-economical");
    }
}
