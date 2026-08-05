# Implementation Plan: Dynamic LLM Model Selector

**Branch**: `011-dynamic-llm-selector` | **Date**: 2026-08-04 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/011-dynamic-llm-selector/spec.md`

**Note**: This template is filled in by the `/speckit-plan` command; its definition describes the execution workflow.

## Summary

Add a dynamic per-module LLM model allocator that engages when the user selects
the `auto` model on a catalog-exposing provider. The allocator treats the agent
as a compound AI system (Chen et al., arXiv:2502.14815): it assigns each
distinct LLM call site ("module") the best-suited model drawn from the active
provider's live `/models` catalog, learns better allocations asynchronously via
an LLM diagnoser triggered only by observable failure, and persists a global
allocation map at `~/.joey/llm-selector/allocations.json`. The whole engine,
diagnoser, map, and a narrow `ModelAllocator` trait live in a new dedicated
crate `joey-llm-selector`; `joey-agent-core` consumes only the trait at the two
real LLM call sites (main turn, history compression) and the subagent dispatch
path. `/llm-selector` is the control surface (inspect, pin, set budget,
disable).

Technical approach (from research.md): build a self-contained capability+cost
scorer inside `joey-llm-selector` — the Rust port has NO existing `auto`
cost-router (it was deliberately not ported from upstream Python), so the
cold-start default and the disable-fallback cannot "layer on" a scorer that
doesn't exist. The scorer consolidates the three scattered catalog sources
(Copilot catalog JSON, OpenRouter JSON, models.dev registry) into one typed
`CandidateModel`; vision support — which is not recorded per-model anywhere
today — is derived from a small curated id-prefix table plus provider
capability hints. The allocation map is a versioned JSON file written via
`joey_core::utils::atomic_json_write` (the existing atomic-replace primitive),
stored under `process_joey_home()` so it is genuinely shared across profiles.

## Technical Context

**Language/Version**: Rust stable (rust-toolchain.toml), edition 2021 — matches
the existing workspace.

**Primary Dependencies**: existing workspace crates only — `joey-core`
(config, `atomic_json_write`, `process_joey_home`), `joey-providers`
(`ProviderProfile`, `ApiMode`, Copilot `fetch_model_catalog`, backoff helpers),
`tokio` (detached diagnoser task), `serde`/`serde_json` (allocation map),
`tracing`. No new external dependency is introduced (Constitution VIII); see
research.md §5 for the explicit "no new dep" justification against alternatives.

**Storage**: a single JSON file at
`~/.joey/llm-selector/allocations.json` (honouring `JOEY_HOME`; resolved via
`process_joey_home()` at `crates/joey-core/src/constants.rs:135` so the map is
machine-global across profiles, per FR-014). Written atomically via
`joey_core::utils::atomic_json_write` (`crates/joey-core/src/utils.rs:156`,
tmp-in-dir + fsync + rename — the same primitive `auth_store` builds on).
SQLite is rejected as unjustified for a small flat map read once per turn and
rewritten occasionally (Constitution VIII; resolved NEEDS CLARIFICATION, see
research.md §3).

**Testing**: `cargo test -p joey-llm-selector` for the new crate (unit tests
for scorer/allocator/map; round-trip tests for the on-disk JSON schema —
Constitution IV); targeted tests in `joey-agent-core` for the trait intercepts
(main turn + compression model swap) and in `joey-cli` for `/llm-selector`
dispatch. `cargo build --workspace` + `cargo test --workspace` stay green on
every increment (Constitution VII).

**Target Platform**: same as the workspace — native `joey` binary on macOS /
Linux / Windows. No new platform surface.

**Project Type**: library crate (`joey-llm-selector`) + narrow trait consumer
edits in `joey-agent-core` + slash-command wiring in `joey-cli`. No UI stack
additions.

**Performance Goals**:
- Allocation resolution on the hot path: < 50µs per module when served from
  the per-turn cache (the cache is a `HashMap<ModuleId, ModelId>` populated at
  turn start); zero network calls on the hot path after turn-start.
- Turn-start refresh: bounded by one map file read (single small JSON, atomic)
  — target < 1ms; no catalog fetch on the hot path (catalog is fetched lazily
  at enablement and refreshed on a TTL, never per turn).
- Diagnoser: runs strictly off the hot path via a detached `tokio::spawn`
  task (FR-009); one LLM call per nominated module, capped by the learning
  budget. Never blocks an interactive turn.

**Constraints**:
- MUST NOT mutate past messages, reorder roles, inject synthetic mid-loop
  messages, or alter the byte-stable system prompt (FR-016, SC-006). The
  system prompt is built once in `Agent::new` (`agent.rs:309-316`) and the
  model id is independent of prompt bytes — verified in research.
- MUST NOT send an unroutable/non-existent model id to the API (FR-015); every
  allocation is validated against the active catalog at resolve time, and a
  stale global-map entry referencing a model absent from the active profile's
  catalog is re-resolved via the cold-start scorer before use (FR-014).
- `cargo build --workspace` and `cargo test --workspace` MUST stay green on
  every increment (Constitution VII, NON-NEGOTIABLE).
- DAG MUST stay acyclic: `joey-llm-selector` depends only on `joey-core` +
  `joey-providers`; `joey-agent-core` and `joey-cli` depend upward on it
  (Constitution VI). Verified acyclic in research.md §4.

**Scale/Scope**: 1 new crate (~6-9 source files, ~1.5-2.5k LOC), trait edits
at 3 call sites in 2 existing crates, 1 new slash command. The module set is
small today (2 real LLM call sites + subagent dispatch — see research.md §2),
so the allocator's real surface is narrow; the `ModuleId` enum is designed to
grow as new call sites are added without breaking the on-disk schema (versioned,
additive).

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Evaluated honestly against all eight principles of
`.specify/memory/constitution.md` v1.1.0.

| Principle | Status | Evidence |
|---|---|---|
| I. Workspace-First Rust | **PASS** | All selector logic lives in a new dedicated crate `crates/joey-llm-selector`, added to `[workspace] members`. Independently buildable/testable: `cargo build -p joey-llm-selector` / `cargo test -p joey-llm-selector`. No code added at workspace root. |
| II. CLI/TUI Parity | **PASS** | `/llm-selector` is registered as a chat slash command (`slash.rs` `REGISTRY`) AND reachable from the CLI text surface. All capabilities (inspect, pin, set budget, disable) are available via text in/out — no UI-only affordance. |
| III. Filesystem Is Source of Truth | **PASS** | The allocation map is a JSON file on disk (`allocations.json`); UI/CLI reads reflect current file contents and writes go back synchronously via atomic rename. No in-memory-only state that can drift. (This principle scopes to spec-kit artifacts by its text, but the selector honors the same file-first discipline.) |
| IV. Test-First for New Crates | **PASS** | `joey-llm-selector` ships unit tests for scorer/allocator/map and round-trip tests for the on-disk JSON schema (file → model → file) alongside implementation. Tasks phase will enumerate the test matrix. |
| V. Incremental, Reviewable Delivery | **PASS** | Decomposed into independently shippable increments matching the user-story priority: (P1) crate skeleton + `auto` sentinel + cold-start scorer + `/llm-selector` inspect; (P1) per-module routing at the 2 call sites; (P2) diagnoser + learning loop + pins; (P3) graceful degradation. Each increment builds and tests green on its own. |
| VI. Modularity and Decoupling | **PASS** | `joey-llm-selector` exposes a narrow `ModelAllocator` trait; `joey-agent-core` depends only on that trait, never on the engine internals. `joey-llm-selector` depends downward on `joey-core` + `joey-providers` only. DAG verified acyclic (research.md §4). A change in the engine never forces edits to `joey-agent-core` beyond the trait. |
| VII. Backward Compatibility (NON-NEGOTIABLE) | **PASS (with versioned-format note)** | Feature is strictly additive: default-off (`model.selector.enabled = false`); selecting `auto` is opt-in; existing concrete-model routing is byte-identical when the feature is off. The `allocations.json` schema is a **new** versioned on-disk public format (`schema_version` field), so there is no prior format to break — but any future breaking change to it will require a MAJOR bump + migration (FR-014). The new `model.selector.*` config keys are additive. Regression coverage: tasks MUST include tests asserting (a) feature-off behavior is unchanged, (b) the 2 intercept call sites still use the configured model when off, (c) `/llm-selector --help` exit code 0. |
| VIII. Performance Discipline & Lean Code | **PASS** | No new external dependency (research.md §5). Hot path is a HashMap lookup (< 50µs target). Diagnoser is off-hot-path via `tokio::spawn`. Performance budgets recorded above and in research.md §6. Atomic JSON write reuses an existing primitive. SQLite rejected as unjustified weight. |

**Gate result (pre-design)**: PASS — no violations. No entries required in
Complexity Tracking. (The one design tension — the spec's assumption that an
`auto` cost-scorer already exists — is resolved in research.md §1 by building a
self-contained scorer; it is not a constitution violation, just a scope
correction documented for the implementer.)

### Post-design re-check (after Phase 1)

Re-evaluated against the materialized `data-model.md` and `contracts/`. All
eight principles still PASS; no new violation emerged from the design. The
design concretized the backward-compatibility story (Constitution VII) rather
than weakening it:

- The `ModuleId` enum carries an additive `Custom(String)` variant + a
  `#[non_exhaustive]`/default-method extension policy, so new call sites do
  not break the on-disk schema or the trait (data-model.md Entity 2,
  contracts/model-allocator-trait.md).
