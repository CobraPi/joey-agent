# Feature Specification: Spec Studio — Visual IDE for Spec Kit

**Feature Branch**: `012-spec-studio-visual-ide`

**Created**: 2026-08-05

**Status**: Draft

**Input**: User description: "Fully implement the whole plan listed in `spec-studio-concept.html` — a Visual IDE concept for Spec Kit ('Spec Studio'). The concept's core thesis is that Spec Kit already produces excellent structured markdown; the problem is that a wall of markdown hides its own structure. The IDE's job is to make the structure that is already in the markdown visible, navigable, and editable. Three pillars: (1) a Stage Bar that answers 'where am I and what's next?'; (2) a Meaning Layer that renders each markdown construct with the visual primitive matching its semantics; (3) a Trace that connects principle → story → requirement → task → file → check as one graph. Markdown stays the single source of truth, byte-for-byte compatible with the CLI; the IDE is a query engine and a staged, reviewable editing surface over it."

**Adapted from** the `spec-studio-concept.html` concept document in this repository, re-targeted to the Joey Agent Rust workspace. This feature **extends and supersedes** the visual foundations established in `specs/001-speckit-visual-ui` (the `joey-speckit-ui` backend + `web/` frontend) and `specs/010-speckit-development-ide` (full artifact authoring + workflow execution). Where `001`/`010` rendered artifacts as documents and provided authoring/execution controls, Spec Studio promotes the rendering into a **semantic graph projection** (the Meaning Layer) and makes round-trip visual editing byte-safe. Constitution Principles II (CLI/TUI parity), III (filesystem is the source of truth — NON-NEGOTIABLE), VII (backward compatibility), and VIII (performance discipline) are governing constraints throughout.

## Clarifications

No `[NEEDS CLARIFICATION]` markers are raised. The concept document is exhaustively specified (14 sections, an explicit build sequence P0–P6, and a 12-point definition of done), so every open decision below is resolved with an informed default and documented in the Assumptions section.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Start a Feature and Orient Without Knowing Spec Kit (Priority: P1)

A developer who has never run a Spec Kit slash command opens the IDE, picks a repository, and is guided through setup into a single landing view that answers four questions at a glance: where is this feature, what is healthy, what is blocked, and what is the one next action. The developer never needs to know artifact names, slash commands, or branch conventions to reach a working state.

**Why this priority**: The first-run experience is the product's front door. The foundational rendering and authoring pillars (US2–US3) depend on a user being able to enter a feature context safely and understand its state deterministically. If orientation is confusing, the rest of the IDE is never reached.

**Independent Test**: Point the IDE at a repository with partial or missing Spec Kit setup, complete the guided setup, and confirm a single landing view renders a deterministic next action, a health summary, a progress figure, a branch binding, an artifact list, and a recent-activity timeline — all derived from on-disk state, with no LLM-generated recommendation.

**Acceptance Scenarios**:

1. **Given** a developer opening the IDE for the first time, **When** they begin setup, **Then** a guided flow walks through repository selection (with read/write validation), Spec Kit setup detection (with repairable gaps named), branch creation or binding (with every affected file shown), outcome description in plain language, and a preview of the proposed paths and agent permissions before anything is written.
2. **Given** the setup preview, **When** the developer confirms, **Then** the feature directory and initial artifact are created in staged mode and the landing view opens — nothing was written to the working tree without confirmation.
3. **Given** an empty, failed, or disconnected state at any point (no Spec Kit setup, no write permission, agent gateway disconnected, unsupported markdown, or a branch changed externally), **When** the state is shown, **Then** exactly one primary recovery action is offered alongside an explanation of what is wrong and which files an action would touch.
4. **Given** a feature is open, **When** the developer views the landing view, **Then** it shows exactly one next-action recommendation computed deterministically from artifact state (never an LLM guess), a progress figure, a health summary (parsing status, open unknowns, orphan requirements), a branch binding with drift state, the artifact list with staleness, and a recent-activity timeline.
5. **Given** the landing view is displayed, **When** the developer activates any tile, **Then** it opens the relevant workflow stage without losing feature context.

---

### User Story 2 - Read and Author Meaning, Not Markdown (Priority: P1)

A developer working on a specification or plan sees each markdown construct rendered with the visual primitive that matches its semantics — priorities become rails, Given/When/Then becomes a flow, success criteria become metric cards, technical context becomes a spec sheet — and can edit through structured forms, inline markdown, or the raw whole file, with every edit writing back to the exact bytes it owns and preserving everything else.

