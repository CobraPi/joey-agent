# joey-llm-selector — dynamic LLM model selection

`joey-llm-selector` (feature 011) is the dynamic per-module LLM model allocator. It engages when the user selects the `auto` model on a catalog-exposing provider: treating the agent as a compound AI system (Chen et al., arXiv:2502.14815), it assigns each distinct LLM call site ("module" — main turn, compression, subagent) the best-suited model drawn from the active provider's live `/models` catalog, learns better allocations asynchronously via a detached LLM diagnoser triggered only by observable failure, and persists a global allocation map at `~/.joey/llm-selector/allocations.json`. `joey-agent-core` consumes only the narrow `ModelAllocator` trait; the engine, diagnoser, scorer, and map internals stay private to this crate (Constitution VI — Modularity and Decoupling).

> See also: [README.md](README.md), [joey-agent-core.md](joey-agent-core.md), [joey-cli.md](joey-cli.md)

## Overview

The crate is a clean-room Rust-native implementation — upstream Hermes' `specs/003-dynamic-llm-selector` cost-scorer (`agent/model_metadata.py`, `model_cost_guard.py`) was deliberately NOT ported (Copilot-specific Python runtime); Joey generalizes the candidate pool to *any* provider exposing a live catalog (Copilot canonical, models.dev for the rest) with its own `ColdStartScorer` (see `PORTING.md` feature 011 section).

Design facts:

- Off by default. The engine is only constructed when `model.selector.enabled` is true OR the configured model is the `auto` sentinel (`try_build_allocator` in `joey-cli`); with no allocator wired, every call site uses the configured model verbatim — byte-identical to pre-feature-011 (Constitution VII).
- Active means: `enabled && configured_model == "auto" && pool non-empty` (`SelectorEngine::compute_active`).
- The hot path `ModelAllocator::resolve` is non-async, O(1) off a per-turn cache; perf budgets asserted in tests: `resolve` < 50µs from cache, `refresh_at_turn_start` < 1ms.
- An empty pool after a catalog fetch auto-disables the selector with a persisted `enabled = false` (FR-017).
- Allocations never leak an unroutable id: stale entries are re-resolved, and the degraded fallback never returns the literal `auto` (FR-020).

## Module map

| File | Contents |
|---|---|
| `src/lib.rs` | crate docs; `pub mod` re-exports of all public types |
| `src/model_allocator.rs` | `ModelAllocator` trait, `Allocation`, `AllocationSource` — the only surface `joey-agent-core` depends on |
| `src/allocator.rs` | `SelectorEngine` (the `ModelAllocator` impl), `SelectorConfig`, per-turn `TurnCache`, pin/unpin, degraded fallback, diagnoser channel plumbing |
| `src/module.rs` | `ModuleId` — the call-site enum (`MainTurn`/`Compression`/`Subagent`/`Custom`) with serde + parse grammar |
| `src/candidate.rs` | `CandidateModel`, `CandidateModelPool`, `CapabilityTier`, `Cost`, `CatalogSource`, catalog consolidators (Copilot / OpenRouter / models.dev / generic probe), id-derived capability tables |
| `src/scorer.rs` | `ColdStartScorer`, `ModuleRequirements` — capability hard-gates + cheapest-capable ranking |
| `src/map.rs` | `AllocationMap`, `AllocationEntry`, `DiagnosticRecord`, `FailureSignal`, `MapError`, on-disk schema v1 at `~/.joey/llm-selector/allocations.json` |
| `src/diagnoser.rs` | detached tokio learning loop, `Observation` channel type, `DiagnoserClient` trait, `LlmDiagnoser` (LLM judge), signal-driven heuristic estimator, `p_j` parser |
| `src/query.rs` | `SelectorQuery`, `StatusReport`/row structs, `render_status` — backing the `/llm-selector` command |

No separate `tests/` directory; all tests are inline `#[cfg(test)]` modules. Dependencies: `joey-core`, `joey-providers`, `anyhow`, `thiserror`, `serde`, `serde_json`, `tokio`, `tracing`, `chrono`, `async-trait` (+ `tempfile` dev).

## Public API

### `model_allocator.rs`

