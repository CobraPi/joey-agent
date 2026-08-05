# Research: Spec-Kit Development IDE

**Feature**: `010-speckit-development-ide` | **Date**: 2026-08-03

This document resolves every technical unknown surfaced in `plan.md`'s
Technical Context and records the alternatives considered for each
dependency/format decision (Constitution VIII — dependency weight recorded
against alternatives; Constitution VII — on-disk formats treated as versioned
public surfaces). The clarifications in `spec.md` already locked the
*category* of each decision (out-of-process agent, Git-backed staging,
JSONL history); this research picks the *mechanism* and justifies it.

> **Note on versions**: the live crate/npm registries were unreachable while
> generating this plan (auth tokens expired). Versions cited below are the
> latest known at writing; tasks.md MUST pin exact versions at
> implementation time and re-confirm the tradeoff summary unchanged.

---

## 1. Out-of-process agent execution: streaming protocol

**Decision**: The backend spawns the `joey` CLI (or the relevant
`/speckit-*` skill wrapper) as an async `tokio::process::Child`, captures
`stdout`/`stderr` line-by-line, and forwards each line as a JSON event over
the existing WebSocket broadcast channel (`AppState::channel_for`, reused
from `specs/001`). Interactive prompts (clarify questions, approvals) are
delivered to the child's `stdin` from a corresponding `POST .../answer` /
`POST .../approve` REST call. This is exactly the model `specs/001`'s
`commands.rs` + `api/ws.rs::run_handler` already use for
`/speckit-implement`.

**Rationale**:
- Keeps `joey-speckit-ui` decoupled from `joey-agent-core` internals
  (Constitution VI — depend only on the CLI contract). The backend's
  `Cargo.toml` never links `joey-agent-core`.
- The agent reuses its real auth/tool/approval stack unchanged (spec
  Clarification "Joey adaptation" Q1; Assumptions).
- Reuses the existing broadcast-channel + WS plumbing — no new transport.

**Interaction envelope** (newline-delimited JSON over WS, one record per
line, so the frontend can parse incrementally):

```json
{ "type": "progress",   "attempt_id": "...", "text": "..." }
{ "type": "tool",       "attempt_id": "...", "name": "edit", "summary": "..." }
{ "type": "question",   "attempt_id": "...", "prompt": "...", "choices": null }
{ "type": "approval",   "attempt_id": "...", "impact": "...", "boundary": "broad" }
{ "type": "output",     "attempt_id": "...", "file": "plan.md", "added": 12 }
{ "type": "status",     "attempt_id": "...", "terminal": "succeeded", "duration_ms": 12345 }
{ "type": "error",      "attempt_id": "...", "message": "...", "recoverable": true }
```

The child's exit code maps to the terminal `status` event
(`0 → succeeded`, non-zero with recoverable classification → `failed` but
reviewable, signal → `cancelled`). `tokio::select!` on the child + a cancel
token gives safe cancellation (FR-014): we drop stdin, send SIGTERM (or
`.kill()` on Windows), drain remaining buffered output, then emit a
truthful `status` recording completed vs incomplete effects.

**Alternatives considered**:
- *In-process library call* (`joey-speckit-ui` depends on
  `joey-agent-core`): rejected — violates Constitution VI and the explicit
  FR-011 mandate; would force coordinated edits across crates on every
  agent change.
- *Separate long-lived agent daemon with IPC*: rejected — adds a third
  process to operate/restart; the per-step subprocess is simpler and
  already proven by `specs/001`.

---

## 2. Git-backed staged changes: index vs. dedicated worktree

**Decision**: Support **both**, selected per run by the change-mode field,
with **dedicated temporary worktree on a staging branch** as the default
staged-mode backing and the **repository index** as a lighter alternative
for single-file edits.

- **Staged mode (default backing = temp worktree)**: create
  `git worktree add --detach <tmp>/joey-stage-<attempt>` rooted at the
  feature's current `HEAD`. The agent runs *inside* this worktree, so its
  writes never touch the user's primary worktree. Accept maps to
  `git diff` → `git apply` into the primary tree (hunk-level with
  `--reject`); reject maps to discarding the worktree.
