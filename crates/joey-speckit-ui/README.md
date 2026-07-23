# joey-speckit-ui (backend)

Local HTTP + WebSocket API for the SpecKit Visual UI feature. Parses
`specs/<feature>/{spec,plan,tasks}.md` into a typed model, serves it with
conflict-checked (reject-on-stale-hash) writes, and streams live updates
(file-watch, clarify sessions, task-execution runs) over WebSocket. See
`specs/001-speckit-visual-ui/contracts/speckit-ui-api.md` for the full API
contract and `specs/001-speckit-visual-ui/plan.md` for architecture.

## Local development (with the frontend)

This backend is normally run alongside the `web/speckit-ui` frontend. In two
terminals from the repo root:

```sh
# Terminal 1 — backend (defaults to port 4173, repo root = cwd)
cargo run -p joey-speckit-ui

# Terminal 2 — frontend dev server (proxies API calls to the backend)
cd web/speckit-ui
npm install   # first time only
npm run dev
```

Environment overrides for the backend:

- `JOEY_SPECKIT_UI_ROOT` — repo root to serve `specs/` from (defaults to the
  current working directory)
- `JOEY_SPECKIT_UI_PORT` — port to listen on (defaults to `4173`)

See `web/speckit-ui/README.md` for frontend-specific dev notes.