```rust
pub trait ModelAllocator: Send + Sync {
    fn resolve(&self, module: ModuleId, turn_has_images: bool,
               needs_tools: bool, token_budget_hint: u64) -> Allocation;
    fn refresh_at_turn_start(&self);
    fn is_active(&self) -> bool;
    fn record_observation(&self, module: ModuleId, signal: FailureSignal,
                          module_input_summary: &str, module_output: &str);
    fn context_window_for(&self, module: ModuleId) -> u64;
    fn report_permanent_error(&self, _module: ModuleId, _model_id: &str) {}
}

pub struct Allocation { pub model_id: String, pub source: AllocationSource }

pub enum AllocationSource { Cached, ColdStartReresolve, DisabledFallback, DegradedFallback }
```

`report_permanent_error` has a default no-op (additive, Constitution VII). `record_observation` must never block — it enqueues to the detached diagnoser.

### `allocator.rs`

```rust
pub struct SelectorConfig {
    pub enabled: bool,             // model.selector.enabled
    pub configured_model: String,  // cfg.model() — disabled fallback
    pub learning_budget: u32,      // model.selector.budget
    pub diagnoser_model: String,   // model.selector.diagnoser_model
} // Default: enabled=false, learning_budget=8, rest empty

pub struct SelectorEngine { /* config/map/pool/cache/fallback_models/provider/
                              observation channel/diagnoser_client, all locked */ }

impl SelectorEngine {
    pub fn new(config: SelectorConfig) -> Self;                       // loads global map
    pub fn new_with_map(config: SelectorConfig, map: AllocationMap) -> Self;
    pub fn new_with_map_path(config: SelectorConfig, map: AllocationMap,
                             map_path: std::path::PathBuf) -> Self;
    pub fn set_pool(&self, pool: CandidateModelPool);
    pub fn auto_disable_on_empty_pool(&self);                         // FR-017, idempotent
    pub fn is_pool_single_model(&self) -> bool;
    pub fn set_fallback_models(&self, models: Vec<String>);           // FR-015/T073
    pub fn set_provider(&self, provider: String);
    pub fn provider(&self) -> String;
    pub fn set_diagnoser_client(&self, client: Option<Arc<dyn DiagnoserClient>>);
    pub fn start_diagnoser(self: &Arc<Self>);                         // spawns learning loop
    pub fn pool(&self) -> CandidateModelPool;
    pub fn map_snapshot(&self) -> AllocationMap;
    pub fn update_config(&self, config: SelectorConfig);              // persists map
    pub fn set_diagnoser_model(&self, model_id: &str) -> Result<(), String>;
    pub fn apply_implicit_pins_from_config(&self, config: &joey_core::Config); // FR-013
    pub fn pin_module(&self, module: ModuleId, model_id: String) -> Result<(), String>;
    pub fn unpin_module(&self, module: &ModuleId) -> Result<(), String>;
    pub fn configured_model(&self) -> String;
    pub fn config_snapshot(&self) -> SelectorConfig;
}
impl ModelAllocator for SelectorEngine { /* resolve, refresh_at_turn_start, is_active,
                                            record_observation, context_window_for,
                                            report_permanent_error */ }
```

`SelectorEngine` implements `ModelAllocator` for all six methods.

### `module.rs`

```rust
#[non_exhaustive]
pub enum ModuleId { MainTurn, Compression, Subagent, Custom(String) }

impl ModuleId {
    pub fn validate_custom_name(name: &str) -> Result<(), String>; // ^[a-z][a-z0-9_]{0,31}$
    pub fn as_str(&self) -> &str;   // "main_turn" | "compression" | "subagent" | name
    pub fn parse(s: &str) -> Result<Self, String>; // accepts custom:<name>
}
```

Serde is `snake_case`; `Custom("x")` round-trips as `{"custom":"x"}` so new variants never break old maps.

### `candidate.rs`

