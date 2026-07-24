//! AgentRegistry: constructs all 11 OmoAgent definitions, resolves models.
//!
//! Port of contracts/agent-registry.md. The registry builds the 11 agents,
//! resolves each model via fallback chains, and marks skipped agents
//! (resolved_model=None).

use crate::agents::{
    atlas_requirement,
    explore_requirement,
    hephaestus_requirement,
    librarian_requirement,
    metis_requirement,
    momus_requirement,
    multimodal_requirement,
    oracle_requirement,
    prometheus_requirement,
    sisyphus_junior_requirement,
    sisyphus_requirement,
    OmoAgent,
};
use crate::categories::{builtin_categories, CategoryConfig};
use crate::mode::{AgentMode, ToolPermissions};
use crate::models::{resolve_model, AvailableModelSet};

/// The registry of all 11 built-in OMO agents, resolved against available
/// providers.
pub struct AgentRegistry {
    agents: Vec<OmoAgent>,
    categories: Vec<CategoryConfig>,
    available_models: AvailableModelSet,
}

/// User-model override map: agent name → model.
/// If config has `omo.agents.<name>.model` set, that model is used directly
/// (BC-009), bypassing the chain.
pub type ModelOverrides = std::collections::HashMap<String, String>;

impl AgentRegistry {
    /// Build the registry, resolving each agent's model via fallback chains
    /// (BC-001 through BC-005). Agents whose models are all unavailable are
    /// marked skipped (resolved_model=None, not dropped).
    pub fn build(available: AvailableModelSet, overrides: &ModelOverrides) -> Self {
        let agents = build_all_agents(&available, overrides);
        let categories = builtin_categories();
        Self {
            agents,
            categories,
            available_models: available,
        }
    }

    /// All 11 agents (including skipped ones) — BC-001.
    pub fn all(&self) -> &[OmoAgent] {
        &self.agents
    }

    /// Only primary agents available for Tab selection (model resolved) — BC-002.
    pub fn available_primary(&self) -> Vec<&OmoAgent> {
        self.agents
            .iter()
            .filter(|a| a.mode.is_primary() && a.resolved_model.is_some())
            .collect()
    }

    /// Canonical Tab order: Sisyphus → Hephaestus → Prometheus → Atlas.
    /// "Default" (the existing joey-agent agent) is prepended by the caller
    /// (joey-cli/joey-tui) which owns the default.
    pub fn tab_order(&self) -> Vec<&OmoAgent> {
        let order = ["sisyphus", "hephaestus", "prometheus", "atlas"];
        order
            .iter()
            .filter_map(|name| self.agents.iter().find(|a| a.name == *name && a.is_available()))
            .collect()
    }

    /// Look up an agent by canonical name.
    pub fn get(&self, name: &str) -> Option<&OmoAgent> {
        self.agents.iter().find(|a| a.name == name)
    }

    /// All 11 categories.
    pub fn categories(&self) -> &[CategoryConfig] {
        &self.categories
    }

    /// The available model set used to build this registry.
    pub fn available_models(&self) -> &AvailableModelSet {
        &self.available_models
    }
}

