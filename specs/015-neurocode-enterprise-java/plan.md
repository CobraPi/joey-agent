# Implementation Plan: NeuroCode — Enterprise Java & Pega Rule System Coding Agent

**Branch**: `015-neurocode-enterprise-java` | **Date**: 2026-08-13 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/015-neurocode-enterprise-java/spec.md`

## Summary

Add an enterprise-Java/Pega-aware coding-assistance engine that (a) classifies
incoming coding requests by complexity and routes them between an economical
and a frontier model tier (composing with the existing `011-dynamic-llm-
selector`'s `ModelAllocator`), (b) maintains a structural dependency graph of
the target codebase in SQLite+FTS5 (built via tree-sitter-java) and assembles
a dependency-aware context graph per request so the model never reasons about
a type whose definition it hasn't been given, (c) understands the Pega
Platform rule system in a version-adaptive way (detecting the declared Pega
version and grounding generation in ingested rule-type metadata), (d) runs an
asynchronous build/verify feedback loop that records successes as patterns and
failures as anti-patterns, and (e) ingests domain knowledge (framework docs,
entities/DTOs, postmortems). The whole engine lives in a new dedicated crate
`joey-neurocode`; `joey-agent-core` consumes only a narrow `NeuroCodeEngine`
trait, and four tools give the model agentic access to the index.

Technical approach (from [research.md](./research.md)): the source plan's
Python/Qdrant stack is re-targeted to this Rust workspace. The decisive
substitution is **SQLite + FTS5 (already bundled) instead of Qdrant** — the
graph-aware retrieval at the heart of the feature is typed-edge traversal
(implements/injects/references), not nearest-neighbor search, so a vector DB
adds weight without benefit (Constitution VIII). The **only new external
dependency is tree-sitter + tree-sitter-java** (the official Rust bindings),
justified by FR-006's hard requirement for deterministic, LLM-independent
Java parsing. Tier routing is a deterministic rule-based classifier (not an
LLM call), composing with spec 011's allocator rather than duplicating it.

## Technical Context

**Language/Version**: Rust stable (rust-toolchain.toml), edition 2021 — matches the existing workspace.

**Primary Dependencies**:
- *New (2)*: `tree-sitter = "0.26"` + `tree-sitter-java = "0.23"` — the official Rust bindings + maintained Java grammar. Justified in research.md §3, §10 (the only new external deps; ~270KB compiled total; no transitive runtime deps). Required by FR-006 (deterministic syntax-aware parsing).
- *Existing workspace crates (reused, no new dep)*: `joey-core` (SQLite via bundled rusqlite, FTS5, `atomic_json_write`, `process_joey_home`, `Config` dotted-key API), `joey-tools` (`Tool` trait, registry, subprocess execution path used by the verify loop), `joey-llm-selector` (`ModelAllocator` trait — NeuroCode composes with it per FR-018), `tokio` (detached feedback-loop task), `serde`/`serde_json`, `tracing`.

**Storage**: A per-project SQLite database at `~/.joey/neurocode/projects/<project-hash>/graph.db` (honouring `JOEY_HOME` via `process_joey_home()`). Uses the workspace's existing bundled rusqlite (separate connection + file from the session DB — never touches `SCHEMA_VERSION = 22`). Stores: structural code-artifact nodes, typed dependency-graph edges, FTS5 index over artifact symbols/metadata, learned patterns + anti-patterns, domain-knowledge provenance. Schema is a new versioned format (`neurocode_schema_version: 1`). See research.md §2, §8.

**Testing**: `cargo test -p joey-neurocode` for the new crate — unit tests for the classifier, graph builder, tree-sitter extractor, and tier resolver; round-trip tests for the SQLite schema (file → model → file, Constitution IV); integration tests for graph expansion and context assembly. Targeted tests in `joey-agent-core` for the trait intercept (classify + assemble_context before model dispatch). Regression tests asserting disabled-state byte-identicality (FR-020, SC-008). `cargo build --workspace` + `cargo test --workspace` stay green on every increment (Constitution VII).

**Target Platform**: same as the workspace — native `joey` binary on macOS / Linux / Windows. No new platform surface.

**Project Type**: library crate (`joey-neurocode`) + narrow trait consumer edit in `joey-agent-core` + tool registrations in `joey-tools` + slash-command wiring in `joey-cli`. No UI stack additions.

**Performance Goals**:
- Complexity classification (`classify`): < 50µs per request — deterministic rule evaluation, no network, no DB read (operates on request text + in-memory scope signals). Hot path (FR-017).
- Context assembly (`assemble_context`): < 5ms for a focused-tier slice, < 20ms for a full-tier graph — one FTS5 query + bounded graph traversal (depth ≤ 2 edges) against the local SQLite index. No network.
- Tree-sitter ingestion: ~2000–5000 LOC/sec (deterministic parse; runs asynchronously off the hot path).
- Verify loop: runs strictly off the hot path via detached `tokio::spawn` (FR-017); bounded by per-step `timeout_sec` config.

**Constraints**:
- MUST NOT mutate past messages, reorder roles, inject synthetic mid-loop messages, or alter the byte-stable system prompt (FR-020, SC-008). NeuroCode changes *what context and which tier* serve a coding task, not message structure or auth.
- MUST NOT duplicate 011's allocation map, learning loop, or diagnostics (FR-018). NeuroCode's tier is an input/constraint to 011, or a direct config lookup when 011 is off.
- `cargo build --workspace` and `cargo test --workspace` MUST stay green on every increment (Constitution VII, NON-NEGOTIABLE).
- DAG MUST stay acyclic: `joey-neurocode` depends only on `joey-core` + `joey-tools` + the `ModelAllocator` trait (from `joey-llm-selector`, same layer); `joey-agent-core` and `joey-cli` depend upward on `joey-neurocode` (Constitution VI). Verified acyclic in research.md §1.
- Subagent cascade (FR-021): NeuroCode config + shared index flow through the existing `joey-orchestration` dispatch path via `parent_config_tree` — no NeuroCode logic is threaded into `joey-orchestration` internals.

**Scale/Scope**: 1 new crate (~10–14 source files, ~2.5–3.5k LOC), trait intercept at 1 call site in `joey-agent-core`, 4 new tools registered in `joey-tools`, 1 new slash command in `joey-cli`, 2 new workspace dependencies. The Pega rule-awareness is a query/metadata layer over the same tree-sitter + SQLite infrastructure, not a separate engine.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Evaluated honestly against all eight principles of `.specify/memory/constitution.md` v1.1.0.

| Principle | Status | Evidence |
|---|---|---|
| I. Workspace-First Rust | **PASS** | All NeuroCode logic lives in a new dedicated crate `crates/joey-neurocode`, added to `[workspace] members`. Independently buildable/testable: `cargo build -p joey-neurocode` / `cargo test -p joey-neurocode`. No code added at workspace root. |
| II. CLI/TUI Parity | **PASS** | `/neurocode` is registered as a chat slash command AND reachable from the CLI text surface. All capabilities (inspect tier config, query graph, trigger index, ingest knowledge, view patterns) are available via text in/out — no UI-only affordance. The four NeuroCode tools are also callable by the model. |
| III. Filesystem Is Source of Truth | **PASS** | The structural index, learned patterns, domain knowledge, and config are all on disk (SQLite `graph.db` + JSON metadata + `config.yaml`). CLI/reads reflect current file contents; writes go back via atomic primitives. No in-memory-only state that can drift. |
| IV. Test-First for New Crates | **PASS** | `joey-neurocode` ships unit tests for classifier/graph-builder/tree-sitter-extractor/tier-resolver and round-trip tests for the on-disk SQLite schema (file → model → file) alongside implementation. The tasks phase will enumerate the full test matrix. |
| V. Incremental, Reviewable Delivery | **PASS** | Decomposed into independently shippable increments matching user-story priority: (P1) crate skeleton + SQLite index + tree-sitter ingestion + cold-start graph assembly; (P1) complexity classifier + tier routing + 011 composition; (P1) Pega version detection + rule-aware metadata; (P2) verify feedback loop + learned patterns/anti-patterns; (P3) domain-knowledge ingestion. Each increment builds and tests green on its own. |
| VI. Modularity and Decoupling | **PASS** | `joey-neurocode` exposes a narrow `NeuroCodeEngine` trait (3 methods); `joey-agent-core` depends only on that trait, never on the engine internals. `joey-neurocode` depends downward on `joey-core` + `joey-tools` + the `ModelAllocator` trait only. DAG verified acyclic (research.md §1). A change in the engine never forces edits to `joey-agent-core` beyond the trait. The verify loop reuses `joey-tools`'s subprocess path, not a second runner. |
| VII. Backward Compatibility (NON-NEGOTIABLE) | **PASS (with versioned-format note)** | Feature is strictly additive: default-off (`neurocode.enabled = false`); when off, the turn loop's code path is byte-identical (engine wrapped in `Option`, `None` = today's behavior). The `graph.db` SQLite schema is a **new** versioned on-disk format (`neurocode_schema_version: 1`), separate file from the session DB — no prior format to break, but any future breaking change requires a documented migration. The new `neurocode.*` config keys are additive. Regression coverage mandated (FR-020): tests asserting (a) feature-off behavior is unchanged, (b) the turn-loop intercept is a no-op when engine is `None`, (c) `/neurocode --help` exit code 0, (d) no system-prompt bytes change. |
| VIII. Performance Discipline & Lean Code | **PASS (with 2-dep justification)** | Total new external dependencies: **2** (`tree-sitter` + `tree-sitter-java`, ~270KB compiled, justified in research.md §3/§10 against regex/hand-written alternatives — FR-006 mandates deterministic parsing they cannot provide). No vector DB (SQLite+FTS5 reused — research.md §2). No embedding model in v1 (deferred, research.md §2). Hot path is deterministic classification (< 50µs) + one local SQLite query (< 20ms). Verify loop off-hot-path via `tokio::spawn`. Performance budgets recorded above and in research.md. |

**Gate result (pre-design)**: PASS — no violations. No entries required in Complexity Tracking.

### Post-design re-check (after Phase 1)

Re-evaluated against the materialized `data-model.md` and `contracts/`. All eight principles still PASS; no new violation emerged from the design. The design concretized the backward-compatibility story (Constitution VII) and the composition story (Constitution VI) rather than weakening them:

- The `NeuroCodeEngine` trait carries only 3 methods and is consumed behind `Option<Arc<dyn …>>` — disabled-state is provably today's path (contracts/neurocode-engine-trait.md).
- The `ComplexityTier` enum is `#[non_exhaustive]` so new tiers don't break the trait or the on-disk config (data-model.md Entity 1, contracts/complexity-route.md).
- The SQLite schema is versioned (`neurocode_schema_version: 1`) from day one with an explicit migration policy (contracts/graph-store-schema.md).
- The `ModelAllocator` composition is strictly additive — NeuroCode passes a tier *constraint hint* into 011's existing `resolve()`, never replacing the allocator (contracts/tier-routing-composition.md).
- The four NeuroCode tools are registered additively in `register_all` and their `check()` returns false when disabled — no shadowing of existing tools (contracts/neurocode-tools.md).
- Subagent cascade flows through the existing `joey-orchestration` dispatch via `parent_config_tree` — no NeuroCode logic enters `joey-orchestration` internals (contracts/subagent-cascade.md; FR-021).

