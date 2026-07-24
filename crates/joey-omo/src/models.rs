//! Model family detection, fallback chains, and resolution algorithm.
//!
//! 1-to-1 port of OMO's `model-core` family detection and `model-fallback`
//! contract BC-006 through BC-010. The resolution algorithm walks an ordered
//! fallback chain, trying exact model ID match first, then family-level fuzzy
//! match per entry.

use std::collections::{HashMap, HashSet};

// ── Billing Plan Aliases (BC-010) ─────────────────────────────────────
//
// OMO fallback chains reference billing namespace names that map to
// canonical providers. These are registered as provider aliases so
// requires_provider checks match correctly.

/// Return billing plan aliases for a given canonical provider name.
/// These are the billing namespace names used in OMO fallback chains.
fn billing_plan_aliases_for(canonical_provider: &str) -> &'static [&'static str] {
    match canonical_provider {
        "zai" => &[
            "zai-coding-plan",
            "bailian-coding-plan",
            "moonshotai-cn",
            "opencode-go",
        ],
        "openrouter" => &[
            "opencode",
            "vercel",
            "kimi-for-coding",
            "moonshotai",
            "ollama-cloud",
            "aihubmix",
            "minimax-coding-plan",
            "minimax-cn-coding-plan",
        ],
        "anthropic" => &["github-copilot", "vercel", "opencode"],
        "openai-api" => &["github-copilot", "vercel", "opencode"],
        "google" => &["github-copilot", "vercel", "opencode"],
        "xai" => &["github-copilot", "vercel", "opencode"],
        "deepseek" => &["github-copilot", "vercel", "opencode"],
        "nous" => &["github-copilot", "vercel", "opencode"],
        _ => &[],
    }
}

// ── ModelFamily ─────────────────────────────────────────────────────

/// A coarse model vendor/family classification derived from the model ID
/// prefix. Used for fuzzy chain resolution (BC-007) and prompt variant
/// selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelFamily {
    /// `claude-*` (opus, sonnet, haiku, fable)
    Anthropic,
    /// `gpt-*` (5.4, 5.5, 5.6-sol, 5.6-terra, 5.6-luna, codex)
    Gpt,
    /// `kimi-*` (k2, k2.6, k2.7, k3)
    Kimi,
    /// `glm-*` (4.6v, 5, 5.1, 5.2)
    Glm,
    /// `gemini-*` (3-flash, 3.1-pro)
    Gemini,
    /// `minimax-*`, `MiniMax-*`
    Minimax,
    /// Anything else.
    Unknown,
}

impl ModelFamily {
    /// Classify a model ID into a family via prefix matching.
    ///
    /// Detection order: exact prefix → lowercase prefix → Unknown.
    /// Covers every model ID that appears in the OMO fallback chains.
    pub fn detect(model_id: &str) -> Self {
        let lower = model_id.to_ascii_lowercase();
        if lower.starts_with("claude-") {
            Self::Anthropic
        } else if lower.starts_with("gpt-") {
            Self::Gpt
        } else if lower.starts_with("kimi-") {
            Self::Kimi
        } else if lower.starts_with("glm-") {
            Self::Glm
        } else if lower.starts_with("gemini-") {
            Self::Gemini
        } else if lower.starts_with("minimax-") || lower.starts_with("minimax_") {
            Self::Minimax
        } else {
            Self::Unknown
        }
    }
}

// ── FallbackEntry ───────────────────────────────────────────────────

/// One candidate in a fallback chain. 1-to-1 port of OMO's
/// `ModelRequirement.fallbackChain[]` entries.
#[derive(Debug, Clone)]
pub struct FallbackEntry {
    /// Acceptable provider namespaces (family-level). Not used for gating in
    /// joey-agent (the available-model set already encodes provider
    /// availability) — retained for fidelity and future per-provider gating.
    pub providers: Vec<String>,
    /// The model ID to try (e.g. "claude-opus-4-8").
    pub model: String,
    /// Optional effort variant ("max", "high", "medium", "xhigh", "low").
    pub variant: Option<String>,
}

impl FallbackEntry {
    /// Construct a fallback entry. Convenience for the chain definitions.
    pub fn new(model: &str, variant: Option<&str>, providers: &[&str]) -> Self {
        Self {
            providers: providers.iter().map(|s| s.to_string()).collect(),
            model: model.to_string(),
            variant: variant.map(|s| s.to_string()),
        }
    }
}

