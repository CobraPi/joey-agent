# Implementation Plan: Semantic code retrieval (RAG) enhancement for /neurocode

**Branch**: `021-please-enhance-neurocode` | **Date**: 2026-08-27 | **Spec**: [spec.md](../spec.md)

**Input**: Feature specification from `/specs/021-please-enhance-neurocode/spec.md`

**Note**: This template is filled in by the `/speckit-plan` command; its definition describes the execution workflow.

## Summary

This plan adds a semantic code retrieval (RAG) layer over the existing neurocode typed code graph: natural-language search, hybrid ranking, incremental background refresh, context-expanded results, and relationship-aware retrieval (spec FR-001..FR-015, outcomes SC-001..SC-006). The approach stores embedded vectors in the existing per-project SQLite `graph.db` and fuses dense-vector scores with the existing FTS5 BM25 index client-side via Reciprocal Rank Fusion; the semantic layer is powered by a fully-local in-process ONNX embedding engine — default profile nomic-embed-text-v1.5 ONNX int8 (768-dim), alternative profile CodeRankEmbed for code-specialized retrieval — loaded via `ort` with a runtime-loaded ONNX Runtime dylib. Optional OpenAI-compatible and Ollama HTTP backends remain for remote/daemon use under the per-project consent model. Refresh is incremental (mtime + SHA-256 change detection, git-CLI rename assist) and swaps in an atomic snapshot. The work lands in a new first-party crate `joey-neurocode-rag` with THREE new pinned dependencies (`ort`, `tokenizers`, `ndarray`) replacing the prior zero-dep posture, justified in research.md R6, wired additively into `joey-tools`, `joey-cli`, `joey-tui`, and `joey-agent-core`. Model artifacts are distributed WITHOUT Hugging Face via a local model_dir plus a project-controlled mirror (research.md R8). Key guarantees: the feature is default-off with byte-identical behavior when disabled, remote embedding backends require per-project recorded and revocable consent, and refresh never blocks agent turns.

## Technical Context

**Language/Version**: Rust 2021, stable toolchain (`rust-toolchain.toml`), workspace v0.19.0.

**Primary Dependencies**: THREE new pinned — ort =2.0.0-rc.13 (exact pin; features `ndarray` + `load-dynamic`; ONNX Runtime dylib sidecar ~10-31MB per platform, loaded at runtime, not embedded), tokenizers 0.23 (default-features = false, offline), ndarray 0.17 — justified with measured weights and alternatives in research.md R6; plus existing workspace deps: rusqlite 0.32 (bundled + FTS5), reqwest 0.12 (rustls-tls/json), sha2 0.10, rayon 1.12, walkdir/ignore, tokio 1, serde/serde_json, chrono, tracing, anyhow/thiserror, tempfile (dev).

**Storage**: existing per-project SQLite `graph.db` under `~/.joey/neurocode/projects/<sha256-of-root>/`; additive schema migration v2→v3 (new chunk/vector/meta tables + indexes, following the established additive-migration pattern from spec 015); per-project `consent.json` sibling file; model_dir `~/.joey/neurocode/models/<profile>/` (`model.onnx` + `tokenizer.json`).

**Testing**: `cargo test -p joey-neurocode-rag` plus integration tests in `joey-neurocode`/`joey-tools`/`joey-cli` touching surfaces; tempfile tempdir fixtures with hand-written source strings (existing pattern); parity test for disabled mode; wire-contract tests pinning embedding request/response JSON; benchmark smoke tests for SC-003/SC-004 budgets.

**Target Platform**: all platforms incl. Windows/MSVC — no new BUILD-time C/C++ dependencies (everything pure Rust or already bundled); the ONNX Runtime is a load-dynamic sidecar dylib (~10-31MB per platform, ~31MB win-msvc) acquired via the SAME project-mirror distribution path as model artifacts (research.md R8 pattern): manual placement, or `/neurocode model fetch --dylib` from `neurocode.rag.local.mirror_url` with per-platform SHA-256 verification recorded by the project; resolution order `ORT_DYLIB_PATH` → `neurocode.rag.local.ort_dylib_path` → system → fetched copy. Works everywhere GIVEN the dylib is present; when absent, clear failure/degradation messaging (keyword-only, never a hard fail).

