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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClassificationSignal {
    pub kind: SignalKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SignalKind {
    /// A keyword match ("refactor", "test", "architecture", ...).
    Keyword,
    /// Scope fan-out (number of artifacts referenced).
    ScopeFanOut,
    /// Structural-graph locality (request touches a hub type).
    GraphHub,
}

/// The result of classifying a coding request (spec Key Entity, data-model.md Entity 2).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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

/// Fan-in/fan-out hub threshold per FR-004: a target with at least this
/// many dependents (or outgoing non-MemberOf edges) is a structural hub.
pub const GRAPH_HUB_DEPENDENTS_THRESHOLD: usize = 4;

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
    /// Optional structural dependency graph for graph-hub evidence
    /// (FR-004). `None` ⇒ legacy-identical classification.
    ///
    /// Shared behind `Arc<Mutex<..>>`: `rusqlite::Connection` is `!Sync`,
    /// so a bare `Arc<DependencyGraph>` would make the classifier `!Send`;
    /// `DependencyGraph` is `Send`, and `Mutex<T: Send>` is `Send + Sync`,
    /// so this shape keeps the classifier shareable across threads.
    graph: Option<std::sync::Arc<std::sync::Mutex<crate::graph::DependencyGraph>>>,
}

impl Default for ComplexityClassifier {
    fn default() -> Self {
        Self {
            economical_keywords: default_economical_keywords(),
            frontier_keywords: default_frontier_keywords(),
            scope_fanout_frontier_threshold: 4,
            pinned_tier: std::sync::Mutex::new(None),
            graph: None,
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
            graph: None,
        }
    }

    /// Attach a structural dependency graph for graph-hub evidence
    /// (FR-004).
    ///
    /// Graph evidence only participates when a graph is explicitly
    /// attached (call sites gate on
    /// `neurocode.enterprise_context.enabled`); `None` ⇒ legacy-identical
    /// classification (FR-005/SC-001).
    pub fn with_graph(
        mut self,
        graph: std::sync::Arc<std::sync::Mutex<crate::graph::DependencyGraph>>,
    ) -> Self {
        self.graph = Some(graph);
        self
    }

