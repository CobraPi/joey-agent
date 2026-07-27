# Feature Specification: Git Checkpoint Startup Performance

**Feature Branch**: `004-git-checkpoint-perf`

**Created**: 2026-07-24

**Status**: Draft

**Input**: User description: "Fully optimize the git integration of joey-agent — right now it sometimes slows down app startup. Reference hermes-agent's more optimized version. Should be as performant as possible while maintaining full functionality."

## Clarifications

### Session 2026-07-24

- Q: Deferred initialization trigger mechanism — background thread, fully lazy on first mutating call, or hybrid cheap-check-at-startup? → A: Fully lazy — no store/snapshot initialization work happens at startup; the store and initial snapshot are created inline on the first mutating tool call or explicit `/checkpoint` request, not via a background thread.
- Q: Default retention limits (snapshots/project, total store cap, max single-file size) when not otherwise configured? → A: Medium — 50 snapshots per project, 2GB total store cap, 50MB max tracked file size.
- Q: Timeout duration for any single git subprocess invocation made by the checkpoint system? → A: 5 seconds.
- Q: How should existing per-session shadow repos (from the current layout) be handled during migration to the shared store? → A: Discard — no history is migrated into the new shared store; old per-session shadow repo directories are removed opportunistically (e.g. during the first pruning pass), never on the startup critical path.
- Q: Retention window (days of inactivity) after which a project's checkpoint data is considered stale and eligible for pruning? → A: 90 days.

## Current State (evidence)

Joey's git integration is the session-start filesystem "checkpoint" system
(`crates/joey-tools/src/vcs.rs`, invoked from `crates/joey-cli/src/repl.rs`).
Today, on every REPL start (and every `/new` session), `CheckpointManager::new`
runs synchronously on the startup path before the banner is shown, and does:

1. If a shadow repo already exists at `~/.joey/checkpoints/<session_id>`,
   delete it recursively (`remove_dir_all`).
2. `git init --bare` a **brand-new repo per session**.
3. `git symbolic-ref HEAD refs/heads/main`.
4. Stage **the entire working tree** (`git add --all -- .`) with no
   `.gitignore`/excludes, then commit it as the initial checkpoint.

This means: no dedup across sessions/worktrees (every session re-stores the
whole project's blobs from scratch, e.g. `node_modules`, `target/`,
`.git/`, build artifacts, media files are not excluded from the snapshot),
no size/retention limits, and the *initial checkpoint scan+add+commit
runs on the startup critical path*, blocking the banner/prompt on large
repos. The upstream Python `hermes-agent` (see
`/Users/jo110366/Development/hermes-agent/tools/checkpoint_manager.py`)
solved exactly this class of problem with a v2 redesign:

- A **single shared shadow store** (`~/.hermes/checkpoints/store`) reused
  across all projects/sessions, with per-project git refs
  (`refs/hermes/<hash16>`) and per-project index files — git's
  content-addressable object DB dedupes blobs across projects/turns, so a
  new worktree/session costs near-zero instead of re-storing everything.
- A **default excludes list** (`node_modules/`, `dist/`, `build/`,
  `target/`, `.git/`, caches, venvs, media/archives, secrets, logs) applied
  via the store's `info/exclude`, so snapshots never walk/hash huge
  irrelevant trees.
- **Lazy / deferred initialization** — the store and initial snapshot are
  not forced synchronously in the hot startup path; checkpointing is
  triggered per-turn (`new_turn()` + `ensure_checkpoint()`) rather than as
  a blocking step before the prompt is shown.
- **Retention/pruning**: max snapshots per project, max total store size,
  max single-file size, orphan/stale project pruning, `git gc --prune=now`.
- **Isolation & robustness**: explicit `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM`
  = devnull so no user global git config (gpgsign, credential helpers,
  hooks) can slow down or block a snapshot; a repair step for `refs/heads`
  after `gc`; a subprocess timeout so a hung git call can't hang startup
  forever; legacy-store migration so upgrades don't silently orphan old data.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Instant startup regardless of project size (Priority: P1)

As a developer running `joey` in a large repository (with `node_modules`,
build artifacts, or many files), I want the CLI to reach the interactive
prompt immediately, without waiting for a full-repository git snapshot to
be scanned, hashed, and committed first.

**Why this priority**: This is the exact complaint — startup is
sometimes slow because of the git checkpoint step. Fixing this alone
removes the user-visible pain even before other improvements land.

**Independent Test**: Start `joey` in a repository containing a large
`node_modules`/`target` directory (thousands of files) and measure wall
time to first interactive prompt, with checkpoints enabled. Compare to
checkpoints disabled — the difference should be negligible after the fix.

**Acceptance Scenarios**:

1. **Given** checkpoints are enabled and the working directory contains a
   large excluded-pattern directory (e.g. `node_modules/`), **When** the
   user starts `joey`, **Then** the interactive prompt appears without
   waiting on a full scan/add/commit of that directory's contents.
2. **Given** a repository with many thousands of small files, **When**
   the user starts a new session repeatedly (`joey`, `/new`), **Then**
   each subsequent session's checkpoint initialization is measurably
   faster than the first (objects are reused, not re-stored).

---

### User Story 2 - Checkpoint/revert functionality is unchanged (Priority: P1)

As a user who relies on `/checkpoint` and `/revert` to snapshot and roll
back file changes made during a session, I want that behavior to keep
working exactly as before after the performance optimization.

**Why this priority**: Performance work must not silently break the
safety-net feature it's optimizing — "full functionality" is an explicit
requirement from the user.

**Independent Test**: Run the existing `vcs.rs` unit test scenarios
(`checkpoint_lifecycle`, `checkpoint_noop_on_no_changes`) plus a manual
session: make edits, `/checkpoint`, make more edits, `/revert <n>`, and
confirm file state matches the checkpoint exactly, including added,
modified, and deleted files.

**Acceptance Scenarios**:

1. **Given** an active session with checkpoints enabled, **When** the
   user creates several checkpoints and reverts to an earlier one,
   **Then** the working directory is restored to exactly that
   checkpoint's file state (same as current behavior).
2. **Given** a file matches a newly-added default exclude pattern (e.g. a
   `.log` file), **When** the user explicitly wants it tracked anyway,
   **Then** there is a documented, low-friction way to do so (or the
   requirement is explicitly scoped out — see Assumptions).

---

### User Story 3 - Bounded, self-maintaining disk usage (Priority: P2)

As a long-time user of `joey`, I want the checkpoint storage under
`~/.joey/checkpoints` to not grow without bound across many projects and
sessions over weeks/months.

**Why this priority**: Directly enables "full optimization" beyond just
startup latency — unbounded storage growth is the other half of the
current design's inefficiency, and matches what the referenced
hermes-agent implementation already solved.

**Independent Test**: Run many sessions across multiple project
directories over a simulated period; confirm total store size stays
under a configured cap and that stale/orphaned per-project data is
pruned automatically without user intervention.

**Acceptance Scenarios**:

1. **Given** checkpoint data exists for a project directory that no
   longer exists on disk, **When** normal pruning runs, **Then** that
   project's checkpoint data is removed.
2. **Given** the total checkpoint store exceeds a configured size limit,
   **When** pruning runs, **Then** the oldest checkpoints (per project)
   are dropped first until the store is back under the limit.

---

### Edge Cases

- What happens when git is not installed or not on `PATH`? (Must degrade
  gracefully to "checkpoints disabled", as today — no startup delay, no
  hard failure.)
- What happens when two `joey` sessions run concurrently in the same
  working directory (shared store, per-project index/ref)? Must not
  corrupt state or deadlock on a shared index file.
- What happens when a repo is enormous (100k+ files) even after excludes
  are applied? Must not hang indefinitely — a timeout/bound must apply.
- What happens on first-ever run when no store exists yet (cold start)?
  Store initialization itself must also not reintroduce a startup stall.
- What happens when the git binary hangs (e.g., waiting on stdin, a
  credential prompt, or a GPG passphrase prompt)? Must be bounded by a
  timeout and never present an interactive prompt to the user.
- What happens during migration from the current per-session shadow-repo
  layout to a shared-store layout? Old per-session data is discarded
  (not migrated) opportunistically during pruning, never on startup —
  see FR-009.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The checkpoint/git integration MUST NOT perform any
  full-repository scan, add, or commit work on the startup path. Store
  initialization and the first snapshot MUST be fully deferred (lazy) —
  triggered inline on the first actual mutating tool call or explicit
  `/checkpoint` request, not via a background thread/task at startup.
- **FR-002**: The system MUST use a single shared shadow git store
  (analogous to hermes-agent's `store/` with per-project refs/indexes)
  instead of allocating a brand-new bare repo per session, so git object
  data is deduplicated across sessions and worktrees of the same project.
- **FR-003**: The system MUST apply a default exclude list (build output,
  dependency directories, VCS metadata, caches, virtualenvs, large media
  binaries, secrets, logs) to every checkpoint snapshot so these are never
  scanned, hashed, or stored.
- **FR-004**: The system MUST isolate all checkpoint git invocations from
  the user's global/system git configuration (no inherited
  `commit.gpgsign`, credential helpers, or hooks) so no interactive
  prompt or external command can stall a checkpoint operation.
