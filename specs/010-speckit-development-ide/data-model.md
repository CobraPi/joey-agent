# Data Model: Spec-Kit Development IDE

**Feature**: `010-speckit-development-ide` | **Date**: 2026-08-03

This document defines the entities the IDE adds *on top of* the
`specs/001-speckit-visual-ui` model (`Feature`, `Specification`,
`UserStory`, `Requirement`, `ClarificationEntry`, `Plan`,
`ConstitutionGate`, `Task`, `AnalysisFinding` — already implemented in
`crates/joey-speckit-ui/src/model.rs`). Those existing entities are
**unchanged**; everything below is strictly additive (Constitution VII).

### Terminology: "run" vs. "attempt"

The two terms appear throughout this feature's artifacts. They are
**distinct** and used consistently:

- **Run** — the *act* of executing a workflow step (the live process). This
  is the `specs/001` term, preserved for backward compatibility: `run_id`,
  `AppState.runs`, `WS /api/runs/{run_id}`, `POST .../tasks/{taskId}/execute`.
  The `specs/001` `run` concept is unchanged.
- **Attempt** — one *persisted execution record* of a run (the auditable
  artifact). Introduced by this feature: `attempt_id`, `WorkflowAttempt`,
  `WS /api/attempts/{id}/stream`, JSONL history records. A re-run of the
  same step creates a **new attempt** linked to the prior via
  `prior_attempt_id` (FR-019).

Every attempt corresponds to exactly one run, but a run may be re-attempted
(multiple attempts per step over time). Code that touches the live process
(`runner.rs`, `AppState.runs`) uses "run"; code that touches persisted
history (`history.rs`, `recovery.rs`, JSONL records) uses "attempt".

On-disk persistence policy (Constitution III):
- Entities whose canonical home is a file (`Artifact`, `WorkflowStep`
  definition, `DependencyLink`) are **derived** from disk on every load,
  never stored as a divergent copy.
- Run/session entities (`WorkflowAttempt`, `AgentInteraction`, `ChangeSet`,
  `WorkspacePreference`) are *supporting metadata* persisted under
  `~/.joey/speckit-ui/` (JSONL / JSON), never a fork of canonical content.

---

## Entity overview

```
FeatureWorkspace 1───* Artifact
                 1───* WorkflowStep (catalog, derived)
                 1───* WorkflowAttempt (history, JSONL)
                 1───1 WorkspacePreference
WorkflowStep    1───* WorkflowAttempt
WorkflowAttempt 1───* AgentInteraction
                 1───1 RunConfiguration (immutable after prepare)
                 1───1 ChangeSet (on/after completion)
                 1───* ValidationFinding
ChangeSet       1───* ChangedFile 1───* Hunk
DependencyLink: Artifact ──> Artifact  (stale propagation graph)
```

---

## 1. FeatureWorkspace

The selected feature + repository context + live session state.

| Field | Type | Notes |
|-------|------|-------|
| `feature_id` | string | e.g. `010-speckit-development-ide` |
| `repo_root` | path | absolute, the git repo root |
| `branch` | string? | current branch (derived from git) |
| `artifacts` | Artifact[] | discovered (FR-003) |
| `workflow` | WorkflowStep[] | derived catalog (FR-008) |
| `active_attempts` | WorkflowAttempt[] | in-flight runs (FR-015 conflict guard) |
| `preference` | WorkspacePreference | layout/last-opened (FR-026) |
| `read_only` | bool | derived: repo is read-only or missing creds (FR-028) |

---

## 2. Artifact

A repository-backed feature document. Extends the implicit notion in
`specs/001` (which only modelled spec/plan/tasks) to **every** authorable
artifact (FR-003/FR-004).

| Field | Type | Validation / Notes |
|-------|------|--------------------|
| `path` | string (repo-relative) | e.g. `specs/010-.../plan.md`, `.specify/memory/constitution.md` |
| `kind` | enum | `spec` \| `plan` \| `tasks` \| `checklist` \| `research` \| `data_model` \| `contract` \| `quickstart` \| `constitution` \| `supporting` |
| `exists` | bool | false → "not yet created" empty state (Edge Cases) |
| `content_hash` | string? | sha256 of current bytes (reuse `conflict::content_hash`) |
| `dirty` | bool | unsaved in-memory edits (FR-005) |
| `save_state` | enum | `clean` \| `dirty` \| `saving` \| `saved` \| `invalid` \| `externally_changed` \| `read_only` (FR-005) |
| `validity` | ValidationFinding[] | required-structure + unresolved-marker checks (FR-007) |
| `workflow_phase` | enum | which lifecycle phase owns it (for explorer grouping) |
| `stale` | bool + `stale_reason`? | set by dependency graph when an upstream changes (FR-021) |