**Project Type**: new first-party workspace crate `crates/joey-neurocode-rag` (Principle I), whose `embed/` module provides a `local_onnx` in-process backend as primary, wired additively into `joey-tools` builtins, `joey-cli` slash commands, `joey-tui` rendering, `joey-agent-core` context path.

**Performance Goals** (explicit budgets, Principle VIII): search p95 < 2s at 100k indexed chunks with warm process (SC-003); incremental refresh of ≤10 changed files < 5s excluding embedding-API latency, touching only changed files' rows (SC-004); vector scan memory budget ≤ 512MB resident at ≤250k chunks via int8 quantization default above 100k chunks; refresh never blocks agent turns (background, `spawn_blocking`); local embedding inference ~10-40ms per 512-token text on an AVX2 CPU (estimate; benchmarked in M7); batch embedding via padded `encode_batch` with rayon parallelism across texts; cold full index of ~1M LOC repo dominated by local embedding throughput, still completing with observable progress and no agent blocking.

**Constraints**: FR-009/SC-005 byte-identical when disabled (parity test mandatory); FR-012 privacy — default disabled, remote backends need per-project recorded revocable consent, pre-fetch only with fully-local backend; no changes to upstream Hermes parity or other subsystems' on-disk formats; explicit tool registration in builtins (no auto-discovery); FTS5 bm25 rank is negated — convert to ascending ordinals before RRF (research.md R1); NO-Hugging-Fetch — model population via manual placement or the project-controlled mirror only, never an HF network fetch (research.md R8); per-model profile pinning — prefix/pooling/dim recorded in index metadata, mismatch forces a semantic rebuild; ONNX Runtime dylib is load-dynamic (not a build-time C/C++ dep), acquired via the model-mirror path (`model fetch --dylib`, per-platform SHA-256; resolution `ORT_DYLIB_PATH` → `neurocode.rag.local.ort_dylib_path` → system → fetched copy), absent dylib degrades to keyword-only with clear messaging, never hard-fails.

**Scale/Scope**: repos up to ~1M LOC → ≤ ~500k chunks; embedding batches of 64 (config validated range 16–128, per rag-config-keys.md); hybrid search top-k default 10.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Verdict | Justification |
|---|---|---|
| 0 Cross-Platform | PASS | Pure Rust + already-bundled C (SQLite); no new build-time native deps — the ONNX Runtime ships as a load-dynamic sidecar dylib via the mirror path (`model fetch --dylib`), so all platforms work GIVEN the dylib is present, with keyword-only degradation (never hard-fail) when absent; Windows/MSVC-safe. |
| I Workspace-First | PASS | New crate `crates/joey-neurocode-rag`, independently buildable/testable via `cargo build/test -p`. |
| II CLI/TUI Parity | PASS | Search exposed via slash command (CLI) and TUI rendering, plus an agent tool; all three surfaces specified in contracts. |
| III Filesystem Source of Truth | PASS | Consent + config are file-backed (`consent.json`, YAML config keys); no UI-only state. |
| IV Test-First | PASS | Tests land alongside each increment: parity, wire-contract, migration, and budget smoke tests. |
| V Incremental Delivery | PASS | 7 milestones, each independently shippable and leaving the workspace green (mapped below). |
| VI Modularity | PASS | Embedding backend behind a trait; RAG engine behind a trait mirroring the `NeuroCodeEngine` pattern; narrow additive wiring. |
| VII Backward Compatibility (NON-NEGOTIABLE) | PASS | All changes additive: schema v3 migration is additive + idempotent; new config keys namespaced `neurocode.rag.*`; new tool/slash names don't collide; disabled path is byte-identical; regression tests mandated (parity test, schema round-trip, existing-tool behavior). |
| VIII Performance Discipline (NON-NEGOTIABLE) | PASS | Three new pinned deps (ort/tokenizers/ndarray) justified with measured weights and alternatives in research.md R6; ort pinned exactly (=2.0.0-rc.13) due to pre-RC breaking changes; `load-dynamic` keeps the binary lean and builds reproducible; budgets updated accordingly (local-inference expectations above); vector-store alternatives still recorded — qdrant/sqlite-vec/usearch/git2/hnsw_rs evaluated and rejected or deferred with reasons (research.md R1/R3). |

