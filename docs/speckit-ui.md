# joey-speckit-ui — SpecKit Visual UI Backend

Local backend for the visual UI over `.specify/`/`specs/` spec-kit
artifacts. **Files are the source of truth** — the UI never holds divergent
state.

## Parsing and model

Parses `specs/<feature>/{spec,plan,tasks}.md` into a typed model; a CST
layer (`cst/`) preserves structure for surgical edits; `writer`/`patch`/
`editor` perform conflict-checked writes (content-hash based,
`conflict.rs`); `recovery` handles crash recovery.

## Workflow engine (`workflow.rs`)

Canonical lifecycle steps — constitution → specify → clarify → plan →
checklist → tasks → analyze → implement → converge → task_to_issue — each
with readiness derived from artifact state + prerequisites, plus a
dependency-link graph for stale propagation and traceability.

## HTTP + WebSocket API (axum)

See `contracts/speckit-ui-api.md` in the crate. Endpoints:
`/api/features`, `/api/project`, `/api/features/:id` (+ `/spec`,
`/tasks/:id`, `/clarify`, `/analyze`, `/tasks/:task_id/execute`,
`/artifacts...`), `/api/init`, with a WebSocket channel for live run
updates. Heavy actions (clarify, analyze, implement) shell out to the
existing `specify` CLI / SpecKit bash scripts as subprocesses — SpecKit's
own logic is never reimplemented.

## Other subsystems

- Debounced filesystem watcher (~500ms) on feature files for
  external-change pickup.
- Git-backed staging area (`staging/`, trait-based) for safe concurrent
  edits with checkpoints and overlap rejection.
- Semantic graph + mapping catalog (`meaning/`) — a derived,
  never-persisted projection powering traceability/coverage widgets.
- JSONL history round-trip; validation.

## CLI

`joey speckit [--port 4173] [--repo-root DIR] [--open]` spawns the backend
(`joey-speckit-ui` binary; falls back to `cargo run -p joey-speckit-ui`)
plus the Vite frontend (web/speckit-ui) together, waits on Ctrl+C. The
backend also runs standalone honoring `JOEY_SPECKIT_UI_ROOT` /
`JOEY_SPECKIT_UI_PORT`.
