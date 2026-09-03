# Research: Enterprise Orchestration Runtime

Phase 0 findings. Each entry: Decision / Rationale / Alternatives considered. Code references were verified against the working tree on 2026-09-02.

## R1. Feature-flag keys and config surface

- **Decision**: `hypercode.execution_graph.enabled` (bool, default false) and `neurocode.enterprise_context.enabled` (bool, default false), plus `hypercode.execution_graph.max_concurrent_workers` (int, default 16) and `hypercode.execution_graph.max_repair_attempts` (int, default 3). All read via the existing dotted-path accessors (`Config::get_bool`/`get_i64`, joey-core config.rs:240-258); defaults added to the default config text block (config.rs:113-127) which bumps `_config_version` additively (34).
- **Rationale**: Mirrors the names proposed in the feature request; matches the existing `hypercode.team.enabled` / `neurocode.auto_index.enabled` boolean-flag pattern; config-only rollout means no CLI-surface change (Principle VII).
- **Alternatives**: New CLI flags (rejected: new public surface, harder to default safely); one shared flag (rejected: the two planes must be enableable independently for parity testing).

## R2. Analysis plane shape — additive trait, not engine rewrite

- **Decision**: New `EnterpriseTaskAnalyzer` trait (`analyze`, `context_for`, `verification_for`, `record_outcome`) + `TaskAnalysis` struct in new joey-neurocode modules (`analysis.rs`, `policy/{resolver,sources}.rs`, `risk.rs`, `verification_plan.rs`, `memory/outcomes.rs`). `DefaultEngine` implements it. `NeuroCodeEngine` (engine.rs:38-105) is untouched.
- **Rationale**: Spec FR-005 requires additive introduction; the existing trait has default-method consumers that must not break.
- **Alternatives**: Extending `NeuroCodeEngine` with default methods (rejected: couples enterprise surface to every engine implementor and risks trait-object drift).

## R3. Classifier signals — completing, not replacing

- **Decision**: Extend `classify` (classifier.rs:155) to score: dependency fan-in/fan-out via `store.dependents_count` + `traverse_edges`/`traverse_to` (graph store.rs:583, mod.rs:92-107); affected-module count via `nodes_by_source_path` grouping; public-API exposure via node signature/kind; ownership boundaries via source-path prefixes; prior anti-pattern hits via `anti_patterns_for_artifacts` (store.rs:647). `SignalKind::GraphHub` (classifier.rs:61-69, currently never produced) becomes scored. Keyword and scope fan-out signals retained.
- **Rationale**: FR-004 lists exactly these dimensions; all data is already in the index — no new parsing.
- **Alternatives**: New parallel classifier (rejected: two sources of truth for tier routing).

## R4. Policy resolution — combine, never first-found-wins

- **Decision**: `policy/resolver.rs` merges layers in explicit precedence order: organization policy → repository instructions → module/directory instructions → matching scoped rules (Copilot `applyTo` globs) → task-specific contract. Sources parsed in `policy/sources.rs`: JOEY.md, AGENTS.md, CLAUDE.md, `.cursorrules`, `.github/copilot-instructions.md` (+ nested `applyTo`-scoped files). Globs apply only to task paths they match. Conflicting layers are surfaced in the binding set, not silently dropped. The legacy first-found-wins chain in `build_context_files_prompt` (joey-agent-core/src/prompt.rs:409-426) remains untouched for the flag-off path; the flag-on path replaces it by consumption of `TaskAnalysis.effective_policies`.
- **Rationale**: FR-002/FR-003; the shallow behavior being replaced is exactly prompt.rs:412-421.
- **Alternatives**: Modifying prompt.rs in place (rejected: breaks flag-off parity and touches ported prompt assembly).

## R5. TaskGraph ownership and the crate DAG

- **Decision**: `TaskGraph`, scheduler, workspace, evaluator, joiner and evidence live in joey-orchestration (new modules task_graph.rs, scheduler.rs, workspace.rs, evaluator.rs, joiner.rs, evidence.rs — none exist today). joey-orchestration defines a narrow `VerificationGate` trait (run commands, parse structured errors, report degraded vs failed) and consumes it; joey-cli implements the adapter over neurocode's `VerifyLoop::run_with_fixes` (verify/mod.rs:139-143). joey-orchestration therefore gains NO dependency on joey-neurocode.
- **Rationale**: Principle I strict DAG (orchestration currently sits above agent-core; neurocode is a sibling consumed only by joey-cli); keeps the scheduler LLM-free and unit-testable with a fake gate.
- **Alternatives**: joey-orchestration depending on joey-neurocode (rejected: new cross-branch edge, violates DAG); runtime in joey-cli (rejected: no library consumers, untestable in isolation).

## R6. Legacy workstream compatibility

- **Decision**: `parse_workstreams` (hypercode.rs:784) and `Workstream` (hypercode.rs:368-373) are kept unchanged. Its output is converted immediately by a new converter into `TaskNode`s (objective = focus, empty read/write sets validated conservatively: empty write sets are treated as "undeclared" and routed SingleWorker per the router, never concurrently dispatched) and validated into a `TaskGraph`. A strict JSON planner format (see contracts/planner-json-format.md) is accepted for new runs.
- **Rationale**: FR-007/FR-008; conservative treatment of undeclared write sets preserves safety without rejecting legacy plans.
- **Alternatives**: Rejecting legacy output when the flag is on (rejected: FR-007 forbids); inferring write sets from prose (rejected: speculation, unsafe).

## R7. Execution-mode routing

