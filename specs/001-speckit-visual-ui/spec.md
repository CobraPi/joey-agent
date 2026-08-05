# Feature Specification: Spec-Kit Visual Workflow UI

**Feature Branch**: `001-speckit-visual-ui`

**Created**: 2026-07-23

**Status**: Draft

**Input**: User description: "Build a visual, interactive UI for the spec-kit workflow (`spec.md`, `plan.md`, `tasks.md`) directly on top of this repository, so a developer can create, refine, and execute spec-kit workflows without editing Markdown by hand or running terminal commands sequentially. Three pillars: (1) an interactive Spec → Plan → Task canvas with status color-coding and inline node editing that writes back to Markdown; (2) a split-screen co-pilot workspace with a document pane and an assistant panel for running the clarify/analyze spec-kit steps, highlighting the document line being updated; (3) a Kanban/task-board view generated from the feature directories with per-task metadata (user story, parallel eligibility, target files) and a single-task 'Execute' flow showing live output. Also requested: a one-click project bootstrapping wizard and a Constitution Compliance gauge tied to the analyze step."

**Adapted from** upstream Hermes Agent `specs/001-speckit-dashboard-ui`, re-targeted to the Joey Agent Rust workspace (Constitution Principle II: CLI/TUI parity; Principle III: filesystem is the source of truth).

## Clarifications

### Session 2026-07-23

- Q: When the UI triggers execution from the Kanban board, does one click run a single task or cascade through eligible tasks? → A: Single-task-per-click — each "Execute" action runs exactly one task (the clicked card) and never cascades to other tasks. The user controls sequencing explicitly.
- Q: How are concurrent writes to the same spec-kit Markdown file reconciled (UI edit racing an external edit or a `/speckit-implement` run)? → A: Reject-on-conflict, no merge, no queue — every mutating request carries the content hash of the file the caller last read; the backend re-hashes on disk immediately before writing and rejects with a conflict if it changed. The user must reload and reapply.
- Q: Is the UI a native desktop/TUI app or a local web frontend? → A: A local-only web frontend (browser) served by a new Rust backend crate (`joey-speckit-ui`) bound to `127.0.0.1`. A native GUI toolkit was rejected to avoid fighting layout for the free-form canvas, and the terminal workflow remains fully usable alongside it (Constitution II).
- Q: Can multiple different features have task executions running concurrently? → A: Lock is per-feature — different features can execute tasks concurrently; within a single feature, non-parallel-eligible tasks must not run simultaneously.
- Q: What does the task board show when a task has zero declared target files? → A: Show an explicit "No target files" label on the card rather than hiding the field or flagging it as a parse error.
- Q: What happens to a malformed or hand-edited entry that does not match the expected Markdown structure? → A: It is surfaced as an `Unparsed` node/card rather than being silently dropped — the view degrades gracefully and never loses data.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Visualize the Spec → Plan → Tasks Hierarchy (Priority: P1)

A developer working on a Joey feature opens the UI, selects a project with an existing `.specify/` workflow, and sees an interactive canvas showing the specification, its technical plan, and every generated task as connected nodes, color-coded by status (Draft, In Progress, Completed), without needing to open any Markdown file or run a CLI command.

**Why this priority**: This is the foundational visualization that every other pillar depends on — the co-pilot workspace and task board both need a working, file-backed model of specs/plans/tasks before they are useful. Without this, the UI is just a Markdown viewer.

**Independent Test**: Can be fully tested by pointing the UI at a repo that already has `specs/<feature>/spec.md`, `plan.md`, and `tasks.md` on disk, confirming the canvas renders one node per artifact with correct parent-child connections and status colors matching each file's actual state (e.g., a task marked `[x]` in `tasks.md` renders Completed).

**Acceptance Scenarios**:

1. **Given** a feature directory containing `spec.md`, `plan.md`, and `tasks.md` with a mix of completed and pending tasks, **When** the user opens the canvas for that feature, **Then** the canvas renders the spec node, the plan node, and one node per task, connected by lines reflecting parent/child derivation, with colors reflecting each artifact's real status.
2. **Given** the canvas is open, **When** the user double-clicks a task node, **Then** an inline editor opens showing that task's Markdown content, and saving the edit writes the updated content back to `tasks.md` on disk within the same task block (no reformatting of unrelated tasks).
3. **Given** a task's status changes on disk (e.g., another process, the CLI, or a `/speckit-implement` skill marks it complete), **When** the UI is viewing the canvas, **Then** the corresponding node's color updates without requiring the user to reload the page.

---

### User Story 2 - Run Spec-Kit Steps From a Split-Screen Co-Pilot Workspace (Priority: P2)

