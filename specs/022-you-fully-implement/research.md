# Research — HyperCode Agent Teams (Phase 0)

Reference: Claude Code "Agent Teams" documentation (v2.1.178) — https://code.claude.com/docs/en/agent-teams. Parity claims trace to that document; repo facts trace to symbols cited in plan.md.

## D1: Team lead architecture

- **Decision**: The HyperCode orchestrator spawns a dedicated lead child (SubagentRole::Orchestrator) whose model is configurable via `hypercode.team.lead_model`; an empty value inherits the orchestrator's effective model through the existing chain (TaskSpec.model > DelegationRequest.model > `delegation.default_model` > parent AgentConfig.model — subagent.rs resolve_model).
- **Rationale**: Mirrors the reference model (a lead coordinating independent teammates); a child lead reuses dispatch_single/budgets/notices unchanged; the user explicitly requested a configurable lead model, which requires the lead to be a separately specifiable agent (spec FR-019).
- **Alternatives considered**: (a) orchestrator session acts as lead directly — rejected: lead model not independently configurable and team coordination would consume the orchestrator's context; (b) lead as a separate OS process — rejected: complexity, no shared in-process registry.

## D2: Team state storage

- **Decision**: File-backed state under `~/.joey/teams/<team>/` (config.json, tasks.json, inboxes/<member>.json), written synchronously on every change; an in-process registry Mutex is the claiming authority; multi-process teams are out of scope.
- **Rationale**: Constitution III (filesystem is the source of truth); FR-012 requires task-list persistence across session restarts; the reference persists task lists for resumed sessions with retention (`cleanupPeriodDays` → `hypercode.team.cleanup_days`, default 7).
- **Alternatives considered**: (a) SQLite via the existing session store — rejected: heavyweight for small JSON documents and couples teams to session-schema versioning; (b) in-memory only (joey-omo team.rs today) — rejected: no resumption, violates FR-012; (c) per-task O_EXCL lock files (the reference notes "Task claiming uses file locking") — deferred: a single-process Mutex provides the same atomicity guarantee here; revisit only if teams ever become multi-process.

## D3: Teammate identity, spawning, and messaging

- **Decision**: `delegate_task` gains optional `team` (team name) and `name` (member name) parameters; the first such spawn (the lead) lazily creates the team record; teammates are Leaf children that KEEP the new team tools but still LOSE `delegate_task` under the existing retain rule (subagent.rs: role==Leaf removes delegate_task) — enforcing "no nested teams, no background subagents from teammates" (FR-010). New tools in a `team` toolset: team_status, team_message, team_tasks (action-based).
- **Rationale**: The reference spawns teammates "via the Agent tool with a name" — delegate_task is its joey analog; TeamCreate/TeamDelete tools were removed upstream before v2.1.178, so no dedicated create tool is added; direct teammate-to-teammate messaging (FR-002) requires shared in-process state, which only joey-orchestration can host given the DAG (joey-omo → joey-orchestration).
- **Alternatives considered**: (a) a separate spawn_team tool — rejected (upstream removed TeamCreate; spec FR-009 wants natural-language start); (b) lead relays all messages — rejected: violates FR-002; (c) execution inside joey-omo/team.rs — rejected: wrong DAG direction, no SubagentManager access.

## D4: Configuration namespace

- **Decision**: `hypercode.team.*` keys: enabled (false), lead_model (""), max_members (8), max_parallel_members (4), message_limit (10), poll_interval_ms (500), cleanup_days (7); defaults mirror joey-omo's TeamModeConfig wherever names overlap. Registration threads the enabled flag the same way register_orchestration_with_resolver_and_allocator threads its resolver today.
- **Rationale**: The feature is HyperCode-facing, so keys sit beside the existing hypercode.explorer/hypercode.implementor tables; joey-omo's TeamModeConfig was never wired to any config parser (verified: no team keys anywhere in config code), so there is no back-compat surface to preserve.
- **Alternatives considered**: (a) `omo.team.*` — rejected: execution lives in joey-orchestration/joey-cli and omo's config surface is agent/category oriented; (b) top-level `team.*` — rejected: inconsistent with established namespacing.

## D5: Mode selection and recording

- **Decision**: Routing is orchestrator guidance, not a classifier: the HyperCode orchestrator overlay gains the documented when-to-use guidance (teams for independent, parallelizable work — research alongside implementation, debugging with competing hypotheses, cross-layer coordination; subagents or a single session for sequential work, same-file edits, dependency-heavy steps; ambiguity → the cheaper mode) and MUST state the chosen mode plus rationale; joey-cli's HypercodeReport gains `mode_decisions: Vec<String>` recorded per run.
- **Rationale**: In the reference, selection is the lead session's judgment guided by documentation; a keyword classifier would be brittle and unauditable against the 90% bar in SC-001; recording satisfies FR-016.
- **Alternatives considered**: (a) heuristic/keyword router — rejected: brittle, opaque failures; (b) always-team-when-enabled — rejected: violates FR-007/FR-008.

## D6: Notifications

- **Decision**: Reuse the background completion-notice path (format_completion_notice → push_background_completion, cap 64 drop-oldest) for teammate finish/fail/stop; mailbox messages are pulled by teammates each iteration via the team tools (directive-instructed), and the lead receives idle notifications as completion notices.
- **Rationale**: The existing machinery already delivers notices at the next turn drain, meeting SC-003's 30-second bar without a new event channel.
- **Alternatives considered**: A new push event channel into child loops — rejected: invasive and duplicates tap.rs/notices.

## D7: Relationship to joey-omo team.rs

- **Decision**: Leave crates/joey-omo/src/team.rs exactly as-is (in-memory primitives, TmuxVisualizer, eligibility tests all stay green); the new execution module supersedes its in-memory mailbox/task list for this feature; tmux visualization remains out of scope per the spec's assumptions.
- **Rationale**: Constitution VII forbids removing/refactoring public surfaces in a feature pass; omo's TeamTask lacks a dependencies field and its Arc<Mutex> primitives do not model persistence, so extension would be a rewrite anyway.
- **Alternatives considered**: Extending omo's primitives in place — rejected: wrong DAG direction for SubagentManager access and it would break the semantics of the 15 existing inline tests.
