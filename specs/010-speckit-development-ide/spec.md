# Feature Specification: Spec-Kit Development IDE

**Feature Branch**: `010-speckit-development-ide`

**Created**: 2026-08-03

**Status**: Draft

**Input**: User description: "Enhance the Spec-Kit visual UI into a professional, full-featured development IDE. Users must be able to update and edit every artifact (spec, plan, tasks, checklists, research, data model, contracts, quickstart), modify and control every workflow step, and run each step with the native Joey Agent — reviewing and selectively accepting the resulting changes."

**Adapted from** upstream Hermes Agent `specs/002-spec-kit-development-ide`, re-targeted to the Joey Agent Rust workspace. Extends the visual UI from `specs/001-speckit-visual-ui` (the `joey-speckit-ui` backend + `web/speckit-ui` frontend) from a viewer into an authoring and execution surface, while preserving Constitution Principle II (CLI/TUI parity) and Principle III (filesystem is the source of truth).

## Clarifications

### Session 2026-08-03

- Q: Must Joey Agent changes be staged for review or written directly to the active repository? → A: The user explicitly selects staged or direct mode for every run. Staged mode keeps candidate changes separate from the working tree until the user applies them; direct mode writes to the working tree as the run proceeds and clearly labels changes as live.
- Q: How long must workflow attempt history survive after the backend or Joey Agent restarts? → A: Persist it locally for 90 days, then automatically expire it.
- Q: What happens to an active workflow after the Joey Agent process or backend restarts? → A: Automatically resume it from the latest safe checkpoint; if no valid checkpoint exists, stop without replaying unconfirmed actions and report preserved effects plus the required recovery action.
- Q: Does modifying a workflow step affect only one run, the project, or the installed definition? → A: Support run-specific and reusable project-level overrides while keeping installed skill/workflow definitions read-only.
- Q: What is the smallest unit users can accept or reject during change review? → A: Individual hunks and files, with dependency warnings before an unsafe partial selection is applied.

### Session 2026-08-03 (Joey adaptation)