    /// The attached dependency graph, if any (FR-004).
    pub fn graph(
        &self,
    ) -> Option<std::sync::Arc<std::sync::Mutex<crate::graph::DependencyGraph>>> {
        self.graph.clone()
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
            if contains_keyword(&text_lower, &kw.to_lowercase()) {
                eco_hits.push(kw.clone());
            }
        }
        for kw in &self.frontier_keywords {
            if contains_keyword(&text_lower, &kw.to_lowercase()) {
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

        // Graph evidence (FR-004): only when a graph is explicitly
        // attached; with no graph the route is identical to the legacy
        // classifier (FR-005/SC-001 parity).
        let mut graph_hits: u32 = 0;
        // Poison recovery (matches the codebase's established pattern):
        // a panic elsewhere while holding the graph mutex must not take
        // down every subsequent classify() call on the hot path — recover
        // the inner graph and continue with graph evidence as normal.
        let graph_guard = self
            .graph
            .as_ref()
            .map(|g| g.lock().unwrap_or_else(|p| p.into_inner()));
        if let Some(graph) = graph_guard.as_deref() {
            let targets: Vec<crate::graph::CodeArtifactNode> = request
                .active_file
                .as_deref()
                .map(|path| graph.store().nodes_by_source_path(path).unwrap_or_default())
                .unwrap_or_default();
            if !targets.is_empty() {
                fn push_graph_hit(
                    signals: &mut Vec<ClassificationSignal>,
                    hits: &mut u32,
                    prefix: &str,
                    what: String,
                ) {
                    *hits += 1;
                    signals.push(ClassificationSignal {
                        kind: SignalKind::GraphHub,
                        detail: format!("{}: {}", prefix, what),
                    });
                }

                // a. Fan-in hub: many artifacts depend on a target.
                for target in &targets {
                    if let Ok(count) = graph.store().dependents_count(target.id) {
                        if count >= GRAPH_HUB_DEPENDENTS_THRESHOLD {
                            push_graph_hit(
                                &mut signals,
                                &mut graph_hits,
                                "fan-in",
                                format!("{} has {} dependents", target.fqcn, count),
                            );
                            break;
                        }
                    }
                }

                // b. Fan-out hub: a target depends on many artifacts
                //    (outgoing non-MemberOf edges).
                for target in &targets {
                    let fan_out = graph
                        .traverse_edges(target.id, None)
                        .map(|edges| {
                            edges
                                .iter()
                                .filter(|(_, kind)| *kind != crate::graph::EdgeKind::MemberOf)
                                .count()
                        })
                        .unwrap_or(0);
                    if fan_out >= GRAPH_HUB_DEPENDENTS_THRESHOLD {
                        push_graph_hit(
                            &mut signals,
                            &mut graph_hits,
                            "fan-out",
                            format!("{} fans out to {}", target.fqcn, fan_out),
                        );
                        break;
                    }
                }

                // c. Affected modules: distinct packages among the targets.
                let mut packages: Vec<&str> = Vec::new();
                for target in &targets {
                    if !packages.contains(&target.package.as_str()) {
                        packages.push(&target.package);
                    }
                }
                if packages.len() >= 3 {
                    push_graph_hit(
                        &mut signals,
                        &mut graph_hits,
                        "affected-modules",
                        format!("spans {} distinct packages", packages.len()),
                    );
                }

                // d. Public API surface: an interface target, or a target
                //    implementing interfaces.
                for target in &targets {
                    if target.kind == crate::graph::ArtifactKind::Interface
                        || !target.implemented_interfaces.is_empty()
                    {
                        push_graph_hit(
                            &mut signals,
                            &mut graph_hits,
                            "public-api",
                            format!("{} is API surface", target.fqcn),
                        );
                        break;
                    }
                }

                // e. Cross-package ownership: targets span >= 2 packages.
                if packages.len() >= 2 {
                    push_graph_hit(
                        &mut signals,
                        &mut graph_hits,
                        "ownership",
                        packages.join(", "),
                    );
                }

                // f. Learned anti-patterns attached to any target.
                let ids: Vec<crate::graph::NodeId> =
                    targets.iter().map(|n| n.id).collect();
                if let Ok(anti_patterns) = graph.store().anti_patterns_for_artifacts(&ids) {
                    if !anti_patterns.is_empty() {
                        push_graph_hit(
                            &mut signals,
                            &mut graph_hits,
                            "anti-pattern",
                            format!("{} learned anti-pattern(s) attached", anti_patterns.len()),
                        );
                    }
                }
            }
        }
        // Graph evidence leans frontier but is capped (max 2) so keyword
        // evidence still dominates (FR-004).
        let graph_bonus: u32 = graph_hits.min(2);

        // Determine the tier from signals.
        let scope_bonus: u32 = if scope > self.scope_fanout_frontier_threshold { 1 } else { 0 };
        let eco_score = eco_hits.len() as u32;
        let frontier_score = frontier_hits.len() as u32 + scope_bonus + graph_bonus;

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
        let reason = if graph_bonus > 0 {
            format!("{}; graph evidence +{}", reason, graph_bonus)
        } else {
            reason
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

/// Word-boundary keyword containment: `needle` matches `haystack` only
/// when the character before/after every occurrence is non-alphanumeric
/// (start/end of text count as boundaries). A bare `contains` let the
/// economical keyword "test" fire on "latest"/"contest", skewing tier
/// routing. Multi-word keywords ("unit test") match on the same rule
/// applied at both ends of the phrase.
fn contains_keyword(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let is_boundary = |c: Option<char>| c.map_or(true, |ch| !ch.is_alphanumeric());
    let mut start = 0;
    while let Some(idx) = haystack[start..].find(needle) {
        let at = start + idx;
        let before = haystack[..at].chars().next_back();
        let after = haystack[at + needle.len()..].chars().next();
        if is_boundary(before) && is_boundary(after) {
            return true;
        }
        start = at + needle.len().max(1);
    }
    false
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
    use crate::graph::{ArtifactKind, CodeArtifactNode, DependencyGraph, EdgeKind, NodeId};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn make_request(text: &str) -> crate::engine::CodingRequest {
        crate::engine::CodingRequest {
            text: text.into(),
            active_file: None,
            active_symbols: vec![],
            project_root: PathBuf::from("."),
            token_budget_hint: 0,
            scope_files: vec![],
        }
    }

    #[test]
    fn economical_classification() {
        let clf = ComplexityClassifier::default();
        let route = clf.classify(&make_request("Write a JUnit test for UserServiceImpl.findById"));
        assert_eq!(route.tier, ComplexityTier::Economical);
        assert!(!route.overridden);
    }

    /// Finding #13: keyword matching is word-boundary based — "test" must
    /// not fire on "latest"/"contest" (substring matches skewed tier
    /// routing toward Economical for unrelated requests).
    #[test]
    fn keyword_match_respects_word_boundaries() {
        let clf = ComplexityClassifier::default();
        // "latest" contains "test" as a substring — must NOT be economical.
        let route = clf.classify(&make_request("update to the latest version everywhere"));
        assert_ne!(
            route.tier,
            ComplexityTier::Economical,
            "'latest' must not fire the 'test' keyword: {}",
            route.reasoning
        );
        // A real standalone "test" still fires.
        let route = clf.classify(&make_request("write a test"));
        assert_eq!(route.tier, ComplexityTier::Economical);
        // And boundary punctuation counts: "test," / "(test)".
        let route = clf.classify(&make_request("fix the flaky test, it fails"));
        assert_eq!(route.tier, ComplexityTier::Economical);
        // Multi-word keywords still match as phrases.
        let route = clf.classify(&make_request("add a unit test for this"));
        assert_eq!(route.tier, ComplexityTier::Economical);
        // Hyphens are boundaries: "contest-driven" must not fire "test"…
        let route = clf.classify(&make_request("make it contest-driven everywhere"));
        assert_ne!(route.tier, ComplexityTier::Economical);
        // …while "test-driven" does.
        let route = clf.classify(&make_request("make it test-driven"));
        assert_eq!(route.tier, ComplexityTier::Economical);
    }

    /// Finding #14: a poisoned graph mutex must degrade gracefully instead
    /// of panicking the classify() hot path.
    #[test]
    fn poisoned_graph_mutex_does_not_panic_classify() {
        let graph = DependencyGraph::open_in_memory().unwrap();
        let shared = Arc::new(Mutex::new(graph));
        // Poison the mutex: lock it on another thread and panic while the
        // guard is held. `DependencyGraph` is `Send`, so the Arc crosses.
        {
            let shared = Arc::clone(&shared);
            let handle = std::thread::spawn(move || {
                let _guard = shared.lock().unwrap();
                panic!("intentional poison while holding the graph mutex");
            });
            let _ = handle.join(); // expected panic; the mutex is now poisoned
        }
        assert!(shared.lock().is_err(), "mutex should be poisoned");

        let clf = ComplexityClassifier::default().with_graph(shared);
        let mut req = make_request("refactor this service");
        req.active_file = Some("src/Thing.java".into());
        let route = clf.classify(&req); // must not panic
        assert_eq!(route.tier, ComplexityTier::Frontier);
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

    fn graph_request(file: &str, text: &str) -> crate::engine::CodingRequest {
        crate::engine::CodingRequest {
            text: text.into(),
            active_file: Some(file.into()),
            active_symbols: vec![],
            project_root: PathBuf::from("."),
            token_budget_hint: 0,
            scope_files: vec![],
        }
    }

    /// Hub with 4 injecting clients; returns (graph, hub node id).
    fn make_graph() -> (DependencyGraph, NodeId) {
        let graph = DependencyGraph::open_in_memory().unwrap();
        let hub_id = graph
            .upsert_node(&CodeArtifactNode::new(
                ArtifactKind::Class,
                "com.example.HubService".into(),
                "com.example".into(),
                "src/HubService.java".into(),
            ))
            .unwrap();
        for i in 0..4 {
            let client_id = graph
                .upsert_node(&CodeArtifactNode::new(
                    ArtifactKind::Class,
                    format!("com.example.Client{}", i),
                    "com.example".into(),
                    format!("src/client{}.java", i),
                ))
                .unwrap();
            graph
                .upsert_edge(client_id, hub_id, EdgeKind::Injects)
                .unwrap();
        }
        (graph, hub_id)
    }

    #[test]
    fn graph_none_preserves_legacy_behavior() {
        let clf = ComplexityClassifier::default();
        let route = clf.classify(&graph_request(
            "src/HubService.java",
            "adjust the hub service wiring",
        ));
        assert!(
            route.signals.iter().all(|s| s.kind != SignalKind::GraphHub),
            "no graph attached ⇒ no GraphHub signals"
        );
        assert_eq!(route.tier, ComplexityTier::AmbiguousDefault);
    }

    #[test]
    fn graph_hub_fan_in_signals_frontier() {
        let (graph, _hub_id) = make_graph();
        let clf = ComplexityClassifier::default().with_graph(Arc::new(Mutex::new(graph)));
        let route = clf.classify(&graph_request(
            "src/HubService.java",
            "adjust the hub service wiring",
        ));
        assert_eq!(route.tier, ComplexityTier::Frontier);
        assert!(route
            .signals
            .iter()
            .any(|s| s.kind == SignalKind::GraphHub && s.detail.starts_with("fan-in")));
    }

    #[test]
    fn graph_anti_pattern_signal() {
        let (graph, hub_id) = make_graph();
        graph
            .store()
            .record_anti_pattern("NPE in HubService", "output", "guard lookup", &[hub_id])
            .unwrap();
        let clf = ComplexityClassifier::default().with_graph(Arc::new(Mutex::new(graph)));
        let route = clf.classify(&graph_request(
            "src/HubService.java",
            "adjust the hub service wiring",
        ));
        assert!(route
            .signals
            .iter()
            .any(|s| s.kind == SignalKind::GraphHub && s.detail.starts_with("anti-pattern")));
    }

    #[test]
    fn graph_public_api_signal() {
        let graph = DependencyGraph::open_in_memory().unwrap();
        graph
            .upsert_node(&CodeArtifactNode::new(
                ArtifactKind::Interface,
                "com.example.Api".into(),
                "com.example".into(),
                "src/api.rs".into(),
            ))
            .unwrap();
        let clf = ComplexityClassifier::default().with_graph(Arc::new(Mutex::new(graph)));
        let route = clf.classify(&graph_request("src/api.rs", "adjust the api surface"));
        assert!(route
            .signals
            .iter()
            .any(|s| s.kind == SignalKind::GraphHub && s.detail.starts_with("public-api")));
    }

    #[test]
    fn graph_bonus_capped_at_two() {
        let graph = DependencyGraph::open_in_memory().unwrap();
        let hub_id = graph
            .upsert_node(&CodeArtifactNode::new(
                ArtifactKind::Interface,
                "com.example.HubApi".into(),
                "com.example".into(),
                "src/HubApi.java".into(),
            ))
            .unwrap();
        for i in 0..4 {
            let client_id = graph
                .upsert_node(&CodeArtifactNode::new(
                    ArtifactKind::Class,
                    format!("com.example.Impl{}", i),
                    "com.example".into(),
                    format!("src/impl{}.java", i),
                ))
                .unwrap();
            graph
                .upsert_edge(client_id, hub_id, EdgeKind::Injects)
                .unwrap();
        }
        graph
            .store()
            .record_anti_pattern("NPE in HubApi", "output", "guard lookup", &[hub_id])
            .unwrap();
        let clf = ComplexityClassifier::default().with_graph(Arc::new(Mutex::new(graph)));
        let route = clf.classify(&graph_request(
            "src/HubApi.java",
            "adjust the hub api wiring",
        ));
        let graph_hub_count = route
            .signals
            .iter()
            .filter(|s| s.kind == SignalKind::GraphHub)
            .count();
        assert!(graph_hub_count >= 3, "expected >= 3 GraphHub signals");
        assert_eq!(route.tier, ComplexityTier::Frontier);
        assert!(route.reasoning.contains("graph evidence +2"));
    }
}
