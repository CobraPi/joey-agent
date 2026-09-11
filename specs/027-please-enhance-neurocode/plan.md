# Implementation Plan: NeuroCode Adaptive Memory

**Branch**: `027-please-enhance-neurocode` | **Date**: 2026-09-09 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/027-please-enhance-neurocode/spec.md`

## Summary

Give the agent two memory types over the existing NeuroCode/RAG infrastructure: episodic memory (one record per completed interactive task or orchestrated workstream/subtask, success or failure) and semantic memory (generalized user coding preferences, explicit or inferred). Memories are stored per-project in the existing `graph.db`, embedded and retrieved through the existing RAG machinery, injected as bounded context so code output adapts to the user, captured automatically with no approval gate (fully automatic, applied immediately — clarified Q1), distilled continuously per episode (clarified Q3), and manageable via `/neurocode memory ...`. `/hypercode` runs both write episodes (extending the verified-outcome capture point) and read memories (extending the goal-prefix injection point). The capability is default-off and strictly additive.

## Technical Context

**Language/Version**: Rust 2021 edition, stable toolchain (workspace `rust-toolchain.toml`)

**Primary Dependencies**: existing workspace crates only — `joey-neurocode` (engine, graph store), `joey-neurocode-rag` (embedding backends, vector encode/scan, hybrid-search fusion), `joey-agent-core` (turn loop, prompt assembly), `joey-cli` (commands, hypercode), `joey-core` (config, secret redaction). Zero new runtime dependencies.

**Storage**: per-project SQLite `graph.db` (`~/.joey/neurocode/projects/<sha256-16-of-root>/graph.db`) extended additively: `NEUROCODE_SCHEMA_VERSION` 3 → 4 via an idempotent `CREATE TABLE IF NOT EXISTS` batch appended to `apply_schema` (the exact v2→v3 pattern), adding `memory_episodes`, `memory_preferences`, `memory_vectors`.

**Testing**: `cargo test -p <crate>` per crate plus `cargo test --workspace`; new integration tests mirroring `crates/joey-neurocode/tests/rag_schema_migration.rs` (additive migration with data intact, idempotent double-open, blob round-trip byte-exact, delete cascade) and default-off no-op regression tests at every wiring point.

**Target Platform**: all platforms the workspace already supports (macOS/Linux/Windows); pure Rust + bundled SQLite, no platform-specific code.

**Project Type**: CLI/TUI coding agent — feature extends `/neurocode`, `/hypercode`, and the agent turn loop.

**Performance Goals**: memory injection adds ≤ 2s p95 to first response (SC-003); target < 100 ms local (one query embedding + dense scan over ≤ thousands of int8/f32 vectors + block formatting). Episode capture and preference distillation run post-turn, off the response critical path (same pattern as `neurocode_auto_reindex`), each episode text capped at 4 KB.

**Constraints**: default-off (`neurocode.memory.enabled = false`) and strictly additive (FR-010); no approval gate anywhere on the memory path (Q1: fully automatic; `memory.approval_required` governs only the separate MEMORY.md display subsystem and is never consulted); per-project scoping, no cross-project reads (FR-012); secret redaction before persist (FR-011); memory rows are never purged or rewritten by code-index refreshes (independent lifecycle).

**Scale/Scope**: per-project episode corpus capped by `neurocode.memory.max_episodes` (default 500, FIFO eviction); preferences unbounded but small in practice (hundreds).

## Constitution Check

*GATE: evaluated against constitution v1.1.0 before Phase 0 research and re-checked after Phase 1 design.*

| # | Principle | Verdict | Evidence |
|---|-----------|---------|----------|
| 0 | Cross-platform compatibility | PASS | Pure Rust + bundled SQLite + `joey_home()` paths; no platform-specific code |
| I | Workspace-first Rust | PASS | All work in existing crates under `crates/`; nothing at workspace root |
| II | CLI/TUI parity | PASS | `/neurocode memory ...` implemented once in the shared command handler; slash/CLI/TUI render the same text |
| III | Filesystem is source of truth | N/A | Feature does not visualize or edit spec-kit artifacts |
| IV | Test-first for new modules | PASS | Schema migration/round-trip, search-leg, injection, capture, and command tests ship alongside each increment |
| V | Incremental, reviewable delivery | PASS | Ordered increments (store → search leg → injection → interactive capture → hypercode → commands); each builds and tests green alone |
| VI | Modularity and decoupling | PASS | Memory behind explicit store APIs in `joey-neurocode::memory`; RAG reuse via existing public functions (`EmbeddingBackend`, `dense_scan`, `rrf_fuse`, quantize encode); crate DAG unchanged; distillation behind a `MemoryDistiller` trait implemented in `joey-cli` (provider access) |
| VII | Backward compatibility (NON-NEGOTIABLE) | PASS | Schema v4 batch is additive-idempotent — an existing v3 DB opens unchanged and gains empty memory tables (same contract v2→v3 established, pinned by a mirrored regression test); default-off yields byte-identical behavior; existing subcommands, flags, config keys, and on-disk formats untouched; regression-coverage tasks are mandated in tasks.md |
| VIII | Performance discipline and lean code | PASS | Zero new dependencies (reuses pinned `ort`, `rusqlite`, `sha2`); budgets recorded in Technical Context; capture/distill off the critical path; injection bounded by config |

Post-Phase-1 re-check (after research.md/data-model.md/contracts/quickstart.md): the design adds three tables (additive DDL), one config-key block (additive rows in the `RAG_CONFIG_KEYS` pattern), one subcommand (additive match arm), two Agent call-sites mirroring existing precedents (`apply_rag_prefetch`, `neurocode_auto_reindex` exit paths), and hypercode extensions at the existing `record_verified_outcomes` / dispatcher-goal-prefix points. No principle is violated; Complexity Tracking stays empty.

## Project Structure

### Documentation (this feature)

```text
specs/027-please-enhance-neurocode/
├── plan.md                                  # this file
├── research.md                              # Phase 0 output
├── data-model.md                            # Phase 1 output
├── quickstart.md                            # Phase 1 output
├── contracts/
│   ├── neurocode-memory-command.md          # /neurocode memory command surface
│   ├── neurocode-memory-config-keys.md      # neurocode.memory.* config keys
│   ├── neurocode-memory-storage.md          # schema v4 DDL, migration, invariants
│   └── neurocode-memory-injection.md        # context injection + hypercode read/write contract
└── tasks.md                                 # /speckit-tasks output (next step)
```

### Source Code (repository root)

```text
crates/joey-neurocode/src/
├── lib.rs                        # NEUROCODE_SCHEMA_VERSION 3 → 4
├── graph/store.rs                # v4 additive migration batch (memory_* tables)
└── memory/
    ├── outcomes.rs               # existing verified-outcome memory (untouched)
    ├── episodes.rs               # NEW: MemoryEpisode + EpisodeStore (memory_episodes)
    ├── preferences.rs            # NEW: MemoryPreference + PreferenceStore (supersede/conflict, memory_preferences)
    └── distill.rs                # NEW: MemoryDistiller trait + heuristic explicit-statement detector

