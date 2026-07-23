# Quickstart: SpecKit Visual UI

Validation guide for proving the feature works end-to-end once implemented.
Implementation details (crate internals, exact CLI flags) belong in
`tasks.md`; this document only covers how to run and validate the result.

## Prerequisites

- Rust workspace builds (`cargo build --workspace` succeeds).
- Node/npm (or the chosen frontend tooling) available for `web/speckit-ui`.
- A feature directory with `spec.md` present, ideally also `plan.md` and
  `tasks.md`, to exercise all three pillars — `specs/001-speckit-visual-ui/`
  itself is a suitable target once this feature reaches implementation.

## Setup

```bash
# Backend
cargo build -p joey-speckit-ui

# Frontend (one-time)
cd web/speckit-ui && npm install
```

## Run

```bash
# Terminal 1: start the backend (serves API on 127.0.0.1, port per its --help)
cargo run -p joey-speckit-ui -- --repo-root . 

# Terminal 2: start/serve the frontend, then open it in a browser
cd web/speckit-ui && npm run dev
```

## Validation Scenarios

Each scenario below maps to an acceptance scenario in `spec.md`.

### V1 — Canvas reflects existing spec/plan/tasks (User Story 1, AS1)

1. Open the UI and select feature `001-speckit-visual-ui`.
2. Confirm the canvas shows: 1 root Specification node, 3 User Story nodes
   connected to it, and one Task node per entry currently in `tasks.md`
   (once `tasks.md` exists for this feature), each connected to its
   `user_story_ref`.
3. Expected outcome: node/connection counts match a manual count of
   `### User Story` headings in `spec.md` and checklist items in `tasks.md` —
   zero missing, zero duplicated (see data-model.md Task validation, SC-002).

### V2 — External file change reflected live (User Story 1, AS2)

1. With the canvas open, in a separate terminal mark a task complete by hand
   (flip `- [ ]` to `- [x]` in `tasks.md`) or run `/speckit-implement` for one
   task.
2. Expected outcome: within 5 seconds (SC-004) the corresponding node's color
   updates to "Completed" without a manual browser reload.

### V3 — Inline edit writes back to disk (User Story 1, AS3)

1. Double-click a task node, edit its description text, save.
2. Expected outcome: `tasks.md` on disk shows the updated text at the same
   line/task ID, with checkbox syntax and file annotations preserved.

### V4 — Clarify flow highlights the right line (User Story 2, AS1–AS2)

1. Introduce a `[NEEDS CLARIFICATION: ...]` marker into a test spec's
   `spec.md` (or use a feature that still has one).
2. From the assistant panel, run the clarify command.
3. Expected outcome: the question appears in the chat box, the document pane
   auto-scrolls to and highlights the marker's line; after answering, the
   marker is replaced in both the UI and on disk (verify via `cat spec.md`).

### V5 — Analyze findings anchor to document lines (User Story 2, AS3)

1. From the assistant panel, run the analyze command on a feature with at
   least one known inconsistency (e.g. a task referencing a nonexistent
   requirement ID).
2. Expected outcome: the finding is shown anchored to the specific
   file/section it concerns, not as a flat unattributed list.

### V6 — Kanban board metadata and single-task execution (User Story 3, AS1–AS2)

1. Open the board for a feature with a `tasks.md` containing at least one
   `[P]`-marked task and one task with target files listed.
2. Confirm the `[P]` task shows a "Parallel" badge and lists its target
   files.
3. Click "Execute Tasks" on one card.
4. Expected outcome: only that card shows "in progress" with live output;
   no other card starts running (Clarifications Q3 — single-task-per-click).
   On completion, the card moves to Done and `tasks.md` marks that task
   complete on disk.

### V7 — Dependency view (User Story 3, AS3)

1. In `tasks.md`, ensure two tasks list an overlapping target file.
2. Toggle the dependency/timeline view on the board.
3. Expected outcome: a visual link or ordering indicator appears between the
   two tasks.

### V8 — Conflict rejection (FR-018, Clarifications Q2)

1. Open a task for inline edit in the UI (fetch its `content_hash`).
2. In a separate terminal, modify `tasks.md` directly (e.g. via
   `/speckit-implement` or a manual edit) so the file's hash changes.
3. Submit the UI edit.
4. Expected outcome: the request is rejected with a visible conflict message;
   `tasks.md` is not corrupted or silently merged; the UI prompts the user to
   reload before reapplying.

### V9 — Constitution Compliance gauge (FR-016)

1. Open a feature whose `plan.md` Constitution Check table has all gates
   passing. Confirm the gauge shows green/pass.
2. Introduce a failing gate (or run analyze against a spec with an
   unjustified violation). Confirm the gauge turns red.

## Success Criteria Cross-Check

- SC-001 (canvas render < 10s): timed manually during V1.
- SC-002 (0 dropped/duplicated tasks): verified during V1.
- SC-003 (clarify resolution < 30s/question): timed during V4.
- SC-004 (external change reflected < 5s): timed during V2.
- SC-006 (zero data-loss): verified across V3, V6, V8 — every UI-initiated
  edit must be confirmed present in the corresponding file, and every
  rejected write (V8) must leave the file exactly as it was before the
  conflicting attempt.
