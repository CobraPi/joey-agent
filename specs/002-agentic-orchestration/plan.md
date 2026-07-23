# Implementation Plan: Agentic Orchestration Engine

**Branch**: `002-agentic-orchestration` | **Date**: 2026-07-23 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/002-agentic-orchestration/spec.md`

## Summary

Add a full agentic orchestration layer to joey-agent, porting the backend
methods from both Hermes (delegate_task with parallel batches,
orchestrator/leaf roles) and Crush (coordinator + SessionAgent, per-subagent
model selection). The layer is delivered in two increments: Increment 1
lands the delegation core (parallel dispatch, isolated subagent contexts,
per-subagent model/tool selection, shared concurrency limiter, structured
lifecycle events); Increment 2 adds session history search, user
clarification, and background process management. The design reuses the
existing turn loop, provider layer, tool registry, AgentEvent channel, and
SQLite session store — no new dependencies required.

## Technical Context

**Language/Version**: Rust 2021 edition, stable 1.80+ (workspace-standard)

**Primary Dependencies**: tokio (async runtime, already workspace dep),
serde/serde_json (serialization), async-trait, the existing
joey-core/joey-providers/joey-tools/joey-agent-core crates. No new external
dependencies — the entire feature is composed from existing workspace crates.

**Storage**: SQLite via rusqlite (existing SessionDb). The schema already
contains the `async_delegations` table for deferred-delivery delegation
results and `messages_fts` for full-text session search. Increment 1 uses
`async_delegations` for opt-in persistent subagent traces. Increment 2
wraps the existing `SessionDb::search()` FTS5 method as a tool.

**Testing**: `cargo test -p <crate>` (workspace-standard). Unit tests
alongside each module. Contract tests for the delegation tool schema and
the session search wrapper. The existing `Transport` trait in agent.rs
enables mocked-provider tests for the subagent turn loop without live API
calls.

**Target Platform**: macOS / Linux native binary (same as existing workspace)

**Project Type**: library (new crate `joey-orchestration`) + additive
integration into `joey-agent-core` (event variants) and `joey-tools`
(builtins registration)

**Performance Goals**:
- SC-001: 3-subtask parallel delegation completes in <=1.5x slowest
  single subtask wall-clock time (vs 3x serial)
- SC-002: Subagent intermediate results consume zero parent context tokens
- SC-005: Session search returns results in <1s for 100+ sessions
- SC-006: Background process lifecycle ops non-blocking (<100ms)
- SC-008: Shared concurrency limiter prevents retry storms under parallel load

**Constraints**:
- Constitution Principle VIII: no new runtime dependencies; zero-copy where
  feasible; minimal allocations on the hot path (tool dispatch, event emission)
- Constitution Principle VII: strictly additive — existing turn loop, tool
  schemas, CLI behavior, and config keys unchanged
- `cargo build --workspace` and `cargo test --workspace` green on every increment
- Per-call provider timeout and iteration budget must be respected by
  subagents (they inherit the parent's config defaults unless overridden)

**Scale/Scope**: 2 crates modified (joey-agent-core, joey-tools), 1 new
crate (joey-orchestration), ~6 new modules, ~15-20 new tests. Estimated
1,500-2,500 lines of Rust.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Workspace-First Rust | **PASS** | All new code lives in a new `crates/joey-orchestration` crate. Independently buildable and testable. No root-level code. |
| II. CLI/TUI Parity | **PASS** | Orchestration tools register through the existing tool registry and are reachable via joey-cli/joey-tui identically to built-in tools. No new UI surface. |
| III. Filesystem Source of Truth | **PASS** | No spec-kit artifact editing involved. N/A for this feature. |
| IV. Test-First for New Crates | **PASS** | Each module in `joey-orchestration` ships with unit tests alongside implementation. The delegation engine gets contract tests proving subagent isolation (no parent history leak). |
| V. Incremental, Reviewable Delivery | **PASS** | Explicitly two-increment delivery (FR-016): Increment 1 = delegation core (P1-P3), Increment 2 = search/clarify/processes (P4-P6). Each builds and tests green independently. |
| VI. Modularity and Decoupling | **PASS** | `joey-orchestration` exposes a narrow public API: `SubagentManager`, `DelegationRequest`, `DelegationResult`. It depends on `joey-agent-core` (Agent, AgentConfig), `joey-tools` (ToolRegistry, ToolContext), and `joey-providers` (ProviderClient) via their public traits. No sibling-internal access. A change in the orchestration crate never forces edits to unrelated crates. |
| VII. Backward Compatibility (NON-NEGOTIABLE) | **PASS** | Strictly additive: new tool registrations, new AgentEvent variants (enum extension is non-breaking), new config keys (all optional with defaults). No existing public surface is modified. The terminal tool's background/pty stubs are activated (their schema already exists), not replaced. |
| VIII. Performance Discipline | **PASS** | No new dependencies. Subagent dispatch uses tokio::task::JoinSet for zero-overhead parallelism. The concurrency limiter is a tokio::sync::Semaphore (permits, not polling). Events are emitted via the existing mpsc channel (no allocation beyond the event struct). Session search wraps existing FTS5 (no new index). Performance budget recorded below. |

**Gate Result**: ALL CLEAR. No violations. No Complexity Tracking entries needed.

## Project Structure

### Documentation (this feature)

```text
specs/002-agentic-orchestration/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
│   ├── delegation-tool.md
│   ├── session-search-tool.md
│   ├── clarify-tool.md
│   └── process-tool.md
└── tasks.md             # Phase 2 output (NOT created by /speckit-plan)
```

### Source Code (repository root)

```text
crates/
├── joey-orchestration/          # NEW CRATE — the orchestration engine
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs               # Public API: SubagentManager, DelegationRequest, DelegationResult
│       ├── manager.rs           # SubagentManager: spawn, batch dispatch, concurrency limiter
│       ├── subagent.rs          # Subagent: isolated Agent instance, summary generation
│       ├── events.rs            # Orchestration-specific AgentEvent extensions
│       └── tests.rs             # Integration tests with mocked Transport
│
├── joey-tools/src/tools/
│   ├── delegation_tool.rs       # NEW — delegate_task tool (registered in builtins.rs)
│   ├── session_search_tool.rs   # NEW — session_search tool (wraps SessionDb::search)
│   ├── clarify_tool.rs          # NEW — clarify tool (structured question to user)
│   └── process_tool.rs          # NEW — process tool (background session lifecycle)
│
├── joey-agent-core/src/
│   └── events.rs                # MODIFIED — add orchestration event variants (additive)
│
└── (existing crates unchanged)
```

**Structure Decision**: A new dedicated crate `joey-orchestration` holds the
orchestration engine (SubagentManager + Subagent + concurrency limiter). The
four new tools live in `joey-tools` alongside existing tools and are
registered through `builtins.rs`. The only modification to an existing crate
is additive `AgentEvent` variants in `joey-agent-core/src/events.rs`. This
follows Constitution Principle I (workspace-first) and Principle VI (narrow
interfaces, acyclic coupling): `joey-orchestration` depends on
`joey-agent-core` / `joey-tools` / `joey-providers` public APIs, and nothing
depends back on `joey-orchestration` except the tool registrations in
`joey-tools`.

### Performance Budget

| Path | Budget | Measurement Method |
|------|--------|--------------------|
| Subagent spawn (create Agent + registry) | <5ms | `Instant::now()` instrumentation in tests |
| Concurrency limiter permit acquisition (uncontended) | <100µs | Semaphore permit benchmark |
| Event emission per subagent lifecycle | <10µs per event | mpsc send timing |
| Session search (100+ sessions, FTS5) | <1s | Existing FTS5, measured in quickstart |
| Background process lifecycle op | <100ms non-blocking | tokio::spawn + handle return |

## Complexity Tracking

> No Constitution Check violations. Table intentionally empty.