/// Construct all 11 OmoAgent definitions with correct names, colors, modes,
/// model requirements, and tool permissions. Resolves each model.
fn build_all_agents(
    available: &AvailableModelSet,
    overrides: &ModelOverrides,
) -> Vec<OmoAgent> {
    let mut agents = Vec::with_capacity(11);

    // ── Primary agents ──────────────────────────────────────────────

    // Sisyphus — the default OMO orchestrator
    agents.push(build_agent(
        "sisyphus",
        "Sisyphus",
        AgentMode::Primary,
        "#6C5CE7",
        "The primary orchestrator — delegates, verifies, manages todos",
        sisyphus_requirement(),
        0.1,
        None,
        ToolPermissions::allow_all(),
        available,
        overrides,
    ));

    // Hephaestus — GPT-only; requires OpenAI-class provider
    agents.push(build_agent(
        "hephaestus",
        "Hephaestus",
        AgentMode::Primary,
        "#D97706",
        "GPT-powered specialist for high-precision coding (requires OpenAI)",
        hephaestus_requirement(),
        0.1,
        Some(32000),
        ToolPermissions::allow_all(),
        available,
        overrides,
    ));

    // Prometheus — read-only planner
    agents.push(build_agent(
        "prometheus",
        "Prometheus",
        AgentMode::Primary,
        "#8B5CF6",
        "Read-only planning consultant — creates plans, never implements",
        prometheus_requirement(),
        0.1,
        None,
        ToolPermissions::allow_all(),
        available,
        overrides,
    ));

    // Atlas — conductor; never writes code
    agents.push(build_agent(
        "atlas",
        "Atlas",
        AgentMode::Primary,
        "#10B981",
        "Master orchestrator — delegates all implementation, verifies results",
        atlas_requirement(),
        0.1,
        None,
        ToolPermissions::allow_all(),
        available,
        overrides,
    ));

    // ── Subagent agents ─────────────────────────────────────────────

    // Oracle — architecture consultant
    agents.push(build_agent(
        "oracle",
        "Oracle",
        AgentMode::Subagent,
        "#3B82F6",
        "Architecture consultant — read-only analysis and design",
        oracle_requirement(),
        0.1,
        None,
        ToolPermissions::allow_all(),
        available,
        overrides,
    ));

    // Librarian — docs/OSS search
    agents.push(build_agent(
        "librarian",
        "Librarian",
        AgentMode::Subagent,
        "#F59E0B",
        "Documentation and OSS search specialist",
        librarian_requirement(),
        0.1,
        None,
        ToolPermissions::allow_all(),
        available,
        overrides,
    ));

    // Explore — fast codebase grep
    agents.push(build_agent(
        "explore",
        "Explore",
        AgentMode::Subagent,
        "#06B6D4",
        "Fast codebase grep and search agent",
        explore_requirement(),
        0.1,
        None,
        ToolPermissions::allow_all(),
        available,
        overrides,
    ));

    // Multimodal-Looker — vision/screenshot analysis
    agents.push(build_agent(
        "multimodal-looker",
        "Multimodal-Looker",
        AgentMode::Subagent,
        "#EC4899",
        "Vision and screenshot analysis agent",
        multimodal_requirement(),
        0.1,
        None,
        ToolPermissions::allow_all(),
        available,
        overrides,
    ));

    // Metis — gap analyzer
    agents.push(build_agent(
        "metis",
        "Metis",
        AgentMode::Subagent,
        "#A855F7",
        "Gap analyzer — identifies missing pieces in plans and implementations",
        metis_requirement(),
        0.1,
        None,
        ToolPermissions::allow_all(),
        available,
        overrides,
    ));

    // Momus — plan reviewer
    agents.push(build_agent(
        "momus",
        "Momus",
        AgentMode::Subagent,
        "#EF4444",
        "Plan reviewer — critical analysis and quality assessment",
        momus_requirement(),
        0.1,
        None,
        ToolPermissions::allow_all(),
        available,
        overrides,
    ));

    // Sisyphus-Junior — task executor; blocks `task` tool (BC-005)
    let mut junior_perms = ToolPermissions::new(
        vec!["call_omo_agent".to_string()],
        vec!["task".to_string(), "delegate_task".to_string()],
    );
    // Allow default toolset tools
    junior_perms.allow("read_file");
    junior_perms.allow("write_file");
    junior_perms.allow("patch");
    junior_perms.allow("terminal");
    junior_perms.allow("search_files");
    junior_perms.allow("web_search");
    junior_perms.allow("web_extract");
    junior_perms.allow("todo");
    junior_perms.allow("skills");

    agents.push(build_agent(
        "sisyphus-junior",
        "Sisyphus-Junior",
        AgentMode::Subagent,
        "#20B2AA",
        "Focused task executor — no delegation, todo discipline, verification gate",
        sisyphus_junior_requirement(),
        0.1,
        Some(64000),
        junior_perms,
        available,
        overrides,
    ));

    debug_assert_eq!(agents.len(), 11, "exactly 11 agents");
    agents
}

