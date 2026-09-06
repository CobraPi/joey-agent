# Feature Specification: Native Spec-Kit Integration with Copilot Command Parity

**Feature Branch**: `026-please-fully-integrate`

**Created**: 2026-09-03

**Status**: Planned

**Input**: User description: "please fully integrate the spec-kit workflow naitively into this agent - I want it to integrate exactly like github copilot - I want all the / and . commands to be available make sure to include everything that is used in the github copilot agent - please also optimize both hypercode and neurocode to work well with the spec-kit workflow."

## Clarifications

### Session 2026-09-03

- Q: Where must the agent discover project-local command bodies (overrides/upstream-installed workflows)? → A: All upstream Copilot locations plus the spec-kit home: `.github/skills/speckit-*/SKILL.md`, `.github/agents/speckit.*.agent.md` (+ companion prompts), and `.specify/` — full discovery parity.
- Q: How should lifecycle awareness (active feature + current step) reach orchestration sessions? → A: Automatically at session start when a spec-kit project is detected (`.specify/` present), injected once into session context; disableable via existing config.
- Q: When a pre-flight script referenced by a command is missing from the repository's scaffold, what must happen? → A: Ship an internal fallback for known scaffolds (older spec-kit versions): substitute the equivalent internal check, warn that a fallback was used, keep going.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Full Lifecycle from Native Commands (Priority: P1)

A developer starts a feature in the agent's interactive session by running the native spec-kit commands — specify, clarify, plan, constitution, checklist, tasks, analyze, implement, converge, taskstoissues — without leaving the session, installing skills, or depending on any external agent binary. Every command of the upstream GitHub Copilot integration is available natively with the same names, pre-flight behavior, prompts, and artifact results.

**Why this priority**: The user's core request is full native availability of the complete upstream command surface; without it nothing else in the feature has a foundation.

**Independent Test**: Run each lifecycle command in a scratch repository initialized for spec-kit and confirm the expected artifact appears or updates on disk; each command is separately invokable and delivers its standalone value.

**Acceptance Scenarios**:

1. **Given** a repository with spec-kit scaffolding, **When** the user runs the native specify command with a feature description, **Then** a numbered feature directory is created containing a spec file, and the active-feature pointer is updated.
2. **Given** a feature with a completed spec, **When** the user runs the native plan command, **Then** a plan file is created in the feature directory from the resolved template.
3. **Given** a feature with a completed plan, **When** the user runs the native tasks command, **Then** a tasks file with checkbox items is created from the resolved template.
4. **Given** a feature with a tasks file containing unchecked items, **When** the user runs the native implement command, **Then** the session executes the tasks and updates the checkboxes in the tasks file.
5. **Given** any lifecycle step, **When** its pre-flight requirements are not met (for example planning without a spec), **Then** the command explains which prerequisite is missing instead of producing partial artifacts.

### User Story 2 - Dot-Form and Slash-Form Command Parity (Priority: P1)

A developer familiar with the upstream GitHub Copilot integration can drive the same lifecycle using either command form the upstream supports: the slash form (`/speckit-<name>`) and the dotted agent-addressing form (`speckit.<name>`). Both forms resolve to the same native command implementations, so muscle memory and documentation from upstream translate directly.

**Why this priority**: "All the / and . commands" is an explicit user requirement; parity of invocation forms is the heart of the request, on par with command coverage.

**Independent Test**: Invoke every command through both forms and confirm identical dispatch, help text, and results.

**Acceptance Scenarios**:

1. **Given** the interactive session, **When** the user types the slash form of any spec-kit command, **Then** it dispatches natively without falling back to an external agent.
2. **Given** the session, **When** the user types the dotted form `speckit.<name>`, **Then** the agent recognizes it as the corresponding native command and executes it identically to the slash form.
3. **Given** either form, **When** the command name is unknown, **Then** the session lists the available spec-kit commands with a suggestion.
4. **Given** command completion UI, **When** the user starts typing a spec-kit command in either form, **Then** matching commands are offered as completions.

### User Story 3 - Bundled Workflow Bodies and Project-Local Overrides (Priority: P2)

A team can rely on the workflow prompt bodies (the instruction text that guides each lifecycle step) shipping with the agent itself, versioned with its releases, while still overriding any of them per-project by placing replacement files in the repository's spec-kit directories. No installation step into the user's global skill directory is required to get started.

**Why this priority**: Removes the fragile external dependency on globally-installed skills, and keeps the project as the source of truth (a constitutional requirement), but is secondary to command availability itself.