**Validation rules** (FR-007): each `kind` has a tolerant structural check
(mirroring the `Status::Unparsed`-on-malformed pattern from `model.rs`):
- `spec`: has Title, ≥1 User Story, ≥1 Requirement, Success Criteria.
- `plan`: has Summary, Technical Context, Constitution Check.
- `tasks`: ≥0 checkbox lines; cycle-free dependency graph (Edge Cases).
- `checklist`: expected section headings present; no unresolved `[ ]` items
  block the dependent step.
- `constitution`: version line parses; ≥1 principle.
Unresolved workflow markers (`NEEDS CLARIFICATION`, `TBD`, `[REMOVE IF
UNUSED]`) are flagged with an actionable location.

---

## 3. WorkflowStep

A core or extension lifecycle stage. **Derived** from the active Spec-Kit
installation + installed `/speckit-*` skills (FR-008); never hand-set.

| Field | Type | Notes |
|-------|------|-------|
| `id` | enum/string | `constitution` \| `specify` \| `clarify` \| `plan` \| `checklist` \| `tasks` \| `analyze` \| `implement` \| `converge` \| `task_to_issue` \| `<extension>` |
| `order` | int | lifecycle position |
| `purpose` | string | human-readable (FR-009) |
| `inputs` | Artifact[] | required input artifacts (FR-009) |
| `outputs` | Artifact[] | expected output artifacts (FR-009) |
| `prerequisites` | WorkflowStep[] | steps that must be succeeded first |
| `available` | bool | false when the skill/script is absent → labelled unavailable, not simulated (Edge Cases; FR-008) |
| `state` | StepState | derived (FR-022) — see state model below |
| `blocking_reason` | string? | when `blocked`: the unmet prerequisite / decision / validation / conflicting run (FR-009) |
| `latest_attempt_id` | string? | most recent relevant attempt |
| `installed_definition_ref` | string | read-only reference to the installed skill (FR-034) |

**StepState** (FR-008/022): `ready` \| `blocked` \| `running` \|
`attention_needed` \| `succeeded` \| `failed` \| `stale` \| `unavailable`.

> `attention_needed` is a **presentation aggregate**, not a persisted
> status (spec US2 note). Derived when the underlying state is
> `awaiting_input` | `awaiting_approval` | `recoverable_failure` |
> `conflicted` | `recovery_failed`; the underlying state + remediation is
> always exposed alongside.

**State derivation** (FR-022): `state = f(current artifact validity,
prerequisite completion, unresolved decisions, validation results, active
runs)`. Pure function of disk + active runs; recomputed on load and on
watched change.

---

## 4. RunConfiguration

The effective inputs for one attempt. **Becomes immutable when
preparation succeeds** (spec Key Entities).

| Field | Type | Notes |
|-------|------|-------|
| `step_id` | string | which step |
| `effective_instructions` | string | installed defaults ⊕ project override ⊕ run-specific edit (FR-010/034) |
| `scope` | Scope | `targets: Artifact[]`, optional `task_ids: string[]` (for implement) |
| `options` | AgentOptions? | server-advertised only: `model`, `reasoning_effort`, `max_iterations` (FR-010) |
| `option_catalog_rev` | string | content-hash of the catalog advertised by `/api/options`; backend rejects stale (FR-010) |
| `change_mode` | enum | `staged` \| `direct` — **mandatory explicit selection every run** (FR-010) |
| `override_id` | string? | project override applied, if any (FR-034) |
| `prepared_at` | timestamp | immutability point |

**Validation** (FR-010): backend validates every `targets` entry against
the active workflow, every option against the configured provider catalog
+ safety bounds, and rejects if `change_mode` is absent or
`option_catalog_rev` is stale.

---

## 5. WorkflowAttempt  *(JSONL-persisted — see contracts/history-jsonl.md)*

One execution of a step by the Joey Agent.

| Field | Type | Notes |
|-------|------|-------|
| `attempt_id` | string (uuid) | |
| `feature_id` | string | |
| `step_id` | string | |
| `initiator` | string | user/principal |
| `started_at` / `ended_at` | timestamp? | |
| `status` | AttemptStatus | see below |
| `run_config` | RunConfiguration | frozen snapshot (immutable after prepare) |
| `transcript` | TranscriptEntry[] | streamed progress/tool/output lines |
| `interactions` | AgentInteraction[] | questions/answers/approvals/decisions |
| `changes` | ChangeSet? | on/after completion (FR-016) |
| `validation` | ValidationFinding[] | post-run analysis result |
| `checkpoint` | Checkpoint? | latest safe recovery point (FR-033) |
| `prior_attempt_id` | string? | links re-runs for comparison (FR-019) |
| `expires_at` | timestamp | `started_at + 90d` (FR-018) |