- The `allocations.json` schema is versioned (`schema_version: 1`) from day
  one with an explicit migration policy (contracts/allocation-map-schema.md).
- The `CandidateModel` consolidator accepts `joey-cli`'s fetched JSON as an
  **input parameter** rather than depending upward on `joey-cli`, preserving
  the acyclic DAG (contracts/candidate-model.md; research.md §4).
- The `/llm-selector` command is purely additive — prefix-resolved, no shadow
  of existing commands (contracts/llm-selector-command.md).

The two scope corrections from research.md (no pre-existing `auto` scorer;
only 2 real LLM call sites + subagent dispatch today) are reflected
consistently across plan, data-model, contracts, and quickstart.

## Project Structure

### Documentation (this feature)

```text
specs/011-dynamic-llm-selector/
├── plan.md              # This file (/speckit-plan command output)
├── research.md          # Phase 0 output (/speckit-plan command)
├── data-model.md        # Phase 1 output (/speckit-plan command)
├── quickstart.md        # Phase 1 output (/speckit-plan command)
├── contracts/           # Phase 1 output (/speckit-plan command)
│   ├── model-allocator-trait.md   # the narrow trait joey-agent-core consumes
│   ├── allocation-map-schema.md   # versioned on-disk JSON format (FR-014)
│   ├── llm-selector-command.md    # /llm-selector slash-command contract
│   └── candidate-model.md         # typed catalog entry the scorer builds
└── tasks.md             # Phase 2 output (/speckit-tasks command - NOT created by /speckit-plan)
```