**Gate verdict: PASS** — no violations; proceed to Phase 0/1 (research.md complete, design artifacts follow).

### Post-Design Re-evaluation (after Phase 1; second pass 2026-08-27)

Gate re-evaluated on 2026-08-27 (second pass) against the revised Phase 1 design — local-ONNX-primary embedding (research.md R2/R6/R8), data-model.md, the six contracts, quickstart.md: **remains PASS for all principles.** Riskiest three:

- **VII Backward Compatibility**: all surfaces additive — schema v3 migration idempotent (rag-store-schema.md), config keys namespaced `neurocode.rag.*` with no renames (rag-config-keys.md), tool/command additions registered only when `neurocode.rag.enabled=true`, including the additive `/neurocode model fetch` subcommand (neurocode-rag-tools.md / neurocode-rag-command.md), byte-identical parity when disabled; contracts pin the regression tests (parity, schema round-trip, `.env` routing, existing-tool behavior).
- **VIII Performance Discipline**: deps ledger now three pinned crates (ort =2.0.0-rc.13 exact, tokenizers offline, ndarray) plus a sidecar ONNX Runtime dylib loaded at runtime, not embedded — measured weights and alternatives in research.md R6; explicit perf budgets present (SC-003 p95 < 2s, SC-004 < 5s, ≤512MB at ≤250k chunks) plus local-inference expectations (~10-40ms per 512-token text) to be benchmarked in M7.
- **III Filesystem Source of Truth**: consent + config are file-backed (`consent.json`, YAML/`.env` keys); model artifacts live only in the `~/.joey` model_dir, populated by manual placement or the project-controlled mirror per R8 — never in the repo; no UI-only state — the TUI renders, never owns state.

## Project Structure

### Documentation (this feature)

```text
specs/021-please-enhance-neurocode/
├── plan.md                          # This file (/speckit-plan command output)
├── research.md                      # Phase 0 output (/speckit-plan) — decisions R1-R8
├── data-model.md                    # Phase 1 output (/speckit-plan)
├── quickstart.md                    # Phase 1 output (/speckit-plan)
├── contracts/
│   ├── embedding-backend.md         # EmbeddingBackend trait + wire contract
│   ├── rag-store-schema.md          # graph.db schema v3 (chunk/vector/meta tables)
│   ├── hybrid-search.md             # RRF fusion of dense + FTS5 BM25
│   ├── rag-config-keys.md           # neurocode.rag.* config keys
│   ├── neurocode-rag-tools.md       # Agent tool surface + registration
│   └── neurocode-rag-command.md     # Slash command + consent subcommand
├── checklists/
│   └── requirements.md              # FR-001..FR-015 verification checklist
└── spec.md                          # Source specification
# tasks.md (Phase 2 output, created by /speckit-tasks — complete)
```

### Source Code (repository root)

```text
crates/
├── joey-neurocode-rag/                  # NEW first-party crate (Principle I)
│   ├── src/
│   │   ├── lib.rs                       # Crate root, RagEngine trait (paired trait tests, Constitution IV), public API
│   │   ├── embed/                       # EmbeddingBackend trait
│   │   │   ├── local_onnx.rs            # PRIMARY: ort session load, mean-pool + L2, model profiles
│   │   │   ├── profiles.rs              # Model profile table: name, query prefix, document prefix, pooling, dim
│   │   │   ├── openai_compat.rs         # OPTIONAL: OpenAI-compatible HTTP backend (wire-pinned)
│   │   │   ├── ollama.rs                # OPTIONAL: Ollama HTTP backend (remote/daemon path)
│   │   │   └── artifacts.rs             # T008: artifact integrity + self-registration
│   │   ├── vector/
│   │   │   ├── store.rs                 # BLOB vector table read/write
│   │   │   ├── quantize.rs              # int8 quantization (>100k chunks default)
│   │   │   └── scan.rs                  # In-memory scan within memory budget
│   │   ├── search/
│   │   │   ├── hybrid.rs                # Dense + FTS5 candidate generation
│   │   │   ├── rrf.rs                   # Reciprocal Rank Fusion (bm25 sign fix, R1)
│   │   │   └── expand.rs                # Context window expansion
│   │   ├── index/
│   │   │   ├── chunker.rs               # Chunk records from parse spans + fallback
│   │   │   ├── incremental.rs           # mtime + SHA-256 change detection, git-CLI assist
│   │   │   └── refresh_worker.rs        # Background refresh, atomic snapshot swap
│   │   ├── consent.rs                   # Per-project consent.json read/write/gate
│   │   ├── config.rs                    # neurocode.rag.* config keys
│   │   └── parity.rs                    # Byte-identical-when-disabled guard
│   └── tests/                           # Parity, wire-contract, migration, budget smoke
├── joey-neurocode/                      # (additive) line spans in parse layer, fallback
│   │                                    #   chunks, schema v2→v3 migration, store APIs
├── joey-tools/                          # (additive) builtins registration + search tool wrapper
├── joey-cli/                            # (additive) slash command grammar + consent + model fetch (incl. `--dylib`) subcommands + wiring
├── joey-tui/                            # (additive) search result rendering
└── joey-agent-core/                     # (additive) background refresh hook + optional pre-fetch gate
```

