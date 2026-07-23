# Phase 0 Research: SpecKit Visual UI

## 1. Web server framework for the local backend

- **Decision**: `axum`
- **Rationale**: Async, built directly on `tokio` (already a workspace
  dependency), first-class WebSocket support (needed for FR-005 live file-change
  push and FR-012 live task-execution output), minimal boilerplate for a small
  local-only API, and it's the most widely used Rust web framework so
  contributors already familiar with the ecosystem can ramp quickly.
- **Alternatives considered**:
  - `actix-web` — mature and fast, but brings its own actor-runtime
    conventions that don't compose as directly with the workspace's existing
    plain-`tokio` async code (e.g. in `joey-gateway`, `joey-agent-core`).
  - `warp` — filter-based API is more implicit/harder to read for a small
    team maintaining this occasionally; less ergonomic WebSocket ergonomics
    than axum's typed extractors.
  - Rolling a bespoke `hyper`-only server — more control, but reinvents
    routing/middleware for no benefit at this project's scale.

## 2. Markdown parsing for spec.md/plan.md/tasks.md

- **Decision**: `pulldown-cmark` for tokenizing, with a thin custom layer on
  top that maps spec-kit's specific conventions (checkbox syntax, `**FR-001**:`
  bold-prefixed requirement IDs, `[P]` parallel markers, `[NEEDS
  CLARIFICATION]` markers) onto the domain model in `data-model.md`.
- **Rationale**: `pulldown-cmark` is the de facto standard Rust CommonMark
  parser (used widely, permissively licensed, no heavy transitive deps),
  and it exposes a streaming event API that's straightforward to fold into a
  structured model while preserving enough position information to support
  round-trip (parse → edit → re-serialize) writes.
- **Alternatives considered**:
  - Regex-only line scanning — fragile against any Markdown variation
    (nested lists, wrapped lines) and would not generalize across the
    template variations already present in `.specify/templates/`.
  - `comrak` — GitHub-flavored, heavier dependency surface, no meaningful
    benefit for this project's needs (no tables/footnotes required in the
    parsed sections).

## 3. Filesystem change detection

- **Decision**: `notify` crate, watching each open feature's `specs/<name>/`
  directory (and `.specify/memory/constitution.md`) with a debounce (~500ms)
  before re-parsing and pushing a WebSocket update.
- **Rationale**: `notify` is the standard cross-platform (macOS/Linux)
  filesystem-event crate in the Rust ecosystem, integrates with `tokio` via
  a channel bridge, and meets the 5-second reflect-external-change target
  (SC-004) with wide margin.
- **Alternatives considered**:
  - Polling on a fixed interval (e.g. re-read + diff every 2s) — simpler, no
    new dependency, but wastes CPU/IO on an idle project and adds latency
    proportional to the interval; `notify` is push-based and lower latency.
  - OS-specific APIs directly (FSEvents/inotify) — `notify` already wraps
    these; no reason to hand-roll platform dispatch.

## 4. Conflict detection / reject-on-conflict write model

- **Decision**: Each parsed file carries a content hash (SHA-256, via the
  already-workspace-dependency `sha2` crate) computed at read/last-successful-write
  time. Every write request from the UI must include the hash it was based
  on; the backend re-hashes the current on-disk content immediately before
  writing and rejects (HTTP 409) if it no longer matches, per the
  Clarifications answer (no silent merge, no queuing — user must reload and
  reapply).
- **Rationale**: This is the standard optimistic-concurrency / ETag pattern,
  requires no new dependency (`sha2` is already in the workspace), and maps
  directly onto FR-018 and the reject-on-conflict clarification.
- **Alternatives considered**:
  - File locking (advisory locks) — doesn't compose well with a
    terminal-driven `/speckit-implement` run editing the same file
    out-of-process; optimistic hash-check is simpler and works regardless of
    which process is writing.
  - Operational-transform / CRDT merge — explicitly rejected by the
    Clarifications answer (multi-writer merge is out of scope; conflicts are
    rejected, not merged).

## 5. Frontend rendering approach

- **Decision**: A small static TypeScript frontend (no heavy SPA framework
  required) using a lightweight, dependency-light canvas/graph library for
  the Spec-to-Task mind-map (Pillar 1) and the dependency/timeline view
  (FR-014), plain DOM + a text-area/CodeMirror-style editor component for the
  split-screen workspace (Pillar 2), and a simple column/card DOM layout for
  the Kanban board (Pillar 3) — no separate graph library needed there.
- **Rationale**: The three pillars have different rendering needs (free-form
  node-link canvas vs. text editor vs. card columns); forcing all three into
  one heavy framework (React/Vue/etc.) would add build complexity
  disproportionate to the UI's actual surface area. A small, framework-light
  approach keeps the frontend's dependency footprint auditable and keeps
  build times fast, consistent with the constitution's "justify new runtime
  dependencies" constraint (applied here to the frontend by analogy, and
  formalized for the frontend project in `web/speckit-ui/package.json`).
- **Alternatives considered**:
  - React + a graph library (e.g. React Flow) — most productive for the
    canvas specifically, but pulls in a large dependency tree for the other
    two pillars that don't need componentized state management at that
    scale.
  - A native desktop app (Tauri/egui) — rejected in Clarifications (Q1) in
    favor of a local web frontend for richer drag-and-drop/canvas UX without
    fighting a native GUI toolkit's layout model.

## Outcome

All items from the Technical Context that could have been marked NEEDS
CLARIFICATION are resolved above (server framework, markdown parsing,
change detection, conflict model, frontend approach). No unresolved unknowns
remain; proceeding to Phase 1 design.