/// Build a single agent, resolving its model via fallback chain.
/// Honors user overrides (BC-009) and requiresProvider constraint (BC-010).
fn build_agent(
    name: &str,
    display_name: &str,
    mode: AgentMode,
    color: &str,
    description: &str,
    mut requirement: crate::models::ModelRequirement,
    temperature: f64,
    max_tokens: Option<u32>,
    tool_permissions: ToolPermissions,
    available: &AvailableModelSet,
    overrides: &ModelOverrides,
) -> OmoAgent {
    // BC-010: requiresProvider constraint checked first.
    if let Some(ref providers) = requirement.requires_provider {
        if !available.has_any_provider(providers) {
            return OmoAgent {
                name: name.to_string(),
                display_name: display_name.to_string(),
                mode,
                color: color.to_string(),
                description: description.to_string(),
                model_requirement: requirement,
                resolved_model: None,
                resolved_variant: None,
                temperature,
                max_tokens,
                tool_permissions,
            };
        }
    }

    // BC-009: user override bypasses the chain entirely.
    if let Some(override_model) = overrides.get(name) {
        return OmoAgent {
            name: name.to_string(),
            display_name: display_name.to_string(),
            mode,
            color: color.to_string(),
            description: description.to_string(),
            model_requirement: requirement,
            resolved_model: Some(override_model.clone()),
            resolved_variant: None,
            temperature,
            max_tokens,
            tool_permissions,
        };
    }

    // Normal chain resolution.
    let resolved = resolve_model(&requirement, available);

    // requiresAnyModel: if true and nothing resolved, agent is skipped.
    // (This is already handled by resolve_model returning None.)
    let _ = &mut requirement; // suppress unused_mut warning

    let (resolved_model, resolved_variant) = match resolved {
        Some((m, v)) => (Some(m), v),
        None => (None, None),
    };

    OmoAgent {
        name: name.to_string(),
        display_name: display_name.to_string(),
        mode,
        color: color.to_string(),
        description: description.to_string(),
        model_requirement: requirement,
        resolved_model,
        resolved_variant,
        temperature,
        max_tokens,
        tool_permissions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T019: AgentRegistry::build() produces exactly 11 agents with canonical names.
    #[test]
    fn registry_produces_eleven_agents() {
        let available = AvailableModelSet::from_models(
            ["claude-opus-4-8".to_string(), "gpt-5.6-sol".to_string()].into_iter(),
        );
        let registry = AgentRegistry::build(available, &ModelOverrides::new());

        assert_eq!(registry.all().len(), 11, "exactly 11 agents (BC-001)");

        let names: Vec<&str> = registry.all().iter().map(|a| a.name.as_str()).collect();
        let expected = [
            "sisyphus",
            "hephaestus",
            "prometheus",
            "atlas",
            "oracle",
            "librarian",
            "explore",
            "multimodal-looker",
            "metis",
            "momus",
            "sisyphus-junior",
        ];
        for name in &expected {
            assert!(names.contains(name), "agent '{}' must be in the registry", name);
        }
    }

    /// T019: available_primary() returns only primary agents with resolved models.
    #[test]
    fn available_primary_returns_resolved_primary_agents() {
        let available = AvailableModelSet::from_models(
            ["claude-opus-4-8".to_string(), "gpt-5.6-sol".to_string()].into_iter(),
        );
        let registry = AgentRegistry::build(available, &ModelOverrides::new());

        let primary = registry.available_primary();
        for agent in &primary {
            assert!(agent.mode.is_primary(), "all must be primary");
            assert!(agent.resolved_model.is_some(), "all must have resolved model");
        }
        // Sisyphus and Prometheus should resolve (opus), Atlas won't (needs sonnet).
        let names: Vec<&str> = primary.iter().map(|a| a.name.as_str()).collect();
        assert!(names.contains(&"sisyphus"));
        assert!(names.contains(&"prometheus"));
    }

    /// T020: Hephaestus skipped when no OpenAI-class provider connected.
    #[test]
    fn hephaestus_skipped_without_openai_provider() {
        // Only anthropic model, no openai provider
        let mut available = AvailableModelSet::new();
        available.add_model("claude-opus-4-8".into());
        // No providers added → Hephaestus requiresProvider check fails

        let registry = AgentRegistry::build(available, &ModelOverrides::new());
        let hephaestus = registry.get("hephaestus").unwrap();
        assert!(
            hephaestus.resolved_model.is_none(),
            "Hephaestus must be skipped without OpenAI provider (BC-003)"
        );
    }

    /// T020: Hephaestus available when OpenAI provider connected.
    #[test]
    fn hephaestus_available_with_openai_provider() {
        let mut available = AvailableModelSet::new();
        available.add_model("gpt-5.6-sol".into());
        available.add_provider("openai".into());

        let registry = AgentRegistry::build(available, &ModelOverrides::new());
        let hephaestus = registry.get("hephaestus").unwrap();
        assert!(
            hephaestus.resolved_model.is_some(),
            "Hephaestus must be available with OpenAI provider + GPT model"
        );
    }

    /// T052: canonical agent names match exactly.
    #[test]
    fn canonical_agent_names() {
        let registry = AgentRegistry::build(AvailableModelSet::new(), &ModelOverrides::new());
        let names: Vec<&str> = registry.all().iter().map(|a| a.name.as_str()).collect();
        let expected = [
            "sisyphus",
            "hephaestus",
            "prometheus",
            "atlas",
            "oracle",
            "librarian",
            "explore",
            "multimodal-looker",
            "metis",
            "momus",
            "sisyphus-junior",
        ];
        assert_eq!(names, expected);
    }

    /// T083: user override bypasses the chain (BC-009).
    #[test]
    fn user_override_bypasses_chain() {
        let available = AvailableModelSet::new(); // empty — nothing resolves
        let mut overrides = ModelOverrides::new();
        overrides.insert("sisyphus".into(), "custom-model".into());

        let registry = AgentRegistry::build(available, &overrides);
        let sisyphus = registry.get("sisyphus").unwrap();
        assert_eq!(
            sisyphus.resolved_model.as_deref(),
            Some("custom-model"),
            "user override must bypass the chain (BC-009)"
        );
    }

    /// Sisyphus-Junior tool permissions (BC-005).
    #[test]
    fn junior_blocks_task_allows_call_omo_agent() {
        let registry = AgentRegistry::build(AvailableModelSet::new(), &ModelOverrides::new());
        let junior = registry.get("sisyphus-junior").unwrap();
        assert!(!junior.tool_permissions.is_allowed("task"), "task must be denied");
        assert!(
            junior.tool_permissions.is_allowed("call_omo_agent"),
            "call_omo_agent must be allowed"
        );
    }

    /// Tab order is Sisyphus → Hephaestus → Prometheus → Atlas.
    #[test]
    fn tab_order_is_canonical() {
        let mut available = AvailableModelSet::new();
        available.add_model("claude-opus-4-8".into());
        available.add_model("gpt-5.6-sol".into());
        available.add_provider("openai".into());
        available.add_model("claude-sonnet-4-6".into());

        let registry = AgentRegistry::build(available, &ModelOverrides::new());
        let order: Vec<&str> = registry.tab_order().iter().map(|a| a.name.as_str()).collect();
        assert_eq!(order, vec!["sisyphus", "hephaestus", "prometheus", "atlas"]);
    }
}
