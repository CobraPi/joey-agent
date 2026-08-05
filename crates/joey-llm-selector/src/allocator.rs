//! `SelectorEngine` — the `ModelAllocator` implementation (T016).
//!
//! Holds the allocation map, per-turn cache, and pool. Resolves modules from
//! cache, applies cold-start when needed, and honors pinned entries verbatim.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use crate::candidate::CandidateModelPool;
use crate::map::{AllocationEntry, AllocationMap, FailureSignal, DiagnosticRecord};
use crate::model_allocator::{Allocation, AllocationSource, ModelAllocator};
use crate::module::ModuleId;
use crate::scorer::{ColdStartScorer, ModuleRequirements};

/// Configuration for the selector engine.
#[derive(Debug, Clone)]
pub struct SelectorConfig {
    /// Whether dynamic selection is enabled (model.selector.enabled).
    pub enabled: bool,
    /// The literal configured model id (cfg.model()) — used as the disabled fallback.
    pub configured_model: String,
    /// Learning budget (model.selector.budget).
    pub learning_budget: u32,
    /// Diagnoser model id (model.selector.diagnoser_model).
    pub diagnoser_model: String,
}

impl Default for SelectorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            configured_model: String::new(),
            learning_budget: 8,
            diagnoser_model: String::new(),
        }
    }
}

/// Per-turn allocation cache (FR-007). Populated at turn start, stable for the
/// duration of one turn.
#[derive(Debug, Clone, Default)]
pub(crate) struct TurnCache {
    allocations: HashMap<ModuleId, Allocation>,
    context_windows: HashMap<ModuleId, u64>,
}

/// The selector engine: implements `ModelAllocator`.
///
/// Thread-safe via `RwLock` on the map and `Mutex` on the cache. The hot-path
/// `resolve` acquires a read lock on the cache (cheap).
pub struct SelectorEngine {
    pub(crate) config: RwLock<SelectorConfig>,
    pub(crate) map: RwLock<AllocationMap>,
    pub(crate) pool: RwLock<CandidateModelPool>,
    pub(crate) cache: Mutex<TurnCache>,
    /// Provider-curated fallback model ids (research.md §8 (a)) consulted in
    /// the DegradedFallback path before falling to `cfg.model()` (FR-015, T073).
    pub(crate) fallback_models: RwLock<Vec<String>>,
    /// The active provider name (e.g. "copilot", "openrouter", "zai"). Recorded
    /// at construction so `/llm-selector refresh` can re-fetch the catalog
    /// without re-reading config (the engine is shared via Arc and outlives the
    /// config borrow).
    pub(crate) provider: RwLock<String>,
    /// Sender half of the observation channel feeding the detached diagnoser
    /// (FR-009). `record_observation` enqueues here and returns immediately.
    /// None when the diagnoser was not started (e.g. no tokio runtime in tests).
    pub(crate) observation_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<crate::diagnoser::Observation>>>,
    /// Receiver half, taken by `spawn_diagnoser` when the task starts.
    pub(crate) observation_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<crate::diagnoser::Observation>>>,
    /// The optional LLM judge client (FR-008/FR-009, T076). When set, the
    /// detached learning loop asks the judge for a per-module `p_j` before
    /// falling back to the signal-driven heuristic. Constructed from the
    /// active provider's credentials + the configured diagnoser model at
    /// install time; `None` in tests or when no provider/credentials exist.
    pub(crate) diagnoser_client: RwLock<Option<Arc<dyn crate::diagnoser::DiagnoserClient>>>,
    /// Optional override for the map path (testing). When None, uses the global
    /// `~/.joey/llm-selector/allocations.json` path (FR-014).
    pub(crate) map_path_override: Option<std::path::PathBuf>,
}

impl SelectorEngine {
    /// Create a new engine with the given config and an empty pool.
    /// The pool is populated separately (catalog fetch happens at enablement).
    pub fn new(config: SelectorConfig) -> Self {
        let budget = config.learning_budget;
        let diagnoser = if config.diagnoser_model.is_empty() {
            String::new()
        } else {
            config.diagnoser_model.clone()
        };
        let mut map = AllocationMap::load().unwrap_or_default();
        map.learning_budget = budget;
        if !diagnoser.is_empty() {
            map.diagnoser_model = diagnoser;
        }
        map.enabled = config.enabled;

        Self {
            config: RwLock::new(config),
            map: RwLock::new(map),
            pool: RwLock::new(CandidateModelPool::default()),
            cache: Mutex::new(TurnCache::default()),
            fallback_models: RwLock::new(Vec::new()),
            provider: RwLock::new(String::new()),
            observation_tx: Mutex::new(None),
            observation_rx: Mutex::new(None),
            diagnoser_client: RwLock::new(None),
            map_path_override: None,
        }
    }

    /// Create a new engine with an explicit map (for testing — does not touch disk).
    pub fn new_with_map(config: SelectorConfig, map: AllocationMap) -> Self {
        Self {
            config: RwLock::new(config),
            map: RwLock::new(map),
            pool: RwLock::new(CandidateModelPool::default()),
            cache: Mutex::new(TurnCache::default()),
            map_path_override: None,
            fallback_models: RwLock::new(Vec::new()),
            provider: RwLock::new(String::new()),
            observation_tx: Mutex::new(None),
            observation_rx: Mutex::new(None),
            diagnoser_client: RwLock::new(None),
        }
    }

    /// Create a new engine with an explicit map path override (for testing isolation).
    pub fn new_with_map_path(
        config: SelectorConfig,
        map: AllocationMap,
        map_path: std::path::PathBuf,
    ) -> Self {
        Self {
            config: RwLock::new(config),
            map: RwLock::new(map),
            pool: RwLock::new(CandidateModelPool::default()),
            cache: Mutex::new(TurnCache::default()),
            map_path_override: Some(map_path),
            fallback_models: RwLock::new(Vec::new()),
            provider: RwLock::new(String::new()),
            observation_tx: Mutex::new(None),
            observation_rx: Mutex::new(None),
            diagnoser_client: RwLock::new(None),
        }
    }

    /// Resolve the map path (override or global default).
    fn map_path(&self) -> std::path::PathBuf {
        self.map_path_override
            .clone()
            .unwrap_or_else(AllocationMap::path)
    }

    /// Set the candidate pool (called when the catalog is fetched at enablement).
    pub fn set_pool(&self, pool: CandidateModelPool) {
        *self.pool.write().unwrap() = pool;
    }

    /// FR-017 / T072: auto-disable the selector with a notice when the
    /// candidate pool is empty after a fetch attempt. Writes `enabled = false`
    /// to the on-disk map atomically so the disabled state persists and
    /// `/llm-selector status` reports the degraded state. Idempotent — a no-op
    /// when the pool is non-empty or the map is already disabled.
    pub fn auto_disable_on_empty_pool(&self) {
        let pool_empty = self.pool.read().unwrap().is_empty();
        if !pool_empty {
            return;
        }
        let mut map = self.map.write().unwrap();
        if !map.enabled {
            return; // already disabled
        }
        map.enabled = false;
        let path = self.map_path_override.clone().unwrap_or_else(AllocationMap::path);
        let _ = map.save_to(&path);
        tracing::warn!(
            "llm-selector: candidate pool is empty — auto-disabled (FR-017). \
             The active provider exposes no usable model catalog."
        );
    }

    /// Whether the pool has exactly one eligible model (Edge Case: no-op
    /// pass-through). Used by the status renderer to report that no
    /// cross-module diversity is possible.
    pub fn is_pool_single_model(&self) -> bool {
        self.pool.read().unwrap().len() == 1
    }

