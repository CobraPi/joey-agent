//! `ModelAllocator` — the narrow trait `joey-agent-core` consumes (T012).
//!
//! This is the ONLY public surface the agent turn loop depends on (Constitution
//! VI). Everything else in this crate is private to the engine.

use crate::map::FailureSignal;
use crate::module::ModuleId;

/// Narrow interface the agent turn loop consumes to resolve which model a
/// given module should call.
///
/// Hot-path methods are non-async and run in O(1) off the per-turn cache.
/// `record_observation` forwards to the detached diagnoser task and MUST NOT
/// block the caller.
pub trait ModelAllocator: Send + Sync {
    /// Resolve the model id for `module` on the current turn.
    ///
    /// - Served from the per-turn cache (FR-007); O(1), no network.
    /// - Honors pinned entries verbatim (FR-012).
    /// - Guarantees the returned id satisfies the module's hard capability
    ///   requirements (FR-005); stale ids are re-resolved (FR-014).
    /// - Returns the literal configured model when the selector is disabled.
    fn resolve(
        &self,
        module: ModuleId,
        turn_has_images: bool,
        needs_tools: bool,
        token_budget_hint: u64,
    ) -> Allocation;

    /// Called at the start of every turn to refresh the per-turn cache from
    /// the on-disk allocation map (FR-007). No-op when disabled.
    fn refresh_at_turn_start(&self);

    /// Whether dynamic allocation is active for the current session.
    fn is_active(&self) -> bool;

    /// Forward an observation to the detached diagnoser (FR-008, FR-009).
    /// Returns immediately; never blocks the interactive turn.
    fn record_observation(
        &self,
        module: ModuleId,
        signal: FailureSignal,
        module_input_summary: &str,
        module_output: &str,
    );

    /// The highest available context window for the model allocated to `module`
    /// (FR-019). Used by call sites to avoid capping below the catalog maximum.
    fn context_window_for(&self, module: ModuleId) -> u64;

    /// Report that `model_id` returned a permanent error at call time
    /// (FR-015 acceptance 2). The selector invalidates its cached allocation
    /// for `module` and marks the entry for re-evaluation so the next `resolve`
    /// picks a live model instead of the dead one. Default no-op so existing
    /// trait objects and tests are unaffected (Constitution VII additive).
    ///
    /// Call sites invoke this when the provider returns a non-retryable
    /// `ModelNotFound`-class error for the model the selector chose. The
    /// existing runtime fallback chain (`try_activate_fallback`) handles
    /// substituting a fallback for the current call; this method ensures the
    /// selector does not keep re-resolving to the dead model on subsequent
    /// turns (research.md §8).
    fn report_permanent_error(&self, _module: ModuleId, _model_id: &str) {}
}

/// A resolved allocation result.
#[derive(Debug, Clone)]
pub struct Allocation {
    /// The model id to send to the API (never "auto"; always concrete).
    pub model_id: String,
    /// Where this allocation came from (for diagnostics).
    pub source: AllocationSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationSource {
    /// Served from the per-turn cache (diagnoser-influenced or cold-start).
    Cached,
    /// Re-resolved this call because the cached id was stale (FR-014).
    ColdStartReresolve,
    /// Selector disabled / `auto` not active — literal configured model.
    DisabledFallback,
    /// Catalog error — fell back to last-known-good or provider fallback (FR-015).
    DegradedFallback,
}