**Why this priority**: This is the heart of the concept ("the Meaning Layer") and the visible payoff that distinguishes Spec Studio from a markdown viewer. The Stage Bar and orientation (US1) are necessary scaffolding, but the meaning widgets are what make the structure already present in the markdown visible. Without them the IDE collapses back into the anti-pattern the concept explicitly rejects.

**Independent Test**: Open a feature with a populated `spec.md` and `plan.md`, confirm every supported markdown construct renders as its matching visual primitive (story card, requirement chip, metric card, entity graph, spec sheet, governance gauge, violation card, tree diff), edit one node through a structured form, and confirm only that node's byte range changed on disk while every untouched byte stayed identical.

**Acceptance Scenarios**:

1. **Given** a `spec.md` containing user stories, acceptance scenarios, functional requirements, success criteria, and key entities, **When** the developer opens the spec board, **Then** each construct renders with its matching primitive: prioritized stories as story cards with Given/When/Then flows, requirements as coverage-aware chips whose modality drives color, success criteria as metric cards showing target/unit/direction, and entities as an interactive graph.
2. **Given** a success criterion with no configured measurement source, **When** it renders as a metric card, **Then** it displays "not measured" rather than a decorative progress bar implying data that does not exist; current values appear only when a named evidence source is configured.
3. **Given** an entity list with explicit and ambiguous relationships, **When** it renders as a graph, **Then** explicit relationships are confirmed edges and inferred relationships are labelled as proposed until the developer confirms or rejects them.
4. **Given** a `plan.md` with technical context, a constitution check, a complexity tracking table, and a project structure tree, **When** the developer opens the plan view, **Then** technical context renders as labelled tiles (with unresolved values shown as directly clickable controls, not color-only text), the constitution check renders as pass/fail rows with evidence and an aggregate, each complexity violation renders as a side-by-side card of rule/need/rejected-alternative, and the project structure renders as a tree diff with exists/planned-missing/not-in-plan status.
5. **Given** any rendered node, **When** the developer wants to edit, **Then** three depths are always available: a structured form (the default, impossible to produce malformed markdown), inline markdown on just that node's range, and the raw whole file as an escape hatch.
6. **Given** the developer edits a node through any depth, **When** the edit is applied, **Then** only that node's byte range is rewritten; whitespace, comments, unknown extensions, and every untouched byte remain byte-identical, and the edit is transactional (a temporary buffer is parsed and validated before the file is atomically replaced).

---

### User Story 3 - Break Work Down and Move Safely on a Board (Priority: P2)

A developer with a generated task list sees it as a board where phases are columns and each task encodes four dimensions visually (completion, owning story, parallel eligibility, target file). The developer can reorder within a phase, toggle completion, and move tasks across phases — with every cross-boundary move pausing for a semantic-impact preview before anything changes on disk.

**Why this priority**: The task board is where `tasks.md` stops being a 200-line checklist and starts being the execution surface. It depends on the Meaning Layer's task-card primitive and the round-trip patch engine from US2, so it follows them. It is the artifact that "suffers most in plain text" and the one structurally already a board.

**Independent Test**: Open a feature with a multi-phase `tasks.md`, confirm phases render as columns and each task card shows its checkbox, story color, parallel badge, file link with existence state, and derived requirement coverage, then toggle a task and move one across phases, confirming the cross-phase move first shows a semantic-impact preview (affected checkpoints, dependencies, exact markdown change) and that the within-phase toggle wrote only the exact checkbox bytes.

**Acceptance Scenarios**:

1. **Given** a `tasks.md` with phases and tasks carrying inline syntax (id, parallel marker, story, target file), **When** the developer opens the tasks board, **Then** each phase renders as a column with a completion count, and each task renders as a card with a native checkbox, a story-colored left border consistent across every view, a parallel-eligibility badge, a file link that states whether the path exists, and a derived requirement chip from the traceability graph.
2. **Given** a task card, **When** the developer toggles its checkbox, **Then** the completion marker is written back to the exact task line in `tasks.md` and no other byte changes.
3. **Given** tasks within one phase, **When** the developer reorders them, **Then** a small source-patch preview is shown and the reorder is applied optimistically with an undo entry.
4. **Given** a task the developer wants to move across phases, **When** the move is initiated, **Then** nothing changes on drop alone; instead a semantic-change preview shows the source phase, destination phase, affected checkpoints, dependency inversions or violations, and the exact markdown change, and the move proceeds only after explicit confirmation.
5. **Given** the tasks board, **When** the developer switches views, **Then** the board (default) is available, with timeline and dependency-graph views available as later additions that render the same underlying graph differently.