crates/joey-neurocode-rag/src/
├── config.rs                     # neurocode.memory.* key specs (additive rows)
└── memory_search.rs              # NEW: memory retrieval leg reusing embed backends,
                                  #      vector quantize encode, dense_scan, rrf_fuse

crates/joey-agent-core/src/
└── agent.rs                      # memory prefetch block (mirrors apply_rag_prefetch) and
                                  # post-turn capture call-sites (mirrors neurocode_auto_reindex
                                  # exits at the three run_turn return paths)

crates/joey-cli/src/
├── commands/neurocode.rs         # "memory" dispatch arm + memory_command_text handler
├── slash.rs                      # /neurocode registration line updated (additive)
├── neurocode_wiring.rs           # memory engine wiring behind default-off gate
└── hypercode.rs                  # episode capture in finalize_graph_run (per workstream) and
                                  # memory injection in the dispatcher goal prefix

Integration tests (workspace convention — crates/<crate>/tests/):
crates/joey-neurocode/tests/memory_schema_migration.rs
crates/joey-neurocode/tests/memory_stores.rs
crates/joey-neurocode-rag/tests/memory_search.rs
crates/joey-agent-core/tests/memory_injection.rs
crates/joey-cli/tests/neurocode_memory_command.rs
crates/joey-cli/tests/hypercode_memory_episodes.rs
```

**Structure Decision**: extend existing crates only (Principles I/VI). Persistence lives beside `outcomes.rs` in `joey-neurocode::memory` (pure SQLite, no provider dependency); retrieval composes existing public functions in `joey-neurocode-rag` rather than forking the search pipeline; provider-backed distillation is implemented in `joey-cli` behind the `MemoryDistiller` trait to keep the crate DAG acyclic. A new crate is not justified: no new dependency weight and a narrow surface (Principle VIII).

## Complexity Tracking

No Constitution Check violations — nothing to justify (table intentionally empty).
