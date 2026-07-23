# joey-agent Constitution

## Core Principles

### I. Workspace-First Rust
Every feature lives in a dedicated crate under `crates/` within the existing Cargo
workspace; no code is added directly to the workspace root. Crates are
self-contained, independently buildable (`cargo build -p <crate>`), and
independently testable (`cargo test -p <crate>`).

### II. CLI/TUI Parity
Any user-facing capability exposed through a new UI must remain reachable
through the existing `joey-cli` / `joey-tui` surfaces (text in/out, stdin/args →
stdout, errors → stderr). Visual/interactive UIs are additive layers on top of
the same underlying data (files on disk, e.g. `.specify/` artifacts), never a
replacement that hides or diverges from the file-backed source of truth.

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
independently shippable increments (e.g. parser/model layer, static tree view,
drag-and-drop, live file sync, chat panel) rather than landed as one
monolithic change. Each increment must build and pass tests on its own.

## Additional Constraints

- Primary language: Rust (2021 edition), matching the existing workspace.
- Any new UI surface must specify its rendering approach explicitly (native
  desktop, TUI-embedded, or a separate web frontend) and justify the choice
  against `joey-tui`/`joey-cli` reuse before introducing a new stack.
- No new runtime dependency (JS framework, GUI toolkit, etc.) is added without
  recording the alternatives considered in `research.md` for the feature.

## Development Workflow

- Follow the spec-kit lifecycle: `/speckit-specify` → `/speckit-clarify`
  (if ambiguous) → `/speckit-plan` → `/speckit-tasks` → `/speckit-implement`.
- Constitution Check gates in `plan.md` must be evaluated honestly; violations
  require an explicit justification recorded in the Complexity Tracking
  section of the plan, not silently skipped.
- `cargo build --workspace` and `cargo test --workspace` (or the affected
  crate subset) must pass before a feature is considered complete.

## Governance

This constitution supersedes ad hoc practices for any spec-kit-managed feature
in this repository. Amendments require an explicit version bump and a note in
the Sync Impact Report at the top of this file's change history. All plans and
task breakdowns must verify compliance with these principles before and after
the design phase.

**Version**: 1.0.0 | **Ratified**: 2026-07-23 | **Last Amended**: 2026-07-23
