# Data Model: Dynamic LLM Model Selector

**Feature**: 011-dynamic-llm-selector | **Phase**: 1 | **Date**: 2026-08-04

Entities the selector introduces or consumes. On-disk shapes are versioned
public formats (Constitution VII); in-memory types are the new crate's public
surface where noted. All Rust types live in `crates/joey-llm-selector/src/`.

---

## Entity 1 — CandidateModel (in-memory, built from catalog consolidation)

A typed view of one chat-capable model in the active provider's catalog. Built
by the `CatalogConsolidator` from the three scattered sources (see research.md
§6). Not persisted directly — the persisted form is the `AllocationEntry`
which stores only the model id.

```rust
/// One model in the active provider's live catalog, normalized to a typed view.
/// (crates/joey-llm-selector/src/candidate.rs)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateModel {
    /// Provider-internal model id, sent verbatim to the API (e.g. "gpt-4.1").
    pub id: String,
    /// Owning provider key (e.g. "copilot", "openrouter").
    pub provider: String,
    /// Highest configurable context window for this model, in tokens.
    /// Source: Copilot capabilities.limits.max_prompt_tokens / models.dev limit.context /
    /// DEFAULT_CONTEXT_LENGTHS substring table. FR-019: allocated models run at this max.
    pub context_window: u64,
    /// Tool/function-calling support. Derived (see research.md §6).
    pub supports_tools: bool,
    /// Vision/image support. Derived from id-prefix table + provider hints (research.md §6).
    pub supports_vision: bool,
    /// Capability tier, used for the cost tie-break (FR-006) and cold-start scorer.
    pub tier: CapabilityTier,
    /// Billing cost if known (models.dev / OpenRouter pricing). None when unavailable.
    pub cost: Option<Cost>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CapabilityTier {
    Flash,      // small/cheap/fast (e.g. haiku, flash, mini)
    Standard,   // mid-tier
    Versatile,  // strong general-purpose (default diagnoser tier, research.md §6)
    Frontier,   // top-tier / flagship
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cost {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}
```

**Validation rules**:
- `id` MUST be non-empty and MUST exist in the active profile's live catalog at
  resolve time (FR-015 — never send an unroutable id). Validation is the
  resolver's job, not the struct's.
- `context_window` MUST be > 0; if no source provides it, the consolidator
  falls back to `DEFAULT_CONTEXT_LENGTHS` (`compression/catalog.rs:83`) and
  finally to a conservative 8_192 default (logged).

**State transitions**: none — immutable once consolidated per catalog fetch.

---

## Entity 2 — ModuleId (in-memory, persisted as string in the map)

A distinct LLM call site in the compound system (research.md §2). The enum is
seeded with the three real intercept points and is extensible additively.

```rust
/// A distinct LLM call site ("module") in the agent's compound system.
/// (crates/joey-llm-selector/src/module.rs)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleId {
    /// The main agent reasoning turn (agent.rs:831 build_request intercept).
    MainTurn,
    /// History compression side-LLM (summary.rs:257 intercept).
    Compression,
    /// A delegated subagent goal (orchestration subagent.rs:19 intercept).
    Subagent,
    /// A named call site added after initial release (additive; Constitution VII).
    /// Persisted as {"custom":"<name>"}; new variants don't break old maps.
    Custom(String),
}
```

**Validation rules**: `Custom` names MUST be non-empty and match
`^[a-z][a-z0-9_]{0,31}$`.

**State transitions**: none.

---

## Entity 3 — AllocationEntry (persisted; one per module)

One row of the allocation map. Persisted in `allocations.json`
(see Entity 5). This is the selector's source of truth for routing.

```rust
/// One module's allocation (persisted). (crates/joey-llm-selector/src/map.rs)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocationEntry {
    pub module: ModuleId,
    /// The model id assigned to this module (sent to the API when called).
    pub model_id: String,
    /// True when the user pinned this module via /llm-selector (FR-012);
    /// pinned entries are exempt from reallocation (FR-012, SC-008).
    #[serde(default)]
    pub pinned: bool,
    /// True when an existing explicit per-task config (e.g. auxiliary.compression.model)
    /// implicitly pins this module (FR-013).
    #[serde(default)]
    pub implicit_pin: bool,
    /// Human-readable reason for the current assignment (FR-011, SC-008).
    #[serde(default)]
    pub reason: String,
    /// Estimated per-module performance p_j in [0.0, 1.0] from the diagnoser.
    /// None until the diagnoser has evaluated this module (FR-008).
    #[serde(default)]
    pub estimated_performance: Option<f64>,
    /// ISO-8601 timestamp of the last change (for /llm-selector history display).
    #[serde(default)]
    pub updated_at: Option<String>,
}
```

**Validation rules**:
- `model_id` is validated against the active catalog at resolve time; if absent
  (e.g. global map loaded under a different profile), the entry is treated as
  stale and re-resolved via the cold-start scorer before any call (FR-014).
- `estimated_performance` when present MUST be in `[0.0, 1.0]`.

**State transitions** (driven by the learning loop, FR-010):
```
[Cold-start assigned] --diagnoser nominates--> [Reallocated] --user pins--> [Pinned]
        ^                          |                                 |
        |                          v                                 v
        +------- re-resolve on stale (FR-014) ------+ <--- pin exempt from reallocation
```
A `Pinned` entry never transitions back except by explicit user unpin
(`/llm-selector unpin <module>`).

---

## Entity 4 — CandidateModelPool (in-memory, built at enablement)

The set of `CandidateModel`s discovered in the active catalog. The selector
allocates from this set (FR-003, SC-005).

