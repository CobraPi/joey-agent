//! OMO category/subagent_type resolution for the delegate_task tool.
//!
//! Bridges joey-omo (which holds the AgentRegistry with category model chains)
//! and joey-orchestration (which holds the delegate_task tool) without creating
//! a circular dependency. The CategoryResolver trait is defined in
//! joey-orchestration; this module implements it using joey_omo::resolve_category.

use std::sync::{Arc, Mutex};

use joey_omo::AgentRegistry;
use joey_orchestration::{CategoryResolver, ResolvedDelegation};

/// CategoryResolver backed by a joey_omo::AgentRegistry.
/// The registry is held in a Mutex<Option<_>> so it can be populated after
/// agent construction (when the provider profile + active model become known).
pub struct OmoCategoryResolver {
    registry: Mutex<Option<AgentRegistry>>,
}

impl OmoCategoryResolver {
    pub fn new() -> Self {
        Self {
            registry: Mutex::new(None),
        }
    }

    /// Populate the resolver with a built AgentRegistry (called after agent
    /// construction when the provider profile is available).
    pub fn populate(&self, registry: AgentRegistry) {
        *self.registry.lock().unwrap() = Some(registry);
    }
}

impl Default for OmoCategoryResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl CategoryResolver for OmoCategoryResolver {
    fn resolve_category(&self, name: &str) -> Option<ResolvedDelegation> {
        let guard = self.registry.lock().unwrap();
        let registry = guard.as_ref()?;
        joey_omo::resolve_category(name, registry).map(|rc| ResolvedDelegation {
            model: rc.model,
            prompt_append: rc.config.prompt_append,
        })
    }

    fn resolve_subagent_type(&self, name: &str) -> Option<ResolvedDelegation> {
        let guard = self.registry.lock().unwrap();
        let registry = guard.as_ref()?;
        let agent = registry.all().iter().find(|a| a.name == name)?;
        agent.resolved_model.as_ref().map(|model| ResolvedDelegation {
            model: model.clone(),
            prompt_append: None,
        })
    }
}

/// Build an OmoCategoryResolver ready for injection into register_orchestration.
/// Returns the resolver as Arc<dyn CategoryResolver>. Call `.populate()` on
/// the OmoCategoryResolver after agent construction to populate it.
pub fn build_omo_resolver() -> Arc<OmoCategoryResolver> {
    Arc::new(OmoCategoryResolver::new())
}