### Source Code (repository root)

```text
crates/joey-llm-selector/                 # NEW crate (Constitution I, IV, VI)
├── Cargo.toml                            # deps: joey-core, joey-providers (+ workspace serde/tokio/tracing)
├── src/
│   ├── lib.rs                            # public re-exports: ModelAllocator trait + types
│   ├── model_allocator.rs                # the ModelAllocator trait (the narrow surface agent-core consumes)
│   ├── candidate.rs                      # CandidateModel + catalog consolidation (Copilot/OR/models.dev → typed)
│   ├── scorer.rs                         # cold-start capability+cost scorer (the new scorer; see research §1)
│   ├── module.rs                         # ModuleId enum (main_turn, compression, subagent, …) — additive
│   ├── allocator.rs                      # SelectorEngine: per-turn cache, resolve(), refresh_at_turn_start()
│   ├── diagnoser.rs                      # detached-tokio LLM diagnoser + learning loop (FR-008/009/010)
│   ├── map.rs                            # AllocationMap: load/save (atomic_json_write), pins, stale re-resolve
│   └── query.rs                          # /llm-selector query API (state, pool, allocations, diagnostics)

crates/joey-agent-core/src/
├── agent.rs                              # EDIT: intercept build_request() line ~835 (main turn)
├── compression/summary.rs                # EDIT: intercept AuxSummaryBackend model resolution (~summary.rs:180)
└── (delegation path)                     # EDIT: joey-orchestration subagent.rs:19 resolve_model() intercept

crates/joey-cli/src/
├── slash.rs                              # EDIT: add /llm-selector to REGISTRY (near slash.rs:69)
├── repl.rs                               # EDIT: add "llm-selector" => ... dispatch arm (~repl.rs:825)
└── commands/llm_selector.rs              # NEW: /llm-selector command handler (text in/out per Constitution II)

Cargo.toml                                # EDIT: add "crates/joey-llm-selector" to [workspace] members + [workspace.dependencies]
crates/joey-agent-core/Cargo.toml         # EDIT: add joey-llm-selector.workspace = true
crates/joey-cli/Cargo.toml                # EDIT: add joey-llm-selector.workspace = true

crates/joey-llm-selector/tests/
├── scorer.rs                             # cold-start scorer unit/contract tests
├── map_round_trip.rs                     # AllocationMap file → model → file (Constitution IV)
├── allocator_cache.rs                    # per-turn cache + stale re-resolve (FR-014)
└── diagnoser_budget.rs                   # budget-bounds + never-blocks-hot-path (FR-009)
```

**Structure Decision**: Single new library crate (`joey-llm-selector`) plugged
behind a narrow trait into the two existing LLM call sites + the subagent
dispatch path, plus a slash-command handler in `joey-cli`. This matches how the
workspace already factors cross-cutting concerns (`joey-mcp`, `joey-cron` are
each their own crate behind a small surface). No new binary, no new UI stack,
no web frontend — Constitution II parity is satisfied by the text-mode
`/llm-selector` command. The DAG stays acyclic because the new crate sits at
the `joey-providers` layer (depends only on `joey-core` + `joey-providers`) and
is consumed upward by `joey-agent-core`/`joey-cli`.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

No violations. Intentionally left blank.

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| — | — | — |