```rust
/// The active provider's consolidated candidate pool. (candidate.rs)
#[derive(Debug, Clone, Default)]
pub struct CandidateModelPool {
    pub models: Vec<CandidateModel>,
    /// Catalog fetch provenance for /llm-selector reporting (FR-001).
    pub source: CatalogSource,
    pub fetched_at: chrono::DateTime<chrono::Utc>,
}

pub enum CatalogSource { Copilot, OpenRouter, ModelsDotDev, GenericProbe, Empty }
```

**Validation rules**:
- A pool with zero chat-capable models triggers auto-disable (FR-017, User
  Story 1 acceptance 4).
- A pool with exactly one eligible model makes the selector a no-op
  pass-through (Edge Cases, spec.md line 126).

**State transitions**: rebuilt on TTL expiry or `/llm-selector refresh`; never
mutated piecemeal.

---

## Entity 5 — AllocationMap (persisted at ~/.joey/llm-selector/allocations.json)

The top-level persisted artifact. Versioned schema (Constitution VII).

On-disk JSON shape (see contracts/allocation-map-schema.md for the full
contract):

```json
{
  "schema_version": 1,
  "updated_at": "2026-08-04T12:00:00Z",
  "enabled": true,
  "diagnoser_model": "gpt-4.1",
  "learning_budget": 8,
  "budget_used_this_cycle": 0,
  "entries": [
    {
      "module": "main_turn",
      "model_id": "gpt-4.1",
      "pinned": false,
      "implicit_pin": false,
      "reason": "cold-start: cheapest versatile tool-capable model",
      "estimated_performance": null,
      "updated_at": "2026-08-04T12:00:00Z"
    },
    {
      "module": "compression",
      "model_id": "claude-haiku-4-5",
      "pinned": false,
      "implicit_pin": false,
      "reason": "diagnoser reallocation: +0.12 p_j vs prior",
      "estimated_performance": 0.81,
      "updated_at": "2026-08-04T12:05:00Z"
    }
  ],
  "diagnostics": [
    {
      "at": "2026-08-04T12:04:00Z",
      "module": "compression",
      "signal": "empty_response",
      "implicated_model": "glm-4.5-flash",
      "rationale": "empty output on compression call; reallocating to claude-haiku-4-5"
    }
  ]
}
```

**Validation rules**:
- `schema_version` MUST be `1` for this feature version; a future breaking
  change requires a MAJOR bump + migration (FR-014, Constitution VII).
- `entries` MUST have at most one row per `ModuleId` (map semantics).
- On load, every entry whose `model_id` is absent from the active catalog is
  flagged stale and re-resolved via the cold-start scorer before use (FR-014).

**State transitions**:
```
[absent] --enable--> [enabled, cold-start entries] --diagnoser--> [enabled, learned]
   |                                                        |
   +------------- disable / auto-disable (FR-017) ----------+
                          |
                          v
              [disabled: entries retained but ignored;
               concrete cfg.model() used for all modules]
```
Writes are atomic (`atomic_json_write`, research.md §3).

---

## Entity 6 — DiagnosticRecord (persisted inside AllocationMap.diagnostics)

A diagnoser judgment, surfaced to the user (FR-018, SC-008).

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticRecord {
    pub at: String,                 // ISO-8601
    pub module: ModuleId,
    pub signal: FailureSignal,      // what triggered the diagnoser (FR-009)
    pub implicated_model: String,
    pub rationale: String,          // diagnoser's natural-language rationale
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureSignal {
    TurnError,
    AuxCallFailure,
    EmptyResponse,
    RetryTriggered,
}
```

**Validation rules**: `diagnostics` is a bounded ring buffer (last N, default
50) to keep the file small.

**State transitions**: append-only; trimmed to the bound.

---

## Entity 7 — LearningBudget (config + in-memory counter)

Bounds diagnoser/model calls per optimization run (FR-009, FR-010).

```rust
pub struct LearningBudget {
    pub max_calls: u32,         // config: model.selector.budget (default 8)
    pub used: u32,              // incremented per diagnoser call this run
}
```

**Validation rules**: `max_calls == 0` disables learning (selector still routes
from the cold-start map). The counter is reset at the start of each
optimization run.

---

## Relationships

```
CandidateModelPool 1 ─── * CandidateModel         (consolidates catalog)
        |
        | feeds
        v
  ColdStartScorer ─��> AllocationEntry (per module) ──┐
        ^                                            │
        | re-resolves stale                          │
        |                                            v
  AllocationMap 1 ─── * AllocationEntry         ModelAllocator trait
        |                                   (resolve(module) -> model_id)
        | also holds                                ^
        └── * DiagnosticRecord                       │
                                                   consumed by
                                         agent.rs:835, summary.rs:257,
                                         subagent.rs:19  (the 3 intercepts)

LearningBudget bounds the Diagnoser, which appends DiagnosticRecord and
mutates AllocationEntry.estimated_performance / model_id.
```

---

## Validation rules summary (cross-cutting)

- **FR-005 capability hard-gates**: at resolve time, a candidate MUST satisfy
  the module's requirements — `supports_tools` for `MainTurn`/`Subagent`,
  `supports_vision` when the turn carries images, `context_window >=` the
  module's token need. The scorer filters on these before applying cost.
- **FR-019 max context window**: the resolver returns the model's
  `context_window` (the catalog max) — it never caps below catalog max unless
  the user explicitly sets one.
- **FR-016 message/prompt stability**: none of these entities touch the
  conversation history or system prompt; they only carry model ids. Verified
  by the intercept-point analysis (research.md §2) — the system prompt is
  built once in `Agent::new` (`agent.rs:309-316`) and is independent of model
  id.