Model artifacts NEVER live in the repository: `model.onnx` + `tokenizer.json` are downloaded via `/neurocode model fetch` (project-controlled mirror, SHA-256 verified) or manually placed into the `~/.joey` model_dir; the ONNX Runtime dylib follows the same mirror distribution path via `/neurocode model fetch --dylib` (or manual placement), per-platform SHA-256 verified.

**Structure Decision**: A new dedicated crate `crates/joey-neurocode-rag` (Principle I) keeps the entire RAG path — embedding backends, vector storage, hybrid search, refresh — isolated behind default-off wiring, leaving `joey-neurocode` lean and its disabled path untouched. Every touch point in existing crates is strictly additive (Principle VII): a schema migration, registrations, command grammar, rendering, and hooks, with no modification of existing behavior or on-disk formats. The workspace DAG stays acyclic: `joey-neurocode-rag` → `joey-neurocode` → `joey-core`, with a one-way edge `joey-tools` → `joey-neurocode-rag`; no existing crate gains a dependency on the new one except that single registration edge.

## Complexity Tracking

No violations — table intentionally empty.

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|

## Milestones

Each milestone maps to spec FRs, is independently shippable, and must leave `cargo build --workspace` and `cargo test --workspace` green.

| Milestone | Scope | Spec coverage |
|---|---|---|
| M1 Parse & chunk foundation | Line spans + fallback chunks in the `joey-neurocode` parse layer; chunk records + schema v3 migration | FR-014 groundwork |
| M2 Local embedding engine + model management | Backend trait, ort-based LocalOnnx embedder (mean-pool + L2), model profiles, model_dir resolution, `/neurocode model fetch` mirror download with SHA-256 verification, dylib acquisition path (`/neurocode model fetch --dylib` from the mirror, per-platform SHA-256, `ORT_DYLIB_PATH` → config → system → fetched resolution), batch + parallel embedding, BLOB vector table, quantization | FR-001 core; research.md R2/R8 |
| M3 Hybrid search tool | RRF fusion of dense + FTS5, file-scope filter, agent tool + slash command + CLI/TUI render, degradation path | FR-001/2/3/8/10 |
| M4 Incremental refresh | mtime+hash change detection, git-CLI rename assist, atomic transactional swap, background worker, purge on delete | FR-004/5; SC-004 |
| M5 Context expansion + consent + status + HTTP backends | Surrounding-lines window, per-project consent file + subcommand, status reporting (backend health + consent state), pre-fetch gate (local-only), HTTP embedding backends (OpenAI-compatible + Ollama native) as the remote path | FR-001 remote; FR-006/12/13/15 |
| M6 Relationship-aware retrieval | Edge-following expansion with bounded depth + dedup over existing graph edges | FR-007 |
| M7 Parity + budgets hardening | Byte-identical-when-disabled parity test, benchmark smoke tests for SC-003/SC-004, benchmark local embedding throughput, verify ort pin stability, verify dylib story on all three platforms (fetch + resolution + absent-dylib degradation messaging), docs update (PORTING.md note if applicable) | FR-009/11; SC-005 |

Note: `tasks.md` (Phase 2, `/speckit-tasks`) decomposes these milestones into implementation tasks.
