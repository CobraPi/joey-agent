# Implementation Plan: Enterprise Orchestration Runtime

**Branch**: `023-enterprise-orchestration-runtime` | **Date**: 2026-09-02 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/023-enterprise-orchestration-runtime/spec.md`

## Summary

One shared enterprise execution protocol across the two commands: NeuroCode becomes the analysis plane (unified per-request `TaskAnalysis`: target/impacted artifacts, combined effective policies, graph-aware complexity, risk, capability tier, scoped verification recipe, structured outcome memory) and HyperCode becomes the execution plane (typed `TaskNode`/`TaskGraph` planning with strict validation, a deterministic runtime-owned scheduler with persisted resumable run state, worktree-isolated parallel writers with patch-bundle integration, an evaluator/repair/escalation completion-gate loop, and graph-based execution-mode routing). Everything ships behind two config flags (`hypercode.execution_graph.enabled`, `neurocode.enterprise_context.enabled`), both default-disabled; flipping both defaults to enabled is the final delivery step, gated on the SC-001 parity check passing in CI. Legacy workstream planning keeps working by immediate conversion into the validated graph.

## Technical Context

**Language/Version**: Rust, edition 2021, stable toolchain (rust-toolchain.toml).

**Primary Dependencies**: Existing workspace crates only — `serde`/`serde_json`, `tokio`, and the system `git` binary invoked via `std::process::Command` (same approach as `CheckpointManager` in joey-tools and `staging_impl.rs` in joey-speckit-ui). Zero new external dependencies.

**Storage**: JSON files under `~/.joey/hypercode/projects/<project-hash>/runs/<run-id>/` (graph.json, nodes/, evidence/, patches/, decisions.jsonl). joey-neurocode's existing SQLite store gains one additive `outcome_memory` table (CREATE TABLE IF NOT EXISTS; no schema-version bump). New config keys in `~/.joey/config.yaml`. No changes to Hermes-compatible formats (SQLite sessions schema 22, jobs.json, SKILL.md).

**Testing**: `cargo test` — per-crate integration tests under `crates/<crate>/tests/` plus inline unit tests, written alongside each module (constitution Principle IV).

**Target Platform**: macOS/Linux CLI (unchanged).

**Project Type**: Cargo workspace — libraries (`joey-neurocode`, `joey-orchestration`) plus CLI wiring (`joey-cli`).

**Performance Goals**: Scheduler wave-dispatch overhead < 1s per wave beyond worker runtime; up to 16 concurrent isolated workers (configurable); analysis-plane latency within the existing `assemble_context` budget (reuses the same index, no re-indexing on the hot path).

**Constraints**: Strict crate DAG preserved (joey-orchestration must NOT depend on joey-neurocode — the evaluator consumes a narrow verification-gate trait defined in joey-orchestration and adapted in joey-cli); all public changes additive (constitution Principle VII); flags default false; `NEUROCODE_SCHEMA_VERSION` and session-store schema untouched.

**Scale/Scope**: Repositories up to the existing NeuroCode index scale (~1M LOC); runs of up to ~100 task nodes; decision log append-only.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Evaluated against `.specify/memory/constitution.md` v1.1.0 — **PASS, no violations** (re-checked after Phase 1 design):

- **Principle 0 (Complete Cross-Platform Compatibility)**: git worktree isolation shells out to the system `git` binary (already required today) with a full-copy fallback where worktrees are unavailable; all new code is plain Rust on existing workspace deps. ✔
- **Principle I (Workspace-First Rust)**: every increment lands in an existing workspace crate and stays independently buildable (`cargo build -p <crate>`) and testable (`cargo test -p <crate>`); full workspace gate before merge. No workspace-root code. ✔
- **Principle II (CLI/TUI Parity)**: the new capability is config-key driven and reachable through the existing `joey-cli` surface (hypercode/neurocode wiring); no new UI layer, so nothing diverges from file-backed state. ✔
- **Principle III (Filesystem Is the Source of Truth, NON-NEGOTIABLE)**: plan/research/data-model/contracts/quickstart live in this feature directory; run-state files under `~/.joey/` are execution artifacts, not spec state. ✔
- **Principle IV (Test-First for New Crates)**: each new module ships with unit + integration tests in the same change — written alongside implementation, never deferred. ✔
- **Principle V (Incremental, Reviewable Delivery)**: work decomposed into 8 independently shippable user stories plus setup/foundational/polish phases; each story builds and passes tests on its own (per-phase checkpoints in tasks.md). ✔
- **Principle VI (Modularity and Decoupling)**: analysis plane lives in joey-neurocode; runtime (task_graph/scheduler/workspace/evaluator/joiner/evidence) lives in joey-orchestration behind its own narrow traits (`VerificationGate`); joey-cli adapts NeuroCode to those traits. Strict DAG preserved — joey-orchestration does NOT depend on joey-neurocode; no new upward edges; joey-agent-core changes: none. ✔
- **Principle VII (Backward Compatibility and Non-Regression, NON-NEGOTIABLE)**: all public changes strictly additive; `Workstream` and `parse_workstreams` preserved; legacy output converted immediately to `TaskGraph`; `ModeRoute::{Subagent, Team}` and `route_mode` retained (new variants and a new graph-based router added alongside); new config keys additive with safe defaults; flags default false give byte-identical legacy behavior (SC-001) with regression task coverage; no ported guidance strings are modified — the legacy first-found-wins context chain in `prompt.rs` stays intact for the flag-off path. ✔
- **Principle VIII (Performance Discipline and Lean Code)**: zero new external dependencies (git operations via the system binary already required today, alternatives recorded in research.md); performance budgets in Technical Context — scheduler wave-dispatch overhead < 1s per wave beyond worker runtime, up to 16 concurrent isolated workers, analysis-plane latency within the existing `assemble_context` budget (same index, no re-indexing on the hot path). ✔

## Project Structure

### Documentation (this feature)

```text
specs/023-enterprise-orchestration-runtime/
├── plan.md              # This file (/speckit-plan command output)
├── research.md          # Phase 0 output (/speckit-plan command)
├── data-model.md        # Phase 1 output (/speckit-plan command)
├── quickstart.md        # Phase 1 output (/speckit-plan command)
├── contracts/           # Phase 1 output (/speckit-plan command)
│   ├── config-keys.md
│   ├── planner-json-format.md
│   ├── run-state-format.md
│   └── public-api.md
└── tasks.md             # Phase 2 output (/speckit-tasks command - NOT created by /speckit-plan)
```

### Source Code (repository root)

```text
crates/
├── joey-neurocode/src/
│   ├── analysis.rs             # EnterpriseTaskAnalyzer trait, TaskAnalysis, adapter wiring
│   ├── policy/mod.rs           # PolicyBinding, effective-policy combination
│   ├── policy/resolver.rs      # hierarchical combine (org → repo → module → scoped → task)
│   ├── policy/sources.rs       # JOEY/AGENTS/CLAUDE + Copilot applyTo parsing
│   ├── risk.rs                 # RiskAssessment/RiskLevel, fan-in/out, API/ownership/concurrency
│   ├── verification_plan.rs    # scoped VerificationPlan derivation
│   ├── memory/outcomes.rs      # OutcomeMemory store + expiry/down-ranking
│   ├── classifier.rs           # extended signals (GraphHub activated, fan-in/out, anti-patterns)
│   └── tier_resolver.rs        # additive tier ranking helper for the escalation ladder
├── joey-orchestration/src/
│   ├── task_graph.rs           # TaskNode, TaskGraph, validation invariants, legacy converter input
│   ├── scheduler.rs            # deterministic wave loop, conflict partitioning, concurrency cap
│   ├── workspace.rs            # worktree isolation (git worktree add --detach, copy fallback)
│   ├── evaluator.rs            # VerificationGate trait, DefectBundle, completion gate
│   ├── joiner.rs               # ChangeBundle verification, three-way patch application
│   └── evidence.rs             # EvidenceRecord, run-state persistence, decisions.jsonl
├── joey-cli/src/
│   └── hypercode.rs            # workstream→graph conversion, graph router, flag wiring,
│                               #   VerifyLoop→VerificationGate adapter, completion-gate await
└── joey-core/src/
    └── config.rs               # default config text gains the four keys from contracts/config-keys.md
```

**Structure Decision**: Additive modules inside existing crates — no new workspace members, preserving the DAG and per-crate buildability. Runtime state modules are colocated in joey-orchestration (scheduler owns no LLM); all model-facing behavior stays in joey-cli wiring.

## Complexity Tracking

No Constitution Check violations — nothing to justify.