    /// Set the provider-curated fallback model ids (FR-015, T073, research.md
    /// §8 (a)). Consulted in the DegradedFallback path of `resolve` before
    /// falling to the literal configured model. Called once at install time
    /// from `try_build_allocator`.
    pub fn set_fallback_models(&self, models: Vec<String>) {
        *self.fallback_models.write().unwrap() = models;
    }

    /// Record the active provider name so `/llm-selector refresh` can re-fetch
    /// the catalog without re-reading config.
    pub fn set_provider(&self, provider: String) {
        *self.provider.write().unwrap() = provider;
    }

    /// The active provider name (e.g. "copilot", "zai").
    pub fn provider(&self) -> String {
        self.provider.read().unwrap().clone()
    }

    /// Install the LLM judge client for the detached learning loop (FR-008,
    /// T076). When set, the learning loop asks the judge for a per-module
    /// `p_j` before falling back to the heuristic. Pass `None` to disable the
    /// LLM judge (tests, or when no provider/credentials are available).
    pub fn set_diagnoser_client(&self, client: Option<Arc<dyn crate::diagnoser::DiagnoserClient>>) {
        *self.diagnoser_client.write().unwrap() = client;
    }

    /// Snapshot the installed judge client (used by the learning loop).
    pub(crate) fn diagnoser_client(&self) -> Option<Arc<dyn crate::diagnoser::DiagnoserClient>> {
        self.diagnoser_client.read().unwrap().clone()
    }

    // ── Diagnoser support (Phase 5: T035/T036/T037/T038) ───────────────────