```rust
pub struct CandidateModel {
    pub id: String, pub provider: String, pub context_window: u64,
    pub supports_tools: bool, pub supports_vision: bool,
    pub tier: CapabilityTier, pub cost: Option<Cost>,
}
pub enum CapabilityTier { Flash, Standard, Versatile, Frontier } // Ord: Flash < … < Frontier
impl CapabilityTier { pub fn cost_weight(self) -> u8; }          // 0..=3
pub struct Cost { pub input_per_mtok: f64, pub output_per_mtok: f64 }

pub struct CandidateModelPool {
    pub models: Vec<CandidateModel>, pub source: CatalogSource,
    pub fetched_at: Option<chrono::DateTime<chrono::Utc>>,
}
pub enum CatalogSource { Empty, Copilot, OpenRouter, ModelsDotDev, GenericProbe }

impl CandidateModelPool {
    pub fn len(&self) -> usize; pub fn is_empty(&self) -> bool;
    pub fn get(&self, id: &str) -> Option<&CandidateModel>;
    pub fn from_consolidated(models: Vec<CandidateModel>, source: CatalogSource) -> Self;
}

pub fn consolidate_copilot(raw: &[Value]) -> (Vec<CandidateModel>, usize);
pub fn consolidate_openrouter(raw: &[Value]) -> (Vec<CandidateModel>, usize);
pub fn consolidate_models_dev(provider: &str, raw: &[Value]) -> (Vec<CandidateModel>, usize);
pub fn consolidate_generic_probe(provider: &str, raw: &[Value]) -> (Vec<CandidateModel>, usize);
pub fn supports_vision_by_id(id: &str) -> bool;
```

Each consolidator returns `(kept, dropped)`. Copilot filters `capabilities.type == "chat"`, prefers `max_context_window_tokens` over `max_prompt_tokens`, infers tools from `supported_endpoints`; OpenRouter reads `supported_parameters`/`architecture.input_modalities`/`context_length`/`pricing` (per-token → per-Mtok, free tiers get `None`); models.dev reads `tool_call`/`limit.context`/`cost`. Vision falls back to a curated id-prefix table; tier comes from id classification (Frontier: `gpt-5`, `claude-opus-4`, `gemini-2.5-pro`, `grok-4`, `o3`, `o4`; Flash: `haiku`/`flash`/`mini`/`nano`/`micro`; Versatile: `gpt-4.1`, `gpt-4o`, `gpt-4.5`, `claude-sonnet-4`, `claude-3-7`, `gemini-2.5`, `grok-3`, `deepseek-v3`, `glm-4.6`, `glm-5`; else Standard); context fallback table ends at `8_192`.

### `scorer.rs`

```rust
pub struct ModuleRequirements {
    pub needs_tools: bool, pub needs_vision: bool, pub min_context_window: u64,
}
impl ModuleRequirements {
    pub fn main_turn(turn_has_images: bool, token_budget_hint: u64) -> Self; // tools=true
    pub fn compression(context_length: u64) -> Self;                        // tools/vision=false
    pub fn subagent(token_budget_hint: u64) -> Self;                        // tools=true
}
pub struct ColdStartScorer;
impl ColdStartScorer {
    pub fn rank<'a>(pool: &'a CandidateModelPool, reqs: &ModuleRequirements) -> Vec<&'a CandidateModel>;
    pub fn pick<'a>(pool: &'a CandidateModelPool, reqs: &ModuleRequirements) -> Option<&'a CandidateModel>;
    pub fn satisfies(m: &CandidateModel, reqs: &ModuleRequirements) -> bool;
    pub fn reason_for(pick: &CandidateModel, reqs: &ModuleRequirements) -> String;
}
```

### `map.rs`

```rust
pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_DIAGNOSTICS: usize = 50;
pub const MAP_REL_PATH: &str = "llm-selector/allocations.json";

pub struct AllocationEntry {
    pub module: ModuleId, pub model_id: String,
    pub pinned: bool, pub implicit_pin: bool, pub reason: String,
    pub estimated_performance: Option<f64>, pub updated_at: Option<String>,
}
pub struct DiagnosticRecord {
    pub at: String, pub module: ModuleId, pub signal: FailureSignal,
    pub implicated_model: String, pub rationale: String,
}
pub enum FailureSignal { TurnError, AuxCallFailure, EmptyResponse, RetryTriggered }

pub struct AllocationMap {
    pub schema_version: u32, pub updated_at: Option<String>, pub enabled: bool,
    pub diagnoser_model: String, pub learning_budget: u32 /* default 8 */,
    pub budget_used_this_cycle: u32,
    pub entries: Vec<AllocationEntry>, pub diagnostics: Vec<DiagnosticRecord>,
}
impl AllocationMap {
    pub fn path() -> PathBuf;                 // process_joey_home().join(MAP_REL_PATH) — machine-global
    pub fn load() -> Result<AllocationMap, MapError>;
    pub fn load_from(path: &Path) -> Result<AllocationMap, MapError>; // missing file = cold start
    pub fn save(&mut self) -> Result<(), MapError>;      // atomic_json_write
    pub fn save_to(&mut self, path: &Path) -> Result<(), MapError>;
    pub fn get(&self, module: &ModuleId) -> Option<&AllocationEntry>;
    pub fn get_mut(&mut self, module: &ModuleId) -> Option<&mut AllocationEntry>;
    pub fn upsert(&mut self, entry: AllocationEntry);
    pub fn remove(&mut self, module: &ModuleId) -> Option<AllocationEntry>;
    pub fn append_diagnostic(&mut self, rec: DiagnosticRecord); // trims to MAX_DIAGNOSTICS
    pub fn entry_index(&self) -> HashMap<ModuleId, &AllocationEntry>;
    pub fn entry_index_mut(&mut self) -> HashMap<ModuleId, &mut AllocationEntry>;
}
pub enum MapError { Io, Json, Anyhow, SchemaVersion(u32) } // thiserror
```