---

### User Story 4 - See Coverage and Trace the Whole Feature (Priority: P2)

A developer or reviewer can trace any requirement to the user story it delivers value for, the tasks that implement it, the files those tasks change, and the checks that verify it — as one connected, clickable graph. Orphans (a requirement no task implements), rogue tasks, unverified work, and constitution breaches become visually impossible to miss, each with a one-click fix.

**Why this priority**: This is described in the concept as "the single highest-value thing a visual IDE can add." The chain from principle to shipped file is theoretically present across five documents and practically invisible in the CLI. It depends on the meaning widgets (US2) and the task board (US3) existing, so it follows them, but it is the comprehension payoff that justifies the whole effort.

**Independent Test**: Open a feature with spec, plan, tasks, and checklists, select one requirement in the spec board, and confirm the tasks board dims everything except the implementing tasks, the file tree highlights the affected files, and the checklist scrolls to the verifying check — all simultaneously; then open the coverage matrix and confirm orphan requirements, rogue tasks, unverified items, and breaches are each flagged with a one-click fix.

**Acceptance Scenarios**:

1. **Given** a connected feature (spec, plan, tasks, checks all present), **When** the developer selects any node in any view, **Then** its full chain is highlighted in every other view simultaneously — selecting a requirement dims unrelated tasks, highlights the affected files, and scrolls to the verifying check.
2. **Given** requirements and tasks, **When** the developer opens the coverage matrix, **Then** it renders requirements against user stories with cell density showing task counts, and orphan requirements (zero implementing tasks) are visually distinct.
3. **Given** the traceability analysis, **When** the developer inspects defects, **Then** four classes are detected and each offers a one-click fix: orphan requirements (generate the missing task), rogue tasks (link or promote to a requirement), unverified items (feed into checklist generation), and constitution breaches (justify or redesign).
4. **Given** `[NEEDS CLARIFICATION]` markers across any artifacts, **When** the developer opens the clarify queue, **Then** all open unknowns are collected in one batched queue (not serial one-at-a-time), each carrying its source line, owning requirement, and downstream blockers, and answering one creates a staged patch under the same review policy as any agent edit.
5. **Given** the coverage state, **When** artifacts change, **Then** the matrix recomputes as a persistent live quality gauge rather than a one-shot prose report.

---

### User Story 5 - Run the Agent and Review Staged Changes Safely (Priority: P2)

A developer can launch any Spec Kit workflow step from the IDE, watch it stream without ever looking frozen, answer mid-run questions as first-class cards, and review every change the agent produces at hunk granularity before anything touches the working tree — because staged-by-default is non-negotiable.

