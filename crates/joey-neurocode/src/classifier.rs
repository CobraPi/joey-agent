//! `ComplexityClassifier` — deterministic rule-based tier classification (FR-001).


use crate::config::NeuroCodeConfig;

/// The model tier a coding request is routed to (FR-001).
///
/// `#[non_exhaustive]`: future tiers may be added without breaking the trait,
/// the on-disk config, or the SQLite schema (Constitution VII).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "lowercase")]
pub enum ComplexityTier {
    /// Suited to boilerplate, unit-test generation, simple refactoring.
    Economical,
    /// Suited to architectural changes, multi-file refactoring, concurrency
    /// debugging, legacy comprehension.
    Frontier,
    /// The defined default when the classifier cannot decide (FR-001
    /// acceptance 3). Resolves to `Economical`.
    AmbiguousDefault,
}

impl Default for ComplexityTier {
    fn default() -> Self {
        ComplexityTier::Economical
    }
}

impl ComplexityTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            ComplexityTier::Economical => "economical",
            ComplexityTier::Frontier => "frontier",
            ComplexityTier::AmbiguousDefault => "ambiguous_default",
        }
    }

    /// Resolve AmbiguousDefault to the configured target tier.
    pub fn resolve_ambiguous(self, default: ComplexityTier) -> ComplexityTier {
        match self {
            ComplexityTier::AmbiguousDefault => default,
            other => other,
        }
    }
}

impl std::fmt::Display for ComplexityTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single deterministic classification signal (research.md §5).
#[derive(Debug, Clone)]
pub struct ClassificationSignal {
    pub kind: SignalKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    /// A keyword match ("refactor", "test", "architecture", ...).
    Keyword,
    /// Scope fan-out (number of artifacts referenced).
    ScopeFanOut,
    /// Structural-graph locality (request touches a hub type).
    GraphHub,
}

/// The result of classifying a coding request (spec Key Entity, data-model.md Entity 2).
#[derive(Debug, Clone)]
pub struct ComplexityRoute {
    /// The resolved tier.
    pub tier: ComplexityTier,
    /// Human-readable classification reasoning (FR-002, SC-002).
    pub reasoning: String,
    /// True if the developer overrode the automatic classification (FR-002).
    pub overridden: bool,
    /// The developer-chosen tier when `overridden` is true.
    pub override_tier: Option<ComplexityTier>,
    /// The deterministic signals that fired (for diagnostics).
    pub signals: Vec<ClassificationSignal>,
}

/// The deterministic, non-async complexity classifier (research.md §5, FR-017).
///
/// Evaluates keyword, scope-fan-out, and graph-hub signals to produce a
/// `ComplexityRoute`. No LLM call — O(1) on the hot path.
pub struct ComplexityClassifier {
    /// Keywords that lean Economical (configurable via config.yaml).
    economical_keywords: Vec<String>,
    /// Keywords that lean Frontier (configurable via config.yaml).
    frontier_keywords: Vec<String>,
    /// Scope fan-out threshold to lean Frontier.
    scope_fanout_frontier_threshold: usize,
    /// The pinned tier override (set by `/neurocode tier pin`).
    pinned_tier: std::sync::Mutex<Option<ComplexityTier>>,
}

impl Default for ComplexityClassifier {
    fn default() -> Self {
        Self {
            economical_keywords: default_economical_keywords(),
            frontier_keywords: default_frontier_keywords(),
            scope_fanout_frontier_threshold: 4,
            pinned_tier: std::sync::Mutex::new(None),
        }
    }
}

impl ComplexityClassifier {
    /// Build from NeuroCode config (contracts/neurocode-command.md).
    ///
    /// Keyword semantics: an absent config key (`None`) activates the
    /// built-in default keyword lists (backward compatible for users who
    /// never configured them); an explicitly present list (`Some`, even
    /// empty) is used exactly as given — `Some([])` disables keyword
    /// matching for that tier.
    pub fn from_config(config: &NeuroCodeConfig) -> Self {
        let economical_keywords = match &config.classifier.economical_keywords {
            None => {
                tracing::debug!(
                    "neurocode classifier: economical_keywords not configured — \
                     using built-in default keyword list"
                );
                default_economical_keywords()
            }
            Some(list) => list.clone(),
        };
        let frontier_keywords = match &config.classifier.frontier_keywords {
            None => {
                tracing::debug!(
                    "neurocode classifier: frontier_keywords not configured — \
                     using built-in default keyword list"
                );
                default_frontier_keywords()
            }
            Some(list) => list.clone(),
        };
        Self {
            economical_keywords,
            frontier_keywords,
            scope_fanout_frontier_threshold: config
                .classifier
                .scope_fanout_frontier_threshold,
            pinned_tier: std::sync::Mutex::new(None),
        }
    }