A developer drafting or refining a specification opens the split-screen workspace: the left pane shows the live Markdown document (spec.md or plan.md), and the right pane is an assistant panel where they can invoke the clarify and analyze spec-kit steps and converse with the assistant about ambiguous requirements, watching the exact line in the left document highlight and update as each clarification is resolved.

**Why this priority**: This is the primary "creation and refinement" loop the user explicitly asked for — it replaces the terminal-driven `/speckit-clarify` flow with an in-UI equivalent and is usable on day one even before the canvas or Kanban board exist.

**Independent Test**: Can be fully tested by opening a spec with at least one `[NEEDS CLARIFICATION: ...]` marker, invoking clarify from the assistant panel, answering the presented question in the chat, and confirming the corresponding marker in the left-hand document is replaced with the user's answer and the affected line is visually highlighted during the update.

**Acceptance Scenarios**:

1. **Given** a spec file with an active `[NEEDS CLARIFICATION]` marker, **When** the user runs the clarify step from the assistant panel and answers the resulting question, **Then** the left document pane updates the corresponding line in place and the change is persisted to the file on disk.
2. **Given** the user is editing the left-hand Markdown pane directly, **When** they type a change, **Then** the change is saved back to the underlying file (with a visible saving/saved indicator) without requiring an explicit action that leaves the file stale if the session ends.
3. **Given** the user runs the analyze step, **When** the report completes, **Then** the assistant panel displays the consistency findings, and any spec sections it flags are visually marked in the left-hand document pane.

---

### User Story 3 - Track and Execute Tasks From a Kanban Board (Priority: P3)

A developer with a feature that has moved past many generated subtasks wants a Kanban board, generated directly from the feature's `tasks.md`, that lets them see task metadata (user story, parallel-eligibility, target files) at a glance, and click "Execute" on a single card to run that one task via `/speckit-implement`, watching the active card's live status and output.

**Why this priority**: This is valuable for larger features but depends on Story 1's file-backed data model already being in place; it is lower priority than the P1/P2 stories because a developer can still get significant value from visualization and drafting before task-scale execution tracking is needed.

**Independent Test**: Can be fully tested by opening a feature with an existing `tasks.md` containing multiple tasks across different priority/user-story groupings, confirming the board renders one column per status and one card per task with the correct metadata, then triggering execution of a single task and confirming its card transitions to Done and that the underlying task's checkbox in `tasks.md` flips to complete when the run reports success — and that no other card starts running.

**Acceptance Scenarios**:

1. **Given** a `tasks.md` with tasks split across phases and user-story groupings, **When** the user opens the task board, **Then** cards are grouped into board columns by completion status and each card shows its user story, phase, parallel-eligibility marker (`[P]`), and target file list (or an explicit "No target files" label).
2. **Given** the user clicks "Execute" on one card, **When** the underlying `/speckit-implement` run starts, **Then** only that card shows a live running state and streamed output (no other card starts), and the card moves to Done or Failed when that task's run concludes; on success the card's checkbox in `tasks.md` is marked complete on disk.
3. **Given** two tasks are mutually dependent (one requires the other's target file), **When** the user views the board's dependency/timeline toggle, **Then** the dependency is visually indicated.

---

### User Story 4 - Bootstrap a Spec-Kit Project and Monitor Constitution Compliance (Priority: P3)

A developer starting spec-kit workflows on a repo that has not yet run `specify init` uses an initialization wizard in the UI to pick a coding-agent integration and script environment, and thereafter sees a persistent "Constitution Compliance" gauge that reflects the latest analyze outcome against the project's `constitution.md`.

**Why this priority**: Onboarding and governance polish; the core creation/refinement/execution loop (Stories 1-3) delivers the requested value without it, so this is delivered last.

**Independent Test**: Can be fully tested by pointing the UI at a repository without a `.specify/` directory, completing the wizard with a chosen integration, and confirming the directory structure `specify init` produces on the CLI appears on disk; then running analyze and confirming the compliance gauge changes state based on the report's pass/fail outcome.

**Acceptance Scenarios**:

1. **Given** a repository with no `.specify/` directory, **When** the user completes the bootstrap wizard selecting an integration and environment, **Then** the UI invokes the equivalent of `specify init --here` with those options and the resulting `.specify/` structure matches what the CLI would produce.
2. **Given** a project with a completed analyze run reporting constitution violations, **When** the user views the UI, **Then** the Constitution Compliance gauge shows a failing state with a link to the specific violations.
3. **Given** an analyze run reports no violations, **When** the user views the UI, **Then** the gauge shows a passing state.

---

### Edge Cases

- What happens when the underlying Markdown file is edited outside the UI (e.g., directly in a text editor, or via the CLI/skill commands) while the UI has it open? The UI must detect the external change and either live-refresh or prompt to reload rather than silently overwriting it on next save (FR-018).
- How does the system handle a `tasks.md`/`plan.md` that does not follow the expected template structure (hand-edited, malformed, or from an older template version)? The canvas and board must degrade gracefully — render an `Unparsed` placeholder node/card — rather than crash the view (FR-003a).
- What happens when a `/speckit-implement` run is already active for a feature and the user tries to trigger another execution for an overlapping/non-parallel-eligible task in the same feature? The UI must block or queue the second execution rather than run conflicting writes concurrently. Executions in different feature directories are independent (FR-012a).
- How does the system behave when no agent session/process is available to run a step? The assistant panel and Execute actions must show a clear disabled/offline state instead of a silent failure.
- What happens when a project has multiple concurrent features under `specs/`? The UI must let the user switch between feature directories without losing unsaved edits in the currently open document.
- What happens when a write is attempted against a stale view of a file? It is rejected with a conflict (no partial write, no silent merge); the user reloads and reapplies (FR-018, Clarifications).

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The UI MUST discover existing spec-kit feature directories (`specs/<NNN-name>/`) in the active project and list them for selection, reading `spec.md`, `plan.md`, and `tasks.md` where present.
- **FR-002**: The UI MUST render an interactive canvas showing one node per Specification, Plan, and Task artifact for a selected feature, with connecting lines representing spec → plan → task derivation.
- **FR-003**: Canvas nodes MUST be color-coded by status (at minimum: Draft/Not Started, In Progress, Completed), derived from the real state of the underlying Markdown (e.g., task checkbox state, presence of unresolved `[NEEDS CLARIFICATION]` markers).
- **FR-003a**: Any recognized-but-malformed entry (a checkbox line without a parseable task id, a non-conforming heading, etc.) MUST be surfaced as an `Unparsed` node/card rather than being silently dropped, so no source data is hidden (Edge Cases).
- **FR-004**: Double-clicking (or an equivalent explicit action) a canvas node MUST open an inline editor for that artifact's content, and saving MUST write the change back to the corresponding file on disk, preserving the rest of the file's content and structure.
- **FR-005**: The UI MUST provide a split-screen workspace with a Markdown document pane (left) and an assistant/command panel (right) for a selected spec or plan file.
- **FR-006**: The assistant panel MUST allow the user to invoke spec-kit steps (at minimum clarify and analyze) against the currently open document, using the same underlying workflow logic the CLI and spec-kit skills use today.
- **FR-007**: When a clarify-style interaction resolves a question, the system MUST update the corresponding location in the left-hand document pane and MUST visually indicate which line(s) changed as a result.
- **FR-008**: Edits made directly in the left-hand document pane MUST be persisted back to the underlying file, with a visible saved/unsaved/saving state indicator.
- **FR-009**: The UI MUST provide a task board view, generated by parsing a feature's `tasks.md`, with one card per task and columns reflecting task completion status.
- **FR-010**: Each task card MUST display, at minimum, the task's user story/phase grouping, whether it is marked parallel-eligible (`[P]`), and its target file(s) as parsed from the task's Markdown line; a task with zero declared target files MUST show an explicit "No target files" label rather than a blank field or a parse-error state.
- **FR-011**: The task board MUST support triggering execution of exactly one task (the clicked card — equivalent to `/speckit-implement` scoped to that single task), reflecting live running/succeeded/failed state, showing streamed output, and MUST update the underlying `tasks.md` checkbox state when the task completes successfully. A single Execute action MUST NOT cascade to other tasks (Clarifications).
- **FR-012**: The task board MUST prevent (block, with a clear message) triggering execution of a task whose declared prerequisite tasks have not yet completed, and MUST offer a dependency/timeline-style view toggle that visualizes these dependencies.
- **FR-012a**: Concurrency control MUST be scoped per feature: task executions in different feature directories (`specs/<NNN-name>/`) MAY run concurrently, while within a single feature, non-parallel-eligible tasks MUST NOT execute simultaneously.
- **FR-013**: The UI MUST detect when an open document has changed on disk outside the current session and MUST auto-refresh or prompt the user rather than silently overwrite the external change on next save.
- **FR-014**: The UI MUST provide a bootstrap/initialization wizard that can set up a new `.specify/` project structure (integration + environment selection) for repositories that do not yet have one, producing output equivalent to running `specify init --here` with the chosen options.
- **FR-015**: The UI MUST surface a Constitution Compliance gauge reflecting the most recent analyze result for the active feature, showing a clear passing/failing state and linking to the specific findings when failing.
- **FR-016**: All writes the UI makes to spec-kit Markdown files MUST be safe for the existing CLI/skill workflows to continue operating on the same files afterward (i.e., the UI is an additional editor of the same source of truth, not a separate data store).
- **FR-017**: Access to UI actions (including triggering `/speckit-implement`, which writes to the filesystem) MUST respect the existing safety/approval boundaries already in force for the terminal workflow; the UI does not bypass approval gates.
- **FR-018**: Every mutating write MUST use optimistic-concurrency control: the caller supplies the content hash of the file it last read; the backend re-hashes the current on-disk content immediately before writing and rejects with a conflict (leaving the file unmodified) if the hashes differ. Conflicts are never silently merged or queued — the caller must reload current state and resubmit.

### Key Entities

- **Feature**: A spec-kit feature directory (`specs/<NNN-name>/`) containing a specification, an optional technical plan, an optional task list, and optional checklists; identified by its directory name and associated short name.
- **Specification (spec.md)**: The user-facing feature description — user stories, requirements, success criteria, assumptions — with a status (Draft/Clarified/Planned) derived from its content and workflow progress.
- **Plan (plan.md)**: The technical implementation plan derived from a Specification; may not exist until `/speckit-plan` has been run for the feature.
- **Task (tasks.md entry)**: An individual actionable unit derived from the Plan, with an id, description, phase/user-story grouping, parallel-eligibility flag, target file(s), completion state, and optional prerequisite relationships to other tasks.
- **Checklist**: A generated quality checklist (e.g., `checklists/requirements.md`) associated with a Specification, containing pass/fail items.
- **Constitution**: The project-level `.specify/memory/constitution.md` governance rules used as the basis for analyze compliance checks.
- **Status**: Shared enum (`Draft`, `InProgress`, `Completed`, `Unparsed`) derived exhaustively from recognized source markers; anything unrecognized resolves to `Unparsed` rather than being ignored.
- **Workflow Run**: A single invocation of a spec-kit step (clarify, analyze, plan, tasks, implement) against a Feature, with a status, output/log, and — for implement runs — the single Task it affected.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A developer can go from an existing spec-kit feature directory to a fully rendered spec → plan → task canvas in under 10 seconds for a feature with up to ~50 tasks.
- **SC-002**: The canvas and board render exactly one node/card per parsed task — zero tasks silently dropped or duplicated — for any conforming `tasks.md`.
- **SC-003**: A developer can resolve a spec's clarification questions entirely inside the co-pilot workspace, with zero required terminal command invocations.
- **SC-004**: An external file change (edit via terminal, CLI, or skill) is reflected in the UI within 5 seconds without a manual reload.
- **SC-005**: Triggering single-task execution from the board reflects the card's running/succeeded/failed state within seconds of the underlying workflow reporting it, and no other card starts running.
- **SC-006**: Zero data loss — every UI-initiated edit confirmed present in the corresponding file, and every rejected (conflicting) write leaves the file exactly as it was before the attempt.
- **SC-007**: 100% of edits made through the UI remain fully readable and editable by the existing spec-kit CLI/skill workflows afterward (no UI-only file format or metadata that breaks CLI parsing).
- **SC-008**: A new repository can go from "no spec-kit project" to a working `.specify/` structure using only the bootstrap wizard, with zero manual file edits.

## Assumptions

- The UI is a local-only web frontend (browser) served by a new Rust backend crate (`joey-speckit-ui`) bound to `127.0.0.1`; it is not a hosted multi-tenant service, and it reuses the existing workspace's async runtime (`tokio`).
- The target repository already has (or will have, via the bootstrap wizard) the standard spec-kit directory layout (`.specify/`, `specs/<NNN-name>/spec.md|plan.md|tasks.md|checklists/`) produced by the `specify` CLI; the UI does not need to support arbitrary non-standard layouts.
- Spec-kit steps (clarify, analyze, plan, tasks, implement) are executed via the same underlying agent/session mechanism the CLI and existing Joey skills already use, not reimplemented as separate logic in the UI.
- A single developer is the primary editor of a given feature's Markdown files at any one time; multi-user concurrent editing/conflict resolution beyond "detect external change and reject-on-conflict" is out of scope (FR-018). Execution locking (FR-012a) is per-feature, so concurrent work across different features is explicitly supported.
- Real-time updates (node color changes, task board state, live output) are delivered over the backend's WebSocket connection rather than requiring a new transport.
- Mobile/touch-optimized layouts for the canvas and Kanban board are out of scope for v1; the target experience is a desktop browser used by a developer.
- The existing terminal/skill workflow and the UI are equally valid editors of the same files (Constitution II — CLI/TUI parity); the UI is strictly additive.