**AttemptStatus**: `preparing` \| `running` \| `awaiting_input` \|
`awaiting_approval` \| `recoverable_failure` \| `conflicted` \|
`recovery_failed` \| `succeeded` \| `failed` \| `cancelled` \|
`recovery_needed` (presentation: `attention_needed`).

**State transitions**:
```
preparing ──prepare ok──> running ──stream done──> succeeded
   │                         │ ┌──question──> awaiting_input ──answer──> running
   │                         │ ┌──approval──-> awaiting_approval─approve> running
   │                         ├──cancel──────> cancelled (truthful effects kept)
   │                         ├──fail(soft)──> recoverable_failure ──recover──> running
   │                         ├──conflict────> conflicted
   │                         └──restart──────> recovery_needed
   │                              ├──valid ckpt──> running (resume, no replay)
   │                              └──no ckpt─────> recovery_failed (preserve effects)
   └──prepare fail──> failed
```
(FR-014 cancellation preserves a truthful record; FR-033 restart resume.)

---

## 6. AgentInteraction

A question/answer, approval/decision, progress, or tool-activity event.

| Field | Type | Notes |
|-------|------|-------|
| `interaction_id` | string | |
| `attempt_id` | string | |
| `kind` | enum | `question` \| `answer` \| `approval_request` \| `approval_decision` \| `progress` \| `tool_activity` |
| `payload` | json | kind-specific (prompt/choices, impact/boundary, tool name+summary, text) |
| `confirmed` | bool | true once the user has answered/approved and the effect is committed to the staging worktree (drives checkpoint, §FR-033) |
| `at` | timestamp | |

---

## 7. ChangeSet, ChangedFile, Hunk  *(Git-backed — see contracts/staging-api.md)*

| Entity | Fields | Notes |
|--------|--------|-------|
| `ChangeSet` | `attempt_id`, `files: ChangedFile[]`, `mode: staged\|direct`, `recovery_action?` | FR-016/017 |
| `ChangedFile` | `path`, `status: added\|modified\|removed`, `additions`, `removals`, `why` (summary), `hunks: Hunk[]`, `accept_state` | FR-016 review |
| `Hunk` | `hunk_id`, `old_range`, `new_range`, `lines`, `accept_state: pending\|accepted\|rejected`, `depends_on: hunk_id[]` | FR-016 |

**Accept/reject rules** (FR-016/SC-016): individual hunks and whole files
are selectable. Applying a partial selection with known dependents emits a
warning *before* application (`depends_on`). Staged mode maps accept to
`git apply` of accepted hunks into the primary tree; reject to discard.
Direct mode labels changes live but still records the change set for
review/recovery.

---

## 8. ValidationFinding

A located issue/warning/info (FR-007, FR-009, FR-024).

| Field | Type | Notes |
|-------|------|-------|
| `finding_id` | string | |
| `severity` | `Info`\|`Warning`\|`Critical` | reuse `model::Severity` |
| `code` | string | machine-readable (e.g. `missing_required_section`) |
| `description` | string | |
| `location` | ArtifactLocation | `path` + `line_or_section` (FR-023 navigation target) |
| `remediation` | string? | recommended next action |

---

## 9. DependencyLink

A traceable upstream→downstream edge for stale propagation + traceability
(FR-021/023/032).

| Field | Type | Notes |
|-------|------|-------|
| `from` | ArtifactLocation | upstream (e.g. a requirement id) |
| `to` | ArtifactLocation | downstream (plan section / task / attempt / finding) |
| `kind` | enum | `requirement_to_plan` \| `plan_to_task` \| `task_to_attempt` \| `attempt_to_finding` \| `artifact_to_step_output` |

Built once per feature load; walked downstream on upstream change to mark
`Artifact.stale` and `WorkflowStep.state = stale` without deleting content
(FR-021).

---

## 10. WorkspacePreference  *(JSON at ~/.joey/speckit-ui/preferences.json)*

Non-content UI preferences (FR-026).

| Field | Type | Notes |
|-------|------|-------|
| `last_feature_id` | string? | |
| `open_artifacts` | path[] | last-opened tabs |
| `active_view` | enum | `editor` \| `workflow` \| `review` \| `readiness` |
| `pane_layout` | json | sizes/collapsed/order |
| `filters` | json | active task/search filters |

Explicitly excludes unsaved artifact content (Constitution III — no
content fork outside the repo).

---

## JSONL record schema (versioned public format — Constitution VII)

Stored at `~/.joey/speckit-ui/history/<feature-id>.jsonl`, one
`WorkflowAttempt` (with nested `run_config`, `transcript`, `interactions`,
`changes` summary, `validation`, `checkpoint`) per line. Mandatory
`schema_version: 1`. Full wire shape is normative in
`contracts/history-jsonl.md`. Any breaking change ⇒ MAJOR bump + migration
+ round-trip test.