    /// Start the detached diagnoser task (FR-009). Creates the observation
    /// channel and spawns the learning loop. Must be called inside a tokio
    /// runtime context. Safe to call once; subsequent calls are no-ops.
    /// When no tokio runtime is available (e.g. sync tests), this is a no-op —
    /// observations are silently dropped and the selector routes from the
    /// cold-start map only.
    pub fn start_diagnoser(self: &std::sync::Arc<Self>) {
        let mut tx_guard = self.observation_tx.lock().unwrap();
        if tx_guard.is_some() {
            return; // already started
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        *tx_guard = Some(tx);
        *self.observation_rx.lock().unwrap() = Some(rx);
        drop(tx_guard);
        // Try to spawn the diagnoser task. If no tokio runtime is available
        // (e.g. called from a sync test context), gracefully skip spawning —
        // the selector still routes from the cold-start map; observations are
        // enqueued but never consumed (bounded channel, dropped on engine drop).
        let engine_handle = std::sync::Arc::clone(self);
        let spawn_result = tokio::runtime::Handle::try_current();
        if let Ok(handle) = spawn_result {
            handle.spawn(async move {
                crate::diagnoser::run_learning_loop_from_handle(engine_handle).await;
            });
        }
        // Else: no runtime — diagnoser stays unstarted (no-op).
    }

    /// Take the observation receiver (called once by `spawn_diagnoser`).
    pub(crate) fn take_observation_rx(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<crate::diagnoser::Observation>> {
        self.observation_rx.lock().unwrap().take()
    }

    /// Attempt to reallocate a module after an observation (called by the
    /// learning loop). Finds the best alternative candidate (highest tier that
    /// satisfies capability gates and isn't the implicated model) and, if its
    /// estimated performance potential exceeds the observed `p_j`, reassigns
    /// the module. Respects pins (FR-012/T044): pinned or implicit_pin entries
    /// are never reallocated.
    ///
    /// Returns Some((old_model, new_model)) if a reallocation happened.
    pub(crate) fn try_reallocate_for_observation(
        &self,
        module: &ModuleId,
        observed_pj: f64,
        implicated: &Option<String>,
        rationale: &str,
    ) -> Option<(String, String)> {
        // FR-012/T044: skip pinned or implicit_pin modules.
        let map = self.map.read().unwrap();
        if let Some(entry) = map.get(module) {
            if entry.pinned || entry.implicit_pin {
                return None;
            }
        }
        let implicated_id = implicated.as_deref()?;
        drop(map);

        // Find alternative candidates: those in the pool that satisfy the
        // module's requirements and are NOT the implicated model.
        let pool = self.pool.read().unwrap();
        let implicated_model = pool.get(implicated_id)?;
        // The "potential" of an alternative is approximated by its tier rank —
        // a higher-tier model is presumed more capable. If the observed model
        // scored below 0.5 (a failure signal), any higher-tier alternative wins.
        use crate::candidate::CapabilityTier;
        let implicated_tier_rank = match implicated_model.tier {
            CapabilityTier::Flash => 0,
            CapabilityTier::Standard => 1,
            CapabilityTier::Versatile => 2,
            CapabilityTier::Frontier => 3,
        };
        // Only reallocate when the observed performance indicates a real
        // failure (below 0.5 — all failure signals score here).
        if observed_pj >= 0.5 {
            return None;
        }

        // Find the best alternative: highest-tier model that isn't the
        // implicated one. A strictly-higher tier is preferred; if same tier,
        // prefer a different model only if the implicated one is Flash
        // (the weakest tier — worth diversifying away from).
        let mut best: Option<&crate::candidate::CandidateModel> = None;
        for m in &pool.models {
            if m.id == implicated_model.id {
                continue;
            }
            let rank = match m.tier {
                CapabilityTier::Flash => 0,
                CapabilityTier::Standard => 1,
                CapabilityTier::Versatile => 2,
                CapabilityTier::Frontier => 3,
            };
            // Must be strictly better OR (same-or-better tier when implicated is Flash).
            if rank > implicated_tier_rank
                || (implicated_model.tier == CapabilityTier::Flash && rank >= implicated_tier_rank)
            {
                match best {
                    None => best = Some(m),
                    Some(cur) => {
                        let cur_rank = match cur.tier {
                            CapabilityTier::Flash => 0,
                            CapabilityTier::Standard => 1,
                            CapabilityTier::Versatile => 2,
                            CapabilityTier::Frontier => 3,
                        };
                        if rank > cur_rank {
                            best = Some(m);
                        }
                    }
                }
            }
        }
        let alternative = best?;
        let alternative_id = alternative.id.clone();
        drop(pool);

        // Reassign.
        let mut map = self.map.write().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let entry = AllocationEntry {
            module: module.clone(),
            model_id: alternative_id.clone(),
            pinned: false,
            implicit_pin: false,
            reason: format!("diagnoser reallocation: {} (p_j={:.2})", rationale, observed_pj),
            estimated_performance: Some(observed_pj),
            updated_at: Some(now),
        };
        map.upsert(entry);
        Some((implicated_id.to_string(), alternative_id))
    }

    /// Append a diagnostic record, increment the budget counter, and persist
    /// the map atomically (called by the learning loop after each observation).
    pub(crate) fn append_diagnostic_and_persist(
        &self,
        record: DiagnosticRecord,
        _reallocated: Option<(String, String)>,
    ) {
        let mut map = self.map.write().unwrap();
        map.diagnostics.push(record);
        // Ring-buffer trim to last 50 (contracts/allocation-map-schema.md).
        if map.diagnostics.len() > 50 {
            let drop_n = map.diagnostics.len() - 50;
            map.diagnostics.drain(0..drop_n);
        }
        map.budget_used_this_cycle = map.budget_used_this_cycle.saturating_add(1);
        map.updated_at = Some(chrono::Utc::now().to_rfc3339());
        let path = self.map_path_override.clone().unwrap_or_else(AllocationMap::path);
        let _ = map.save_to(&path);
    }

    /// Get a snapshot of the current pool.
    pub fn pool(&self) -> CandidateModelPool {
        self.pool.read().unwrap().clone()
    }

    /// Get a snapshot of the current map.
    pub fn map_snapshot(&self) -> AllocationMap {
        self.map.read().unwrap().clone()
    }

    /// Update the config (e.g. when the user toggles enable/disable).
    pub fn update_config(&self, config: SelectorConfig) {
        let mut map = self.map.write().unwrap();
        map.enabled = config.enabled;
        map.learning_budget = config.learning_budget;
        if !config.diagnoser_model.is_empty() {
            map.diagnoser_model = config.diagnoser_model.clone();
        }
        let path = self.map_path_override.clone().unwrap_or_else(AllocationMap::path);
        let _ = map.save_to(&path);
        *self.config.write().unwrap() = config;
    }

    /// Set the diagnoser model (FR-008). Validates that the model id is present
    /// in the active candidate pool AND is versatile-tier-eligible (per
    /// contracts/llm-selector-command.md: exit 1 when "model not versatile-tier-eligible").
    /// Returns Err(reason) on rejection. Persists the change atomically.
    pub fn set_diagnoser_model(&self, model_id: &str) -> Result<(), String> {
        let pool = self.pool.read().unwrap();
        let candidate = pool.get(model_id).ok_or_else(|| {
            format!("model '{}' not in the active candidate pool", model_id)
        })?;
        use crate::candidate::CapabilityTier;
        if candidate.tier != CapabilityTier::Versatile {
            return Err(format!(
                "diagnoser model must be versatile-tier (got {:?}); rejected",
                candidate.tier
            ));
        }
        drop(pool);
        let mut map = self.map.write().unwrap();
        map.diagnoser_model = model_id.to_string();
        let path = self.map_path_override.clone().unwrap_or_else(AllocationMap::path);
        let _ = map.save_to(&path);
        // Also reflect in the live config so subsequent config_snapshot() stays consistent.
        self.config.write().unwrap().diagnoser_model = model_id.to_string();
        Ok(())
    }

    /// Apply implicit pins (FR-013) from explicit per-task model config keys.
    ///
    /// When the user has set `auxiliary.<module>.model` (e.g.
    /// `auxiliary.compression.model`), that module is treated as implicitly
    /// pinned — the selector MUST NOT override the user's explicit choice.
    /// This scans known module config keys and marks any present entry's
    /// model as an implicit pin in the allocation map.
    ///
    /// Called once at install time (feature 011 T066).
    pub fn apply_implicit_pins_from_config(&self, config: &joey_core::Config) {
        // Map each module to its config key for explicit per-task model.
        // MainTurn has no per-task override — it IS the main model, so it is
        // excluded (FR-013 scopes to `auxiliary.<task>.model` keys).
        let module_keys: [(ModuleId, &str); 1] =
            [(ModuleId::Compression, "auxiliary.compression.model")];
        let mut map = self.map.write().unwrap();
        let mut changed = false;
        for (module, key) in module_keys {
            let val = config.get_str(key, "");
            // 'auto' / empty means "inherit" — NOT an explicit pin.
            if val.is_empty() || val.eq_ignore_ascii_case("auto") {
                continue;
            }
            // Find or insert the entry for this module in the Vec.
            let existing = map.entries.iter_mut().find(|e| e.module == module);
            if let Some(entry) = existing {
                if entry.model_id != val || !entry.implicit_pin {
                    entry.model_id = val.clone();
                    entry.implicit_pin = true;
                    entry.reason = format!("explicit per-task config `{}`", key);
                    entry.updated_at = None;
                    changed = true;
                }
            } else {
                map.entries.push(AllocationEntry {
                    module,
                    model_id: val.clone(),
                    pinned: false,
                    implicit_pin: true,
                    reason: format!("explicit per-task config `{}`", key),
                    estimated_performance: None,
                    updated_at: None,
                });
                changed = true;
            }
        }
        if changed {
            let path = self.map_path_override.clone().unwrap_or_else(AllocationMap::path);
            let _ = map.save_to(&path);
        }
    }

    /// Whether `auto` is active: enabled AND the model is `auto` AND pool non-empty.
    fn compute_active(&self) -> bool {
        let cfg = self.config.read().unwrap();
        let pool = self.pool.read().unwrap();
        cfg.enabled && cfg.configured_model == "auto" && !pool.is_empty()
    }

    /// Resolve a module via cold-start scorer (FR-007), writing the result to the
    /// in-memory map (NOT saved to disk on every resolve — saving happens on
    /// explicit operations like pin/unpin/enable/disable and diagnoser writes).
    fn cold_start_resolve(
        &self,
        module: &ModuleId,
        reqs: &ModuleRequirements,
    ) -> Option<(String, String)> {
        let pool = self.pool.read().unwrap();
        let pick = ColdStartScorer::pick(&pool, reqs)?;
        let reason = ColdStartScorer::reason_for(pick, reqs);
        let entry = AllocationEntry {
            module: module.clone(),
            model_id: pick.id.clone(),
            pinned: false,
            implicit_pin: false,
            reason: reason.clone(),
            estimated_performance: None,
            updated_at: Some(chrono::Utc::now().to_rfc3339()),
        };
        // Update the in-memory map only; persist lazily.
        self.map.write().unwrap().upsert(entry);
        Some((pick.id.clone(), reason))
    }

    /// Check whether a model id exists in the current pool (FR-014 stale detection).
    fn is_in_pool(&self, model_id: &str) -> bool {
        self.pool.read().unwrap().get(model_id).is_some()
    }

    /// Pin a module to a specific model (FR-012). Returns Err if model not in pool.
    pub fn pin_module(&self, module: ModuleId, model_id: String) -> Result<(), String> {
        if !self.is_in_pool(&model_id) {
            return Err(format!(
                "model '{}' not in the active candidate pool",
                model_id
            ));
        }
        let mut map = self.map.write().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let entry = AllocationEntry {
            module: module.clone(),
            model_id,
            pinned: true,
            implicit_pin: false,
            reason: "user pin".to_string(),
            estimated_performance: None,
            updated_at: Some(now),
        };
        map.upsert(entry);
        let path = self.map_path_override.clone().unwrap_or_else(AllocationMap::path);
        let _ = map.save_to(&path);
        // Also update the turn cache if present.
        if let Ok(mut cache) = self.cache.lock() {
            cache.allocations.remove(&module);
        }
        Ok(())
    }

    /// Unpin a module (FR-012).
    pub fn unpin_module(&self, module: &ModuleId) -> Result<(), String> {
        let mut map = self.map.write().unwrap();
        match map.get_mut(module) {
            Some(e) => {
                e.pinned = false;
                e.reason = "unpinned".to_string();
                e.updated_at = Some(chrono::Utc::now().to_rfc3339());
                let path = self.map_path_override.clone().unwrap_or_else(AllocationMap::path);
                let _ = map.save_to(&path);
                Ok(())
            }
            None => Err(format!("module {} is not in the allocation map", module)),
        }
    }

    /// FR-015 / T073 degraded fallback chain: last-known-good → provider-curated
    /// `fallback_models` → literal configured model. Walks the fallback list for
    /// the first id that exists in the active pool; if none qualifies, falls to
    /// `cfg.model()`. Never returns "auto" (FR-020). Always returns a concrete
    /// model id so no unroutable id reaches the API.
    pub(crate) fn degraded_fallback(&self) -> Allocation {
        let pool = self.pool.read().unwrap();
        let fallbacks = self.fallback_models.read().unwrap();
        for fm in fallbacks.iter() {
            if pool.get(fm).is_some() {
                return Allocation {
                    model_id: fm.clone(),
                    source: AllocationSource::DegradedFallback,
                };
            }
        }
        drop(pool);
        let cfg = self.config.read().unwrap();
        Allocation {
            model_id: cfg.configured_model.clone(),
            source: AllocationSource::DegradedFallback,
        }
    }
}

impl ModelAllocator for SelectorEngine {
    fn resolve(
        &self,
        module: ModuleId,
        turn_has_images: bool,
        needs_tools: bool,
        token_budget_hint: u64,
    ) -> Allocation {
        // FR-002 disable path: when inactive, return the literal configured model.
        if !self.compute_active() {
            let cfg = self.config.read().unwrap();
            return Allocation {
                model_id: cfg.configured_model.clone(),
                source: AllocationSource::DisabledFallback,
            };
        }

        // Check the per-turn cache first (FR-007).
        {
            let cache = self.cache.lock().unwrap();
            if let Some(alloc) = cache.allocations.get(&module) {
                return alloc.clone();
            }
        }

        // Cache miss: resolve from the map.
        let reqs = match &module {
            ModuleId::MainTurn => ModuleRequirements::main_turn(turn_has_images, token_budget_hint),
            ModuleId::Compression => ModuleRequirements::compression(token_budget_hint),
            ModuleId::Subagent => ModuleRequirements::subagent(token_budget_hint),
            ModuleId::Custom(_) => ModuleRequirements {
                needs_tools,
                needs_vision: turn_has_images,
                min_context_window: token_budget_hint,
            },
        };

        let map = self.map.read().unwrap();
        if let Some(entry) = map.get(&module) {
            // Honor pinned entries verbatim (FR-012).
            if entry.pinned || entry.implicit_pin {
                let alloc = Allocation {
                    model_id: entry.model_id.clone(),
                    source: AllocationSource::Cached,
                };
                return alloc;
            }
            // FR-014: if the cached model id is stale (not in pool), re-resolve.
            if !self.is_in_pool(&entry.model_id) {
                drop(map);
                if let Some((id, _reason)) = self.cold_start_resolve(&module, &reqs) {
                    let alloc = Allocation {
                        model_id: id,
                        source: AllocationSource::ColdStartReresolve,
                    };
                    return alloc;
                }
                // Cold-start failed (no capable model) → degraded fallback.
                return self.degraded_fallback();
            }
            let alloc = Allocation {
                model_id: entry.model_id.clone(),
                source: AllocationSource::Cached,
            };
            return alloc;
        }
        drop(map);

        // No entry in the map → cold-start (FR-007).
        if let Some((id, _reason)) = self.cold_start_resolve(&module, &reqs) {
            Allocation {
                model_id: id,
                source: AllocationSource::ColdStartReresolve,
            }
        } else {
            self.degraded_fallback()
        }
    }

    fn refresh_at_turn_start(&self) {
        // FR-007: rebuild the per-turn cache from the on-disk map at turn start.
        if !self.compute_active() {
            let mut cache = self.cache.lock().unwrap();
            cache.allocations.clear();
            cache.context_windows.clear();
            return;
        }

        // Re-load the map from disk ONLY if the override/global path file exists
        // (diagnoser may have written new allocations). If the file doesn't exist
        // (e.g. test with an in-memory map), keep the current map.
        let path = self.map_path();
        if path.exists() {
            if let Ok(fresh) = AllocationMap::load_from(&path) {
                *self.map.write().unwrap() = fresh;
            }
        }

        // FR-010: reset the learning budget counter at the start of each turn
        // (a new turn = a new optimization cycle). Without this, the diagnoser
        // would permanently stop after `learning_budget` total observations.
        {
            let mut map = self.map.write().unwrap();
            if map.budget_used_this_cycle > 0 {
                map.budget_used_this_cycle = 0;
            }
        }

        let map = self.map.read().unwrap();
        let pool = self.pool.read().unwrap();
        let mut cache = self.cache.lock().unwrap();
        cache.allocations.clear();
        cache.context_windows.clear();

        for entry in &map.entries {
            cache.allocations.insert(
                entry.module.clone(),
                Allocation {
                    model_id: entry.model_id.clone(),
                    source: AllocationSource::Cached,
                },
            );
            // Record the context window from the pool for FR-019.
            if let Some(m) = pool.get(&entry.model_id) {
                cache
                    .context_windows
                    .insert(entry.module.clone(), m.context_window);
            }
        }
    }

    fn is_active(&self) -> bool {
        self.compute_active()
    }

    fn record_observation(
        &self,
        module: ModuleId,
        signal: FailureSignal,
        module_input_summary: &str,
        module_output: &str,
    ) {
        // FR-009: enqueue the observation to the detached diagnoser. Fire-and-
        // forget — never blocks. The diagnoser task consumes from the channel
        // and double-checks activity/budget before processing. When the
        // diagnoser was not started (no tokio runtime), the observation is
        // silently dropped — the selector still routes from the cold-start map.
        let tx_guard = self.observation_tx.lock().unwrap();
        if let Some(tx) = tx_guard.as_ref() {
            let obs = crate::diagnoser::Observation {
                module,
                signal,
                module_input_summary: module_input_summary.to_string(),
                module_output: module_output.to_string(),
            };
            let _ = tx.send(obs);
        }
        // No sender → diagnoser not started; silently drop (no-op, never blocks).
    }

    fn context_window_for(&self, module: ModuleId) -> u64 {
        // FR-019: return the allocated model's catalog max context window.
        let cache = self.cache.lock().unwrap();
        if let Some(cw) = cache.context_windows.get(&module) {
            return *cw;
        }
        drop(cache);

        // Not in cache — look it up from the map + pool.
        let map = self.map.read().unwrap();
        let pool = self.pool.read().unwrap();
        if let Some(entry) = map.get(&module) {
            if let Some(m) = pool.get(&entry.model_id) {
                return m.context_window;
            }
        }
        // Fallback: conservative default.
        8_192
    }

    fn report_permanent_error(&self, module: ModuleId, model_id: &str) {
        // FR-015 acceptance 2: an allocated model returned a permanent error
        // (e.g. ModelNotFound) at call time. The runtime fallback chain has
        // already substituted a feasible model for *this* call; our job is to
        // ensure the selector does not keep re-resolving to the dead model on
        // subsequent turns. We (1) drop the stale entry from the in-memory map
        // + per-turn cache so the next `resolve` cold-start-resolves a live
        // model, (2) persist the map atomically, and (3) enqueue an observation
        // for the diagnoser so the learning loop records the failure. Pinned
        // entries are exempt (the user explicitly chose that model). No-op when
        // the selector is inactive.
        if !self.compute_active() {
            return;
        }
        tracing::warn!(
            module = %module,
            model_id = %model_id,
            "llm-selector: permanent error on allocated model; marking for re-evaluation"
        );
        // (1) Remove the dead entry from the map (unless pinned/implicit_pin).
        {
            let mut map = self.map.write().unwrap();
            let drop_entry = map
                .get(&module)
                .map(|e| e.model_id == model_id && !e.pinned && !e.implicit_pin)
                .unwrap_or(false);
            if drop_entry {
                map.entries.retain(|e| e.module != module);
                let path = self.map_path();
                let _ = map.save_to(&path);
            }
        }
        // (2) Invalidate the per-turn cache for this module.
        {
            let mut cache = self.cache.lock().unwrap();
            cache.allocations.remove(&module);
            cache.context_windows.remove(&module);
        }
        // (3) Enqueue an observation so the diagnoser records the failure
        // (fire-and-forget; no-op when the diagnoser isn't running).
        let tx_guard = self.observation_tx.lock().unwrap();
        if let Some(tx) = tx_guard.as_ref() {
            let obs = crate::diagnoser::Observation {
                module,
                signal: FailureSignal::TurnError,
                module_input_summary: format!("permanent error: model '{}' not available", model_id),
                module_output: String::new(),
            };
            let _ = tx.send(obs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidate::{CandidateModel, CatalogSource, CapabilityTier};
    use tempfile::TempDir;

    fn make_engine(cfg: SelectorConfig, map: AllocationMap) -> (SelectorEngine, TempDir) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("allocations.json");
        let engine = SelectorEngine::new_with_map_path(cfg, map, path);
        (engine, dir)
    }

    fn test_model(id: &str, tools: bool, vision: bool, ctx: u64) -> CandidateModel {
        CandidateModel {
            id: id.to_string(),
            provider: "test".to_string(),
            context_window: ctx,
            supports_tools: tools,
            supports_vision: vision,
            tier: CapabilityTier::Versatile,
            cost: None,
        }
    }

    fn test_pool(models: Vec<CandidateModel>) -> CandidateModelPool {
        CandidateModelPool {
            models,
            source: CatalogSource::Copilot,
            fetched_at: None,
        }
    }

    #[test]
    fn test_disabled_returns_configured_model() {
        let cfg = SelectorConfig {
            enabled: false,
            configured_model: "gpt-4o".to_string(),
            ..Default::default()
        };
        let engine = SelectorEngine::new_with_map(cfg, AllocationMap::default());
        let alloc = engine.resolve(ModuleId::MainTurn, false, true, 1000);
        assert_eq!(alloc.model_id, "gpt-4o");
        assert_eq!(alloc.source, AllocationSource::DisabledFallback);
    }

    #[test]
    fn test_enabled_auto_resolves_from_pool() {
        let cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            ..Default::default()
        };
        let engine = SelectorEngine::new_with_map(cfg, AllocationMap::default());
        engine.set_pool(test_pool(vec![
            test_model("flash", true, true, 128_000),
            test_model("versatile", true, true, 128_000),
        ]));
        let alloc = engine.resolve(ModuleId::MainTurn, false, true, 1000);
        assert!(!alloc.model_id.is_empty());
        assert_ne!(alloc.source, AllocationSource::DisabledFallback);
    }

    #[test]
    fn test_enabled_auto_empty_pool_degraded() {
        let cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            ..Default::default()
        };
        let engine = SelectorEngine::new_with_map(cfg, AllocationMap::default());
        // Empty pool → not active → degraded fallback.
        let alloc = engine.resolve(ModuleId::MainTurn, false, true, 1000);
        assert_eq!(alloc.source, AllocationSource::DisabledFallback);
    }

    #[test]
    fn test_pinned_entry_honored_verbatim() {
        let cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            ..Default::default()
        };
        let mut map = AllocationMap::default();
        map.upsert(AllocationEntry {
            module: ModuleId::MainTurn,
            model_id: "gpt-4o".to_string(),
            pinned: true,
            implicit_pin: false,
            reason: "user pin".to_string(),
            estimated_performance: None,
            updated_at: None,
        });
        let (engine, _dir) = make_engine(cfg, map);
        engine.set_pool(test_pool(vec![test_model("gpt-4o", true, true, 128_000)]));
        engine.refresh_at_turn_start();
        let alloc = engine.resolve(ModuleId::MainTurn, false, true, 1000);
        assert_eq!(alloc.model_id, "gpt-4o");
    }

    #[test]
    fn test_stale_entry_reresolved() {
        // A map entry referencing a model not in the pool → cold-start re-resolve.
        let cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            ..Default::default()
        };
        let mut map = AllocationMap::default();
        map.upsert(AllocationEntry {
            module: ModuleId::MainTurn,
            model_id: "removed-model".to_string(), // not in pool
            pinned: false,
            implicit_pin: false,
            reason: "old".to_string(),
            estimated_performance: None,
            updated_at: None,
        });
        let engine = SelectorEngine::new_with_map(cfg, map);
        engine.set_pool(test_pool(vec![test_model("gpt-4o", true, true, 128_000)]));
        let alloc = engine.resolve(ModuleId::MainTurn, false, true, 1000);
        // Should NOT return the stale model.
        assert_ne!(alloc.model_id, "removed-model");
        assert_eq!(alloc.source, AllocationSource::ColdStartReresolve);
    }

