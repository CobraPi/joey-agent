# Implementation Plan: SpecKit Visual UI

**Branch**: `001-speckit-visual-ui` | **Date**: 2026-07-23 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/001-speckit-visual-ui/spec.md`

**Note**: This template is filled in by the `/speckit-plan` command; its definition describes the execution workflow.

## Summary

Build a local, browser-rendered visual front end for this repository's
spec-kit workflow (`spec.md` / `plan.md` / `tasks.md` under `specs/`). A new
Rust backend crate (`joey-speckit-ui`) parses those Markdown files into a
structured model, serves it over a local HTTP+WebSocket API, watches the
files for external changes, and serializes writes so the UI and terminal
workflows never corrupt each other (per the Clarifications: reject-on-conflict
semantics). A separate static web frontend (vanilla TS + a small canvas/graph
library, no heavy framework) consumes that API to render three views: the
Spec-to-Task canvas (User Story 1), the split-screen co-pilot workspace tied
to `/speckit-clarify` and `/speckit-analyze` (User Story 2), and the Kanban
task board with single-task-per-click execution wired to `/speckit-implement`
(User Story 3). The filesystem under `specs/<feature>/` and
`.specify/memory/constitution.md` remains the sole source of truth throughout.

## Technical Context

**Language/Version**: Rust 1.75+ (workspace edition 2021) for the backend;
TypeScript (compiled, no build-step framework runtime requirement beyond a
bundler) for the browser frontend.

**Primary Dependencies**: `axum` (HTTP + WebSocket server, chosen as the new
dependency — see research.md), `tokio` (already a workspace dependency),
`notify` (filesystem watch, new dependency), `serde`/`serde_json` (already
workspace dependencies), `pulldown-cmark` (Markdown parsing, new dependency)
for the backend; a lightweight canvas/graph rendering library (selected in
research.md) for the frontend.

**Storage**: Files on disk only — `specs/<feature>/{spec.md,plan.md,tasks.md,checklists/*.md}`
and `.specify/memory/constitution.md`. No database.

**Testing**: `cargo test` (unit + contract round-trip tests) for the backend
crate, matching existing workspace convention; a lightweight browser test
harness (Playwright, already available in this environment's tool-belt) for
frontend acceptance scenarios.

**Target Platform**: Local developer machine (macOS/Linux), backend process
bound to `127.0.0.1`, opened in the user's default browser. Not a hosted
multi-tenant service (per Assumptions).

**Project Type**: Web application (local backend + local frontend), added as
a new workspace member alongside the existing `joey-cli`/`joey-tui`/`joey-gateway`
crates.

**Performance Goals**: Canvas render for a feature with up to ~50 tasks in
under 10s from cold open (SC-001); external file-change reflected in the UI
within 5s (SC-004); board correctly reflects 100% of parsed tasks (SC-002).

**Constraints**: Single local user at a time (no real-time multi-user
collaboration, per Assumptions); conflicting writes are rejected, never
silently merged (FR-018, Clarifications); UI must not require deprecating
the existing terminal/slash-command workflow (constitution II).

**Scale/Scope**: One feature directory open at a time in the UI; task counts
in the tens to low hundreds (per Kanban board pain point in the spec), not
tested against thousands of tasks in this iteration.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|---|---|---|
| I. Workspace-First Rust | PASS | New backend lives in its own crate, `crates/joey-speckit-ui`, added to the workspace `members` list; independently buildable/testable via `cargo build -p joey-speckit-ui` / `cargo test -p joey-speckit-ui`. |
| II. CLI/TUI Parity | PASS | The UI is additive: it reads/writes the same `specs/`/`.specify/` files the existing `joey-cli` slash-command flow and `/speckit-*` skills already use. Nothing in this plan removes or hides the terminal workflow; a user can keep using `specify`/skills exclusively. |
| III. Filesystem Is the Source of Truth (NON-NEGOTIABLE) | PASS | FR-004/FR-005/FR-008/FR-013/FR-017 in the spec, and the reject-on-conflict write model from Clarifications, are carried directly into data-model.md and the API contract: every read parses current files, every write goes through one serialized path, no UI-only cache is authoritative. |
| IV. Test-First for New Crates | PASS (planned) | Phase 1 will define contract/round-trip tests (parse → model → serialize → byte-for-byte-equivalent-content) as part of the crate's initial test suite, written alongside the parser rather than deferred. Enforced at `/speckit-tasks` time by ordering parser+test tasks before UI-consuming tasks. |
| V. Incremental, Reviewable Delivery | PASS (planned) | Matches the spec's own P1/P2/P3 story priorities: parser/model + canvas (P1) ships first and is independently useful, then co-pilot workspace (P2), then Kanban board (P3). `/speckit-tasks` will preserve this ordering. |
| Additional Constraint: justify new runtime deps | PASS (documented) | `axum`, `notify`, and `pulldown-cmark` are new dependencies; each is justified with alternatives considered in research.md, as the constitution requires. |

No unjustified violations. Complexity Tracking section left empty.

## Project Structure

### Documentation (this feature)

```text
specs/001-speckit-visual-ui/
├── plan.md              # This file (/speckit-plan command output)
├── research.md          # Phase 0 output (/speckit-plan command)
├── data-model.md         # Phase 1 output (/speckit-plan command)
├── quickstart.md         # Phase 1 output (/speckit-plan command)
├── contracts/            # Phase 1 output (/speckit-plan command)
│   └── speckit-ui-api.md
├── checklists/
│   └── requirements.md
└── tasks.md              # Phase 2 output (/speckit-tasks command - NOT created by /speckit-plan)
```

### Source Code (repository root)

```text
crates/
├── joey-speckit-ui/                 # NEW crate: backend for this feature
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── main.rs                  # binary entry point: starts server, opens browser
│       ├── model.rs                 # Feature/Specification/Plan/Task/Clarification/Finding types
│       ├── parser/
│       │   ├── mod.rs
│       │   ├── spec.rs              # spec.md -> Specification/UserStory/FR nodes
│       │   ├── plan.rs              # plan.md -> Plan/ConstitutionCheck nodes
│       │   └── tasks.rs             # tasks.md -> Task nodes (id, status, [P], files)
│       ├── writer.rs                # serialize model edits back to Markdown, conflict detection
│       ├── watcher.rs                # notify-based file watch -> change events
│       ├── commands.rs               # invokes existing speckit skills/scripts (clarify/analyze/implement)
│       ├── api/
│       │   ├── mod.rs
│       │   ├── rest.rs               # GET feature tree, PATCH node, POST init
│       │   └── ws.rs                 # WebSocket: file-change push, task-run output stream
│       └── conflict.rs               # version/etag-based reject-on-conflict logic
│   └── tests/
│       ├── parser_roundtrip.rs
│       └── conflict_detection.rs
├── joey-cli/                         # existing; unchanged, still the terminal entry point
├── joey-tui/                         # existing; unchanged
└── ...

web/speckit-ui/                       # NEW: static frontend, not a Cargo crate
├── package.json
├── src/
│   ├── canvas/                       # Pillar 1: Spec-to-Task mind-map
│   ├── workspace/                    # Pillar 2: split-screen co-pilot
│   ├── board/                        # Pillar 3: Kanban board
│   └── api-client.ts                 # REST + WebSocket client
└── tests/
    └── e2e/                          # Playwright acceptance tests per user story
```

**Structure Decision**: Web application structure (backend + frontend), added
as a new `joey-speckit-ui` workspace member crate for the backend and a
sibling `web/speckit-ui/` static frontend project (not part of the Cargo
workspace, built independently). This keeps the existing single-project Rust
layout (`crates/*`) intact per constitution principle I, while isolating the
new browser-facing pieces so `joey-cli`/`joey-tui` remain untouched and the
new crate can be developed/tested independently (principle V).

## Complexity Tracking

> Fill ONLY if Constitution Check has violations that must be justified

No violations — table intentionally left empty.