- **Direct mode**: the agent runs in the primary worktree; changes are
  labelled live (FR-016). Recovery still works because every accepted
  interaction is checkpointed to a Git tree-ish (§6).
- **Index-only variant**: for single-artifact edits where a full worktree
  is overkill, stage to the index (`git add -p` semantics) and apply on
  accept. Used by the lightweight editor path, not the runner.

**Rationale**:
- A separate worktree cleanly separates run-attributed changes from the
  user's unrelated uncommitted work (spec Assumptions; FR-016) — Git's own
  index/worktree separation provides this for free.
- Accept/reject/recover map to native Git primitives
  (`git checkout`, `git restore`, `git apply --reject`, `git diff`),
  which survive backend restarts because they operate on the on-disk repo
  (FR-033) — no in-memory state to lose.
- No overlay filesystem or scratch store is introduced (Constitution VIII;
  spec Clarification "Joey adaptation" Q2).

**Conflict guard (FR-015)**: before creating a worktree for a run, the
backend checks whether any in-flight attempt's change set overlaps the
candidate's target paths (computed from the step's declared artifact
targets + a pre-run `git status`). Overlap → 409 `conflicting_run`.
Independent features (different `specs/<id>/` subtrees, disjoint source
paths) may run concurrently.

**Alternatives considered**:
- *Out-of-tree scratch directory + manual file copy*: rejected — loses Git
  semantics, makes hunk-level accept/reject and recovery fragile, and is
  explicitly forbidden by FR-016.
- *Overlay FS (fuse/unionfs)*: rejected — platform-specific, heavyweight,
  requires privileges, violates Constitution VIII.

---

## 3. Git primitives in Rust: `gix` vs. `git2` vs. `git` CLI subprocess

**Decision**: Prefer **`gix`** (gitoxide, pure Rust) for index/tree/diff
operations *where its API is sufficient*, and **shell out to the system
`git` CLI** for the few operations where `gix` is incomplete (notably
`git worktree add` lifecycle and `git apply --reject` hunk application).

Concretely:
- `gix`: read HEAD/tree, enumerate index entries, compute blobs/diffs,
  write blobs, update refs — the read-side and object-side of staging.
- `git` CLI subprocess (via the existing `tokio::process::Command` helper
  in `commands.rs`): `worktree add/remove`, `apply --reject`,
  `checkout --patch`. These are well-trodden, idempotent, and already the
  pattern `commands.rs` uses for `/speckit-*`.

