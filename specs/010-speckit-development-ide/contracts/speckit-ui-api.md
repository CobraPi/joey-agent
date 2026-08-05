# API Contract (additive): Spec-Kit Development IDE Backend

**Feature**: `010-speckit-development-ide`

This contract specifies the **additions** to the `specs/001-speckit-visual-ui`
API (`contracts/speckit-ui-api.md`, implemented in
`crates/joey-speckit-ui/src/api/{rest,ws}.rs`). All `specs/001` endpoints
are **preserved unchanged** (Constitution VII); everything below is
strictly additive. Shared error shape and conventions are inherited from
`specs/001` (`{ "error", "message" }`; `error` codes: `not_found`,
`conflict`, `invalid_request`, `internal_error`, plus new
`conflicting_run`, `stale_option_catalog`, `recovery_failed`).

All bodies are JSON. `{id}` = feature id. Paths are repo-relative and
URL-encoded.

---

## Artifacts (FR-003/004/005/006/007)

### GET /api/features/{id}/artifacts
Discover authorable artifacts (spec, plan, tasks, checklists, research,
data-model, contracts, quickstart, constitution, supporting) without
assuming all exist.

Response 200:
```json
{ "artifacts": [
  { "path": "specs/010-.../plan.md", "kind": "plan", "exists": true,
    "content_hash": "sha256:...", "save_state": "clean", "validity": [],
    "workflow_phase": "plan", "stale": false }
] }
```

### GET /api/features/{id}/artifacts/{path}
Fetch raw text + rendered outline (sections with line ranges) for source +
reading views (FR-006). `{path}` is URL-encoded repo-relative.

Response 200:
```json
{ "path": "...", "kind": "plan", "text": "...", "content_hash": "sha256:...",
  "outline": [ { "title": "Summary", "line": 9 }, { "title": "Technical Context", "line": 13 } ],
  "save_state": "clean", "validity": [ { "severity": "Warning", "code": "unresolved_marker",
    "description": "NEEDS CLARIFICATION at line 21", "location": { "path": "...", "line_or_section": "21" } } ] }
```
Response 404: artifact does not exist (`exists:false` artifacts return 404
on GET text; the explorer still lists them so the user can create them).

### PATCH /api/features/{id}/artifacts/{path}
Whole-file or section-scoped edit (extends `specs/001`'s single-line
`PATCH .../spec`). Conflict-checked via `based_on_hash` (FR-020/SC-005).

Request:
```json
{ "new_text": "...", "based_on_hash": "sha256:...",
  "scope": "whole" }
```
`scope`: `whole` (default) or `{ "section": "<heading or id>" }`.

Response 200: `{ "content_hash": "sha256:..." }`
Response 409 (external change): `{ "error": "conflict", "current_hash": "sha256:...", "message": "..." }`
Response 422 (`invalid`): `{ "error": "invalid_request", "validity": [ <ValidationFinding> ], "message": "..." }`

---

## Workflow catalog & readiness (FR-008/009/021/022)

### GET /api/features/{id}/workflow
Response 200:
```json
{ "steps": [
  { "id": "plan", "order": 40, "purpose": "...", "available": true,
    "inputs": [ { "path": ".../spec.md", "kind": "spec" } ],
    "outputs": [ { "path": ".../plan.md", "kind": "plan" } ],
    "prerequisites": [ "specify" ],
    "state": "ready", "blocking_reason": null, "latest_attempt_id": null,
    "installed_definition_ref": "skill:speckit-plan" },
  { "id": "task_to_issue", "available": false, "state": "unavailable",
    "blocking_reason": "skill not installed" }
] }
```
`state` ∈ `ready|blocked|running|attention_needed|succeeded|failed|stale|unavailable`.
Derived server-side from artifact validity + prerequisites + active runs
(FR-022); recomputed on load and on watched file change (FR-021 stale).

### GET /api/options
Server-advertised agent option catalog (FR-010). Content-hash revision
pins the run config.
```json
{ "revision": "sha256:...", "models": [...], "reasoning_efforts": [...],
  "max_iterations": { "min": 1, "max": 100, "default": 25 } }
```

### GET /api/features/{id}/workflow/{step}/config
Effective merged instructions for a step: installed defaults ⊕ project
override ⊕ (no run edit at this point) (FR-034).
```json
{ "step_id": "plan", "installed": { "instructions": "..." },
  "override": { "override_id": "...", "instructions": "..." },
  "effective_instructions": "..." }
```

### PUT /api/features/{id}/workflow/{step}/override
Create/replace a project-level override (FR-034). Installed definitions
remain read-only.
Request: `{ "instructions": "..." }` → 200 `{ "override_id": "..." }`

### DELETE /api/features/{id}/workflow/{step}/override
Remove the project override; effective reverts to installed. → 204

---

## Run lifecycle (FR-010/011/012/013/014/019/033)

### POST /api/features/{id}/workflow/{step}/run
Prepare + start a run through the **native Joey Agent, out-of-process**
(FR-011). The body is the prepared `RunConfiguration`.