    /// Classify a coding request into a `ComplexityRoute` (FR-001).
    ///
    /// Deterministic, non-async, O(1) — operates on request text + in-memory
    /// scope signals (research.md §5).
    pub fn classify(&self, request: &crate::engine::CodingRequest) -> ComplexityRoute {
        // Check pinned override first (FR-002).
        if let Ok(pinned) = self.pinned_tier.lock() {
            if let Some(tier) = *pinned {
                return ComplexityRoute {
                    tier,
                    reasoning: format!("developer-pinned tier: {}", tier),
                    overridden: true,
                    override_tier: Some(tier),
                    signals: vec![],
                };
            }
        }

        let text_lower = request.text.to_lowercase();
        let mut signals = Vec::new();

        // Keyword signals.
        let mut eco_hits = Vec::new();
        let mut frontier_hits = Vec::new();
        for kw in &self.economical_keywords {
            if text_lower.contains(&kw.to_lowercase()) {
                eco_hits.push(kw.clone());
            }
        }
        for kw in &self.frontier_keywords {
            if text_lower.contains(&kw.to_lowercase()) {
                frontier_hits.push(kw.clone());
            }
        }
        if !eco_hits.is_empty() {
            signals.push(ClassificationSignal {
                kind: SignalKind::Keyword,
                detail: format!("economical keywords: {}", eco_hits.join(", ")),
            });
        }
        if !frontier_hits.is_empty() {
            signals.push(ClassificationSignal {
                kind: SignalKind::Keyword,
                detail: format!("frontier keywords: {}", frontier_hits.join(", ")),
            });
        }

        // Scope fan-out signal.
        let scope = request.active_symbols.len();
        if scope > self.scope_fanout_frontier_threshold {
            signals.push(ClassificationSignal {
                kind: SignalKind::ScopeFanOut,
                detail: format!("{} artifacts referenced (threshold {})", scope, self.scope_fanout_frontier_threshold),
            });
        }

        // Determine the tier from signals.
        let eco_score = eco_hits.len();
        let frontier_score = frontier_hits.len()
            + if scope > self.scope_fanout_frontier_threshold { 1 } else { 0 };

        let (tier, reason) = if frontier_score > eco_score && frontier_score > 0 {
            (
                ComplexityTier::Frontier,
                format!(
                    "frontier signals (score {}) exceed economical (score {})",
                    frontier_score, eco_score
                ),
            )
        } else if eco_score > 0 && eco_score >= frontier_score {
            (
                ComplexityTier::Economical,
                format!(
                    "economical signals (score {}) >= frontier (score {})",
                    eco_score, frontier_score
                ),
            )
        } else {
            (
                ComplexityTier::AmbiguousDefault,
                "no decisive signals — ambiguous default".to_string(),
            )
        };

        ComplexityRoute {
            tier,
            reasoning: reason,
            overridden: false,
            override_tier: None,
            signals,
        }
    }

    /// Pin a tier override for subsequent classifications (FR-002).
    pub fn pin_tier(&self, tier: ComplexityTier) {
        if let Ok(mut pinned) = self.pinned_tier.lock() {
            *pinned = Some(tier);
        }
    }

    /// Unpin the tier override — revert to automatic classification (FR-002).
    pub fn unpin_tier(&self) {
        if let Ok(mut pinned) = self.pinned_tier.lock() {
            *pinned = None;
        }
    }

    /// Get the current pinned tier, if any.
    pub fn pinned_tier(&self) -> Option<ComplexityTier> {
        self.pinned_tier.lock().ok().and_then(|g| *g)
    }
}

