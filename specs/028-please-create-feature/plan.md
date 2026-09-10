# Implementation Plan: Context Economy

**Branch**: `028-please-create-feature` | **Date**: 2026-09-10 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `specs/028-please-create-feature/spec.md`

## Summary

Implement the six context-economy techniques as five default-on mechanisms with per-mechanism disable switches and contractual byte-identical behavior when any mechanism is disabled: (1) a session scratchpad tool for pre-compaction externalization of findings; (2) a deterministic state block injected at request-tail; (3) mid-turn tool-result hygiene reusing compression pass-2 one-liners; (4) boundary-aligned cleanup gated on todo-completion; (5) standing economy guidance plus retrieval-verification nudge. All persistence-adjacent behavior routes through existing, sanctioned patterns: the neurocode stash mechanism for injection, todo_tool::current for state reads, compressor pass-2 helpers for condensation, and the messages-FTS5 index (already covering tool_calls arguments) for post-session discoverability. No schema changes, no new crates, no new dependencies.

## Technical Context

**Language/Version**: Rust (workspace, edition 2021, stable toolchain per rust-toolchain.toml)

**Primary Dependencies**: existing crates only — joey-tools, joey-agent-core, joey-core, joey-providers types; zero new external dependencies

**Storage**: filesystem only — `~/.joey/scratchpads/<sanitized-key>-<fnv1a-hex>/scratchpad.md` (plain Markdown, atomic writes, flock sibling lock); no SQLite changes (SCHEMA_VERSION stays 22)

**Testing**: cargo test per-crate (scoped `-p` during implementation); full `cargo test --workspace` exactly once at the final acceptance gate

**Target Platform**: all platforms joey-agent supports (macOS/Linux/Windows cross-platform, constitution principle 0)

**Project Type**: CLI agent workspace (multi-crate library + bin)

**Performance Goals**: state block render + hygiene sweep < 10 ms combined per turn (pure in-memory string ops over bounded inputs); scratchpad append/read < 50 ms local I/O; zero model calls added to any critical path (hygiene is deterministic; guidance is static; boundary uses existing compressor)

**Constraints**: system prompt rendered once per session (never rebuilt per turn — repo invariant); SQLite SCHEMA_VERSION pinned at 22; upstream-verbatim strings untouched; prompt/KV cache friendliness (injections at request tail, not prefix)

**Scale/Scope**: sessions up to max_turns 90 with thousands of tool results; scratchpad bounded to max_entry_chars 8000 and LRU sessions like todo store (64)

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|---|---|---|
| 0. Cross-platform | PASS | scratchpad path sanitization handles Windows-illegal chars in session keys (research R5); all mechanisms pure Rust + std fs |
| I. Workspace-first | PASS | no new crates; changes in joey-tools (scratchpad tool, registry cache), joey-agent-core (state block, hygiene, boundary, guidance, verify nudge), joey-core (config defaults) |
| II. CLI/TUI parity | PASS | no new UI surfaces; scratchpad is a plain tool; state block visible in transcript tools if needed |
| III. Filesystem truth | PASS | scratchpad is file-based; all artifacts under specs/028-...; no UI-only state |
| IV. Test-first | PASS | every task ships with tests alongside (see tasks.md, generated later) |
| V. Incremental delivery | PASS | five mechanisms are five independently shippable increments |
| VII. Non-regression | PASS (conditional) | DEFAULT-ON REQUIRES a written acceptance story: (a) when-disabled byte-parity tests per mechanism (regression coverage mandated by VII); (b) default-on integration test asserting scratchpad registered + state block rendered + hygiene/boundary active under default config; (c) config key additive only. The when-disabled parity tests remain the contractual safety net even though defaults are on. |
| VIII. Performance discipline | PASS | state block: O(items) render, bounded 1200 chars; hygiene: single pass over tool results with token estimator already on the hot path; boundary: existing compressor call; scratchpad: append-only fs writes with atomic replace; schema memoization reduces per-request serialization. Budgets listed in Technical Context. |

Post-Phase-1 re-check: design maintains all gates; VII's "conditional" resolved by contracts (parity-when-disabled tests enumerated in contracts/context-economy-config-keys.md §Non-regression and quickstart.md validation scenarios V1-V3).

## Project Structure

### Documentation (this feature)

```text
specs/028-please-create-feature/
├── plan.md              # This file
├── research.md          # Phase 0 output
└── Phase 1 outputs: data-model.md, quickstart.md, contracts/ (4 files)
```

### Source Code (repository root)

```text
crates/joey-core/src/config.rs           # DEFAULT_CONFIG_YAML: scratchpad.*, state_block.*, compression.midturn_*, compression.boundary_*, agent.context_economy_guidance, agent.retrieval_verification_nudge
crates/joey-tools/src/tools/scratchpad_tool.rs    # NEW
crates/joey-tools/src/tools/mod.rs               # module decl + pub use
crtools = crates/joey-tools/src/builtins.rs        # register scratchpad (config-gated via check() when disabled)
crates/joey-tools/src/registry.rs                # definitions() memoization
crates/joey-agent-core/src/state_block.rs         # NEW renderer
crates/jompey-agent-core/src/agent.rs             # fields, build_request tail-injection, boundary at run_turn exits, verify nudge, dedupe keys
crates/joey-agent-core/src/guidance.rs            # CONTEXT_ECONOMY_GUIDANCE const
crates/joey-agent-core/src/prompt.rs              # gated injection (~L835 pattern)
.rs summary: joey-core 1 file, joey-tools 4, joey-agent-core 5
```

**Structure Decision**: additive edits to existing crates only, following the DAG joey-tools → joey-agent-core (scratchpad lives in joey-tools so sub-agents get it via toolsets; state block/hygiene/boundary live in joey-agent-core next to the turn loop). No new crates (constitution I — nothing justifies a new crate).

## Complexity Tracking

> No Constitution violations to justify. VII's default-on condition is discharged by the parity test contract, not by complexity exceptions.
