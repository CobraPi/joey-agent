# SpecKit Visual UI — Frontend

Standalone TypeScript frontend for the SpecKit Visual UI feature
(`specs/001-speckit-visual-ui/`). Consumes the `joey-speckit-ui` Rust backend's
REST + WebSocket API (see `../../specs/001-speckit-visual-ui/contracts/speckit-ui-api.md`)
and is intentionally **not** part of the Cargo workspace — it's a plain npm
project (Vite + vanilla TS, no framework).

## Structure

- `src/api-client.ts` — typed fetch/WebSocket client matching every endpoint
  in the API contract, including `based_on_hash` / 409-conflict handling.
- `src/canvas/` — Pillar 1: Spec-to-Task mind-map canvas (SVG-based).
- `src/workspace/` — Pillar 2: split-screen co-pilot workspace (document pane,
  assistant panel, constitution gauge).

## Local development (with the backend)

Run the backend and this frontend together in two terminals from the repo
root:

```sh
# Terminal 1 — backend
cargo run -p joey-speckit-ui

# Terminal 2 — this frontend
cd web/speckit-ui
npm install   # first time only
npm run dev
```

See `../../crates/joey-speckit-ui/README.md` for backend environment
variables (port, repo root).
- `src/board/` — Pillar 3: Kanban board (board, task card, dependency view).
- `src/init-wizard.ts` — guided `POST /api/init` form.
- `tests/e2e/` — Playwright specs exercising UI logic against a mocked API
  layer (`tests/mocks/mock-backend.ts`), since the real backend may not be
  running.

## Setup

```bash
cd web/speckit-ui
npm install
```

## Run (dev)

```bash
# Terminal 1: backend (once crates/joey-speckit-ui exists)
cargo run -p joey-speckit-ui -- --repo-root .

# Terminal 2: frontend
cd web/speckit-ui
npm run dev
```

Open the printed local URL (default `http://localhost:5173`) in a browser.

## Build

```bash
npm run build      # tsc --noEmit && vite build -> dist/
npm run typecheck  # tsc --noEmit only
```

## Lint / Format

```bash
npm run lint
npm run format
```

## Tests

The Playwright suite intercepts `fetch`/WebSocket traffic with a fake backend
(`tests/mocks/mock-backend.ts`) so it can run without `joey-speckit-ui`. It
proves out the load-bearing product constraints from `spec.md`:

- Canvas renders the full Specification → UserStory → Plan → Task hierarchy
  with zero dropped/duplicated nodes, color-coded by status, and never
  silently drops malformed/`Unparsed` entries.
- The canvas auto-refreshes from the `watch` WebSocket within the UI's own
  event loop (no manual reload) when `tasks.md`/`plan.md`/`spec.md` changes.
- Inline edits PATCH back to disk using `based_on_hash`; a `409` conflict
  surfaces a visible message and reload prompt — no silent retry or merge.
- The assistant panel's `/clarify` flow highlights the correct document line
  and the `/analyze` flow anchors findings to a specific file/section while
  driving the constitution gauge.
- Clicking **Execute Task** on one Kanban card runs **only** that task — even
  when another eligible/parallel task shares a target file — matching the
  single-task-per-click Clarifications answer (no cascading execution).
- The dependency view links tasks that share `target_files` only when
  toggled on.

Run them:

```bash
npx playwright install chromium   # first time only
npm run test:e2e
```

`playwright.config.ts` boots `npm run dev` on port 4173 automatically if a
dev server isn't already running (`reuseExistingServer: true` locally).

## Deviations from plan.md / tasks.md

- The Playwright `webServer.port` option didn't reliably detect a boot on
  `127.0.0.1` in this sandboxed environment (Vite's own IPv6/`localhost`
  binding); switched to `webServer.url: 'http://localhost:4173'` with
  `reuseExistingServer: true` so the health check polls the same host Vite
  actually serves on. No behavioral difference for real usage.
- `document-pane.ts` ships a minimal line-tagging Markdown renderer rather
  than a full Markdown library, consistent with plan.md's "no heavy
  framework" constraint; it's sufficient to support scroll/highlight by line
  and anchor text, which is all the clarify/analyze flows need for the MVP.
- The double-click inline editor uses `window.prompt` for text input rather
  than a custom modal, to keep the MVP canvas implementation minimal; this is
  swappable with a richer editor without touching `api-client.ts` or the
  conflict-handling logic.
