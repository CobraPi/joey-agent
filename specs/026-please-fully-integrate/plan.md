# Implementation Plan: Native Spec-Kit Integration with Copilot Command Parity

**Branch**: `026-please-fully-integrate` | **Date**: 2026-09-03 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/026-please-fully-integrate/spec.md`

## Summary

Make the full upstream spec-kit command surface native to Joey Agent: all ten lifecycle commands (specify, clarify, plan, constitution, checklist, tasks, analyze, implement, converge, taskstoissues) invokable as `/speckit-<name>` AND dotted `speckit.<name>`, with workflow bodies bundled in the binary (project-local overrides honored from `.github/skills/`, `.github/agents/`+prompts, and `.specify/`), twenty extension hook points executed per `.specify/extensions.yml`, upstream handoff chaining, and session-start lifecycle-context injection. Orchestration (hypercode/OMO conductor) gains lifecycle-step detection and tasks.md-driven fan-out; neurocode gains feature-scoped context selection, scope-prioritized indexing, and acceptance-criteria-aware verification planning. Everything is additive and disableable via a new `speckit.*` config section.

## Technical Context

**Language/Version**: Rust 2021 edition, stable toolchain (workspace `rust-toolchain.toml`)

**Primary Dependencies**: existing workspace crates only — joey-cli (dispatch), joey-core (config defaults), joey-agent-core (prompt assembly slots), joey-neurocode (context/index/verification), joey-omo (conductor prompt), joey-speckit-ui (spec/plan/tasks parser, reused read-only); `serde_yaml` (already in workspace via joey-core) for extensions.yml; std `include_str!` for bundled bodies. No new external dependencies.

**Storage**: filesystem only — `.specify/feature.json`, `specs/<feature>/{spec,plan,tasks}.md`, `.specify/extensions.yml`, `.github/{skills,agents,prompts}/` layouts; no database changes.

**Testing**: `cargo test -p <crate>` per crate plus full `cargo test --workspace`; new parity tests enumerate the 10 commands x 2 forms and 20 hook points against pinned upstream tables.

**Target Platform**: all platforms Joey supports (macOS, Linux, Windows); pre-flight scripts use the scaffold's bash variants with PowerShell-scaffold fallback handling per FR-002.

**Project Type**: CLI/TUI agent (existing Cargo workspace, 17 crates).

**Performance Goals**: lifecycle command dispatch (pre-flight + body load + turn submission) under 2 seconds excluding model inference (SC-003); lifecycle context injected exactly once per session (no per-turn prompt work); feature-scoped indexing under 3 seconds where unscoped sweeps took 10+ seconds (SC-005a).

**Constraints**: system prompt built once per session — lifecycle context must slot in at session construction, never per-turn; system-prompt prefix caches must stay warm; strictly additive public surfaces (constitution VII); zero new external crates (constitution VIII).

**Scale/Scope**: 10 bundled bodies (~1-2 KB each), 20 hook points, 2 dispatch surfaces (REPL + TUI), 3 consuming subsystems (slash layer, orchestration, neurocode).

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Evidence |
|---|---|---|
| 0. Cross-platform | PASS | Pure Rust + filesystem; script fallbacks handle platform variants (FR-002) |
| I. Workspace-First Rust | PASS | All code in existing crates under crates/; nothing at workspace root |
| II. CLI/TUI Parity | PASS | Dotted + slash forms dispatch identically in repl.rs and tui.rs via shared speckit_slash functions |
| III. Filesystem Source of Truth | PASS | Lifecycle state derived on demand from .specify/specs files; no UI-only state; injection is read-only over files |
| IV. Test-First | PASS | Parity/contract/regression tests specified per component in tasks phase; bundled-body resolution round-trip tests |
| V. Incremental Delivery | PASS | Four shippable increments: (1) bodies+dispatch, (2) hooks+handoffs, (3) lifecycle injection+orchestration, (4) neurocode scoping |
| VI. Modularity | PASS | Reuses joey-speckit-ui parser instead of a second markdown parser; narrow new-module APIs (speckit_bodies/speckit_hooks/speckit_lifecycle) |
| VII. Backward Compat | PASS | Existing 12 /speckit-* commands keep names/behavior; speckit.enabled=false restores exact prior behavior; CONFIG_VERSION bump is additive keys only |
| VIII. Performance Discipline | PASS | No new dependencies (include_str! over rust-embed); once-per-session injection; budgets recorded in Technical Context |

Post-Phase-1 re-check: unchanged — design introduces no violations; Complexity Tracking left empty.

## Project Structure

### Documentation (this feature)

specs/026-please-fully-integrate/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
│   ├── command-surface.md
│   ├── config-keys.md
│   ├── lifecycle-state.md
│   └── hooks.md
└── tasks.md             # Phase 2 output (NOT created by /speckit-plan)

### Source Code (repository root)

crates/joey-cli/src/
├── speckit_slash.rs       # extended: dotted-form entry point, bundled fallback in body resolution
├── speckit_bodies.rs      # NEW: vendored bodies (include_str!) + resolution chain + frontmatter (handoffs/scripts/tools) parsing
├── speckit_hooks.rs       # NEW: extensions.yml model, 20 hook points, mandatory/optional execution
├── speckit_lifecycle.rs   # NEW: step detection, session-start context block, tasks.md→TaskNode adapter (via joey-speckit-ui model)
├── speckit_bodies/*.md    # NEW: ten vendored workflow body files
├── repl.rs / tui.rs       # extended: `speckit.` dotted intercept, session-start lifecycle injection (repl)
├── oneshot.rs / slash_menu.rs  # extended: lifecycle injection in oneshot sessions; both-form completion candidates
├── hypercode.rs / neurocode_wiring.rs  # extended: write_set exclusivity checks; FeatureScope→scope_files wiring
├── slash.rs               # extended: dotted aliases recorded on the ten speckit REGISTRY entries
├── main.rs                # extended: module wiring
crates/joey-core/src/config.rs                          # speckit: defaults block (enabled, lifecycle_context, hooks)
crates/joey-neurocode/src/engine.rs                     # CodingRequest: additive scope_files field (default empty)
crates/joey-neurocode/src/context/discovery.rs          # seed find_primary_nodes from scope_files
crates/joey-neurocode/src/auto_index.rs                 # scope-prioritized reindex ordering
crates/joey-neurocode/src/verification_plan.rs          # additive acceptance_criteria input
crates/joey-omo/src/agents/prompts/conductor.rs         # dynamic CURRENT LIFECYCLE STATE block appended after static doctrine
Tests (alongside implementation, per constitution IV):
crates/joey-cli/src/tests/speckit_native.rs             # parity (10x2 forms), 20 hook points, resolution chain, dotted dispatch
crates/joey-neurocode/tests/scope_enrichment.rs         # scoped context + acceptance-criteria verification
crates/joey-omo/tests/verify_prompts.rs                 # extended: dynamic lifecycle block assertions

**Structure Decision**: extension of existing crates only — no new crate is justified (all logic is either CLI dispatch, config, or additive fields in existing engines); reuse of joey-speckit-ui avoids a second markdown parser.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| (none) | | |