**Independent Test**: Rename the global skills directory away and confirm all commands still work from the bundled bodies; then add a project-local override for one command and confirm it is preferred.

**Acceptance Scenarios**:

1. **Given** a fresh machine with no globally installed spec-kit skills, **When** the user runs any lifecycle command, **Then** the bundled workflow body is used and the command succeeds.
2. **Given** a project-local override file for a command body, **When** that command runs, **Then** the project-local file's content is used instead of the bundled body.
3. **Given** a repository initialized by upstream spec-kit tooling with command bodies installed under the `.github` layouts, **When** any lifecycle command runs, **Then** those bodies are discovered and used per the precedence rules (project-local over bundled).
4. **Given** bundled bodies, **When** a newer agent release ships updated bodies, **Then** upgrading the agent refreshes them without reinstallation steps.

### User Story 4 - Extension Hooks Execution (Priority: P2)

A team already using spec-kit extensions can keep their hooks working when they drive the lifecycle from inside the agent. Hook commands registered for lifecycle steps (before/after each of the ten commands) are discovered from the project's extension configuration file, and executable hooks run automatically at the right moment, with mandatory hooks blocking until finished and optional hooks surfaced with their invocation path.

**Why this priority**: Hooks are part of "everything used in the GitHub Copilot agent" and required for exact behavioral parity, but they only matter to projects that opt into extensions.

**Independent Test**: Register a mandatory before-hook that creates a marker file, run the corresponding command, and confirm the marker exists before the command proceeds.

**Acceptance Scenarios**:

1. **Given** a project extension configuration registering a mandatory before-specify hook, **When** the user runs the specify command, **Then** the hook executes to completion before specification begins.
2. **Given** an optional after-tasks hook, **When** the tasks command completes, **Then** the hook is surfaced with its command name and description rather than silently skipped.
3. **Given** an extension configuration that is invalid or unparseable, **When** any command runs, **Then** hook checking is skipped silently and the command proceeds normally.
4. **Given** a hook with a non-empty condition expression, **When** hooks are gathered, **Then** condition evaluation is left to the extension runtime rather than guessed by the command layer.
5. **Given** a disabled hook entry, **When** hooks are gathered, **Then** it is excluded.

### User Story 5 - Command Handoff Chaining (Priority: P2)

A developer moves through the lifecycle without re-typing context: when a step completes and upstream defines a handoff to the next step (for example specify handing off to plan or clarify), the session offers or executes the chained next step with the prior step's outputs carried forward, matching upstream handoff semantics including the send flag behavior.

**Why this priority**: Handoffs are an upstream mechanism the Copilot integration uses for cross-command chaining; supporting them is required for exact parity but builds on the core commands being native first.

**Independent Test**: Complete a specify step and confirm the plan/clarify handoff is offered exactly as upstream defines it.

**Acceptance Scenarios**:

1. **Given** a completed specify step whose workflow defines a handoff to plan, **When** the step finishes, **Then** the session offers the plan step with the handoff prompt.
2. **Given** a handoff marked as auto-send, **When** the step finishes, **Then** the next step is invoked automatically.
3. **Given** a handoff not marked auto-send, **When** the step finishes, **Then** the next step is offered but not started without user confirmation.

### User Story 6 - Spec-Kit-Aware Orchestration (Priority: P2)

A developer running the agent's multi-agent orchestration mode (its delegation/conductor layer) on a spec-kit project gets lifecycle-aware behavior: the conductor detects the active feature and lifecycle step from the on-disk artifacts, uses lifecycle-appropriate specialists during planning phases (read-only researchers during specify/clarify/plan), parallel implementation fan-out during implement, and a single final verification pass at acceptance — without the user manually briefing the conductor each session.

**Why this priority**: The user explicitly asked to optimize the orchestration layer for spec-kit, but it presumes the native command layer exists first.

**Independent Test**: Point a session at a feature whose tasks file has unchecked items and confirm the conductor proposes implementation fan-out; point it at a feature with no spec and confirm it proposes the specify step.

**Acceptance Scenarios**:

1. **Given** a project with an active feature and no spec file, **When** an orchestration session starts, **Then** the conductor identifies the specify step as current and briefs read-only planning specialists only.
2. **Given** a feature with a tasks file with unchecked items, **When** an orchestration session starts, **Then** the conductor fans out implementation specialists for unblocked tasks in parallel and never assigns two specialists the same file.
3. **Given** a feature with all tasks checked, **When** an orchestration session starts, **Then** the conductor runs exactly one final verification pass and reports results.
4. **Given** a spec-kit project is present, **When** an orchestration session starts, **Then** lifecycle context is detected and injected automatically once at session start, without any user command, and this behavior can be disabled via existing configuration.
5. **Given** the conductor's spec-kit doctrine prompt, **When** any spec-kit step is active, **Then** read-only specialists are dispatched during specify/clarify/plan steps and implementors only during implement.

### User Story 7 - Spec-Kit-Aware Code Intelligence (Priority: P3)

A developer working on a spec-kit feature gets code intelligence (the agent's code-context and verification engine) that is aware of the feature's scope: context enrichment prioritizes files listed in the feature's plan, task list, and research notes; verification planning aligns with the acceptance criteria in the spec; and indexing stays within the feature's read/write sets to keep it fast.

**Why this priority**: The user explicitly asked to optimize the code-intelligence layer for spec-kit, but it is an enhancement over correctly scoped, fast context delivery rather than a prerequisite for anything else.

**Independent Test**: With a feature active, request a change touching a file in the feature's task list and confirm the injected context includes that file's entities and references to the feature's artifacts.

**Acceptance Scenarios**:

1. **Given** an active feature with a plan listing relevant modules, **When** a code question is asked, **Then** the injected code context prioritizes entities from those modules.
2. **Given** an active feature, **When** indexing runs, **Then** it prioritizes the feature's listed files over an unscoped full-repository sweep.
3. **Given** a spec with acceptance criteria, **When** verification planning runs, **Then** the plan references the spec's acceptance criteria.
4. **Given** no active feature, **When** any code-intelligence capability runs, **Then** behavior is unchanged from today (no regression).

### Edge Cases

- When the repository has spec-kit scaffolding from an older spec-kit version whose scripts differ (for example a missing template-resolution script), the agent substitutes an internal fallback equivalent for known scaffolds, warns that a fallback was used, and proceeds. (Resolved by clarification.)
- How does the system handle a project extension configuration that is invalid YAML? (Hook discovery is skipped silently, per upstream semantics.)
- What happens when two command forms (slash and dotted) collide with an existing user command name? (Native names win or are disambiguated with a clear error; never silently shadow user commands.)
- What happens when a tasks file contains parallelizable task markers but the tasks share files? (They are treated as sequential; the collision is surfaced.)
- How does the system handle a feature directory renamed or deleted mid-session while the active-feature pointer still references it? (Stale pointer is detected; user is asked to reselect or re-run specify.)
- What happens when a pre-flight script present in the scaffold fails or cannot run on the user's platform? (A clear error names the script and platform variant expected; no partial artifacts are written.)
- What happens when a hook command itself fails? (Failure is reported with the hook name; mandatory hooks stop the step, optional hooks are logged and skipped.)

---

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The agent MUST provide native implementations of all ten upstream spec-kit lifecycle commands — specify, clarify, plan, constitution, checklist, tasks, analyze, implement, converge, taskstoissues — invokable from its interactive sessions without external agent binaries.
- **FR-002**: Each lifecycle command MUST run the same pre-flight checks and scripts as the upstream GitHub Copilot integration (per-command script name and flags) before executing its workflow body. When a referenced pre-flight script is missing from the repository's scaffold, the agent MUST substitute an internal fallback equivalent for known older scaffolds and warn that a fallback was used before proceeding.
- **FR-003**: Every lifecycle command MUST be invokable in both the slash form `/speckit-<name>` and the dotted agent-addressing form `speckit.<name>`, resolving to identical native behavior.
- **FR-004**: The workflow instruction bodies for all lifecycle commands MUST ship bundled with the agent (release-versioned, no global skill installation required), with per-project override files taking precedence when present. Project-local bodies MUST be discovered from the upstream Copilot layouts — `.github/skills/speckit-<name>/SKILL.md` and `.github/agents/speckit.<name>.agent.md` (with companion prompt files) — as well as the `.specify/` directory.
- **FR-004a**: The bundled bodies and command metadata MUST include the upstream frontmatter semantics — handoffs (label, agent, prompt, send flag), scripts (per-platform variants), and tools references — parsed and honored at dispatch time.
- **FR-005**: The agent MUST discover and execute extension hooks defined in the project's extension configuration for all twenty hook points (before/after each of the ten commands), honoring enabled flags, optional/mandatory semantics, and silent-skip on invalid configuration.
- **FR-006**: The agent MUST support upstream handoff semantics: when a step completes and its workflow defines a handoff, the next step is offered (or auto-invoked when the send flag is set) with prior outputs carried forward.
- **FR-007**: The agent MUST expose lifecycle status and help surfaces (current active feature, current step, per-command guidance) natively in the session, equivalent to or exceeding upstream status/help capabilities.
- **FR-008**: The orchestration layer (conductor/delegation) MUST detect the active spec-kit feature and current lifecycle step from on-disk artifacts automatically at session start when a spec-kit project is present, inject that lifecycle context into the session once (preserving provider prompt-prefix cache friendliness), and adapt its dispatch pattern accordingly (read-only specialists during specify/clarify/plan; parallel implementors during implement; single final verification at acceptance). This behavior MUST be disableable via existing configuration.
- **FR-009**: The orchestration layer MUST incorporate the feature's task dependencies and per-task write sets when fanning out implementation work, never assigning two concurrent specialists the same file.
- **FR-010**: The code-intelligence layer MUST, when a spec-kit feature is active, prioritize the feature's plan/tasks/research file lists when selecting code context and indexing targets, and align verification plans with the spec's acceptance criteria.
- **FR-011**: All spec-kit integration behavior MUST be strictly additive: existing command names, configuration keys, on-disk formats, and behaviors not related to spec-kit MUST remain unchanged (no regressions, per repository governance).
- **FR-012**: All new spec-kit integration code MUST ship with tests alongside implementation, including contract/round-trip tests for any parsing or serialization of spec-kit artifact files, and regression tests for every touched public surface.
- **FR-013**: Users MUST be able to disable any or all of the new behavior via existing configuration mechanisms, restoring pre-feature behavior exactly.

### Key Entities

- **Spec-Kit Command**: A native lifecycle command: name (slash and dotted forms), workflow body (bundled or project-overridden), pre-flight script binding, handoff definitions, tools references.
- **Extension Hook**: A project-defined command bound to a lifecycle hook point: extension name, command name, description, prompt, optional/mandatory classification, enabled flag, condition expression (evaluated by the extension runtime, not the command layer).
- **Handoff**: The definition of a chained next step: label, target command, handoff prompt, send flag.
- **Lifecycle State**: the active feature pointer and derived current step (absent spec → specify; spec only → clarify/plan; plan only → tasks; tasks with unchecked items → implement; all checked → acceptance).
- **Workflow Body**: The instruction text for a lifecycle step; bundled copy plus optional project-local override; precedence: project-local > bundled.
- **Feature Scope**: the set of files a feature is expected to read and write, derived from its plan and task list; used by orchestration fan-out and code-intelligence prioritization.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: 100% of the ten upstream lifecycle commands are available natively in both slash and dotted forms, verified by a parity test that enumerates the upstream command set and checks each form dispatches natively.
- **SC-002**: 100% of the twenty upstream extension hook points are supported, verified by a parity test enumerating hook points against upstream's definition.
- **SC-003**: Frequently executed lifecycle transitions complete within 2 seconds of user invocation (pre-flight + body load + turn submission), excluding model inference time.
- **SC-004**: Users can complete a full specify → converge cycle on a fresh repository in a single interactive session using only native commands, verified by end-to-end walkthrough.
- **SC-004a**: 90% of users familiar with the upstream Copilot integration can drive the lifecycle here without consulting additional documentation, measured by walkthrough observation or feedback.
- **SC-005**: With a spec-kit feature active, code-context injection includes the feature's scoped files in at least 95% of sampled queries touching those files.
- **SC-005a**: Feature-scoped indexing completes within 3 seconds on repositories where an unscoped sweep took 10 or more seconds.
- **SC-006**: All pre-existing tests continue to pass (no regressions), and every new public surface ships with regression coverage.
- **SC-006a**: The complete native command surface (names, forms, help text) is covered by tests asserting prior behavior is preserved where behavior predates this feature.

## Assumptions

- The upstream parity target is the current upstream spec-kit Copilot integration command set (ten commands, two invocation layouts, twenty hook points, handoffs frontmatter, per-command pre-flight scripts), as audited from upstream sources at the time of writing; where upstream evolves, a configuration-controlled refresh path exists rather than hardcoding to a frozen snapshot.
- Existing scaffolding scripts in repositories (create-new-feature, setup-plan, setup-tasks, check-prerequisites) remain the source of truth for pre-flight behavior; the agent reuses them rather than reimplementing their logic.