fn default_economical_keywords() -> Vec<String> {
    [
        "test", "getter", "setter", "boilerplate", "implement method", "junit", "mock",
        "stub", "tostring", "equals", "hashcode", "builder", "dto", "create", "unit test",
        "pytest", "jest", "vitest", "unittest", "docstring", "comment", "scaffold",
        "rename", "typo", "log statement", "constant",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn default_frontier_keywords() -> Vec<String> {
    [
        "refactor", "architecture", "concurrency", "redesign", "migrate", "debug",
        "transactional", "deadlock", "race condition", "streams", "optional",
        "performance", "optimize", "thread-safe", "async", "await", "goroutine",
        "channel", "unsafe", "borrow", "lifetime", "ownership", "move semantics",
        "promise", "closure", "asyncio", "generator", "decorator", "middleware",
        "hook", "observer", "callback hell", "middleware chain", "memory leak",
        "circular dependency", "design pattern",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NeuroCodeConfig;
    use std::path::PathBuf;

    fn make_request(text: &str) -> crate::engine::CodingRequest {
        crate::engine::CodingRequest {
            text: text.into(),
            active_file: None,
            active_symbols: vec![],
            project_root: PathBuf::from("."),
            token_budget_hint: 0,
        }
    }

    #[test]
    fn economical_classification() {
        let clf = ComplexityClassifier::default();
        let route = clf.classify(&make_request("Write a JUnit test for UserServiceImpl.findById"));
        assert_eq!(route.tier, ComplexityTier::Economical);
        assert!(!route.overridden);
    }

    #[test]
    fn frontier_classification() {
        let clf = ComplexityClassifier::default();
        let route = clf.classify(
            &make_request("Refactor UserServiceImpl to use Optional, fix @Transactional boundary, migrate to Streams"),
        );
        assert_eq!(route.tier, ComplexityTier::Frontier);
    }

    #[test]
    fn ambiguous_default_for_neutral_input() {
        let clf = ComplexityClassifier::default();
        let route = clf.classify(&make_request("Help me with this code"));
        assert_eq!(route.tier, ComplexityTier::AmbiguousDefault);
    }

    #[test]
    fn pinned_tier_overrides() {
        let clf = ComplexityClassifier::default();
        clf.pin_tier(ComplexityTier::Frontier);
        let route = clf.classify(&make_request("Write a test"));
        assert_eq!(route.tier, ComplexityTier::Frontier);
        assert!(route.overridden);
        clf.unpin_tier();
        let route2 = clf.classify(&make_request("Write a test"));
        assert_eq!(route2.tier, ComplexityTier::Economical);
    }

    #[test]
    fn scope_fanout_leans_frontier() {
        let clf = ComplexityClassifier::default();
        let mut req = make_request("update these methods");
        req.active_symbols = vec!["a", "b", "c", "d", "e", "f"]
            .into_iter()
            .map(String::from)
            .collect();
        let route = clf.classify(&req);
        // 6 symbols > threshold 4, no economical keyword → Frontier
        assert_eq!(route.tier, ComplexityTier::Frontier);
    }

    #[test]
    fn from_config_uses_config_keywords() {
        let mut cfg = NeuroCodeConfig::default();
        cfg.classifier.frontier_keywords = Some(vec!["supercalifragilistic".into()]);
        let clf = ComplexityClassifier::from_config(&cfg);
        let route = clf.classify(&make_request("supercalifragilistic change"));
        assert_eq!(route.tier, ComplexityTier::Frontier);
    }

    #[test]
    fn absent_config_keywords_fall_back_to_built_in_defaults() {
        // Keys never configured (None) → built-in defaults active.
        let cfg = NeuroCodeConfig::default();
        assert!(cfg.classifier.economical_keywords.is_none());
        assert!(cfg.classifier.frontier_keywords.is_none());
        let clf = ComplexityClassifier::from_config(&cfg);
        let eco = clf.classify(&make_request("Write a JUnit test for UserServiceImpl"));
        assert_eq!(eco.tier, ComplexityTier::Economical);
        let frontier = clf.classify(&make_request(
            "Refactor UserServiceImpl to use Optional and fix the race condition",
        ));
        assert_eq!(frontier.tier, ComplexityTier::Frontier);
    }

    #[test]
    fn explicit_empty_keyword_lists_disable_keyword_matching() {
        // Explicitly `[]` → no keyword signal at all: a prompt stuffed with
        // default economical keywords must NOT flip the tier via keywords.
        let mut cfg = NeuroCodeConfig::default();
        cfg.classifier.economical_keywords = Some(vec![]);
        cfg.classifier.frontier_keywords = Some(vec![]);
        let clf = ComplexityClassifier::from_config(&cfg);
        let route = clf.classify(&make_request(
            "investigate and diagnose why the JUnit test and unit test for the dto fails",
        ));
        assert_eq!(route.tier, ComplexityTier::AmbiguousDefault);
        assert!(
            !route
                .signals
                .iter()
                .any(|s| s.kind == SignalKind::Keyword),
            "no keyword signals should fire when lists are explicitly empty"
        );
    }

    #[test]
    fn custom_keyword_list_replaces_built_ins() {
        // Some(custom) → custom list used exactly; built-ins inactive.
        let mut cfg = NeuroCodeConfig::default();
        cfg.classifier.economical_keywords = Some(vec!["boondoggle".into()]);
        cfg.classifier.frontier_keywords = Some(vec!["supercalifragilistic".into()]);
        let clf = ComplexityClassifier::from_config(&cfg);
        // Built-in economical keyword alone no longer routes economical.
        let neutral = clf.classify(&make_request("Write a JUnit test for the dto"));
        assert_eq!(neutral.tier, ComplexityTier::AmbiguousDefault);
        // Custom economical keyword routes economical.
        let eco = clf.classify(&make_request("fix this boondoggle"));
        assert_eq!(eco.tier, ComplexityTier::Economical);
        // Custom frontier keyword routes frontier.
        let frontier = clf.classify(&make_request("supercalifragilistic change"));
        assert_eq!(frontier.tier, ComplexityTier::Frontier);
    }
}