// ── ModelRequirement ────────────────────────────────────────────────

/// A fallback chain — ordered list of model candidates with constraints.
///
/// Port of OMO's `ModelRequirement` type (data-model.md).
#[derive(Debug, Clone)]
pub struct ModelRequirement {
    /// Ordered candidates (tried first → last).
    pub fallback_chain: Vec<FallbackEntry>,
    /// If true, the agent activates if ANY chain entry resolves.
    pub requires_any_model: bool,
    /// If set, at least one listed provider must be connected for the agent
    /// to be available (checked before chain resolution — BC-010).
    pub requires_provider: Option<Vec<String>>,
}

impl Default for ModelRequirement {
    fn default() -> Self {
        Self {
            fallback_chain: Vec::new(),
            requires_any_model: false,
            requires_provider: None,
        }
    }
}

// ── AvailableModelSet ───────────────────────────────────────────────

/// A set of available model IDs, built from joey-providers' connected
/// profiles + configured models. Provides O(1) exact-match and family-level
/// fuzzy lookup for fallback chain resolution.
#[derive(Debug, Clone, Default)]
pub struct AvailableModelSet {
    /// Exact model IDs known to be available.
    models: HashSet<String>,
    /// Family → list of available model IDs in that family.
    /// Used for BC-007 family-level fuzzy matching.
    family_index: HashMap<ModelFamily, Vec<String>>,
    /// Connected provider names (used by BC-010 requiresProvider).
    connected_providers: HashSet<String>,
}

impl AvailableModelSet {
    /// Create an empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a set from a single connected provider profile + the active model.
    ///
    /// Registers the provider under its canonical name **and** every alias, so
    /// `requiresProvider` gating (BC-010) matches the namespace names used in
    /// the OMO fallback chains (e.g. `"openai"` is an alias of the
    /// `"openai-api"` profile; `"github-copilot"` is an alias of `"copilot"`).
    /// Seeds concrete model IDs from the active model, the profile's default
    /// aux model, and its curated fallback list, so fallback chains resolve via
    /// exact and family-level fuzzy matches.
    pub fn from_connected(profile: &joey_providers::ProviderProfile, active_model: &str) -> Self {
        let mut set = Self::new();
        set.add_provider(profile.name.to_string());
        for alias in profile.aliases {
            set.add_provider((*alias).to_string());
        }
        // Add billing plan aliases (BC-010): OMO fallback chains reference
        // billing namespace names that map to canonical providers. These are
        // registered here so requires_provider checks match correctly.
        for billing_plan in billing_plan_aliases_for(profile.name) {
            set.add_provider(billing_plan.to_string());
        }
        if !active_model.is_empty() {
            set.add_model(active_model.to_string());
        }
        if !profile.default_aux_model.is_empty() {
            set.add_model(profile.default_aux_model.to_string());
        }
        for m in profile.fallback_models {
            set.add_model((*m).to_string());
        }
        set
    }

    /// Build a set from an iterator of available model IDs.
    pub fn from_models<I>(models: I) -> Self
    where
        I: IntoIterator<Item = String>,
    {
        let mut set = Self::new();
        for m in models {
            set.add_model(m);
        }
        set
    }

    /// Add a known-available model ID.
    pub fn add_model(&mut self, model: String) {
        let family = ModelFamily::detect(&model);
        self.family_index
            .entry(family)
            .or_default()
            .push(model.clone());
        self.models.insert(model);
    }

    /// Add a connected provider name (for requiresProvider checks).
    pub fn add_provider(&mut self, provider: String) {
        self.connected_providers.insert(provider);
    }

    /// Does the set contain this exact model ID?
    pub fn contains_exact(&self, model: &str) -> bool {
        self.models.contains(model)
    }

    /// Does the set contain ANY model in this family?
    pub fn contains_family(&self, family: ModelFamily) -> bool {
        self.family_index.contains_key(&family)
    }

    /// Return the first available model ID in this family (if any).
    /// Used for BC-007 fuzzy resolution.
    pub fn first_in_family(&self, family: ModelFamily) -> Option<&str> {
        self.family_index
            .get(&family)
            .and_then(|v| v.first())
            .map(|s| s.as_str())
    }

    /// Is this provider connected?
    pub fn has_provider(&self, provider: &str) -> bool {
        self.connected_providers.contains(provider)
    }

