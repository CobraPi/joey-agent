# Contract: CandidateModel & Catalog Consolidation

**Feature**: 011-dynamic-llm-selector | **Surface**: in-memory public type + consolidator
**Owning crate**: `joey-llm-selector`
**Stability**: public API (Constitution VII) — fields are additive; a field
removal or type change requires a MAJOR bump.

This is the typed view of a model in the active provider's catalog, plus the
consolidator that normalizes the three scattered JSON sources into it
(research.md §6). The candidate pool the selector allocates from is
`Vec<CandidateModel>`.

---

## Type

```rust
// crates/joey-llm-selector/src/candidate.rs

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateModel {
    pub id: String,
    pub provider: String,
    pub context_window: u64,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub tier: CapabilityTier,
    pub cost: Option<Cost>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CapabilityTier { Flash, Standard, Versatile, Frontier }

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cost { pub input_per_mtok: f64, pub output_per_mtok: f64 }
```

## Catalog sources (inputs to the consolidator)

The consolidator accepts the existing (untyped) fetch results and normalizes
them. It does NOT introduce a new HTTP path — it consumes what's already there:

| Source | Existing fetch | Fields read |
|---|---|---|
| Copilot | `joey_providers::copilot::fetch_model_catalog` (`crates/joey-providers/src/copilot.rs:518`) → `Vec<Value>` | `id`; context via `catalog_context_window` (`copilot.rs:602`); tools via `supported_endpoints`/`model_api_mode` (`copilot.rs:584-594`, `:463`); chat-type filter `capabilities.type == "chat"` (`copilot.rs:576-583`) |
| OpenRouter | `joey_cli::model_catalog::fetch_openrouter_models` (`model_catalog.rs:617`) / `fetch_models_with_pricing` (`:696`) | `id`; tools via `openrouter_model_supports_tools` (`model_catalog.rs:587`); cost from `pricing` |
| models.dev | `joey_cli::model_catalog::fetch_models_dev` (`model_catalog.rs:323`, 1h TTL) | `tool_call` (`:435`); `limit.context` (`:470`); `cost.{input,output}` (`:449`) |
| Generic OpenAI-compat | `probe_api_models` (`model_catalog.rs:505`) | `id` only; capabilities inferred from id table |

Note: the Copilot catalog fetch lives in `joey-providers` (reachable from the
new crate directly); the OpenRouter/models.dev/probe fetches live in
`joey-cli`. For the selector to use the latter without an upward dependency
(back to `joey-cli`, which would create a cycle), the consolidator accepts the
fetched `Value`/`Vec<Value>` as an **input parameter** from the caller
(`joey-cli` at enablement), rather than calling `joey-cli` directly. This keeps
the DAG acyclic (research.md §4).

## Vision-support derivation (the metadata gap)

No source exposes a reliable vision field today. The consolidator derives
`supports_vision` from:

1. A curated id-prefix table in `candidate.rs` (additive — adding a prefix is
   non-breaking). Initial set (to be tuned in implementation):
   `gpt-4o`, `gpt-4.1`, `gpt-5`, `claude-3-7`, `claude-sonnet-4`,
   `claude-opus-4`, `claude-haiku-4`, `gemini-`, `grok-vision`.
2. A provider hint when present: Copilot `capabilities.supports.vision` (if
   exposed), OpenRouter `architecture.input_modalities` contains `"image"`.
   A positive hint overrides the table; absence of a hint falls back to the
   table.

`supports_tools` is derived similarly: OpenRouter `supported_parameters`
contains `"tools"`, models.dev `tool_call`, Copilot endpoint membership; absent
all signals, the id-prefix table provides a conservative default.

## Tier assignment

`tier` is derived from the model id + cost using a small classifier
(non-breaking, additive rules). Heuristic:
- `Frontier`: top per-vendor flagship ids (`gpt-5`, `claude-opus-4`,
  `gemini-2.5-pro`, `grok-4`).
- `Versatile`: strong general-purpose (`gpt-4.1`, `claude-sonnet-4`,
  `gemini-2.5-flash` …) — default diagnoser tier.
- `Standard`: mid-tier.
- `Flash`: id contains `haiku`/`flash`/`mini`/`nano`, or cost is the lowest
  tier.

The classifier is the cold-start scorer's cost tie-break input (FR-006:
"within 5% of top p_j, prefer the cheaper model" → lower tier wins).

## Validation

- `id` non-empty; `context_window > 0` (fall back chain: catalog →
  `DEFAULT_CONTEXT_LENGTHS` (`compression/catalog.rs:83`) → 8_192, logged).
- A candidate is dropped from the pool if its `id` cannot be normalized or its
  chat-type filter fails (Copilot `capabilities.type != "chat"`).

## Pool semantics

- The pool is rebuilt on TTL expiry, on `/llm-selector refresh`, or on provider
  switch. It is never mutated piecemeal.
- Empty pool → auto-disable with notice (FR-017, User Story 1 acceptance 4).
- Single-model pool → selector becomes a no-op pass-through (Edge Cases).
- The pool is not persisted; it is rebuilt from the catalog. Only allocation
  decisions (which reference pool ids) are persisted in the map.

## Backward compatibility

`CandidateModel` is a new type — no existing type is changed. Adding fields is
additive; the consolidator ignores unknown JSON fields rather than failing
(forward-compatible with provider schema drift).