## Project Structure

### Documentation (this feature)

```text
specs/015-neurocode-enterprise-java/
├── plan.md                          # This file (/speckit-plan command output)
├── research.md                      # Phase 0 output (/speckit-plan command)
├── data-model.md                    # Phase 1 output (/speckit-plan command)
├── quickstart.md                    # Phase 1 output (/speckit-plan command)
├── contracts/                       # Phase 1 output (/speckit-plan command)
│   ├── neurocode-engine-trait.md    # the narrow trait joey-agent-core consumes
│   ├── graph-store-schema.md        # versioned SQLite on-disk schema (graph.db)
│   ├── complexity-route.md          # ComplexityTier + ComplexityRoute types
│   ├── tier-routing-composition.md  # how NeuroCode composes with 011's ModelAllocator
│   ├── neurocode-tools.md           # the 4 NeuroCode tools' contracts
│   ├── neurocode-command.md         # /neurocode slash-command contract
│   └── subagent-cascade.md          # FR-021: subagent inherit+share contract
└── tasks.md                         # Phase 2 output (/speckit-tasks — NOT created by /speckit-plan)
```

### Source Code (repository root)

```text
crates/joey-neurocode/                       # NEW crate (Constitution I, IV, VI)
├── Cargo.toml                              # deps: joey-core, joey-tools, joey-llm-selector (trait only),
│                                           #   tree-sitter, tree-sitter-java (+ workspace serde/tokio/tracing/rusqlite)
├── src/
│   ├── lib.rs                             # public re-exports: NeuroCodeEngine trait + types
│   ├── engine.rs                          # NeuroCodeEngine trait + default engine (the narrow surface agent-core consumes)
│   ├── classifier.rs                      # ComplexityClassifier: deterministic rule-based tier classification (FR-001)
│   ├── tier_resolver.rs                   # TierModelResolver: tier → model id (config lookup or 011 composition) (FR-018)
│   ├── graph/
│   │   ├── mod.rs                         # DependencyGraph: typed-edge graph over CodeArtifactNodes (FR-004)
│   │   ├── store.rs                       # SQLite + FTS5 structural store (graph.db) (FR-004/005, research.md §2)
│   │   ├── node.rs                        # CodeArtifactNode + structural metadata (FR-005)
│   │   └── edge.rs                        # DependencyGraphEdge types (implements/injects/references/...)
│   ├── parse/
│   │   ├── mod.rs                         # tree-sitter ingestion pipeline (FR-006)
│   │   ├── java.rs                        # tree-sitter-java extraction: types/methods/fields/annotations/imports
│   │   └── pega.rs                        # Pega rule-pattern queries over the Java AST (FR-009, research.md §4)
│   ├── context/
│   │   ├── mod.rs                         # ContextAssembler: graph expansion + tier-adaptive formatting (FR-007/008)
│   │   └── budget.rs                      # tier context-budget sizing
│   ├── pega/
│   │   ├── mod.rs                         # Pega rule-system integration (FR-009)
│   │   ├── version.rs                     # version-adaptive detection (research.md §4, Clarification Q4)
│   │   └── metadata.rs                    # rule-type metadata ingestion (domain knowledge)
│   ├── verify/
│   │   ├── mod.rs                         # feedback loop: detached tokio task (FR-010/011/012)
│   │   ├── runner.rs                      # subprocess execution (reuses joey-tools path)
│   │   └── parse.rs                       # error-output parsing (Checkstyle XML, compiler errors)
│   ├── memory/
│   │   ├── mod.rs                         # learned patterns + anti-patterns (FR-011)
│   │   └── domain.rs                      # domain-knowledge ingestion (FR-013/014)
│   └── config.rs                          # NeuroCodeConfig: load from config.yaml dotted keys

crates/joey-tools/src/
└── builtins.rs                             # EDIT: register 4 NeuroCode tools in register_all (conditionally-enabled)

crates/joey-agent-core/src/
└── agent.rs (or turn-loop equivalent)      # EDIT: intercept before model dispatch — call engine.classify() + engine.assemble_context()
                                            #       when engine.is_active(); no-op when None (FR-020)

crates/joey-cli/src/
├── slash.rs                               # EDIT: add /neurocode to REGISTRY
├── repl.rs                                # EDIT: add "neurocode" => ... dispatch arm
└── commands/neurocode.rs                  # NEW: /neurocode command handler (text in/out per Constitution II)

Cargo.toml                                  # EDIT: add "crates/joey-neurocode" to [workspace] members + [workspace.dependencies]
                                            #       add tree-sitter + tree-sitter-java to [workspace.dependencies]
crates/joey-agent-core/Cargo.toml          # EDIT: add joey-neurocode.workspace = true
crates/joey-cli/Cargo.toml                 # EDIT: add joey-neurocode.workspace = true

crates/joey-neurocode/tests/
├── classifier.rs                          # deterministic tier-classification tests (FR-001)
├── graph_round_trip.rs                    # DependencyGraph SQLite schema file → model → file (Constitution IV)
├── tree_sitter_extract.rs                 # Java AST extraction correctness (FR-006)
├── context_assembly.rs                    # graph expansion + tier-adaptive formatting (FR-007/008)
├── pega_version_detect.rs                 # version-adaptive Pega detection (FR-009, Q4)
├── verify_loop.rs                         # feedback loop: failure-fed correction + pattern recording (FR-010/011)
└── regression_disabled.rs                 # disabled-state byte-identicality (FR-020, SC-008)
```

**Structure Decision**: Single new library crate (`joey-neurocode`) plugged behind a narrow 3-method trait (`NeuroCodeEngine`) into one call site in `joey-agent-core`, plus 4 tool registrations in `joey-tools` and a slash-command handler in `joey-cli`. This matches how the workspace already factors cross-cutting concerns (`joey-mcp`, `joey-cron`, `joey-llm-selector` are each their own crate behind a small surface). No new binary, no new UI stack, no web frontend — Constitution II parity is satisfied by the text-mode `/neurocode` command. The DAG stays acyclic because the new crate sits at the `joey-tools`/`joey-llm-selector` layer (depends only on `joey-core` + `joey-tools` + the `ModelAllocator` trait) and is consumed upward by `joey-agent-core`/`joey-cli`.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

No violations. Intentionally left blank.

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| — | — | — |