    /// True if any of the listed providers is connected.
    pub fn has_any_provider(&self, providers: &[String]) -> bool {
        providers
            .iter()
            .any(|p| self.connected_providers.contains(p))
    }

    /// Number of available models.
    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// Is the set empty?
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }
}

// ── Resolution ──────────────────────────────────────────────────────

/// The result of resolving a fallback chain: the model ID and optional
/// variant.
pub type ResolvedModel = (String, Option<String>);

/// Walk a fallback chain and return the first resolvable model.
///
/// Algorithm (contracts/model-fallback.md BC-006 → BC-010):
///   1. Try each entry in declared order.
///   2. For each entry: try exact model ID match first (BC-007).
///   3. If no exact match, try family-level fuzzy match (BC-007).
///   4. If no entry resolves, return None (agent skipped — BC-008).
///
/// The `requiresProvider` constraint (BC-010) is checked by the caller
/// (registry) BEFORE invoking this function, because it gates the entire
/// agent rather than the chain.
pub fn resolve_model(
    requirement: &ModelRequirement,
    available: &AvailableModelSet,
) -> Option<ResolvedModel> {
    for entry in &requirement.fallback_chain {
        // Exact match first (BC-007).
        if available.contains_exact(&entry.model) {
            return Some((entry.model.clone(), entry.variant.clone()));
        }
        // Family-level fuzzy match (BC-007).
        let family = ModelFamily::detect(&entry.model);
        if let Some(fuzzy_model) = available.first_in_family(family) {
            return Some((fuzzy_model.to_string(), entry.variant.clone()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T016: ModelFamily::detect() correctly classifies known model IDs.
    #[test]
    fn detect_classifies_known_models() {
        assert_eq!(
            ModelFamily::detect("claude-opus-4-8"),
            ModelFamily::Anthropic
        );
        assert_eq!(
            ModelFamily::detect("claude-sonnet-4-6"),
            ModelFamily::Anthropic
        );
        assert_eq!(ModelFamily::detect("gpt-5.6-sol"), ModelFamily::Gpt);
        assert_eq!(ModelFamily::detect("kimi-k3"), ModelFamily::Kimi);
        assert_eq!(ModelFamily::detect("glm-5"), ModelFamily::Glm);
        assert_eq!(ModelFamily::detect("gemini-3.1-pro"), ModelFamily::Gemini);
        assert_eq!(ModelFamily::detect("minimax-m3"), ModelFamily::Minimax);
        assert_eq!(ModelFamily::detect("MiniMax-M3"), ModelFamily::Minimax);
        assert_eq!(ModelFamily::detect("unknown-model"), ModelFamily::Unknown);
        assert_eq!(ModelFamily::detect("big-pickle"), ModelFamily::Unknown);
    }

    /// T017: resolve_model with only Anthropic available resolves sisyphus
    /// chain entry 1 (claude-opus-4-8 exact match); with only Glm available
    /// resolves entry 4 (glm-5 family match); with no providers returns None.
    #[test]
    fn resolve_model_exact_and_family_match() {
        // Sisyphus chain: opus → kimi-k3 → gpt → glm-5 → big-pickle
        let chain = ModelRequirement {
            fallback_chain: vec![
                FallbackEntry::new("claude-opus-4-8", Some("max"), &["anthropic"]),
                FallbackEntry::new("kimi-k3", None, &["kimi-for-coding"]),
                FallbackEntry::new("gpt-5.6-sol", Some("medium"), &["openai"]),
                FallbackEntry::new("glm-5", None, &["zai"]),
                FallbackEntry::new("big-pickle", None, &["opencode"]),
            ],
            requires_any_model: true,
            requires_provider: None,
        };

        // Only Anthropic available → entry 1 exact match
        let anthropic_only =
            AvailableModelSet::from_models(["claude-opus-4-8".to_string()].into_iter());
        let (model, variant) = resolve_model(&chain, &anthropic_only).unwrap();
        assert_eq!(model, "claude-opus-4-8");
        assert_eq!(variant.as_deref(), Some("max"));

        // Only GLM available → entry 4 family match (glm-5)
        let glm_only = AvailableModelSet::from_models(["glm-5".to_string()].into_iter());
        let (model2, _) = resolve_model(&chain, &glm_only).unwrap();
        assert_eq!(model2, "glm-5");

        // No providers → None
        let empty = AvailableModelSet::new();
        assert!(resolve_model(&chain, &empty).is_none());
    }

    /// T018: resolve_model respects chain order — if entries 1-3 unavailable
    /// but entry 4 available, entry 4 is selected (not entry 1).
    #[test]
    fn resolve_model_respects_chain_order() {
        let chain = ModelRequirement {
            fallback_chain: vec![
                FallbackEntry::new("claude-opus-4-8", None, &[]),
                FallbackEntry::new("kimi-k3", None, &[]),
                FallbackEntry::new("gpt-5.6-sol", None, &[]),
                FallbackEntry::new("glm-5", None, &[]),
            ],
            requires_any_model: false,
            requires_provider: None,
        };
        // Only glm-5 available → entry 4 wins
        let available = AvailableModelSet::from_models(["glm-5".to_string()].into_iter());
        let (model, _) = resolve_model(&chain, &available).unwrap();
        assert_eq!(model, "glm-5");
    }

    /// T082 (partial): family-level fuzzy match resolves when only a
    /// different model in the same family is available.
    #[test]
    fn resolve_model_family_fuzzy_match() {
        let chain = ModelRequirement {
            fallback_chain: vec![
                FallbackEntry::new("claude-opus-4-8", None, &[]),
                FallbackEntry::new("glm-5", None, &[]),
            ],
            requires_any_model: false,
            requires_provider: None,
        };
        // glm-5.2 is available but chain asks for glm-5 → family match
        let available = AvailableModelSet::from_models(["glm-5.2".to_string()].into_iter());
        let (model, _) = resolve_model(&chain, &available).unwrap();
        // Family match returns the first available GLM model (glm-5.2)
        assert_eq!(model, "glm-5.2");
    }

    /// from_connected() registers the provider under every alias + seeds model
    /// IDs from the active model + profile catalog.
    #[test]
    fn from_connected_registers_aliases_and_models() {
        // zai aliases include "glm"; fallback_models include glm-5.2/glm-5.
        let profile = joey_providers::profile::get_profile("zai").unwrap();
        let set = AvailableModelSet::from_connected(&profile, "glm-5.2");
        assert!(set.has_provider("zai"));
        assert!(set.has_provider("glm"), "alias 'glm' must be registered");
        assert!(set.contains_exact("glm-5.2"), "active model seeded");
        assert!(set.contains_exact("glm-5"), "fallback model seeded");
        assert!(set.contains_family(ModelFamily::Glm));
    }

    /// from_connected() makes OpenAI alias "openai" available for BC-010.
    #[test]
    fn from_connected_openai_alias_matches_chain_namespace() {
        let profile = joey_providers::profile::get_profile("openai-api").unwrap();
        let set = AvailableModelSet::from_connected(&profile, "gpt-5.6-sol");
        // The OMO hephaestus chain references the namespace "openai".
        assert!(
            set.has_provider("openai"),
            "openai alias must be registered"
        );
        assert!(set.contains_exact("gpt-5.6-sol"));
    }

    /// BC-010: billing plan aliases are registered for zai provider.
    #[test]
    fn from_connected_registers_billing_plan_aliases_for_zai() {
        let profile = joey_providers::profile::get_profile("zai").unwrap();
        let set = AvailableModelSet::from_connected(&profile, "glm-5.2");
        // OMO fallback chains reference these billing namespace names
        assert!(set.has_provider("zai-coding-plan"));
        assert!(set.has_provider("bailian-coding-plan"));
        assert!(set.has_provider("moonshotai-cn"));
        assert!(set.has_provider("opencode-go"));
    }

    /// BC-010: billing plan aliases are registered for openrouter provider.
    #[test]
    fn from_connected_registers_billing_plan_aliases_for_openrouter() {
        let profile = joey_providers::profile::get_profile("openrouter").unwrap();
        let set = AvailableModelSet::from_connected(&profile, "claude-opus-4-8");
        // OMO fallback chains reference these billing namespace names
        assert!(set.has_provider("opencode"));
        assert!(set.has_provider("vercel"));
        assert!(set.has_provider("kimi-for-coding"));
        assert!(set.has_provider("moonshotai"));
        assert!(set.has_provider("ollama-cloud"));
        assert!(set.has_provider("aihubmix"));
    }
}
