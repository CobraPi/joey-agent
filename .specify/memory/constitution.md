<!--
==========================================================================
Sync Impact Report
==========================================================================
Version change: 1.0.0 → 1.1.0

Modified principles: none renamed.

Added principles:
  - VI. Modularity and Decoupling
  - VII. Backward Compatibility and Non-Regression (NON-NEGOTIABLE)
  - VIII. Performance Discipline and Lean Code

Added constraints (Additional Constraints section):
  - Public-surface stability requirement (APIs, CLI flags, config keys,
    file formats, trait definitions).
  - Performance-budget notation requirement for perf-sensitive paths.
  - Dependency weight must be justified against runtime cost.

Removed sections: none.

Templates requiring updates:
  - .specify/templates/plan-template.md   — ✅ aligned (Constitution
    Check gate is dynamically sourced from this file; Technical Context
    already carries Performance Goals + Constraints fields).
  - .specify/templates/spec-template.md   — ✅ aligned (Success Criteria
    already mandates measurable, technology-agnostic outcomes; Key
    Entities and FR slots support the new principles without change).
  - .specify/templates/tasks-template.md  — ✅ aligned (Phase N already
    lists regression-adjacent polish tasks; user-story phasing already
    enforces independent, non-breaking delivery). Note: when the tasks
    command materializes a real tasks.md, it MUST emit regression-coverage
    tasks for any feature touching a public surface — the generic sample
    does not yet name this explicitly, but the principle now mandates it.

Deferred TODOs: none.

Rationale: three new principles added (MINOR bump per semver — new
principles constitute materially expanded governance, not a breaking
redefinition of existing principles).
==========================================================================
-->

# joey-agent Constitution

## Core Principles

### I. Workspace-First Rust
Every feature lives in a dedicated crate under `crates/` within the existing
Cargo workspace; no code is added directly to the workspace root. Crates are
self-contained, independently buildable (`cargo build -p <crate>`), and
independently testable (`cargo test -p <crate>`).

### II. CLI/TUI Parity
Any user-facing capability exposed through a new UI must remain reachable
through the existing `joey-cli` / `joey-tui` surfaces (text in/out,
stdin/args → stdout, errors → stderr). Visual/interactive UIs are additive
layers on top of the same underlying data (files on disk, e.g. `.specify/`
artifacts), never a replacement that hides or diverges from the file-backed
source of truth.

### III. Filesystem Is the Source of Truth (NON-NEGOTIABLE)
For any tool that visualizes or edits spec-kit artifacts (spec.md, plan.md,
tasks.md, checklists), the Markdown/JSON files under `.specify/` remain
authoritative. UI edits must write back to those files synchronously (no
UI-only state that can drift from disk); reads must reflect current file
contents, not a stale in-memory cache.

### IV. Test-First for New Crates
New crates/modules add unit tests alongside implementation and, where they
parse or serialize spec-kit files, contract/round-trip tests proving
file → model → file preserves intent. Tests are written before or alongside
implementation, not deferred to a later cleanup pass.

### V. Incremental, Reviewable Delivery
Large UI efforts (canvas, kanban, co-pilot panel) are decomposed into
independently shippable increments (e.g. parser/model layer, static tree
view, drag-and-drop, live file sync, chat panel) rather than landed as one
monolithic change. Each increment must build and pass tests on its own.

### VI. Modularity and Decoupling
Every crate and module MUST expose a narrow, explicit interface (a trait, a
small public API, or a documented config boundary) and depend only on those
abstractions, never on a sibling module's internal implementation. Coupling
between crates MUST be acyclic and minimized: a change in one crate MUST NOT
force coordinated edits to unrelated crates. New features are added by
composing existing modules or plugging a new one in behind an established
trait boundary — never by threading new logic through shared core paths.
Rationale: the project MUST remain a system where any single feature can be
added, swapped, or removed in isolation without rippling through the rest.

### VII. Backward Compatibility and Non-Regression (NON-NEGOTIABLE)
New features MUST be strictly additive: they extend the system without
breaking existing behavior or introducing regressions. Every public surface
— public APIs, CLI flags and exit codes, config keys, on-disk file formats,
and trait definitions — MUST stay backward-compatible. A breaking change to
any public surface requires a MAJOR version bump and an explicit, documented
migration path recorded in the feature's plan. Any feature that touches a
public surface MUST ship with regression coverage (tests asserting prior
behavior is preserved) before it can be considered complete. `cargo build
--workspace` and `cargo test --workspace` MUST stay green on every increment.
Rationale: regressions erode trust faster than missing features build it;
the cost of a breaking change is always paid by the user.

### VIII. Performance Discipline and Lean Code
Every design decision MUST account for its runtime cost. Code MUST be lean:
prefer zero-copy parsing, minimal allocations, and the simplest algorithm
that meets the requirement; avoid speculative generality and abstractions
that are not exercised by a concrete need. A new dependency is justified by
a concrete, measurable benefit, not by convenience, and its weight (binary
size, compile time, transitive surface) MUST be recorded against the
alternatives in the feature's `research.md`. Performance-sensitive paths
MUST be identified in the plan and carry an explicit budget or benchmark
note (target latency, throughput, or memory bound). No feature is acceptable
if it meaningfully degrades the steady-state latency, throughput, or memory
footprint of existing functionality.
Rationale: a native Rust binary is chosen specifically for performance; code
that silently trades that away defeats the project's foundational premise.

## Additional Constraints

- Primary language: Rust (2021 edition), matching the existing workspace.
- Any new UI surface must specify its rendering approach explicitly (native
  desktop, TUI-embedded, or a separate web frontend) and justify the choice
  against `joey-tui`/`joey-cli` reuse before introducing a new stack.
- No new runtime dependency (JS framework, GUI toolkit, etc.) is added
  without recording the alternatives considered in `research.md` for the
  feature, including the dependency's impact on binary size and compile
  time (Principle VIII).
- Public surfaces (APIs, CLI flags, config keys, file formats, traits) are
  treated as stable contracts (Principle VII); changes to them are gated by
  the versioning policy below.

## Development Workflow

- Follow the spec-kit lifecycle: `/speckit-specify` → `/speckit-clarify`
  (if ambiguous) → `/speckit-plan` → `/speckit-tasks` → `/speckit-implement`.
- Constitution Check gates in `plan.md` must be evaluated honestly against
  all eight principles; violations require an explicit justification
  recorded in the Complexity Tracking section of the plan, not silently
  skipped.
- `cargo build --workspace` and `cargo test --workspace` (or the affected
  crate subset) must pass before a feature is considered complete.
- Any feature touching a public surface MUST include regression-coverage
  tasks in `tasks.md` (Principle VII).
- Any feature introducing a performance-sensitive path MUST record its
  performance budget in `plan.md` (Principle VIII).

## Governance

This constitution supersedes ad hoc practices for any spec-kit-managed
feature in this repository. Amendments require an explicit version bump and
a note in the Sync Impact Report at the top of this file's change history.

Versioning policy (semantic):

- MAJOR: backward-incompatible change to a principle — a principle removed,
  redefined incompatibly, or a NON-NEGOTIABLE constraint relaxed.
- MINOR: a new principle or section added, or materially expanded guidance.
- PATCH: clarifications, wording, typo fixes, non-semantic refinements.

Compliance review: all plans and task breakdowns MUST verify compliance with
these principles before and after the design phase. Constitution Check gates
in `plan.md` are the enforcement point; the Complexity Tracking section is
the only sanctioned outlet for justified, documented deviations.

**Version**: 1.1.0 | **Ratified**: 2026-07-23 | **Last Amended**: 2026-07-23