A `schema_version` mismatch is a hard error ("Refusing to silently migrate").

### `diagnoser.rs`

```rust
pub trait DiagnoserClient: Send + Sync {
    async fn estimate_performance(&self, signal: &FailureSignal, module: &ModuleId,
                                  module_input_summary: &str, module_output: &str) -> Option<f64>;
}
pub struct LlmDiagnoser { /* ProviderClient + model */ }
impl LlmDiagnoser {
    pub fn try_new(provider: &str, base_url: &str,
                   diagnoser_model: &str, api_key: Option<String>) -> Option<Self>;
}
```

`LlmDiagnoser` builds a strict judge prompt (input/output excerpts capped at 1200 chars each, `max_tokens = 16`, `temperature = 0.0`) and parses a leading decimal from the first line, clamped to `[0,1]`; any failure returns `None` → heuristic fallback. (`Observation` and the learning-loop fns are `pub(crate)`.)

### `query.rs`

```rust
pub struct SelectorQuery<'a> { /* engine: &'a SelectorEngine */ }
impl<'a> SelectorQuery<'a> {
    pub fn new(engine: &'a SelectorEngine) -> Self;
    pub fn status(&self) -> StatusReport;          // FR-001
    pub fn pool(&self) -> Vec<CandidateRow>;       // FR-003
    pub fn diagnostics(&self, limit: usize) -> Vec<DiagnosticRow>; // FR-018
    pub fn enable(&self); pub fn disable(&self);   // FR-002
    pub fn set_budget(&self, budget: u32);         // FR-009
    pub fn set_diagnoser_model(&self, model_id: &str) -> Result<(), String>; // FR-008
    pub fn pin(&self, module: ModuleId, model_id: String) -> Result<(), String>; // FR-012
    pub fn unpin(&self, module: &ModuleId) -> Result<(), String>;
}
pub struct StatusReport { active, enabled, configured_model, pool_size, pool_source,
                          pool_is_single_model, diagnoser_model, learning_budget,
                          budget_used, entries: Vec<AllocationRow> }
pub fn render_status(report: &StatusReport) -> String;
```

(`AllocationRow`, `CandidateRow`, `DiagnosticRow` are the plain row structs behind those reports.)

## Selection algorithm

Inputs to `resolve(module, turn_has_images, needs_tools, token_budget_hint)`:

1. Inactive (`!enabled || model != "auto" || pool empty`) → `AllocationSource::DisabledFallback` with the literal configured model.
2. Per-turn cache hit → `Cached` (populated by `refresh_at_turn_start`, stable for the turn).
3. Map entry present:
   - `pinned || implicit_pin` → honored verbatim as `Cached` (FR-012; user pins and `auxiliary.<task>.model` config pins).
   - model id not in the live pool (stale, FR-014) → cold-start re-resolve (`ColdStartReresolve`) or degraded fallback.
   - otherwise → `Cached`.
4. No entry → cold-start: `ColdStartScorer::pick` = cheapest capable model.

Cold start (`ColdStartScorer`): hard capability gates first (FR-005 — tools, vision, `context_window >= min`), then sort by tier `cost_weight()` ascending, tie-broken by `input + output` per-Mtok cost. Reason strings look like `cold-start: cheapest capable (versatile, tool-capable, ctx>=128k)`.

Diagnosis (learning loop, detached tokio task): `record_observation` enqueues an `Observation` and returns immediately. The loop, per observation:

| Step | Rule |
|---|---|
| Gate | skip when `!is_active()`, `learning_budget == 0`, or `budget_used_this_cycle >= learning_budget` |
| Estimate `p_j` | LLM judge (`DiagnoserClient`) if installed and it returns `Some`; else heuristic: `TurnError` = 0.15, `EmptyResponse` = 0.10, `AuxCallFailure` = 0.25, `RetryTriggered` = 0.30 (empty output) / 0.45 |
| Reallocate | only when `p_j < 0.5`; never for pinned/implicit-pin entries; pick the best alternative that is strictly higher-tier than the implicated model (or same-or-higher tier when the implicated model is `Flash`); upsert entry with `reason = "diagnoser reallocation: … (p_j=…)"` |
| Record | append a `DiagnosticRecord` (ring-buffer trimmed to 50), increment `budget_used_this_cycle`, persist the map atomically |

Budget semantics: `budget_used_this_cycle` counts processed observations; it resets to 0 in `refresh_at_turn_start` (each turn = a new optimization cycle, FR-010); `budget: 0` disables learning entirely (routing from the cold-start map only).

Fallbacks (`degraded_fallback`, FR-015): provider-curated `fallback_models` (first id present in the pool) → configured model (if non-empty and not `auto`) → first pool entry → configured model. Never returns `"auto"` (FR-020). `report_permanent_error` (e.g. `ModelNotFound` at call time) drops the dead non-pinned map entry + cache slot and enqueues a `TurnError` observation, so the next `resolve` cold-start-resolves a live model. `auto_disable_on_empty_pool` persists `enabled = false` when a fetch yields zero models (FR-017); a single-model pool makes the selector a no-op pass-through (no cross-module diversity).

## Integration

Call sites and wiring:

| Consumer | What it does |
|---|---|
| `joey-cli/src/llm_selector.rs` | `try_build_allocator(&Config)` — builds the engine when `model.selector.enabled` or model `auto`; applies implicit pins, threads profile `fallback_models`, fetches the pool (Copilot catalog for copilot-wire providers incl. ai-usage-hud proxy; models.dev otherwise), auto-disables on empty pool, installs `LlmDiagnoser` judge, starts the diagnoser. Also the `/llm-selector` slash-command handler (`llm_selector_slash` / render-only `llm_selector_slash_text`) |
| `joey-cli/src/main.rs` | `joey llm-selector <subcommand…>` — `LlmSelectorArgs` with `trailing_var_arg`; joins args and forwards to the same handler (byte-identical output; exit 0 ok / 1 err) |
| `joey-cli/src/oneshot.rs`, `repl.rs` | call `try_build_allocator` and `agent.install_model_allocator(allocator)`; also thread it into `register_orchestration_with_allocator` |
| `joey-agent-core/src/agent.rs` | `set_model_allocator` / `install_model_allocator` (latter also wires compression); `resolve_main_turn_model` resolves `ModuleId::MainTurn` (skipping allocator picks a copilot-wire provider can't serve); turn start calls `refresh_at_turn_start` and adapts `compressor.context_length` via `context_window_for` (FR-019); retry/empty-turn paths call `record_observation` (`RetryTriggered`); permanent model errors call `report_permanent_error` |
| `joey-agent-core/src/compression/summary.rs` | resolves `ModuleId::Compression` when the allocator is active |
| `joey-orchestration/src/delegation_tool.rs` | resolves `ModuleId::Subagent` when the effective model is `auto` (never sends `auto` to the API) |

`/llm-selector` subcommands (default `status`): `status`, `pool`, `allocations`, `diagnostics [-n <n>]` (default 20), `pin <module> <model>`, `unpin <module>`, `budget <n>`, `diagnoser [<model>]`, `enable`, `disable`, `refresh`, `help`/`-h`/`--help`; alias `/llm-s`. Module grammar: `main_turn | compression | subagent | custom:<name>`. `diagnoser <model>` rejects models not in the pool or not `Versatile` tier.

## Configuration

| Key | Type | Default | Meaning |
|---|---|---|---|
| `model.selector.enabled` | bool | `false` | master switch; the `auto` model also engages it |
| `model.selector.budget` | int | `8` | max diagnoser observations per learning cycle (0 = no learning) |
| `model.selector.diagnoser_model` | str | `''` | model id acting as LLM judge (empty = no judge, heuristic only) |
| `model.default: auto` | str | — | activation sentinel; a concrete model bypasses the selector |
| `auxiliary.compression.model` | str | — | explicit per-task model → implicit pin (FR-013); `auto`/empty = inherit |

On-disk state: `~/.joey/llm-selector/allocations.json` (`schema_version: 1`, atomic writes, machine-global across profiles via `process_joey_home()`).

## Testing

All tests are inline (`cargo test -p joey-llm-selector`); no `tests/` dir.

`allocator.rs` (29): `test_disabled_returns_configured_model`; `test_enabled_auto_resolves_from_pool`; `test_enabled_auto_empty_pool_degraded`; `test_pinned_entry_honored_verbatim`; `test_stale_entry_reresolved`; `test_context_window_returns_pool_max`; `test_record_observation_does_not_block`; `test_implicit_pins_from_config`; `test_implicit_pins_skipped_for_auto`; `test_is_active_false_with_empty_pool`; `test_is_active_true_after_set_pool`; `test_auto_disable_on_empty_pool`; `test_auto_disable_noop_with_pool`; `test_is_pool_single_model`; `test_degraded_fallback_uses_fallback_models`; `test_degraded_fallback_falls_to_configured`; `test_catalog_failure_completes_via_fallback`; `test_removed_model_reresolves_to_live_catalog_model`; `test_report_permanent_error_reresolves_live_model`; `test_report_permanent_error_respects_pins`; `test_report_permanent_error_noop_when_inactive`; `test_diagnoser_reallocates_on_failure`; `test_diagnoser_respects_pins`; `test_diagnoser_respects_implicit_pins`; `test_append_diagnostic_persists_and_increments_budget`; `test_record_observation_noop_without_diagnoser`; `test_diagnostics_ring_buffer_trim`; `test_perf_resolve_within_50us_from_cache`; `test_perf_refresh_at_turn_start_within_1ms`.

`scorer.rs` (6): `test_satisfies_capability_gates`; `test_never_assigns_incapable`; `test_picks_cheapest_capable`; `test_cost_tiebreak_within_tier`; `test_empty_pool`; `test_single_model_pool`.

`map.rs` (7): `test_round_trip_preserves_entries`; `test_round_trip_default_fields_when_absent`; `test_round_trip_custom_module`; `test_missing_file_is_cold_start`; `test_schema_version_mismatch_is_error`; `test_diagnostics_ring_trim`; `test_upsert_replaces_existing`.

`candidate.rs` (8): `test_vision_prefix_table`; `test_classify_tier`; `test_default_context_length`; `test_consolidate_copilot_chat_filter`; `test_copilot_context_window_prefers_full_window`; `test_consolidate_openrouter`; `test_consolidate_openrouter_free_tier_no_cost`; `test_pool_coverage_sc005`.

`module.rs` (5): `test_serde_snake_case`; `test_serde_custom`; `test_roundtrip_all_variants`; `test_validate_custom_name_ok`; `test_validate_custom_name_bad`.

`diagnoser.rs` (9): `test_estimate_performance_turn_error`; `test_estimate_performance_empty_response`; `test_estimate_performance_retry`; `test_estimate_performance_in_range`; `test_parse_pj_plain_decimal`; `test_parse_pj_clamps_and_trims`; `test_parse_pj_rejects_prose`; `test_learning_loop_uses_judge_when_present`; `test_learning_loop_falls_back_to_heuristic_when_judge_none`.

`query.rs` (3): `test_render_disabled`; `test_render_no_catalog`; `test_render_active`.

Consumer-side coverage lives in `joey-cli/src/llm_selector.rs` tests (allocator gating, provider-name resolution/magnetization, byte-parity rendering) and `joey-agent-core/src/agent.rs` `feature011_*` tests (no-allocator verbatim model, turn-start no-op, inactive-allocator fallback, prompt/history stability).

## See also

- [joey-agent-core.md](joey-agent-core.md) — turn loop intercepts and compression wiring
- [joey-cli.md](joey-cli.md) — command tree, REPL slash commands, `joey llm-selector`
- [joey-providers.md](joey-providers.md) — provider profiles, fallback models, Copilot catalog fetch
- [joey-orchestration.md](joey-orchestration.md) — subagent delegation intercept
- [README.md](README.md) — feature-page index
- `specs/011-dynamic-llm-selector/spec.md` and `PORTING.md` (feature 011 section) — spec and parity status