- **Decision**: New graph-based router in hypercode.rs alongside the existing `route_mode` (hypercode.rs:566-572) and `ModeRoute {Subagent, Team}` (561-564), which remain for flag-off parity. New variants `SingleWorker`, `DagSubagents`, `ParallelSubagents` join the enum (additive); the router implements: write-set overlap → SingleWorker; strict dependency depth > 2 → DagSubagents; independent components ≥ 2 with cross-component coordination → Team; independent components ≥ 2 → ParallelSubagents; else SingleWorker. Team mode seeds `team_tasks` from the validated graph.
- **Rationale**: FR-023/FR-024; replaces the `workstream_count >= 2` heuristic (hypercode.rs:566) that treated workstream count as proof of independence.
- **Alternatives**: Mutating route_mode in place (rejected: flag-off parity).

## R8. Scheduler loop and run persistence

- **Decision**: Deterministic loop in scheduler.rs: ready-wave → conflict partition (overlapping write sets sequenced) → bounded parallel dispatch (semaphore, default 16 — reuses the dispatch_requests parallel-wave pattern, manager.rs:1325) → await evaluation → transition → replan only when blocked. Run state persisted per research contract run-state-format.md under `~/.joey/hypercode/projects/<project-hash>/runs/<run-id>/` (built from `constants::joey_home()`); decisions appended to `decisions.jsonl` per transition. Resume refuses a changed baseline revision (FR-030).
- **Rationale**: FR-011..FR-014; wave/semaphore pattern already proven in `dispatch_requests` with stable ordering (determinism).
- **Alternatives**: State in SQLite (rejected: run artifacts are files-by-design for auditability; session DB is a Hermes-compatible format we must not extend casually).

## R9. Worktree isolation and integration

- **Decision**: workspace.rs creates isolated writers via `git worktree add --detach` under `worktree/<task-id>` (precedent: joey-speckit-ui/src/staging_impl.rs:48-69), falling back to a full copy where worktrees are unavailable. Writers produce `ChangeBundle`s (baseline sha, declared/actual write sets, `patches/<task-id>.patch` from `git diff`, evidence). joiner.rs verifies actual vs declared write sets, applies patches with three-way conflict detection (`git apply --3way` with `--check` dry-run first; conflicts surfaced, never partially applied), refreshes NeuroCode incrementally after each integration, and creates no user-visible commits unless requested.
- **Rationale**: FR-015..FR-018; worktree add/remove is already exercised in-repo; system-git approach avoids new dependencies.
- **Alternatives**: libgit2 binding (rejected: new dependency, Principle V); copying without git (rejected: loses baseline sha and three-way bases).

## R10. Evaluator, repair and escalation ladder

- **Decision**: evaluator.rs awaits each worker's gate inline (detached `run_detached` stays available for informational background only — never a completion gate). Failures produce `DefectBundle` (failed commands from `VerifyResult`s, policy violations, reviewer findings, changed paths) routed to a repair worker; the real repair callback replaces today's no-op `|_| false` (verify/mod.rs:224, 262) via the joey-cli adapter. Escalation follows the exhaustion ladder: escalate when a tier's repair attempts are exhausted (`hypercode.execution_graph.max_repair_attempts`, default 3), fail only when the highest tier is exhausted. A tier-ranking helper is added additively in tier_resolver.rs (`ComplexityTier` has no ordering today).
- **Rationale**: FR-019..FR-022 + clarifications Q4/Q5; degraded gates (command unavailable) are recorded degraded, never complete the task, never trigger code-defect repair, and clear only on runnable command or explicit user acknowledgment.
- **Alternatives**: Keeping detached verification as the gate (rejected: FR-019); cost-model escalation (rejected in clarification Q5 for determinism).

## R11. Structured outcome memory

- **Decision**: `OutcomeMemory` records (spec fields incl. task signature, revision, artifact ids, policy ids, failure/resolution signatures, evidence ids, confidence, hit count, last confirmed at) stored in one additive neurocode SQLite table (`CREATE TABLE IF NOT EXISTS outcome_memory`; `NEUROCODE_SCHEMA_VERSION` stays 3 — additive table, existing DBs open unchanged). Records are written only from verified outcomes. Consultation compares stored artifact signatures/hashes against current ones: changed ⇒ down-rank/expire. This replaces OMO free-text wisdom scraping (`extract_wisdom`/`accumulate_wisdom`, joey-omo orchestrator.rs:201/273) as guidance source when the flag is on; the OMO notepad remains as persona notes, never as execution guidance.
- **Rationale**: FR-025/FR-026 and story 7; provenance-aware Reflexion.
- **Alternatives**: JSON file store (rejected: needs indexed lookup by artifact/signature; SQLite already embedded); keep wisdom scraping (rejected: FR/SC-008).

## R12. Risk-triggered specialist review

- **Decision**: Risk dimensions in risk.rs (public-API exposure, security-sensitive paths, concurrency, fan-out breadth, ownership crossing) map to triggering `oracle`/`momus` review personas (already hardcoded in joey-omo registry.rs:206-328; team.rs:220 hard-rejects them as team members — they are invoked as reviewers, not members). Findings enter the DefectBundle loop. No reviewer configured ⇒ recorded notice, run proceeds (edge case per spec).
- **Rationale**: FR-022 + story 8; reuses existing personas; no new persona mechanism.
- **Alternatives**: New reviewer persona type (rejected: duplicates oracle/momus).

## R13. Events and joey-agent-core

- **Decision**: Zero joey-agent-core changes. Run observability uses the existing orchestration recorder/tap (`set_recorder_tap`, manager.rs:563) plus decisions.jsonl and evidence records; no new AgentEvent variants needed for v1 (existing `DelegationBatchComplete` covers wave completion reporting).
- **Rationale**: Spec layer table says agent-core gets "narrow traits and additive events; no enterprise orchestration logic" — v1 needs neither; smallest possible blast radius.
- **Alternatives**: New AgentEvent variants (rejected for v1: enum extension ripples across every consumer; taps suffice).