**Rationale**:
- `gix` is pure Rust (no C toolchain), matches the project's
  bundled/no-system-dependency stance (AGENTS.md: "bundled rusqlite — no
  system SQLite dependency required"), and is already trusted by Cargo.
- Falling back to `git` CLI for the awkward worktree/apply-patch cases
  avoids depending on `gix` APIs that are still maturing; the CLI is
  universally present wherever Spec-Kit runs (it requires a git repo).
- Keeps the backend effectively single-binary for the hot path
  (readiness, history, diff inspection) while delegating only the
  mutating staging moves to a tool that is guaranteed present.

**Cost vs. alternatives**:

| Option | Binary/compile cost | Runtime dep | Coverage of needed ops | Verdict |
|--------|--------------------|-------------|------------------------|---------|
| `gix` (pure Rust) | +moderate compile time, larger binary (object DB code) | none (pure Rust) | read/object side complete; worktree/patch apply partial | **Use for read/object side** |
| `git2` (libgit2 FFI) | +C toolchain at build; smaller than gix | none (static link) | most ops incl. worktree | Rejected — C build dep conflicts with project's pure-Rust/bundled stance |
| `git` CLI subprocess only | zero Rust cost | requires `git` on PATH | 100 % | **Use for worktree/apply** (already required for Spec-Kit) |
| Custom patch/overlay FS | high dev cost | none | partial, fragile | Rejected (Constitution VIII) |

**Tasks.md MUST**: pin the exact `gix` version, record its
`cargo build -p joey-speckit-ui` compile-time delta against the
`specs/001` baseline, and add a hermetic test that runs the staging moves
against a temp bare repo fixture (`tests/staging_git.rs`).

**Pinned version + recorded cost (T001, 2026-08-03):**
- `gix = "0.66"` with `default-features = false, features = ["basic",
  "blob-diff", "index", "revision", "status", "serde"]`.
- Compile-time delta: `cargo build -p joey-speckit-ui` clean build with gix
  added = **~50s** (was ~4s without gix). The delta is dominated by gix's
  transitive crates (gix-diff, gix-status, gix-index, gix-worktree, etc.).
- Binary-size delta: moderate (+several MB in debug profile; release-profile
  impact is smaller due to LTO + strip).
- Tradeoff justified: gix provides pure-Rust git read/object operations
  (HEAD/tree/index/diff/blobs/refs) without a C toolchain dependency, matching
  the project's bundled/no-system-dependency stance (Constitution VIII).
  The git CLI subprocess handles worktree lifecycle + `git apply --reject`
  (100% coverage, universally present wherever Spec-Kit runs).

---

## 4. Frontend dependencies: diff view + resizable panes

**Decision**:
- **`diff` (jsdiff)** — framework-agnostic, line- and word-level diff
  producing structured change objects suitable for rendering additions/
  removals with hunk boundaries. Drives the review view's hunk/file
  accept-reject (FR-016).
- **`split.js`** — tiny (~1 KB) framework-agnostic splitter for the
  resizable/collapsible/reorderable workspace panes (FR-002, FR-026).
- No UI *framework* is added. The frontend stays vanilla TypeScript +
  Vite (as built in `specs/001`), per the constitution's "no new runtime
  dependency (JS framework…) without recording alternatives" constraint —
  introducing React/Vue solely for this feature would be unjustified
  weight when the existing vanilla-TS app already renders the canvas,
  task board, and co-pilot panel.

**Rationale**:
- Both libs are framework-agnostic, so they compose with the existing
  vanilla-TS component style without a rewrite.
- `diff` (jsdiff) is the de-facto standard for structured textual diffs in
  the browser and exposes the hunk granularity FR-016 requires.
- `split.js` is far smaller than `allotment`/`splitpanes` (which are
  React/Vue respectively) and needs no virtual DOM.

**Cost vs. alternatives**:

| Option | Size | Framework | Fit | Verdict |
|--------|------|-----------|-----|---------|
| `diff` (jsdiff) | ~30 KB min | none | line/word/hunk diffs | **Use** |
| `diff-match-patch` | ~40 KB | none | char-level (Google) | Rejected — char-level is wrong granularity for code review; line-level jsdiff maps better to hunks |
| `react-diff-viewer` | +React | React | polished UI | Rejected — drags in React |
| `split.js` | ~1 KB | none | pane splitting | **Use** |
| `allotment` / `splitpanes` | larger | React/Vue | pane splitting | Rejected — framework lock-in |
| Custom splitter | 0 deps | none | trivial but reinvents edge cases (touch, a11y) | Rejected — `split.js` already covers a11y/keyboard |

**Tasks.md MUST**: pin exact versions, record the bundle-size delta in
`vite build` output vs. the `specs/001` baseline, and ensure keyboard
accessibility (FR-027/SC-011) — `split.js` exposes ARIA roles; the diff
view must add its own.

**Pinned versions + recorded cost (T002, 2026-08-03):**
- `diff = "^5.2.0"` (jsdiff) — ~30 KB min, framework-agnostic.
- `split.js = "^1.6.5"` — ~1 KB, framework-agnostic.
- Both are devDependencies (client-side libraries); no runtime server cost.
- TypeScript type declarations provided via `src/vendor.d.ts` for both.
- Bundle-size delta is minimal (~31 KB combined uncompressed, ~12 KB gzipped).

---

## 5. History format: append-only JSONL + streamed reads at scale

**Decision**: One file per feature at
`~/.joey/speckit-ui/history/<feature-id>.jsonl`; each line is a
self-contained, newline-delimited JSON attempt record. The record carries
a mandatory `schema_version` field (Constitution VII public format).
90-day expiry is a periodic file-mtime sweep that deletes lines older
than 90 days (or, if a feature file's newest record expires, the whole
file).

**Record shape (v1 — finalized in `data-model.md` and
`contracts/history-jsonl.md`)**:

```json
{"schema_version":1,"attempt_id":"...","feature_id":"...","step":"plan",
 "initiator":"...","started_at":"<rfc3339>","ended_at":null,
 "status":"running","change_mode":"staged","effective_instructions":"...",
 "scope":{"targets":["specs/.../plan.md"],"options":{"model":"...","reasoning":"...","max_iter":...}},
 "option_catalog_rev":"...","override_id":null,
 "transcript":[...],"interactions":[...],"changes":null,
 "validation":null,"checkpoint":{"tree_ish":"...","last_confirmed_interaction_id":"..."},
 "prior_attempt_id":null,"expires_at":"<rfc3339 +90d>"}
```

**Streaming / scale (SC-010 — 500 tasks / 100 attempts / 1 000 files)**:
- **Append** is O(1): a single `writeln!` to the end of the file.
- **Read** is lazy/streamed: the history endpoint serves records via a
  generator that decodes line-by-line with `serde_json::Deserializer::from_reader`
  (zero-copy into the response), so a 100-attempt file is never fully
  buffered. The frontend paginates / virtualizes the list.
- The `changes` sub-record for a 1 000-file change set is stored as a
  *summary* (path + counts + blob refs) in the JSONL line; full per-hunk
  diffs are resolved on demand from the Git tree-ish in `checkpoint`, not
  duplicated into JSONL. This keeps each line bounded even for huge
  change sets.
- Expiry: a startup task + hourly tick scans
  `~/.joey/speckit-ui/history/*.jsonl`, rewrites files without expired
  lines (atomic temp + rename), and removes empty files.

**Rationale**:
- No new dependency (workspace already standardizes on `serde_json`) and
  no new schema-versioned DB (Constitution VIII).
- 90-day expiry is trivial (mtime sweep) vs. a SQL `DELETE` + vacuum.
- Append-only is crash-safe: a partial last line is skipped on read
  (tolerant parser pattern from `model.rs::Status::Unparsed`).

**Alternatives considered**:
- *SQLite (new table in `joey-core`'s session store or a dedicated DB)*:
  rejected — introduces a second schema/versioned format into
  `joey-speckit-ui` (Constitution VII risk) and a query engine for a
  sequential append-and-read log (Constitution VIII — unjustified).
- *One JSON file per attempt*: rejected — explodes file count at scale,
  directory-listing cost, no natural ordering without sort.
- *Per-feature single pretty-printed JSON array*: rejected — not
  append-safe (must rewrite whole file), not streamable.

---

## 6. Restart recovery: safe checkpoints

**Decision**: Each attempt records a **checkpoint** after every *confirmed*
interaction (an answered question, an approved action, a completed tool
step whose effect is committed to the staging worktree). The checkpoint
references (a) the Git `tree_ish` of the staging worktree at that point
and (b) the `last_confirmed_interaction_id`. On backend/agent restart,
`recovery.rs` loads the latest checkpoint:

- **Valid checkpoint exists** → resume the attempt by re-spawning the
  agent with the feature context + the conversation transcript up to the
  confirmed interaction, *without* replaying unconfirmed actions
  (FR-033 / SC-015). The Git worktree already holds the confirmed
  effects.
- **No valid checkpoint** → mark the attempt `recovery_failed`, preserve
  the effects recorded so far (the worktree + transcript), and emit a
  clear recovery action ("discard staging worktree" or "apply confirmed
  changes then re-run"). Never silently replay unconfirmed actions.

**Rationale**:
- Git is the durable substrate: a tree-ish survives any process crash, so
  "the latest safe point" is just "the last committed worktree state".
- Checkpoints are co-located with the JSONL attempt record (rewriting the
  in-progress line on each confirmation), so recovery needs only the
  history file + the repo — no separate checkpoint store (Constitution
  VIII).

**Alternatives considered**:
- *Event-sourcing the whole run and replaying on restart*: rejected —
  risks re-executing side-effecting tool calls (shell writes, network);
  FR-033 explicitly forbids replaying unconfirmed actions.
- *In-memory resume only (no durable checkpoint)*: rejected — fails the
  restart requirement entirely.

---

## 7. Workflow readiness & stale propagation

**Decision**: `workflow.rs` derives each step's state purely from current
artifact state + prerequisite completion + unresolved decisions +
validation results + active runs (FR-022), never from a hand-set flag.
States: `ready`, `blocked`, `running`, `attention_needed` (presentation
aggregate derived from awaiting-input/approval/recoverable-failure/
conflicted/recovery-failed — spec US2 note), `succeeded`, `failed`,
`stale`, `unavailable` (FR-008).

**Stale propagation (FR-021 / SC-007 < 3 s)**: a dependency graph over
artifacts is built once per feature load (spec → plan → tasks →
implementation → convergence; constitution → all). When the file watcher
(`watcher.rs`, reused) reports an upstream artifact change, the graph is
walked downstream and affected nodes are marked `stale` *without deleting
their content*. The watcher already debounces and pushes over WS
(`specs/001`), so the < 3 s budget is the debounce window + graph walk +
WS round-trip — comfortably within range for an in-process graph of a few
hundred nodes.

**Dependency links (FR-023 / FR-032)**: the same graph powers
requirement → plan-section → task → attempt → finding traceability
(SC-032's end-to-end progress summary).

**Alternatives considered**:
- *Manual status field the user sets*: rejected (FR-022 explicitly
  forbids status-from-a-flag-alone).
- *Re-deriving on every poll*: rejected — wasteful; derive on load + on
  watched change event only.

---

## 8. External-change detection (reuse, not reinvent)

**Decision**: Reuse the `specs/001` content-hash conflict model unchanged
(`conflict.rs::content_hash`, `writer.rs::replace_line_if_unchanged`,
409-on-conflict). The new multi-artifact `editor.rs` composes the same
primitive: every write carries `based_on_hash`; a mismatch → 409 with
`current_hash`, no partial write (FR-020 / SC-005 100 %). The file watcher
pushes `file_changed` events so the frontend can offer reload/compare
before a competing save (FR-020 acceptance 3).

**Rationale**: the model is already specified, tested, and proven in
`specs/001`; extending it to more artifact types is additive and needs no
new mechanism.

---

## 9. Accessibility (FR-027 / SC-011)

**Decision**: All primary journeys (authoring, execution, review,
approval, recovery) are keyboard-reachable with visible focus and
descriptive ARIA labels. The diff view and pane layout (§4) carry their
own ARIA roles; status badges expose `aria-label` from the derived state
text; the run panel surfaces questions/approvals as focusable regions with
`role="alert"` for blocking prompts. Playwright e2e (SC-011) includes a
keyboard-only journey.

**Rationale**: constitution's "Additional Constraints" require any UI
surface to justify its rendering approach; accessibility is part of that
contract and is non-negotiable for a desktop-class IDE.

---

## 10. Open items deferred to `tasks.md` (Phase 2)

These are implementation-scoped, not design unknowns, and are listed here
only so the tasks command picks them up:

- Exact pinned versions for `gix`, `diff`, `split.js` + recorded
  compile/bundle-size deltas.
- Hermetic test fixtures: temp bare git repo (staging), subprocess
  harness that fakes the `joey` CLI contract (runner), JSONL
  round-trip + migration test (history).
- Option-catalog revisioning: how the backend advertises
  model/reasoning/max-iter options and invalidates a stale
  `option_catalog_rev` (FR-010) — design is "backend exposes a
  `/api/options` catalog with a content-hash revision; the run config
  pins the revision and the backend rejects a run whose revision is
  stale".
- Project-override storage location (`~/.joey/speckit-ui/overrides/`)
  and merge semantics (FR-034) — design is "JSON per feature+step,
  merged over installed defaults; effective merged instructions are
  inspectable and removable".