- **FR-005**: Every git subprocess invocation made by the checkpoint
  system MUST be bounded by a timeout of 5 seconds, after which the
  operation fails gracefully (checkpoint skipped/disabled for that call)
  rather than hanging the session.
- **FR-006**: The system MUST continue to support creating checkpoints,
  listing them, and reverting the working directory to any prior
  checkpoint, with identical externally-observable behavior to the
  current `/checkpoint` and `/revert` commands.
- **FR-007**: The system MUST enforce configurable retention limits (max
  snapshots per project, max total store size, max single tracked file
  size) and automatically prune orphaned (working directory no longer
  exists) or stale (unused beyond a retention window) project data.
  Default limits when not otherwise configured: 50 snapshots per project,
  2GB total store cap, 50MB max tracked file size, 90-day stale-project
  retention window.
- **FR-008**: If git is unavailable, or checkpoint initialization fails
  for any reason, the system MUST disable checkpoints for that session
  without delaying startup or crashing.
- **FR-009**: Existing per-session shadow repos left behind by the
  current implementation MUST be discarded (not migrated) safely: no
  history is imported into the new shared store, and old per-session
  shadow repo directories are removed opportunistically (e.g. during the
  first pruning pass) rather than on the startup critical path, so no
  user intervention or startup performance regression occurs during
  cleanup.
- **FR-010**: All on-disk paths, git ref naming, and config isolation
  strategy MUST remain rooted at Joey's home directory convention
  (`~/.joey/checkpoints/...`, honoring `JOEY_HOME` overrides) consistent
  with the rest of the codebase's state-and-config conventions.

### Key Entities

- **Shadow Store**: The single shared bare git repository holding all
  checkpoint history for all projects, keyed by project path hash.
- **Project Ref**: A per-project git ref (e.g. `refs/joey/<hash>`)
  pointing at that project's checkpoint history tip within the shared
  store.
- **Project Index**: A per-project git index file used when staging that
  project's checkpoint, avoiding cross-project index contention.
- **Project Metadata**: Small per-project record (working directory path,
  created/last-touched timestamps) used for pruning and listing.
- **Checkpoint**: A single commit within a project's history representing
  a snapshot of the working tree at a point in time (unchanged concept
  from today).

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Starting `joey` in a large repository (10k+ files including
  a large excluded-pattern directory) with checkpoints enabled adds no
  more than 100ms of observable startup latency versus checkpoints
  disabled.
- **SC-002**: Repeated session starts in the same project directory show
  disk usage growth for checkpoint data that is near-zero per additional
  session when no files changed, versus the full per-project blob
  duplication seen today.
- **SC-003**: Total checkpoint storage across all projects stays within a
  user-configurable cap indefinitely, without manual cleanup, verified
  over a simulated multi-week/multi-project usage pattern.
- **SC-004**: 100% of existing checkpoint/revert test scenarios continue
  to pass unmodified in behavior (same file-state outcomes), confirming
  no functional regression.
- **SC-005**: No checkpoint-related git subprocess call can hang the
  session indefinitely — every call completes or fails within its
  configured timeout in 100% of test runs, including simulated
  unresponsive-git scenarios.

## Assumptions

- The existing `/checkpoint` and `/revert` command surface (as exposed
  via the REPL) is not changing — this is a performance/architecture
  optimization of the underlying git integration, not a UX redesign.
- Excluding common build/dependency/VCS directories from checkpoint
  snapshots by default is acceptable and desirable; there is no
  requirement in this feature to let users track an excluded file via a
  bespoke override UI — if such an escape hatch is wanted later, it can
  be a follow-up (config-driven exclude-list customization is a
  reasonable stretch goal but not required for this feature to be
  considered complete).
- "As performant as possible while maintaining full functionality" is
  interpreted as: startup path is effectively unaffected by checkpoint
  initialization, and existing checkpoint/revert semantics (create,
  list, revert) are fully preserved — not as literally zero overhead ever
  under any workload.
- It is acceptable to migrate away from the current one-shadow-repo-
  per-session layout; old per-session shadow repos are ephemeral by
  design (per `vcs.rs` doc comment: "cleaned up when the session ends")
  so no user-facing data migration guarantee is required beyond not
  crashing if stale per-session directories are found on disk.
- Rust `git2` (libgit2 bindings) or continuing to shell out to the `git`
  binary are both viable implementation choices; the specification does
  not mandate one over the other — that decision belongs in the
  implementation plan (`/speckit-plan`), which should evaluate both
  against the FRs above (especially FR-004 config isolation and FR-005
  timeouts).
- This feature targets the `joey-tools::vcs` checkpoint system
  specifically; it does not cover any other git-related tooling in the
  codebase (e.g. a hypothetical future `git` tool exposed to the model)
  unless such a tool already exists and is found to share the same
  startup-path performance issue during planning.