Request:
```json
{ "effective_instructions": "...", "scope": { "targets": [ {"path":".../plan.md"} ] },
  "options": { "model": "...", "reasoning_effort": "...", "max_iterations": 25 },
  "option_catalog_rev": "sha256:...", "change_mode": "staged", "override_id": null,
  "prior_attempt_id": null }
```
Response 202: `{ "attempt_id": "...", "ws": "/api/attempts/{attempt_id}/stream" }`
Response 409 `conflicting_run`: an in-flight attempt's change set overlaps
`scope.targets` (FR-015).
Response 409 `stale_option_catalog`: `option_catalog_rev` ≠ current
`/api/options` revision (FR-010).
Response 422: `change_mode` missing, a target invalid for the step, or an
option outside safety bounds (FR-010).

The configuration becomes **immutable** once preparation succeeds
(data-model §4).

### POST /api/attempts/{attempt_id}/answer
Answer a pending question (FR-013). Request: `{ "interaction_id": "...", "answer": "..." }`
→ 200 `{ "confirmed": true }` (confirmed ⇒ checkpoint advanced, FR-033).

### POST /api/attempts/{attempt_id}/approve
Respond to an approval request (FR-013/017/SC acceptance 5).
Request: `{ "interaction_id": "...", "decision": "approve|reject", "note": "..." }`
→ 200 `{ "confirmed": true }`

### POST /api/attempts/{attempt_id}/cancel
Cancel a running/waiting attempt (FR-014). Safe stop; records completed vs
incomplete effects truthfully. → 202, terminal `status` event follows on WS.

### POST /api/attempts/{attempt_id}/recover
Recover a `recoverable_failure` / `recovery_needed` attempt (FR-017/033).
Warns if recovery would affect unrelated user changes.
→ 200 `{ "resumed": true }` or 409 `recovery_failed` with preserved-effects summary.

---

## Change review (FR-016/017/020/SC-016)

### GET /api/attempts/{attempt_id}/changes
List the change set (files + hunks + accept states + dependency warnings).
```json
{ "attempt_id": "...", "mode": "staged", "recovery_action": null,
  "files": [ { "path": "...", "status": "modified", "additions": 12, "removals": 3,
    "why": "agent rewrote plan summary", "accept_state": "pending",
    "hunks": [ { "hunk_id": "h1", "old_range": "10,5", "new_range": "10,8",
      "accept_state": "pending", "depends_on": ["h2"] } ] } ] }
```

### POST /api/attempts/{attempt_id}/changes/apply
Apply a (partial) selection of hunks/files. Staged mode maps to
`git apply` of accepted hunks into the primary tree; rejected are
discarded (FR-016).
Request: `{ "selection": [ {"path":"...","hunks":["h1"]} ], "apply_all_accepted": false }`
Response 200: `{ "applied": [...], "warnings": [ {"hunk_id":"h1","depends_on":["h2"],"message":"..."} ] }`
A partial selection with known dependents returns warnings **before**
application (SC-016); the client may re-confirm.

---

## History (FR-018/019/031)

### GET /api/features/{id}/history?limit=&before=
Streamed, paginated attempt records (newest first) from the JSONL file.
Each record is a self-contained `WorkflowAttempt` summary (full shape in
`contracts/history-jsonl.md`). Lazy/streamed decode so 100 attempts stay
< 2 s (SC-010).
```json
{ "attempts": [ { "attempt_id":"...", "step_id":"plan", "status":"succeeded",
   "started_at":"...", "ended_at":"...", "prior_attempt_id":null, "changes_count": 4 } ],
  "next_cursor": "..." }
```

---

## Preferences (FR-026)

### GET /api/features/{id}/preferences
→ 200 `WorkspacePreference` (last feature, open artifacts, active view, pane layout, filters).

### PUT /api/features/{id}/preferences
Replace preferences. Must reject any embedded artifact *content* (422) —
preferences are non-content only (Constitution III).

---

## WebSocket (additions)

### WS /api/attempts/{attempt_id}/stream
Streams the run/interaction envelope for a started attempt (research.md
§1): `progress`, `tool`, `question`, `approval`, `output`, `status`,
`error` events. Reuses the `specs/001` broadcast-channel plumbing
(`AppState::channel_for`). Reconnect-safe: on reconnect the client
receives subsequent events; the terminal `status` event is sticky until
the attempt is finalized and the channel is torn down.

The existing `specs/001` WS endpoints (`/api/features/{id}/watch`,
`/api/features/{id}/session/{session_id}`, `/api/runs/{run_id}`) remain.

---

## Status / connectivity (FR-028)

A `/api/health` endpoint advertises: backend reachable, agent binary
discovered, credentials present, repo writable. The frontend maps absence
to `disconnected` / `unavailable_agent` / `missing_credential` /
`read_only` / `failed_write` states with a clear recovery action, and
never presents an unknown result as success (FR-028).