**Why this priority**: The run experience is where trust is won or lost, and staged-by-default is "what makes a visual agent IDE trustworthy enough to use on real branches." It depends on the workflow model (US1's stage bar) and the meaning widgets (US2) being in place, and it is the trust payoff that makes the IDE safe rather than dangerous.

**Independent Test**: Start a workflow step from the IDE, confirm a tool-call timeline (not a text log) streams with elapsed time and phase labels and progressive artifact previews, trigger a clarifying question and answer it as a card, then review the produced changes at hunk granularity labelled by semantic meaning, accepting some hunks and rejecting others, and confirm the working tree changed only for accepted hunks.

**Acceptance Scenarios**:

1. **Given** a ready workflow step, **When** the developer starts it, **Then** a tool-call timeline streams each read/write/search as a row with a state icon, artifacts appear progressively in their destination widget as they are produced, and an elapsed time and phase label keep long steps readable as working rather than hung.
2. **Given** a running step, **When** the agent asks a clarifying question or requests permission, **Then** it surfaces as a first-class interactive card (not a buried log line) and the same run resumes with its context intact after the developer responds.
3. **Given** a running step, **When** the developer closes the tab or the connection drops, **Then** the run is not killed or lost — reconnecting reattaches to the in-flight run.
4. **Given** a step that crashed or was interrupted, **When** the developer reconnects, **Then** a recovery surface offers resume, retry, or discard with a truthful summary of preserved effects.
5. **Given** a completed step, **When** the developer reviews its output, **Then** every produced change is staged (never auto-applied), presented as a diff with hunks labelled by semantic meaning (not just line numbers), and the developer can accept or reject at hunk granularity with the working tree changing only for accepted hunks.
6. **Given** an accepted hunk that resolves a clarify question or satisfies a requirement, **When** it is accepted, **Then** the matching clarify card clears and the coverage matrix updates in one consistent action.
7. **Given** questions, permissions, live runs, failures, and review decisions, **When** the developer opens the activity surface, **Then** they share one chronological center, each item tagged as a draft, a derived event, or a proposed repo patch.

---

### Edge Cases

- The selected repository has Spec Kit initialized but no features, or a feature is missing one or more expected artifacts.
- An artifact was produced by an older or customized template and does not contain the expected headings or inline syntax.
- A markdown construct is unsupported by the Meaning Layer — parseable nodes render visually while unsupported ranges stay editable as raw text; the view never blanks out.
- The developer edits a node while a run is touching the same file — the edited node locks and the agent's output for that node diverts to the review pane instead of clobbering the edit mid-thought.
- An external change lands on disk while the developer has unsaved edits — a revision-hash mismatch blocks the write and a three-way merge card compares base, current file, and proposed patch at semantic-block level.
- An anchor no longer resolves because the document structure changed underneath — the node degrades to read-only with a "structure changed — reopen" prompt; it never guesses a new range.
- A patch fails validation — no file is replaced; the proposed buffer and parser diagnostics remain available for repair or raw review.
- A teammate continues the workflow purely on the CLI without the GUI — the feature must remain fully operable, with CLI-compatible identity and binding sidecars maintained and private display preferences kept outside the repo.
- A branch changes underneath the IDE — the IDE warns and shows changed nodes and their impact rather than silently showing another feature's data.
- A feature has hundreds of tasks (200+) — the board must remain interactive and within its performance budget.
- The agent produces malformed or partial markdown — partial render applies to parseable nodes, the rest falls back to raw text.

## Requirements *(mandatory)*

### Functional Requirements

#### First-run journey and orientation

- **FR-001**: The IDE MUST provide a guided first-run flow that walks a developer through, in order: repository selection (with read/write access validation before continuing), Spec Kit setup detection (naming missing templates, constitution, integration, and repairable gaps), branch creation or binding (showing every file the binding changes), outcome description in plain language, and a preview of the proposed slug, branch, generated paths, and agent permissions — with nothing written to the working tree until the developer confirms.
- **FR-002**: For every empty, failed, or disconnected state (no Spec Kit setup, no write permission, agent gateway disconnected, unsupported markdown, or branch changed externally), the IDE MUST display exactly one primary recovery action alongside a plain-language explanation of what is wrong and which repository files the recovery would touch.
- **FR-003**: Navigation MUST be organized by developer intent (Overview → Define → Design → Break down → Build → Review), with `spec.md`, `plan.md`, and `tasks.md` appearing inside those stages as source indicators and escape hatches rather than as knowledge the developer must possess upfront.

#### Feature home (Atlas) and the Stage Bar

- **FR-004**: The IDE MUST provide a single feature landing view that answers, at a glance: current stage and progress, health (parsing status, open unknowns, orphan requirements), branch binding with drift state, the artifact list with staleness, and a recent-activity timeline — each tile opening the relevant workflow stage without losing feature context.
- **FR-005**: The landing view MUST show exactly one next-action recommendation at a time, computed deterministically from artifact state and run history (never generated by a language model), so that the recommendation is reproducible, auditable, instant, and free.
- **FR-006**: A compact five-stage workflow indicator (Define → Design → Break down → Build → Review) MUST remain persistently visible in the feature header, answering "where am I and what is next?", with Spec Kit command-level detail expanding only when the developer opens the current stage.
- **FR-007**: Each workflow step's state (Done / Active / Blocked / Locked) MUST be computed deterministically from artifacts on disk plus run history — never by a language model — where Done means the output artifact exists, parses cleanly, and is newer than its inputs; Active is the single recommended next action or in-flight run; Blocked names the exact missing or stale prerequisite; and Locked steps are shown greyed rather than hidden so the whole journey is legible from the start.
- **FR-008**: When a step is blocked, the IDE MUST show what failed and the single button that fixes it as a gate card — never a stack trace and never a bare "command failed."

#### The Meaning Layer — semantic rendering

- **FR-009**: Every supported markdown construct MUST render with the visual primitive that encodes its exact semantic, never a generic markdown blob. The mapping catalog comprises: prioritized user stories → story cards with priority labels and move controls; Given/When/Then → three-field flows; atomic requirements → coverage-aware requirement chips with modality-driven color; `[NEEDS CLARIFICATION]` markers → actionable question buttons; success criteria → target metric cards; key entities → interactive graphs; task lines → task cards with parallel/story/file/requirement channels; phase headings → board columns; checkpoints → checkpoint gates; technical context → spec sheets; constitution checks → pass/fail rows with an aggregate; complexity-tracking entries → violation cards; project-structure trees → tree diffs; and checklist items → checklist rows with category grouping.
- **FR-010**: Each rendered value MUST visually distinguish its origin: source (read directly from markdown), derived (computed from the semantic graph), or overlay (external evidence or private IDE state). Current values for success criteria MUST appear only when a named measurement source is configured; otherwise the card states "not measured" and no decorative element implies data that does not exist.
- **FR-011**: Entity relationships inferred from prose MUST be labelled as proposed and require explicit developer confirmation or rejection before they affect traceability; explicit relationships are confirmed edges.

#### Round-trip editing safety

- **FR-012**: The IDE MUST parse markdown losslessly into a concrete syntax tree that preserves whitespace, comments, unknown extensions, and every untouched byte, so that rendering and re-serialization never reformat or lose content the developer did not touch.
- **FR-013**: Every parsed node MUST carry one anchor contract: a UTF-8 byte range, the expected source bytes, a document revision hash, and a structural fingerprint. These checks — not reparsing alone — are what make visual editing safe.
- **FR-014**: Before applying any write, the IDE MUST verify that the document revision hash and expected bytes still match the current file; on any mismatch it MUST route to a three-way merge rather than attempting write-back. Edits MUST be surgical (changing one node rewrites only that node's range), applied transactionally (a temporary buffer is parsed and validated before the file is atomically replaced), and every accepted visual edit MUST produce an undo entry containing the verified inverse patch.
- **FR-015**: Three editing depths MUST always be available for any node: a structured form (the default, which cannot produce malformed markdown), inline markdown on just that node's range, and the raw whole file as a guaranteed escape hatch. No editing depth may be the only path to a change.
- **FR-016**: The IDE MUST handle concurrent writers with fixed, non-silent behavior: developer edits during a run lock the edited node and divert the agent's output for that node to the review pane; external on-disk changes trigger a reparse and block the write on a revision mismatch, offering a three-way merge at semantic-block level when the developer has unsaved edits; an anchor that no longer resolves degrades the node to read-only with a reopen prompt (never guessing a new range); malformed markdown produces a partial render (parseable nodes get widgets, the rest falls back to raw text); and a failed patch validation replaces no file while keeping the proposed buffer and diagnostics available.

#### Tasks board

- **FR-017**: The tasks board MUST render each phase as a column with a completion count, and each task as a card exposing four encoded dimensions visually: a native completion checkbox, an owning-story indicator whose color is consistent across every view, a parallel-eligibility badge, and a target-file link that states whether the path exists (or an explicit "no target files" label). Derived requirement coverage chips come from the traceability graph, not the source line.
- **FR-018**: Toggling a task checkbox MUST write the completion marker back to the exact task line in `tasks.md` and change no other byte.
- **FR-019**: Reordering tasks within a phase MUST apply optimistically and show a small source-patch preview with an undo entry. Moving a task across phases MUST change nothing on drop alone — it MUST first show a semantic-change preview naming the source and destination phases, affected checkpoints, any dependency inversion or violation, and the exact markdown change, and proceed only after explicit confirmation. Every drag MUST also have an equivalent Move menu so keyboard and assistive-technology users have the same capability.
- **FR-020**: The tasks board MUST offer at least the board view by default, with timeline and dependency-graph views as later additions over the same underlying graph (phases as horizontal bands with checkpoints as diamonds and parallel tasks in lanes; nodes and edges from dependency clauses with cycles rendered distinctly).

#### Traceability and the clarify queue

- **FR-021**: The IDE MUST render the traceability spine — principle → user story → requirement → task → file → check — as one connected graph, and selecting any node in any view MUST highlight its full chain in every other open view simultaneously (a selected requirement dims unrelated tasks, highlights affected files, and scrolls to verifying checks).
- **FR-022**: A coverage matrix MUST render requirements against user stories with cell density indicating how many tasks implement each intersection, and orphan requirements (zero implementing tasks) MUST be visually distinct from covered ones.
- **FR-023**: The IDE MUST detect and offer one-click fixes for four defect classes: orphan requirements (generate the missing task), rogue tasks (link to a requirement or promote), unverified implemented items (feed into checklist generation), and constitution breaches (justify or redesign). The cross-artifact consistency analysis MUST render as a persistent live matrix rather than a one-shot prose report.
- **FR-024**: Every `[NEEDS CLARIFICATION]` marker across all artifacts MUST be collected into a single batched clarify queue (not serial one-at-a-time blocking). Each queue item MUST carry its source line, owning requirement, and downstream blockers; answering MUST create a proposed source patch reviewed under the same staged-change policy as any agent edit, and accepted answers MUST be recorded with timestamp, author, and the reviewed patch.

#### Agent runs and staged review

- **FR-025**: Agent output MUST land in a staging area first; the working tree MUST change only after the developer accepts hunks in the review pane. Staged-by-default is a non-negotiable trust constraint — no agent edit is auto-applied to the working tree.
- **FR-026**: A single persistent Agent Activity Center MUST present questions, requested permissions, proposed actions, live runs, failures, and review decisions as one chronological surface, with each item tagged as a draft, a derived event, or a proposed repository patch.
- **FR-027**: A running step MUST never look frozen: the IDE MUST render a tool-call timeline (not a text log) where each read/write/search is a row with a state icon, stream agent output progressively into its destination widget as it is produced (with optimistic skeletons where content is still arriving), show an elapsed time and phase label, and support reattaching to an in-flight run after a tab close or reconnect.
- **FR-028**: A crashed or interrupted run MUST surface a recovery interface offering resume, retry, or discard with a truthful summary of preserved effects and completed work.
- **FR-029**: Review MUST be diff-first: every produced change MUST be presented with hunks labelled by semantic meaning (e.g. "adds requirement FR-016"), and the developer MUST be able to accept or reject at hunk granularity. Accepting a hunk that resolves a clarify question or satisfies a requirement MUST clear the matching clarify card and update the coverage matrix in one consistent action; the working tree changes only for accepted hunks.
- **FR-030**: Cancelling a running step MUST preserve a truthful record of completed and incomplete effects and MUST NOT falsely mark the step successful.

#### Architecture and source-of-truth constraints

- **FR-031**: Repository state MUST remain readable and fully operable without the IDE: a teammate on the CLI never needs the GUI to continue the workflow. CLI-compatible identity and binding sidecars MUST remain supported; private display preferences MUST stay outside the repository.
- **FR-032**: The system MUST be organized as three layers: Truth (markdown, CLI-compatible sidecars, and git — written only by the agent through reviewed patches or the developer's accepted edits, with unknown syntax preserved and untouched ranges never rewritten), Meaning (a lossless concrete syntax tree plus a derived semantic graph where every node keeps its byte range, expected bytes, revision hash, and structural fingerprint), and Overlay (private IDE state — run history, private bindings, anchored comments, board positions, filters — that MUST never dirty the working tree). Shared binding metadata is a separate, explicit repo-sidecar mode that the IDE previews before enabling.
- **FR-033**: The IDE MUST drive the existing Spec Kit workflow methods over the established gateway surface (run preparation, start, streamed events, interaction response, cancel/retry/recover, and run attach/get) — no new transport is required, and no new execution engine substitutes for the native agent.

#### UX techniques and accessibility

- **FR-034**: The IDE MUST provide semantic zoom across three altitudes (whole-feature Atlas → single-artifact Board → single-node Focus) where zooming changes information density, not just scale, and a command palette reachable by keyboard through which every action, artifact, requirement, and task is reachable by typing.
- **FR-035**: Direct manipulation MUST be optimistic for same-boundary operations (local reorder, checkbox toggle) and MUST pause for a semantic-impact preview, dependency validation, and explicit confirmation for any cross-boundary move; every drag MUST have a Move-menu and undo equivalent.
- **FR-036**: The IDE MUST use progressive disclosure (secondary detail deferred behind explicit disclosure; nothing depending on hover), focus-and-context highlighting (selecting a node dims unrelated content across every open panel instead of navigating away), and deterministic guidance (every "next action" computed by rules, never by a language model).
- **FR-037**: Every primary authoring, workflow, review, approval, and recovery journey MUST be completable with keyboard-only navigation, with visible focus and descriptive labels. State MUST always be conveyed as color plus icon plus text (never color alone), meet WCAG AA contrast, and expose native semantics and status via live regions; touch targets on small screens MUST meet accessibility size guidance.
- **FR-038**: Every feature, node, run, and review state MUST have a deep link, with selection, filters, scroll position, and staged status surviving view changes and browser Back/Forward navigation. Motion MUST be optional and disabled under reduced-motion preferences.
- **FR-039**: Purpose-built responsive modes MUST be provided: desktop supporting graph authoring and multi-panel comparison; tablet prioritizing structured forms and board review; mobile focusing on status, questions, approvals, and diffs rather than precision graph manipulation.

#### Capacity, compatibility, and scope

- **FR-040**: The IDE MUST remain interactive for large features — including boards of at least 200 tasks — within its stated performance budget, without hiding records or preventing completion of the primary workflow.
- **FR-041**: The IDE MUST preserve byte-for-byte CLI compatibility of every artifact under `specs/###-feature/`: edits write back through verified byte anchors and the IDE never reformats an untouched range. This re-states Constitution Principle III as a hard, testable contract for this feature.
- **FR-042**: The IDE MUST NOT introduce WYSIWYG rich-text editing (it round-trips markdown lossily), a proprietary project database (it breaks the git-native contract), LLM-generated next-step hints (non-deterministic, slow, costly, and occasionally wrong about something computable exactly), a requirement that users know terminal commands (raw markdown remains an in-product escape hatch but is never required), or auto-applying agent edits (staged-by-default is non-negotiable).

### Key Entities

- **Feature**: A Spec Kit feature directory; its repository context, open artifacts, current layout, readiness summary, branch binding, and active runs.
- **Artifact**: A repository-backed feature document (specification, plan, task list, checklist, research note, data model, contract, quickstart) with path, type, current content version (revision hash), validity, dirty state, and dependency relationships.
- **Node**: A parsed semantic unit in the Meaning Layer, carrying its kind, owning artifact, UTF-8 byte anchor, expected source bytes, document revision hash, structural fingerprint, semantic properties, and edges to related nodes.
- **Workflow Step**: A core or extension Spec Kit lifecycle stage with identity, order, purpose, prerequisites, expected inputs and outputs, availability, and a computed state.
- **Workflow Attempt**: One execution of a workflow step by the agent, with status, timestamps, transcript, interactions, outputs, validation, recovery checkpoints, and links to prior attempts.
- **Agent Interaction**: A question, answer, approval request, approval decision, progress event, or tool activity tied to an active attempt.
- **Change Set / Hunk**: A staged artifact or source-file change attributed to an attempt, divided into reviewable hunks with acceptance state, dependency warnings, semantic labels, and recovery actions.
- **Defect**: A detected traceability problem — orphan requirement, rogue task, unverified item, or constitution breach — with its source nodes and one-click fix.
- **Clarify Item**: An open `[NEEDS CLARIFICATION]` marker collected into the queue, with its source location, owning requirement, downstream blockers, and proposed resolution patch.
- **Overlay Record**: Private IDE state (run history, private bindings, anchored comments, board positions, filters, panel layout) keyed by repository and branch, never written to the working tree.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: At least 95% of developers in usability testing can complete the full Spec Kit workflow — from first-run setup through implementation review — entirely within the IDE, without opening a terminal, excluding external account authorization the IDE cannot perform.
- **SC-002**: A developer can determine the feature's current stage, health, blockers, and single next action from the landing view in under 30 seconds.
- **SC-003**: 100% of "next action" recommendations are computed deterministically from artifact state; zero are generated by a language model.
- **SC-004**: A reader can understand a feature's scope, priority, open questions, and measurable targets from the spec board in under 30 seconds.
- **SC-005**: 100% of visual edits preserve every byte outside the edited node's anchor range in round-trip tests across all supported artifact types.
- **SC-006**: 100% of writes are guarded by a revision-hash and expected-bytes check; zero writes succeed through an undetected external change.
- **SC-007**: 100% of agent-produced changes land in staging first; zero agent edits touch the working tree without an explicit hunk-level acceptance.
- **SC-008**: A developer can answer any clarifying question and accept the resulting patch within 3 clicks from the moment the question surfaces.
- **SC-009**: 100% of orphan requirements, rogue tasks, unverified items, and constitution breaches present in the artifacts are detected and surfaced with a one-click fix; zero defects present in the data are hidden.
- **SC-010**: A board of 200 tasks renders its initial view within the stated performance budget and remains interactive (filtering, toggling, scrolling) for at least 95% of interactions.
- **SC-011**: 100% of primary journeys — authoring, workflow execution, review, approval, and recovery — are completable with keyboard-only navigation and pass the project's accessibility acceptance review (visible focus, descriptive labels, color-plus-icon-plus-text state, WCAG AA contrast).
- **SC-012**: Artifacts edited through the IDE remain usable by the installed Spec Kit skills and the native agent workflows operating outside the UI in 100% of compatibility tests — confirming the git-native, CLI-compatible contract.
- **SC-013**: A teammate using only the CLI can continue any feature worked on in the IDE in 100% of tested scenarios, with no GUI-only state required to proceed.
- **SC-014**: At least 85% of pilot users rate the IDE's workflow clarity, change control, and overall professionalism as 4 or 5 on a 5-point scale.

## Assumptions

- This feature extends and evolves the visual foundations from `specs/001-speckit-visual-ui` (`joey-speckit-ui` Rust backend + `web/` frontend) and `specs/010-speckit-development-ide` (artifact authoring, workflow execution, staged/direct change modes, Git-backed staging, JSONL run history). Spec Studio's distinguishing contribution is the Meaning Layer (lossless semantic parsing) and byte-safe round-trip visual editing layered on top of those existing capabilities; prior capabilities are reused, not duplicated.
- The concept document names an upstream dependency stack (a reactive web framework, a flow/graph library, a code editor, a markdown renderer, a diff viewer, resizable panels, an animation library, a charting library, and a CSS framework). Per Constitution Additional Constraints, the specific stack choice and its justification against `joey-tui`/`joey-cli` reuse, binary size, and compile time belong in this feature's `research.md` and `plan.md`, not in this specification. The spec deliberately stays technology-agnostic at the capability level; the existing `web/` frontend's already-present stack is the assumed baseline unless `research.md` documents a justified addition.
- The target primary experience is a desktop-class workspace in the local browser; tablet and mobile are purpose-built reduced modes (status, questions, approvals, diffs), not full authoring surfaces — matching the concept's responsive-mode definition.
- The native agent (configured model, tools, safety approvals, feature context) remains the authoritative execution experience, driven out-of-process over the established gateway (per FR-033 and the `specs/010` FR-011 model); the IDE introduces no separate reduced-capability execution engine.
- Repository files remain the single source of truth (Constitution III). The Overlay layer (run history, private bindings, anchored comments, board positions, filters) is supporting metadata that never replaces or forks canonical artifacts; losing it costs convenience, never authored work.
- The build sequence defined in the concept (P0 lossless parser + patch engine; P1 first-run + stage model; P2 meaning widgets; P3 boards; P4 trace + clarify; P5 activity center + review; P6 polish) is the assumed delivery order. P0 is treated as the critical foundation: if the lossless parser and optimistic-concurrency patch engine are not solid, every visual edit becomes a file-corruption risk and the concept collapses.
- The concept's 12-point definition of done (connect/validate/initialize; bind a branch; author/amend the constitution; describe an outcome and generate a staged spec; answer clarifications; generate and edit the design; generate/reorder tasks with semantic-impact previews; run analysis and repair defects; generate and complete checklists; scope agent permissions and run implementation; review semantic hunks selectively with undo and conflict resolution; recover from a failed or interrupted run) is the acceptance surface for the "100% of the workflow in the UI" test and is covered by the user stories and functional requirements above.
- State is always conveyed as color plus icon plus text (never color alone), with WCAG AA contrast, from the first phase — accessibility semantics are part of every primitive, not a late pass, matching both the concept and Constitution Principle VII's non-regression bar.
- The project's constitution (`.specify/memory/constitution.md`, v1.1.0) is the governance baseline for the Constitution Check gates surfaced in plan review and the analyze step; this specification does not relax any principle.
