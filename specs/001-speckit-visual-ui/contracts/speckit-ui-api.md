# API Contract: SpecKit Visual UI Backend

Local-only HTTP + WebSocket API served by the `joey-speckit-ui` backend on
`127.0.0.1` (port configurable, default TBD in tasks). Consumed exclusively by
the `web/speckit-ui` frontend. All request/response bodies are JSON unless
noted. This contract exists to keep the frontend and backend independently
buildable/testable (constitution I, V) and to make the reject-on-conflict
write model (data-model.md, FR-018) explicit and verifiable.

## GET /api/features

List available feature directories under `specs/`.

Response 200:
```json
{
  "features": [
    { "id": "001-speckit-visual-ui", "title": "SpecKit Visual UI", "status": "Draft" }
  ]
}
```

## GET /api/features/{id}

Fetch the fully parsed Feature model (Specification + Plan + Tasks), per
`data-model.md`, plus the current `content_hash` for each source file so the
frontend can use it in subsequent writes.

Response 200:
```json
{
  "id": "001-speckit-visual-ui",
  "spec": { "...": "Specification fields", "content_hash": "sha256:..." },
  "plan": { "...": "Plan fields (nullable)", "content_hash": "sha256:..." },
  "tasks": [ { "...": "Task fields" } ],
  "tasks_content_hash": "sha256:..."
}
```

Response 404: feature directory does not exist.

If `plan.md` or `tasks.md` is absent, the corresponding field is `null` /
`[]` and the response includes `"missing": ["plan", "tasks"]` so the frontend
can render the "not yet created" empty state (Edge Cases) and offer to
trigger the missing step.

## PATCH /api/features/{id}/spec

Apply an inline edit to a specific part of `spec.md` (User Story, Requirement,
Clarification answer, etc.) — implements FR-004 and FR-008.

Request:
```json
{
  "target": { "type": "requirement", "id": "FR-012" },
  "new_text": "...",
  "based_on_hash": "sha256:..."
}
```

Response 200 (success): updated node + new `content_hash`.

Response 409 (conflict): file changed on disk since `based_on_hash` was read.
```json
{ "error": "conflict", "current_hash": "sha256:...", "message": "spec.md changed on disk. Reload and reapply your edit." }
```
No partial write occurs on 409, per the Clarifications reject-on-conflict
answer and FR-018.

## PATCH /api/features/{id}/tasks/{taskId}

Apply an inline edit to a single task's description (FR-004), using the same
`based_on_hash` / 409-conflict semantics as above.

## POST /api/features/{id}/clarify

Invoke the equivalent of `/speckit-clarify` for the feature (FR-006, FR-007).
Streams subsequent questions over the WebSocket channel (see below) rather
than in the HTTP response, since it's an interactive multi-turn flow.

Response 202: `{ "session_id": "..." }` — client subscribes to
`ws://.../api/features/{id}/session/{session_id}` for the question/answer
exchange.

## POST /api/features/{id}/clarify/{session_id}/answer

Submit an answer to the currently pending clarification question (FR-008).

Request: `{ "answer": "..." }`

Response 200: `{ "updated_line": "...", "spec_content_hash": "sha256:..." }`
— the frontend uses `updated_line` to highlight/scroll the document pane.

## POST /api/features/{id}/analyze

Invoke `/speckit-analyze` for the feature (FR-009). Response includes a list
of `AnalysisFinding` (data-model.md), each anchored to a file + line/section,
plus an overall `constitution_compliance: "Pass" | "Fail"` field used to
drive the gauge (FR-016).

## POST /api/features/{id}/tasks/{taskId}/execute

Trigger execution of exactly one task (FR-012), equivalent to running
`/speckit-implement` scoped to that task. Per Clarifications Q3, this
endpoint is single-task only — it never cascades to other tasks.

Response 202: `{ "run_id": "..." }` — client subscribes to
`ws://.../api/runs/{run_id}` for live output text (FR-012) and the terminal
status event (`succeeded` | `failed`), after which the backend updates
`tasks.md` (checkbox + status) and the frontend moves the card to Done
(FR-013).

## POST /api/init

Guided initialization (FR-015): equivalent of `specify init --here
--integration <agent> --script <type>`.

Request: `{ "integration": "hermes", "script": "sh" }`

Response 200: `{ "success": true, "output": "..." }` (stdout/stderr of the
underlying `specify` invocation, for display/troubleshooting).

## WebSocket /api/features/{id}/watch

Pushes an event whenever `spec.md`, `plan.md`, or `tasks.md` for this feature
changes on disk (from any source — UI or terminal), implementing FR-005 and
SC-004 (reflected within 5s).

Event:
```json
{ "type": "file_changed", "file": "tasks.md", "content_hash": "sha256:..." }
```

The frontend refetches `GET /api/features/{id}` (or a lighter diff endpoint,
left as a tasks.md-time implementation detail) upon receiving this event.

## Error format (shared)

All non-2xx responses share:
```json
{ "error": "<machine-readable code>", "message": "<human-readable message>" }
```
`error` values used above: `not_found`, `conflict`, `invalid_request`,
`internal_error`.
