# Data Model: SpecKit Visual UI

Entities derived from `spec.md` § Key Entities, refined with concrete fields
and validation rules needed for parsing/serialization.

## Status (shared enum)

Used by Specification, Plan, and Task nodes (FR-003, FR-003a).

| Value | Meaning | Derived from |
|---|---|---|
| `Draft` | Artifact exists but work has not started | `spec.md`/`plan.md` `**Status**: Draft`, or a task checkbox `- [ ]` with no active run |
| `InProgress` | Actively being worked on | A task currently executing (FR-012 run in flight), or a plan/spec explicitly marked in progress |
| `Completed` | Finished | Task checkbox `- [x]`/`- [X]`, or a spec/plan `**Status**: Completed`/`Approved` |
| `Unparsed` | Content exists but does not match the expected structure | Edge case: malformed/hand-edited entry (spec.md Edge Cases) — surfaced, never dropped |

Mapping is exhaustive: any recognized source marker resolves to exactly one of
these four values; anything else falls back to `Unparsed` rather than being
silently ignored (Edge Cases in spec.md).

## Feature

Represents one `specs/<NNN-name>/` directory.

| Field | Type | Notes |
|---|---|---|
| `id` | string | Directory name, e.g. `001-speckit-visual-ui` |
| `branch_name` | string | From `plan.md` header / git branch if available |
| `created` | date | From spec.md `**Created**` |
| `spec` | Specification | 1:1 |
| `plan` | Plan? | Absent if `plan.md` not yet created (Edge Cases) |
| `tasks` | Task[] | Empty if `tasks.md` not yet created |
| `content_hash` | map<path, sha256> | One hash per tracked file, used for conflict detection (FR-018) |

## Specification

Represents `spec.md`.

| Field | Type | Notes |
|---|---|---|
| `title` | string | From `# Feature Specification: <title>` |
| `status` | Status | From `**Status**:` field |
| `user_stories` | UserStory[] | One per `### User Story N - ... (Priority: PX)` |
| `functional_requirements` | Requirement[] | One per `- **FR-NNN**: ...` bullet |
| `key_entities` | string[] | Bullet text under `### Key Entities` |
| `success_criteria` | string[] | Bullet text under `### Measurable Outcomes` |
| `clarifications` | ClarificationEntry[] | Parsed from `## Clarifications` sessions, if present |

### UserStory (child of Specification)

| Field | Type | Notes |
|---|---|---|
| `id` | string | e.g. `US1` (derived from ordinal) |
| `title` | string | |
| `priority` | string | `P1`/`P2`/`P3`/... |
| `acceptance_scenarios` | string[] | Given/When/Then bullet text |

### Requirement (child of Specification)

| Field | Type | Notes |
|---|---|---|
| `id` | string | e.g. `FR-001`, must be unique within the spec |
| `text` | string | Requirement body |

### ClarificationEntry (child of Specification)

| Field | Type | Notes |
|---|---|---|
| `session_date` | date | From `### Session YYYY-MM-DD` |
| `question` | string | |
| `answer` | string | |

## Plan

Represents `plan.md`.

| Field | Type | Notes |
|---|---|---|
| `status` | Status | Derived from whether Constitution Check gates all pass (see below) |
| `summary` | string | `## Summary` section text |
| `constitution_gates` | ConstitutionGate[] | Rows of the `## Constitution Check` table |
| `technical_context` | map<string,string> | Key/value pairs under `## Technical Context` |

### ConstitutionGate (child of Plan)

| Field | Type | Notes |
|---|---|---|
| `principle` | string | e.g. "I. Workspace-First Rust" |
| `result` | enum(`Pass`,`Fail`) | Drives the Constitution Compliance gauge (FR-016) |
| `notes` | string | |

Validation: if any `ConstitutionGate.result == Fail` and it is not listed in
`## Complexity Tracking` with a justification, the Plan's overall compliance
is `Fail` and the gauge (FR-016) MUST render red.

## Task

Represents a single entry in `tasks.md`.

| Field | Type | Notes |
|---|---|---|
| `id` | string | e.g. `T001`; must be unique within the feature |
| `description` | string | Task text after the ID |
| `status` | Status | `- [ ]` → not Completed (Draft or InProgress); `- [x]`/`- [X]` → Completed |
| `parallel_eligible` | bool | True if the line contains a `[P]` marker (FR-011) |
| `target_files` | string[] | File paths mentioned/annotated on the task line (FR-011) |
| `user_story_ref` | string? | Which `UserStory.id` this task implements, if determinable from section grouping |
| `run_state` | enum(`Idle`,`Running`,`Succeeded`,`Failed`) | Ephemeral, not persisted to Markdown; drives FR-012/FR-013 live UI state |

Validation:
- `id` must match the project's existing task-ID convention already used in
  `tasks.md` templates (e.g. `T###`); a line that has a checkbox but no
  parseable ID becomes `status = Unparsed` (Edge Cases) rather than being
  dropped.
- Exactly one task line maps to exactly one Task entity — no task may map to
  zero or multiple nodes (Success Criteria SC-002: "zero tasks silently
  dropped or duplicated").

## AnalysisFinding

Represents a single result of `/speckit-analyze` (not persisted to a file by
this feature; held in-memory per session and used to (a) annotate the
document pane per FR-009 and (b) drive the Constitution Compliance gauge
alongside the Plan's `constitution_gates`).

| Field | Type | Notes |
|---|---|---|
| `severity` | enum(`Info`,`Warning`,`Critical`) | |
| `target_file` | string | Which of spec.md/plan.md/tasks.md this concerns |
| `target_line_or_section` | string | Anchor used for FR-009 highlighting |
| `description` | string | |

## Conflict / write model (supports FR-018)

Every mutating API call (see `contracts/speckit-ui-api.md`) must include the
`content_hash` of the target file the caller last read. The backend:

1. Computes the current SHA-256 of the target file.
2. If it matches the caller's hash, applies the edit, re-serializes the file,
   recomputes and returns the new hash.
3. If it does not match, rejects with a conflict response (HTTP 409) and does
   **not** write — per the Clarifications answer, no merge, no queue. The
   caller must re-fetch current state and resubmit.

## Relationships summary

```
Feature 1───1 Specification 1───* UserStory
        │                    1───* Requirement
        │                    1───* ClarificationEntry
        1───0..1 Plan        1───* ConstitutionGate
        1───* Task ──0..1──> UserStory   (user_story_ref)
        (AnalysisFinding is transient, not owned by Feature persistently)
```