- Q: How does the `joey-speckit-ui` backend reach the "native Joey Agent" (FR-011) — in-process library call or out-of-process? → A: Out-of-process. The backend spawns the `joey` CLI (or the relevant skill wrapper) as a subprocess and streams its stdout/stderr/interaction over the existing WebSocket channel, the same model `specs/001` uses for its `/speckit-implement` wrapper. This keeps `joey-speckit-ui` decoupled from `joey-agent-core` internals (Constitution VI: depend only on an established interface, here the CLI contract) and lets the agent reuse its real auth/tool/approval stack unchanged.
- Q: Where does staged mode hold candidate changes while they are under review (FR-016)? → A: Git-backed. Candidates are written to the Git index (or a dedicated temporary worktree/branch on the same repository) so that accept/reject/recover map to native Git primitives (`git checkout`, `git restore`, hunk-level `git apply --reject`), recovery survives backend restarts, and run-attributed changes are naturally distinguished from the user's unrelated uncommitted work. No out-of-tree scratch store or overlay filesystem is introduced (Constitution VIII).
- Q: What durable store backs the 90-day workflow history (FR-018)? → A: Append-only JSONL — one file per feature under `~/.joey/speckit-ui/history/<feature-id>.jsonl`, each line a self-contained attempt record. This adds no new dependency (the workspace already standardizes on `serde_json` and `joey-core` already owns `~/.joey/`), makes 90-day expiry a trivial file-mtime sweep, and lets the review UI stream records without a query engine. A dedicated SQLite DB (or piggybacking on `joey-core`'s session store) is rejected as unjustified for a sequential append-and-read log (Constitution VIII) and to avoid introducing a second schema/versioned format into `joey-speckit-ui` (Constitution VII).

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Author Every Spec-Kit Artifact in One Workspace (Priority: P1)

A developer opens a Spec-Kit feature and works in a professional, project-oriented workspace where the specification, clarifications, implementation plan, research, data model, contracts, tasks, checklists, and supporting documents are discoverable and editable without leaving the UI. The developer can edit `plan.md` as a first-class artifact rather than treating it as a read-only workflow result.

**Why this priority**: Full artifact authoring is the foundation of an IDE. Workflow controls are not trustworthy if users cannot inspect and deliberately modify the source documents they operate on.

**Independent Test**: Open an existing feature containing a populated specification, plan, tasks, checklist, and supporting documents; select and edit each supported artifact; verify changes, validation state, and navigation remain synchronized with the repository files.

**Acceptance Scenarios**:

1. **Given** an existing feature with specification, plan, tasks, checklists, and supporting documents, **When** the user opens the IDE, **Then** a feature explorer lists the available artifacts by workflow phase and opens the selected artifact in an editor with a rendered preview.
2. **Given** `plan.md` is open, **When** the user changes a section and saves, **Then** the updated plan is persisted, its modified state is visible, and unrelated content is preserved.
3. **Given** an artifact has unsaved edits, **When** the user switches files, features, or views, **Then** the IDE preserves the draft or explicitly asks whether to save or discard it rather than losing work.
4. **Given** an artifact contains unresolved placeholders, malformed required sections, or incomplete checklist items, **When** it is opened or edited, **Then** the IDE identifies the affected locations and explains what must be resolved before dependent steps can run.

---

### User Story 2 - Control the Complete Workflow with Joey Agent (Priority: P1)

A developer sees the feature's complete Spec-Kit lifecycle as an ordered set of controllable steps. For each applicable step, the developer can inspect its purpose and inputs, modify the prompt or execution scope, run it with the native Joey Agent, answer questions, monitor progress, cancel it, and review the resulting changes before continuing.

**Why this priority**: The defining enhancement is replacing a passive viewer with a control surface for the full Spec-Kit development lifecycle while retaining Joey Agent's reasoning and tool capabilities.

**Independent Test**: Starting from an existing draft specification, run each applicable workflow step through the IDE, including an interactive step that requests input, and verify the native Joey Agent executes in feature context, reports progress, writes the expected artifacts, and leaves an auditable result.

**Acceptance Scenarios**:

1. **Given** a selected feature, **When** the user opens workflow controls, **Then** the IDE shows every supported step in lifecycle order — constitution, specify, clarify, plan, checklist, tasks, analyze, implement, converge, and task-to-issue publication where available — with clear ready, blocked, running, attention-needed, succeeded, failed, and stale states.
2. **Given** a workflow step is ready, **When** the user opens its run configuration, **Then** the user can review and modify the step instructions, scope, artifact targets, available agent options, and explicitly select staged or direct change mode before starting the run.
3. **Given** the user has refined a step for repeated use in the current project, **When** they save those instructions as a project override, **Then** later runs in that project use the override while the installed workflow definition and other projects remain unchanged.
4. **Given** the user starts a workflow step, **When** execution begins, **Then** it runs through the native Joey Agent in the selected feature and repository context and streams meaningful status, tool activity, questions, and outputs into the IDE.
5. **Given** the Joey Agent needs a decision or approval, **When** it pauses, **Then** the IDE presents the request in context and resumes the same run after the user responds.
6. **Given** a step is running, **When** the user chooses cancel, **Then** the run stops safely, reports which changes were completed, and does not falsely mark the step successful.
7. **Given** a step has completed, **When** the user reviews it, **Then** the IDE shows the changed artifacts, validation results, run transcript, duration, and final status before the next dependent step is started.

`attention-needed` is a presentation aggregate rather than a persisted attempt status. The IDE derives it when the precise state is awaiting input, awaiting approval, blocked by a recoverable finding, conflicted, or recovery-failed, and always exposes the underlying state and remediation.

---

### User Story 3 - Review, Refine, and Re-run Agent Changes Safely (Priority: P1)

A developer remains in control of agent-authored changes. The IDE presents document and code changes as reviewable differences, allows selective acceptance or rejection where safe, and supports editing the affected artifact before re-running a step. It protects external edits and makes recovery from an unwanted or failed run straightforward.

**Why this priority**: A professional development environment must make autonomous changes inspectable and reversible. Without this control, running implementation steps from the interface risks hidden or destructive repository changes.

**Independent Test**: Run a workflow that modifies multiple files, review the resulting changes, retain one change while reverting another through supported recovery controls, edit the plan, re-run the affected step, and verify no unrelated work is overwritten.

**Acceptance Scenarios**:

1. **Given** a completed run changed documents or source files, **When** the user opens the run review, **Then** every affected file is listed with additions, removals, and a summary of why it changed.
2. **Given** a run's changes are under review, **When** the user accepts or rejects an individual hunk or file, **Then** the IDE warns about dependent changes before applying an unsafe partial selection and the workspace and workflow state reflect the resulting repository contents.
3. **Given** a file changed outside the IDE after it was loaded, **When** the user attempts to save or apply agent output, **Then** the IDE blocks silent overwrite and provides reload, compare, and deliberate resolution choices.
4. **Given** a workflow step failed or produced an unsatisfactory result, **When** the user edits its inputs and re-runs it, **Then** the new attempt is recorded separately and the earlier attempt remains available for comparison.
5. **Given** a run is about to perform broad or destructive changes, **When** the action reaches the configured approval boundary, **Then** the IDE pauses and requires explicit user approval with a plain-language impact summary.

---

### User Story 4 - Navigate Work as an Integrated Development Project (Priority: P2)

A developer can move fluidly between workflow, documents, tasks, execution output, and repository changes using a coherent desktop-class interface. The IDE remembers the user's working layout and surfaces current status without forcing them to reconstruct context after every navigation action.

**Why this priority**: Integrated navigation and workspace ergonomics distinguish a professional IDE from a collection of disconnected UI tabs, but they depend on the core authoring and execution controls.

**Independent Test**: Work through a multi-document feature using only keyboard and pointer navigation, resize the primary panes, filter tasks, move between a running workflow and its changed files, reload the page, and verify the working context is restored.

**Acceptance Scenarios**:

1. **Given** a feature is open, **When** the IDE loads, **Then** it presents a unified layout containing feature/artifact navigation, a primary editor or visual view, workflow controls, and a contextual agent/run panel.
2. **Given** the user resizes, collapses, or reorders supported workspace panes, **When** they return to the feature, **Then** the chosen layout and last-open artifacts are restored.
3. **Given** a feature contains many tasks and documents, **When** the user searches or filters, **Then** matching artifacts, requirements, task identifiers, and run records are reachable without manually scanning every file.
4. **Given** a validation finding, task, graph node, or run event refers to an artifact location, **When** the user activates the reference, **Then** the IDE opens the correct artifact and focuses the relevant section.
5. **Given** the user relies on keyboard navigation or assistive technology, **When** they operate core authoring, workflow, review, and approval controls, **Then** all required actions remain available with visible focus and descriptive labels.

---

### User Story 5 - Understand Readiness and Delivery Progress (Priority: P2)

A developer or reviewer can determine what is complete, what is stale, what is blocked, and what should happen next across the feature. The IDE derives readiness from the actual artifacts and workflow history and provides actionable guidance rather than a decorative progress indicator.

**Why this priority**: Mature workflows need trustworthy status and traceability, especially after a user manually edits an upstream specification or plan.

**Independent Test**: Complete several workflow steps, modify an upstream requirement, and verify dependent plan, analysis, and task outputs become visibly stale with a clear recommended next action.

**Acceptance Scenarios**:

1. **Given** artifacts and workflow runs exist for a feature, **When** the user views the lifecycle, **Then** each step's state is derived from current artifact validity, prerequisites, and its most recent relevant run.
2. **Given** the user changes an upstream artifact after dependent artifacts were generated, **When** the change is saved, **Then** affected downstream steps are marked stale without deleting their current content.
3. **Given** a step is blocked, **When** the user inspects it, **Then** the IDE lists the unmet prerequisite, unresolved decision, failed validation, or active conflicting run and offers the appropriate navigation or recovery action.
4. **Given** a reviewer opens a completed feature, **When** they inspect its history, **Then** they can trace requirements to plan sections, tasks, implementation runs, and final convergence findings.

### Edge Cases

- The selected repository has Spec-Kit initialized but no features, or a feature is missing one or more expected artifacts.
- An artifact was produced by an older or customized template and does not contain the expected headings.
- A workflow or extension step is unavailable in the active Spec-Kit installation; it must be labeled unavailable rather than simulated.
- A run remains active when the browser reconnects, reloads, or temporarily loses its live connection.
- The Joey Agent process or backend exits during a step; the run must reconnect to its latest safe checkpoint automatically, or report that recovery is impossible without repeating unconfirmed actions when no safe checkpoint exists.
- The user edits an upstream artifact while a dependent step is running.
- Two UI views or an external editor attempt to update the same artifact.
- A run changes files outside the selected feature directory, including user source code with unrelated uncommitted changes.
- A workflow emits a large transcript or modifies hundreds of files; the IDE must remain navigable and preserve access to the complete record.
- A feature has cyclical, missing, or contradictory task dependencies.
- A user attempts to run a blocked step, start a conflicting second run for the same feature, or close the IDE while input or approval is pending.
- The repository is read-only, a write fails partway through, or available storage is exhausted.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The IDE MUST extend the existing Spec-Kit visual UI (`joey-speckit-ui` backend + `web/speckit-ui` frontend from `specs/001-speckit-visual-ui`) and use the repository's Spec-Kit artifacts as the authoritative feature content; it MUST NOT require a separate copy of specifications, plans, or tasks.
- **FR-002**: The IDE MUST provide a unified, resizable workspace containing feature and artifact navigation, an artifact editor/preview area, workflow controls, and contextual Joey Agent run information.
- **FR-003**: The artifact explorer MUST discover and open the specification, plan, tasks, checklists, research, data model, contracts, and other feature-local supporting documents without assuming all artifacts exist.
- **FR-004**: Users MUST be able to edit `spec.md`, `plan.md`, `tasks.md`, checklists, and supported feature-local documents as first-class artifacts.
- **FR-005**: Editing MUST provide clear dirty, saving, saved, invalid, externally changed, and read-only states and MUST preserve unrelated document content when saving a focused change.
- **FR-006**: The IDE MUST provide both source-oriented editing and rendered reading for supported text artifacts, with navigation between document outline entries and referenced locations.
- **FR-007**: The IDE MUST validate required artifact structure and unresolved workflow markers and MUST associate each finding with an actionable artifact location.
- **FR-008**: The IDE MUST expose the complete set of Spec-Kit workflow steps available to the active project (constitution, specify, clarify, plan, checklist, tasks, analyze, implement, converge, task-to-issue publication), including installed extension steps, while clearly distinguishing unavailable steps.
- **FR-009**: Each workflow step MUST display its purpose, required inputs, prerequisites, expected outputs, current state, and recommended next action.
- **FR-010**: Before starting a step, users MUST be able to inspect and modify run instructions, execution scope, and target artifacts. The IDE MAY expose only server-advertised agent options: configured model selection, reasoning effort, and maximum iteration limit. The backend MUST validate every selected target and option against the active workflow, configured provider catalog, and safety bounds. Every run MUST require an explicit staged or direct change-mode selection, and reusable project configuration MUST NOT silently select that mode.
- **FR-011**: Every runnable workflow step MUST execute through the native Joey Agent in the selected repository and feature context; the IDE MUST NOT substitute a separate reduced-capability execution engine. The `joey-speckit-ui` backend MUST drive the agent out-of-process by spawning the `joey` CLI (or the relevant `/speckit-*` skill wrapper) as a subprocess in the feature's repository context, streaming progress/questions/output over the existing WebSocket channel, rather than linking against `joey-agent-core` internals (Constitution VI — depend only on the CLI contract, not shared core paths).
- **FR-012**: A Joey Agent run MUST support streamed progress, tool activity, user questions, approval requests, final output, and an explicit terminal status.
- **FR-013**: Users MUST be able to answer a pending question or approval request and continue the same Joey Agent run with its conversation and feature context intact.
- **FR-014**: Users MUST be able to cancel a running or waiting workflow, and cancellation MUST preserve a truthful record of completed and incomplete effects.
- **FR-015**: The system MUST prevent incompatible simultaneous writes within the same feature (building on the reject-on-conflict write model from `specs/001`) while allowing independent features to run concurrently when their repository effects do not conflict.
- **FR-016**: The IDE MUST present all artifact and source-file changes from a run in a review view that identifies affected files and shows additions and removals; users MUST be able to accept or reject individual hunks and whole files, and the IDE MUST identify dependent changes and warn before an unsafe partial selection is applied. Staged mode MUST keep candidate changes separate from the active repository until the user applies them, while direct mode MUST write changes to the active repository as the run proceeds and clearly label them as live. Staged-mode candidates MUST be Git-backed — held in the repository's Git index or a dedicated temporary worktree/branch — so that accept/reject/recover map to native Git primitives (`git checkout`/`git restore`/hunk-level `git apply --reject`), recovery survives a backend restart (FR-033), and run-attributed changes are naturally distinguished from the user's unrelated uncommitted work. No out-of-tree scratch store or overlay filesystem is introduced (Constitution VIII).
- **FR-017**: The IDE MUST provide safe recovery controls for a failed, cancelled, or unwanted run, and MUST warn when recovery would affect unrelated user changes.
- **FR-018**: Each run attempt MUST retain its step, initiator, start and end times, status, effective instructions and scope, transcript, questions and answers, changed-file list, and validation result in local durable history that survives backend and Joey Agent restarts for 90 days, after which it MUST expire automatically. History MUST be stored as append-only JSONL — one file per feature at `~/.joey/speckit-ui/history/<feature-id>.jsonl`, each line a self-contained attempt record — so no new database dependency or schema version is introduced (Constitution VIII) and 90-day expiry reduces to a file-mtime sweep. The JSONL record schema is a versioned on-disk public format (Constitution VII): any breaking change to it requires a MAJOR bump and a documented migration.
- **FR-019**: Re-running a step MUST create a distinct attempt linked to prior attempts so users can compare outcomes without losing history.
- **FR-020**: The system MUST detect external file changes before overwriting an artifact (the content-hash conflict model from `specs/001`) and MUST offer reload, compare, and deliberate conflict-resolution choices.
- **FR-021**: When an upstream artifact changes, the IDE MUST mark affected downstream workflow results as stale, explain the dependency, and preserve the existing downstream artifacts until the user regenerates or edits them.
- **FR-022**: Workflow readiness MUST be derived from current artifact state, unresolved decisions, prerequisite completion, validation results, and active runs rather than from a manually assigned status alone.
- **FR-023**: The IDE MUST allow a user to navigate from a workflow finding, task, requirement reference, graph node, or run event to the relevant artifact and location.
- **FR-024**: Task controls MUST support inspecting and editing a task, viewing prerequisites and target files, running one eligible task, running an eligible selection, answering agent prompts, and reviewing resulting changes.
- **FR-025**: The IDE MUST support search and filtering across feature artifacts, requirement identifiers, task identifiers, workflow states, and run history.
- **FR-026**: The IDE MUST restore the user's last selected feature, open artifacts, active view, and supported pane layout without persisting unsaved sensitive content outside the repository.
- **FR-027**: Core authoring, workflow execution, review, approval, and recovery actions MUST be keyboard accessible and expose descriptive labels and visible focus.
- **FR-028**: The IDE MUST display disconnected, reconnecting, unavailable-agent, missing-credential, read-only, and failed-write states with a clear recovery action and MUST NOT present an operation as successful when its result is unknown.
- **FR-029**: Access to editing and agent execution MUST remain subject to the existing safety approval boundaries already in force for the terminal workflow; the IDE does not introduce a separate permission model.
- **FR-030**: The IDE MUST preserve compatibility with Spec-Kit skills and Joey Agent workflows operating on the same repository artifacts outside the UI.
- **FR-031**: The IDE MUST support feature histories large enough to include at least 500 tasks, 100 workflow attempts, and 1,000 changed files without hiding records or preventing completion of the primary workflow.
- **FR-032**: The IDE MUST provide an end-to-end feature progress summary that traces requirements through plan sections, tasks, implementation attempts, and convergence findings.
- **FR-033**: Active workflow attempts MUST record safe recovery checkpoints and, after a Joey Agent or backend restart, automatically resume from the latest valid checkpoint; if no valid checkpoint exists, the attempt MUST stop without replaying unconfirmed actions and clearly report the preserved effects and required recovery action.
- **FR-034**: Users MUST be able to apply instruction changes to one run or save them as a reusable override scoped to the current project; installed workflow/skill definitions MUST remain read-only, and users MUST be able to inspect the effective merged instructions and remove a project override to restore the installed definition.

### Key Entities

- **Feature Workspace**: The selected Spec-Kit feature and its repository context, open artifacts, current layout, readiness summary, and active runs.
- **Artifact**: A repository-backed feature document such as a specification, plan, task list, checklist, research note, data model, contract, or quickstart guide; includes path, type, current version (content hash), validity, dirty state, and dependency relationships.
- **Workflow Step**: A core or extension-provided stage in the Spec-Kit lifecycle; includes identity, order, purpose, prerequisites, expected inputs and outputs, availability, and current state.
- **Run Configuration**: The effective instructions, scope, validated target artifacts, server-advertised model/reasoning/iteration options, explicit change mode, option-catalog revision, and optional project-level override selected for one workflow attempt; distinguishes installed defaults, project overrides, and run-specific edits and becomes immutable when preparation succeeds.
- **Workflow Attempt**: One execution of a workflow step by the Joey Agent; includes status, timestamps, transcript, interactions, outputs, validation, safe recovery checkpoints, relationship to earlier attempts, and a local retention expiration 90 days after the attempt.
- **Agent Interaction**: A question, answer, approval request, approval decision, progress event, or tool activity associated with an active attempt.
- **Change Set**: The artifact and source-file changes attributed to an attempt, divided into reviewable files and hunks with acceptance state, dependency warnings, and any recovery action.
- **Validation Finding**: A located issue, warning, or informational result associated with an artifact, workflow prerequisite, analysis, or convergence check.
- **Dependency Link**: A traceable relationship between an upstream requirement or artifact and a downstream plan section, task, workflow output, or finding.
- **Workspace Preference**: Non-content user preferences such as selected feature, open artifact, active view, pane sizes, and filters.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: At least 90% of developers in usability testing can locate, edit, validate, and save an existing implementation plan without terminal use or assistance on their first attempt.
- **SC-002**: A developer can start any ready core Spec-Kit workflow step with reviewed instructions and scope in under 60 seconds from opening the feature.
- **SC-003**: 100% of supported workflow steps visibly identify whether they are ready, blocked, running, waiting for input, succeeded, failed, unavailable, or stale; no unknown state is displayed as success.
- **SC-004**: User questions, approval requests, progress updates, and cancellation outcomes appear in the active workspace within 2 seconds of being reported by the Joey Agent under normal local operating conditions.
- **SC-005**: 100% of externally changed artifacts are detected before an IDE save would overwrite them in concurrency validation tests.
- **SC-006**: 100% of completed workflow attempts provide a reviewable record containing effective inputs, final status, changed files, output, and validation outcome.
- **SC-007**: After an upstream specification or plan edit, all directly dependent generated outputs are marked stale within 3 seconds and include a usable next-step explanation.
- **SC-008**: At least 95% of participants can complete the full specify-to-converge lifecycle using only the IDE, excluding external account authorization that the IDE cannot perform.
- **SC-009**: In recovery testing, users can identify and recover from a failed or cancelled attempt without losing unrelated pre-existing repository changes in 100% of tested scenarios.
- **SC-010**: A feature with 500 tasks and 100 recorded attempts remains interactive enough for users to open an artifact, filter tasks, or inspect a run within 2 seconds for at least 95% of measured interactions.
- **SC-011**: All primary authoring, execution, review, approval, and recovery journeys can be completed using keyboard-only navigation and pass the project's accessibility acceptance review.
- **SC-012**: Artifacts edited through the IDE remain usable by the installed Spec-Kit skills and native Joey Agent workflows in 100% of compatibility tests.
- **SC-013**: At least 85% of pilot users rate the IDE's workflow clarity, change control, and overall professionalism as 4 or 5 on a 5-point scale.
- **SC-014**: Workflow attempts remain reviewable after backend and Joey Agent restarts throughout their 90-day retention period, and expired attempts are no longer available after that period in 100% of retention tests.
- **SC-015**: In restart testing, every active attempt with a valid safe checkpoint resumes without repeating already confirmed actions, and every attempt without one stops with a truthful recovery status and preserved-effects summary.
- **SC-016**: In change-review testing, users can accept or reject any individual hunk or file, and 100% of partial selections with known dependent changes display a warning before application.

## Assumptions

- This feature enhances the existing Spec-Kit Visual UI from `specs/001-speckit-visual-ui`; existing canvas, workspace, task-board, bootstrap, conflict-safe writer, and constitution-gauge capabilities are reused rather than duplicated.
- The target experience is a desktop-class development workspace in the local browser frontend; a mobile-optimized authoring experience is outside this feature's scope.
- The native Joey Agent (`joey-agent-core` turn loop, configured model, tools, safety approvals, feature context, interactive conversation behavior) is the authoritative execution experience, invoked out-of-process via the `joey` CLI (see FR-011 and the Joey-adaptation clarification) so the `joey-speckit-ui` crate never links against `joey-agent-core`.
- Core workflow steps are shown when supported by the active Spec-Kit installation and the installed Joey skills (`/speckit-*`). Installed extensions may contribute additional steps, but the IDE does not pretend an unavailable command exists.
- Repository files remain the source of truth (Constitution III). Run history and workspace preferences are retained as supporting metadata under the local `joey-speckit-ui` data store (append-only JSONL under `~/.joey/speckit-ui/history/`, per FR-018), but they do not replace or fork canonical feature artifacts.
- A user may have unrelated uncommitted repository changes before a workflow begins; the IDE must preserve and distinguish them from changes attributable to the run. In staged mode this distinction is provided for free by Git's index/worktree separation (FR-016).
- Automatic acceptance, committing, pushing, issue publication, or destructive recovery is outside the default flow and requires an explicit user action and any existing safety approval.
- Collaborative simultaneous text editing is outside the initial scope; safe external-change detection and deliberate conflict resolution are required (reusing the content-hash model from `specs/001`).
- The existing safety/approval controls remain in force; this feature does not introduce a separate user or permission model.
- The project's constitution file (`.specify/memory/constitution.md`, v1.1.0) is treated as the governance baseline for the Constitution Check gates surfaced in plan review and the analyze step.