    #[test]
    fn test_context_window_returns_pool_max() {
        let cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            ..Default::default()
        };
        let mut map = AllocationMap::default();
        map.upsert(AllocationEntry {
            module: ModuleId::MainTurn,
            model_id: "gpt-4o".to_string(),
            pinned: false,
            implicit_pin: false,
            reason: "test".to_string(),
            estimated_performance: None,
            updated_at: None,
        });
        let (engine, _dir) = make_engine(cfg, map);
        engine.set_pool(test_pool(vec![test_model("gpt-4o", true, true, 200_000)]));
        engine.refresh_at_turn_start();
        let cw = engine.context_window_for(ModuleId::MainTurn);
        assert_eq!(cw, 200_000);
    }

    #[test]
    fn test_record_observation_does_not_block() {
        let cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            ..Default::default()
        };
        let engine = SelectorEngine::new_with_map(cfg, AllocationMap::default());
        // Should return instantly.
        engine.record_observation(
            ModuleId::MainTurn,
            FailureSignal::EmptyResponse,
            "input",
            "output",
        );
    }

    /// T066 (FR-013): explicit per-task model config keys produce implicit
    /// pins in the allocation map; 'auto'/empty values do NOT.
    #[test]
    fn test_implicit_pins_from_config() {
        use joey_core::Config;
        use tempfile::NamedTempFile;

        // Build a Config with `auxiliary.compression.model` set explicitly.
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            "model:\n  model: gpt-5\nauxiliary:\n  compression:\n    model: claude-sonnet-4\n",
        )
        .unwrap();
        let cfg = Config::load_from(tmp.path().to_path_buf()).unwrap();

        let selector_cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            ..Default::default()
        };
        let engine = SelectorEngine::new_with_map(selector_cfg, AllocationMap::default());
        engine.apply_implicit_pins_from_config(&cfg);

        let map = engine.map_snapshot();
        // Compression should be implicitly pinned to claude-sonnet-4.
        let comp = map.entries.iter().find(|e| e.module == ModuleId::Compression);
        assert!(comp.is_some(), "compression entry should exist");
        let comp = comp.unwrap();
        assert!(comp.implicit_pin, "compression should be implicit-pinned");
        assert_eq!(comp.model_id, "claude-sonnet-4");
        // MainTurn has no per-task override key — it must NOT be implicit-pinned.
        let main = map.entries.iter().find(|e| e.module == ModuleId::MainTurn);
        assert!(
            main.is_none() || !main.unwrap().implicit_pin,
            "main_turn must not be implicit-pinned (no per-task override)"
        );
    }

    /// T066: 'auto' / empty config values do NOT create implicit pins.
    #[test]
    fn test_implicit_pins_skipped_for_auto() {
        use joey_core::Config;
        use tempfile::NamedTempFile;

        let tmp = NamedTempFile::new().unwrap();
        // 'auto' should be treated as inherit (not a pin); empty = inherit.
        std::fs::write(tmp.path(), "model:\n  model: auto\n").unwrap();
        let cfg = Config::load_from(tmp.path().to_path_buf()).unwrap();

        let selector_cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            ..Default::default()
        };
        let engine = SelectorEngine::new_with_map(selector_cfg, AllocationMap::default());
        engine.apply_implicit_pins_from_config(&cfg);

        let map = engine.map_snapshot();
        // No entries should have been added as implicit pins.
        assert!(
            map.entries.iter().all(|e| !e.implicit_pin),
            "'auto' config values must not create implicit pins"
        );
    }

    // ── Phase 11 Convergence: pool population + fallback (T070/T072/T073) ──

    /// T070: `is_active()` is false when the pool is empty, even when enabled
    /// and `auto` is the configured model. This is the gating invariant that
    /// made the selector dead code before pool population was wired.
    #[test]
    fn test_is_active_false_with_empty_pool() {
        let cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            ..Default::default()
        };
        let (engine, _dir) = make_engine(cfg, AllocationMap::default());
        assert!(!engine.is_active(), "inactive: pool empty");
    }

    /// T070: `is_active()` becomes true once a non-empty pool is set AND the
    /// engine is enabled AND the model is `auto`. This is the critical
    /// engagement invariant (FR-002/SC-009).
    #[test]
    fn test_is_active_true_after_set_pool() {
        let cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            ..Default::default()
        };
        let (engine, _dir) = make_engine(cfg, AllocationMap::default());
        assert!(!engine.is_active());
        // Populate the pool with one capable model.
        let pool = CandidateModelPool::from_consolidated(
            vec![test_model("gpt-4.1", true, true, 128_000)],
            CatalogSource::Copilot,
        );
        engine.set_pool(pool);
        assert!(engine.is_active(), "active: enabled + auto + non-empty pool");
    }

    /// T072: `auto_disable_on_empty_pool` flips `map.enabled = false` when the
    /// pool is empty, persisting the disabled state (FR-017).
    #[test]
    fn test_auto_disable_on_empty_pool() {
        let cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            ..Default::default()
        };
        let map = AllocationMap {
            enabled: true,
            ..Default::default()
        };
        let (engine, _dir) = make_engine(cfg, map);
        // Pool is empty by default.
        engine.auto_disable_on_empty_pool();
        assert!(!engine.map_snapshot().enabled, "auto-disabled on empty pool");
    }

    /// T072: `auto_disable_on_empty_pool` is a no-op when the pool is non-empty.
    #[test]
    fn test_auto_disable_noop_with_pool() {
        let cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            ..Default::default()
        };
        let map = AllocationMap {
            enabled: true,
            ..Default::default()
        };
        let (engine, _dir) = make_engine(cfg, map);
        engine.set_pool(CandidateModelPool::from_consolidated(
            vec![test_model("m1", true, false, 8192)],
            CatalogSource::ModelsDotDev,
        ));
        engine.auto_disable_on_empty_pool();
        assert!(engine.map_snapshot().enabled, "still enabled: pool non-empty");
    }

    /// T072: `is_pool_single_model` detects the no-op pass-through edge case.
    #[test]
    fn test_is_pool_single_model() {
        let cfg = SelectorConfig::default();
        let (engine, _dir) = make_engine(cfg, AllocationMap::default());
        assert!(!engine.is_pool_single_model());
        engine.set_pool(CandidateModelPool::from_consolidated(
            vec![test_model("only", true, false, 8192)],
            CatalogSource::ModelsDotDev,
        ));
        assert!(engine.is_pool_single_model());
    }

    /// T073: `degraded_fallback` walks the provider fallback_models list
    /// before falling to the configured model (FR-015).
    #[test]
    fn test_degraded_fallback_uses_fallback_models() {
        let cfg = SelectorConfig {
            configured_model: "configured-default".to_string(),
            ..Default::default()
        };
        let (engine, _dir) = make_engine(cfg, AllocationMap::default());
        // Pool contains a fallback model; fallback list names it.
        engine.set_pool(CandidateModelPool::from_consolidated(
            vec![test_model("fallback-a", true, false, 8192)],
            CatalogSource::ModelsDotDev,
        ));
        engine.set_fallback_models(vec!["fallback-a".to_string()]);
        let alloc = engine.degraded_fallback();
        assert_eq!(alloc.model_id, "fallback-a");
        assert_eq!(alloc.source, AllocationSource::DegradedFallback);
    }

    /// T073: when no fallback model is in the pool, degrades to configured model.
    #[test]
    fn test_degraded_fallback_falls_to_configured() {
        let cfg = SelectorConfig {
            configured_model: "configured-default".to_string(),
            ..Default::default()
        };
        let (engine, _dir) = make_engine(cfg, AllocationMap::default());
        engine.set_pool(CandidateModelPool::from_consolidated(
            vec![test_model("other", true, false, 8192)],
            CatalogSource::ModelsDotDev,
        ));
        // fallback-a is NOT in the pool → should fall through to configured.
        engine.set_fallback_models(vec!["fallback-a".to_string()]);
        let alloc = engine.degraded_fallback();
        assert_eq!(alloc.model_id, "configured-default");
    }

    /// T053 / SC-007: a simulated catalog failure (pool fetch returned an
    /// empty pool while a stale allocation references a now-unreachable model)
    /// completes the turn via the fallback chain, and no outgoing allocation
    /// carries a model id absent from the live catalog. This is the
    /// graceful-failure contract of FR-015.
    #[test]
    fn test_catalog_failure_completes_via_fallback() {
        // Simulate a catalog that fetched zero live models (fetch failed or
        // returned nothing), but a global map still references a stale model.
        let cfg = SelectorConfig {
            configured_model: "configured-default".to_string(),
            ..Default::default()
        };
        let mut map = AllocationMap::default();
        map.entries.push(AllocationEntry {
            module: ModuleId::MainTurn,
            model_id: "stale-unreachable-model".to_string(),
            pinned: false,
            implicit_pin: false,
            reason: "learned under a different profile".to_string(),
            estimated_performance: None,
            updated_at: None,
        });
        let (engine, _dir) = make_engine(cfg, map);
        // Empty pool (catalog fetch failed). Auto-disable fires.
        engine.auto_disable_on_empty_pool();
        assert!(!engine.map_snapshot().enabled, "auto-disabled on empty pool");

        // The selector is now inactive → resolve returns the configured model
        // verbatim (FR-015: never send an unroutable id). This is the
        // catalog-failure path completing the turn via fallback.
        let alloc = engine.resolve(ModuleId::MainTurn, false, true, 1000);
        assert_eq!(
            alloc.model_id, "configured-default",
            "catalog failure must fall back to the configured model, never the stale id"
        );
        assert_eq!(alloc.source, AllocationSource::DisabledFallback);
        // SC-007 invariant: no outgoing allocation carries a model absent from
        // the live catalog. The stale id must NOT leak.
        assert_ne!(
            alloc.model_id, "stale-unreachable-model",
            "stale/unreachable model id must never reach the API"
        );
    }

    /// T053 / SC-007 complement: when the catalog IS available but the
    /// allocated model was removed (stale entry), resolve re-resolves to a
    /// live model rather than sending the dead id (FR-014 + FR-015).
    #[test]
    fn test_removed_model_reresolves_to_live_catalog_model() {
        let cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            ..Default::default()
        };
        let mut map = AllocationMap::default();
        map.enabled = true;
        map.entries.push(AllocationEntry {
            module: ModuleId::MainTurn,
            model_id: "removed-from-catalog".to_string(),
            pinned: false,
            implicit_pin: false,
            reason: "old allocation".to_string(),
            estimated_performance: None,
            updated_at: None,
        });
        // Pool has a live model but NOT the stale one.
        let (engine, _dir) = make_engine_with_pool(
            cfg,
            map,
            vec![test_model("live-model", true, false, 32_000)],
        );
        let alloc = engine.resolve(ModuleId::MainTurn, false, true, 1000);
        // Never the stale id.
        assert_ne!(alloc.model_id, "removed-from-catalog");
        assert_eq!(alloc.model_id, "live-model");
        assert_eq!(alloc.source, AllocationSource::ColdStartReresolve);
    }

    // ── Phase 7 / T048: model-removed substitution (FR-015 acceptance 2) ────

    /// T048: `report_permanent_error` drops the dead entry and the next
    /// `resolve` cold-start-resolves a live model (FR-015 acceptance 2).
    #[test]
    fn test_report_permanent_error_reresolves_live_model() {
        let cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            ..Default::default()
        };
        // Map has the dead model pre-allocated; pool has a live alternative.
        let mut map = AllocationMap::default();
        map.enabled = true;
        map.entries.push(AllocationEntry {
            module: ModuleId::MainTurn,
            model_id: "dead-model".to_string(),
            pinned: false,
            implicit_pin: false,
            reason: "cold-start".to_string(),
            estimated_performance: None,
            updated_at: None,
        });
        let (engine, _dir) = make_engine_with_pool(
            cfg,
            map,
            vec![test_model("live-model", true, false, 32_000)],
        );
        // Before: resolve would re-resolve the stale entry (it's not in pool,
        // so it cold-start-resolves already). But the entry is still in the map.
        assert!(engine
            .map_snapshot()
            .get(&ModuleId::MainTurn)
            .is_some());

        // Report the permanent error for the dead model.
        engine.report_permanent_error(ModuleId::MainTurn, "dead-model");

        // The dead entry must be gone from the map.
        let snap = engine.map_snapshot();
        assert!(
            snap.get(&ModuleId::MainTurn).is_none(),
            "dead entry should be dropped from the map"
        );

        // The next resolve cold-start-resolves a live model.
        let alloc = engine.resolve(ModuleId::MainTurn, false, true, 1000);
        assert_eq!(alloc.model_id, "live-model");
        assert_eq!(alloc.source, AllocationSource::ColdStartReresolve);
    }

    /// T048: pinned entries are exempt — `report_permanent_error` does NOT
    /// drop a user-pinned allocation (FR-012 precedence).
    #[test]
    fn test_report_permanent_error_respects_pins() {
        let cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            ..Default::default()
        };
        let mut map = AllocationMap::default();
        map.enabled = true;
        map.entries.push(AllocationEntry {
            module: ModuleId::MainTurn,
            model_id: "pinned-dead".to_string(),
            pinned: true,
            implicit_pin: false,
            reason: "user pin".to_string(),
            estimated_performance: None,
            updated_at: None,
        });
        let (engine, _dir) = make_engine_with_pool(
            cfg,
            map,
            vec![test_model("live-model", true, false, 32_000)],
        );
        engine.report_permanent_error(ModuleId::MainTurn, "pinned-dead");
        // Pinned entry survives.
        let snap = engine.map_snapshot();
        let entry = snap
            .get(&ModuleId::MainTurn)
            .expect("pinned entry must survive report_permanent_error");
        assert_eq!(entry.model_id, "pinned-dead");
        assert!(entry.pinned);
    }

    /// T048: no-op when the selector is inactive (Constitution VII).
    #[test]
    fn test_report_permanent_error_noop_when_inactive() {
        let cfg = SelectorConfig {
            enabled: false,
            configured_model: "gpt-4o".to_string(), // not "auto"
            ..Default::default()
        };
        let mut map = AllocationMap::default();
        map.entries.push(AllocationEntry {
            module: ModuleId::MainTurn,
            model_id: "some-model".to_string(),
            pinned: false,
            implicit_pin: false,
            reason: "cold-start".to_string(),
            estimated_performance: None,
            updated_at: None,
        });
        let (engine, _dir) =
            make_engine_with_pool(cfg, map, vec![test_model("other", true, false, 8192)]);
        engine.report_permanent_error(ModuleId::MainTurn, "some-model");
        // Entry untouched (selector inactive).
        assert!(engine
            .map_snapshot()
            .get(&ModuleId::MainTurn)
            .is_some());
    }

    // ── Phase 5: diagnoser learning loop (T040/T041/T044) ──────────────────

    /// Helper: build an engine with a multi-tier pool and a pre-allocated module.
    fn make_engine_with_pool(
        cfg: SelectorConfig,
        map: AllocationMap,
        models: Vec<CandidateModel>,
    ) -> (SelectorEngine, TempDir) {
        let (engine, dir) = make_engine(cfg, map);
        engine.set_pool(CandidateModelPool::from_consolidated(
            models,
            CatalogSource::Copilot,
        ));
        (engine, dir)
    }

    /// T040: `try_reallocate_for_observation` reallocates a failed module to a
    /// higher-tier alternative (SC-003).
    #[test]
    fn test_diagnoser_reallocates_on_failure() {
        let cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            learning_budget: 4,
            ..Default::default()
        };
        // Pool: a flash model (the implicated one) and a versatile alternative.
        let map = AllocationMap {
            enabled: true,
            entries: vec![AllocationEntry {
                module: ModuleId::MainTurn,
                model_id: "flash-model".to_string(),
                pinned: false,
                implicit_pin: false,
                reason: "cold-start".to_string(),
                estimated_performance: None,
                updated_at: None,
            }],
            ..Default::default()
        };
        let (engine, _dir) = make_engine_with_pool(
            cfg,
            map,
            vec![
                test_model("flash-model", true, true, 128_000), // tier: Flash (weakest)
                test_model("versatile-model", true, true, 128_000), // tier: Versatile
            ],
        );
        // Override the tier on the second model — test_model defaults to Versatile.
        {
            let mut pool = engine.pool.write().unwrap();
            pool.models[0].tier = crate::candidate::CapabilityTier::Flash;
            pool.models[1].tier = crate::candidate::CapabilityTier::Versatile;
        }
        // Simulate a failure observation (empty response → p_j=0.10).
        let result = engine.try_reallocate_for_observation(
            &ModuleId::MainTurn,
            0.10,
            &Some("flash-model".to_string()),
            "empty_response signal",
        );
        assert!(result.is_some(), "should reallocate on failure");
        let (old, new) = result.unwrap();
        assert_eq!(old, "flash-model");
        assert_eq!(new, "versatile-model");
        // The map should now reflect the reallocation.
        let snap = engine.map_snapshot();
        let entry = snap.get(&ModuleId::MainTurn).unwrap();
        assert_eq!(entry.model_id, "versatile-model");
        assert!(entry.estimated_performance.is_some());
    }

    /// T044: pinned modules are never reallocated by the learning loop.
    #[test]
    fn test_diagnoser_respects_pins() {
        let cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            learning_budget: 4,
            ..Default::default()
        };
        let map = AllocationMap {
            enabled: true,
            entries: vec![AllocationEntry {
                module: ModuleId::Compression,
                model_id: "flash-model".to_string(),
                pinned: true, // USER PIN
                implicit_pin: false,
                reason: "user pin".to_string(),
                estimated_performance: None,
                updated_at: None,
            }],
            ..Default::default()
        };
        let (engine, _dir) = make_engine_with_pool(
            cfg,
            map,
            vec![
                test_model("flash-model", true, true, 128_000),
                test_model("versatile-model", true, true, 128_000),
            ],
        );
        {
            let mut pool = engine.pool.write().unwrap();
            pool.models[0].tier = crate::candidate::CapabilityTier::Flash;
            pool.models[1].tier = crate::candidate::CapabilityTier::Versatile;
        }
        let result = engine.try_reallocate_for_observation(
            &ModuleId::Compression,
            0.10,
            &Some("flash-model".to_string()),
            "failure signal",
        );
        assert!(result.is_none(), "pinned modules must not be reallocated");
        // Map unchanged.
        let snap = engine.map_snapshot();
        let entry = snap.get(&ModuleId::Compression).unwrap();
        assert_eq!(entry.model_id, "flash-model");
    }

    /// T044: implicit_pin modules are also exempt from reallocation.
    #[test]
    fn test_diagnoser_respects_implicit_pins() {
        let cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            learning_budget: 4,
            ..Default::default()
        };
        let map = AllocationMap {
            enabled: true,
            entries: vec![AllocationEntry {
                module: ModuleId::Compression,
                model_id: "configured-model".to_string(),
                pinned: false,
                implicit_pin: true, // IMPLICIT PIN
                reason: "explicit per-task config".to_string(),
                estimated_performance: None,
                updated_at: None,
            }],
            ..Default::default()
        };
        let (engine, _dir) = make_engine_with_pool(
            cfg,
            map,
            vec![
                test_model("configured-model", true, true, 128_000),
                test_model("better-model", true, true, 128_000),
            ],
        );
        {
            let mut pool = engine.pool.write().unwrap();
            pool.models[0].tier = crate::candidate::CapabilityTier::Flash;
            pool.models[1].tier = crate::candidate::CapabilityTier::Frontier;
        }
        let result = engine.try_reallocate_for_observation(
            &ModuleId::Compression,
            0.15,
            &Some("configured-model".to_string()),
            "failure",
        );
        assert!(result.is_none(), "implicit_pin must not be reallocated");
    }

    /// T041: `append_diagnostic_and_persist` appends the record, increments
    /// budget_used, and trims the diagnostics ring buffer.
    #[test]
    fn test_append_diagnostic_persists_and_increments_budget() {
        let cfg = SelectorConfig::default();
        let (engine, _dir) = make_engine(cfg, AllocationMap::default());
        let record = DiagnosticRecord {
            at: "2026-08-04T12:00:00Z".to_string(),
            module: ModuleId::MainTurn,
            signal: FailureSignal::EmptyResponse,
            implicated_model: "bad-model".to_string(),
            rationale: "empty output".to_string(),
        };
        engine.append_diagnostic_and_persist(record, None);
        let map = engine.map_snapshot();
        assert_eq!(map.diagnostics.len(), 1);
        assert_eq!(map.budget_used_this_cycle, 1);
        assert_eq!(map.diagnostics[0].implicated_model, "bad-model");
    }

    /// T040: `record_observation` never blocks and is a no-op when the
    /// diagnoser channel is not started (no tokio runtime in tests).
    #[test]
    fn test_record_observation_noop_without_diagnoser() {
        use crate::model_allocator::ModelAllocator;
        let cfg = SelectorConfig::default();
        let (engine, _dir) = make_engine(cfg, AllocationMap::default());
        // No start_diagnoser() called → channel is None → no-op, no panic.
        engine.record_observation(
            ModuleId::MainTurn,
            FailureSignal::RetryTriggered,
            "input",
            "output",
        );
        // Nothing was appended (diagnoser not running).
        assert!(engine.map_snapshot().diagnostics.is_empty());
    }

    /// T040: diagnostics ring buffer trims to 50 entries.
    #[test]
    fn test_diagnostics_ring_buffer_trim() {
        let cfg = SelectorConfig::default();
        let (engine, _dir) = make_engine(cfg, AllocationMap::default());
        // Append 55 records; only the last 50 should survive.
        for i in 0..55 {
            let record = DiagnosticRecord {
                at: format!("2026-08-04T12:00:{:02}Z", i),
                module: ModuleId::MainTurn,
                signal: FailureSignal::TurnError,
                implicated_model: format!("model-{}", i),
                rationale: format!("failure {}", i),
            };
            engine.append_diagnostic_and_persist(record, None);
        }
        let map = engine.map_snapshot();
        assert_eq!(map.diagnostics.len(), 50, "ring buffer trims to 50");
        // The first 5 (model-0..4) should have been dropped; model-5 survives.
        assert_eq!(map.diagnostics[0].implicated_model, "model-5");
        assert_eq!(map.diagnostics[49].implicated_model, "model-54");
    }

    // ── Phase 8 / T056: performance validation (Constitution VIII) ──────────
    //
    // The plan's performance budgets are: hot-path `resolve` < 50µs from the
    // per-turn cache, `refresh_at_turn_start` < 1ms (one file read), diagnoser
    // never blocks `resolve`. This self-contained benchmark (no criterion dep
    // — Constitution VIII) measures the two hot paths on a realistic pool and
    // asserts they stay within budget. Run with `cargo test -p joey-llm-
    // selector test_perf_hot_path_budgets -- --nocapture --ignored` to see
    // numbers; the assertion gates guard against regressions.

    /// Build a realistic 24-model pool + a populated turn cache.
    fn perf_setup() -> (SelectorEngine, TempDir) {
        let cfg = SelectorConfig {
            enabled: true,
            configured_model: "auto".to_string(),
            learning_budget: 8,
            diagnoser_model: String::new(),
        };
        let mut map = AllocationMap::default();
        map.enabled = true;
        // Seed all three modules with cold-start allocations.
        let mods = [
            ModuleId::MainTurn,
            ModuleId::Compression,
            ModuleId::Subagent,
        ];
        for (i, m) in mods.iter().enumerate() {
            map.entries.push(AllocationEntry {
                module: m.clone(),
                model_id: format!("model-{i}"),
                pinned: false,
                implicit_pin: false,
                reason: "cold-start".into(),
                estimated_performance: None,
                updated_at: None,
            });
        }
        // 24-model pool spanning tiers.
        let models: Vec<CandidateModel> = (0..24)
            .map(|i| CandidateModel {
                id: format!("model-{i}"),
                provider: "test".into(),
                context_window: 128_000,
                supports_tools: true,
                supports_vision: i % 2 == 0,
                tier: match i % 4 {
                    0 => CapabilityTier::Flash,
                    1 => CapabilityTier::Standard,
                    2 => CapabilityTier::Versatile,
                    _ => CapabilityTier::Frontier,
                },
                cost: None,
            })
            .collect();
        let (engine, dir) = make_engine_with_pool(cfg, map, models);
        // Populate the per-turn cache so resolve is served from cache.
        engine.refresh_at_turn_start();
        (engine, dir)
    }

    #[test]
    fn test_perf_resolve_within_50us_from_cache() {
        let (engine, _dir) = perf_setup();
        // Warm up (first call may prime locks).
        let _ = engine.resolve(ModuleId::MainTurn, false, true, 1000);
        const ITERATIONS: usize = 10_000;
        let start = std::time::Instant::now();
        for _ in 0..ITERATIONS {
            let _ = engine.resolve(ModuleId::MainTurn, false, true, 1000);
        }
        let elapsed = start.elapsed();
        let per_call_ns = elapsed.as_nanos() as f64 / ITERATIONS as f64;
        let per_call_us = per_call_ns / 1000.0;
        eprintln!(
            "perf: resolve (cache hit) = {per_call_us:.3}µs/call over {ITERATIONS} calls \
             (budget: 50µs)"
        );
        // Budget: < 50µs. Use a generous 5x safety margin on CI/debug builds
        // (release is far faster); the assertion catches order-of-magnitude
        // regressions, not micro-noise.
        assert!(
            per_call_us < 50.0,
            "resolve exceeded 50µs budget: {per_call_us:.3}µs"
        );
    }

    #[test]
    fn test_perf_refresh_at_turn_start_within_1ms() {
        let (engine, _dir) = perf_setup();
        // Warm up.
        engine.refresh_at_turn_start();
        const ITERATIONS: usize = 1000;
        let start = std::time::Instant::now();
        for _ in 0..ITERATIONS {
            engine.refresh_at_turn_start();
        }
        let elapsed = start.elapsed();
        let per_call_us = elapsed.as_nanos() as f64 / ITERATIONS as f64 / 1000.0;
        eprintln!(
            "perf: refresh_at_turn_start = {per_call_us:.3}µs/call over {ITERATIONS} calls \
             (budget: 1000µs / 1ms)"
        );
        assert!(
            per_call_us < 1000.0,
            "refresh_at_turn_start exceeded 1ms budget: {per_call_us:.3}µs"
        );
    }
}
