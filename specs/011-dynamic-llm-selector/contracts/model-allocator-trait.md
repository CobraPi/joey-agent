# Contract: ModelAllocator Trait

**Feature**: 011-dynamic-llm-selector | **Surface**: public Rust trait
**Owning crate**: `joey-llm-selector` (defines) → consumed by `joey-agent-core`, `joey-cli`
**Stability**: public API contract (Constitution VII) — additive changes only;
breaking change requires MAJOR bump + migration note.

This is the **narrow** interface `joey-agent-core` depends on (Constitution VI).
Everything else in `joey-llm-selector` (engine, diagnoser, scorer internals)
is private to the crate.

---

## The trait

```rust
// crates/joey-llm-selector/src/model_allocator.rs

/// Narrow interface the agent turn loop consumes to resolve which model a
/// given module should call. The implementation owns the per-turn cache,
/// catalog consolidation, cold-start scorer, diagnoser, and allocation map.
///
/// Hot-path methods are non-async and run in O(1) off the per-turn cache.
/// The async `record_observation` forwards to the detached diagnoser task
/// and MUST NOT block the caller.
pub trait ModelAllocator: Send + Sync {
    /// Resolve the model id for `module` on the current turn.
    ///
    /// - Served from the per-turn cache (populated at `refresh_at_turn_start`);
    ///   O(1), no network, no allocation.
    /// - Honors pinned entries verbatim (FR-012).
    /// - Guarantees the returned id exists in the active catalog and satisfies
    ///   the module's hard capability requirements (FR-005); if the cached id
    ///   is stale (absent from catalog), it is re-resolved via the cold-start
    ///   scorer before return (FR-014).
    /// - Returns the literal configured model id (cfg.model()) when the
    ///   selector is disabled or `auto` is not active, so callers can call
    ///   this unconditionally.
    ///
    /// `turn_has_images` gates vision support (FR-005). `needs_tools` gates
    /// tool-calling support. `token_budget_hint` is the module's approximate
    /// token need (used to filter context window, never to cap below catalog max).
    fn resolve(
        &self,
        module: ModuleId,
        turn_has_images: bool,
        needs_tools: bool,
        token_budget_hint: u64,
    ) -> Allocation;

    /// Called at the start of every turn to refresh the per-turn cache from
    /// the on-disk allocation map and apply any diagnoser-driven reallocations
    /// produced since the last turn (FR-007). Cheap: one file read.
    /// No-op when the selector is disabled.
    fn refresh_at_turn_start(&self);

    /// Whether dynamic allocation is active for the current session
    /// (i.e. `auto` is the configured model AND the selector is enabled AND
    /// the active provider exposes a non-empty catalog). Drives User Story 1.
    fn is_active(&self) -> bool;

    /// Forward an observation to the detached diagnoser (FR-008, FR-009).
    /// Returns immediately (the call is enqueued to a channel consumed by a
    /// `tokio::spawn` task); never blocks the interactive turn.
    ///
    /// `signal` is the observable failure that triggered this call (FR-009).
    /// Ignored when the selector is inactive or the learning budget is
    /// exhausted. Non-failure turns produce no observation (per spec).
    fn record_observation(
        &self,
        module: ModuleId,
        signal: FailureSignal,
        module_input_summary: &str,
        module_output: &str,
    );

    /// Returns the highest available context window for the model currently
    /// allocated to `module` (FR-019). Used by call sites to avoid capping
    /// below the model's catalog maximum.
    fn context_window_for(&self, module: ModuleId) -> u64;
}

/// A resolved allocation result.
#[derive(Debug, Clone)]
pub struct Allocation {
    /// The model id to send to the API (never "auto"; always concrete).
    pub model_id: String,
    /// Whether this allocation came from the dynamic selector or is the
    /// literal configured model (disabled/fallback path). For diagnostics.
    pub source: AllocationSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationSource {
    /// Served from the per-turn cache (diagnoser-influenced or cold-start).
    Cached,
    /// Re-resolved this call because the cached id was stale (FR-014).
    ColdStartReresolve,
    /// Selector disabled / `auto` not active — literal cfg.model().
    DisabledFallback,
    /// Catalog error — fell back to last-known-good or provider fallback_models (FR-015).
    DegradedFallback,
}
```

## Inputs / outputs

| Method | Inputs | Output | Hot path? |
|---|---|---|---|
| `resolve` | module, image/tool flags, token hint | `Allocation` | **YES** (sync, O(1)) |
| `refresh_at_turn_start` | — | () | turn start (1 file read) |
| `is_active` | — | bool | cheap |
| `record_observation` | module, signal, summaries | () | enqueue only (async side work) |
| `context_window_for` | module | u64 | cheap |

## Contract invariants

1. **Never returns `"auto"`** — `resolve` always returns a concrete model id
   (FR-020). When the selector is inactive, it returns `cfg.model()` (which may
   be whatever the user set, but the agent only calls `resolve` meaningfully
   when `auto` is active).
2. **Never blocks** — `resolve`, `refresh_at_turn_start`, `is_active`, and
   `context_window_for` are sync and allocation/network-free on the steady
   path. `record_observation` is fire-and-forget.
3. **Never mutates conversation state** — the trait carries only model ids and
   metadata; it never touches `Agent::history` or the system prompt (FR-016).
4. **Pinned entries are final** — `resolve` returns a pinned module's model
   verbatim regardless of diagnoser output (FR-012).

## Wiring (consumer side)

Three call sites consume the trait (research.md §2):

| Intercept | File:line | Call |
|---|---|---|
| Main turn | `crates/joey-agent-core/src/agent.rs:831-841` (`build_request`) | `allocator.resolve(ModuleId::MainTurn, has_images, needs_tools, token_budget)` → replace `self.config.model.clone()` at line 835 |
| Compression | `crates/joey-agent-core/src/compression/summary.rs:180` (`AuxSummaryBackend::from_config`) | `allocator.resolve(ModuleId::Compression, false, false, ctx_len)` when `auxiliary.compression.model` is unset/`auto` |
| Subagent | `crates/joey-orchestration/src/subagent.rs:19` (`resolve_model`) | inject `allocator.resolve(ModuleId::Subagent, …)` into the priority chain |

Plus the observation hook in `call_with_retries` (`agent.rs:909`, `agent.rs:996`
retry emit) → `allocator.record_observation(MainTurn, RetryTriggered, …)`.

## Backward compatibility

- When the trait is not wired (selector disabled / `auto` not active), the
  three call sites behave byte-identically to today (they use the configured
  model). Regression tests MUST assert this (Constitution VII).
- The trait is `+ Send + Sync`; new methods added in future MUST be default
  methods to preserve the trait object's backward compatibility (Constitution VII).
