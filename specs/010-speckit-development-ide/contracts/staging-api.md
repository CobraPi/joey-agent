# Contract: Git-Backed Staging & Recovery

**Feature**: `010-speckit-development-ide` | **Implements**: FR-010,
FR-015, FR-016, FR-017, FR-020, FR-033

Defines how candidate changes are held, reviewed, accepted/rejected at
hunk granularity, and recovered — all via native Git primitives, with no
out-of-tree scratch store or overlay filesystem (Constitution VIII; spec
Clarification "Joey adaptation" Q2; FR-016).

## Interface

```text
trait StagingArea {
    /// Create a staging area for an attempt in `mode`.
    ///  - direct: returns the primary worktree root (agent writes live).
    ///  - staged: creates a temp worktree on a staging branch and returns it.
    async fn open(&self, repo_root: &Path, attempt_id: &str, mode: ChangeMode,
                  scope: &Scope) -> Result<StagingRoot, StagingError>;

    /// Snapshot the current staging state as a Git tree-ish (checkpoint).
    async fn checkpoint(&self, root: &StagingRoot) -> Result<Checkpoint, StagingError>;

    /// Compute the change set (files + hunks) vs the primary tree.
    async fn diff(&self, root: &StagingRoot) -> Result<ChangeSet, StagingError>;

    /// Apply a selection of accepted hunks/files into the primary tree;
    /// reject warnings for partial selections with known dependents.
    async fn apply(&self, root: &StagingRoot, selection: &Selection)
        -> Result<ApplyOutcome, StagingError>;

    /// Discard staging (reject all). Safe recovery (FR-017).
    async fn discard(&self, root: &StagingRoot) -> Result<(), StagingError>;
}
```

## Backing store

- **Direct mode**: the agent runs in the primary worktree. Changes are
  live and labelled (FR-016). A `Checkpoint` (tree-ish) is still recorded
  after each confirmed interaction so recovery works (FR-033). The change
  set is reviewed post-run and can be partially reverted via
  `git restore`/`git checkout` of rejected hunks.
- **Staged mode (default backing = dedicated temp worktree)**:
  - Created via `git worktree add --detach <tmp>/joey-stage-<attempt>`
    rooted at the feature's current `HEAD`.
  - The agent runs *inside* this worktree (see `workflow-runner.md`); its
    writes never touch the user's primary worktree.
  - **Accept** = compute `git diff` between worktree and primary, apply
    accepted hunks into the primary tree via `git apply --reject`
    (hunk-level; `--reject` leaves unappliable hunks in `.rej` for review).
  - **Reject** = discard the worktree (`git worktree remove`).

## Conflict guard (FR-015)

Before opening a staging area, the backend computes the candidate scope's
affected paths (declared artifact targets + a pre-run `git status` of the
feature subtree). If any in-flight attempt's change set overlaps those
paths, open returns `StagingError::ConflictingRun` → HTTP 409
`conflicting_run`. Independent features (disjoint subtrees) run
concurrently.

## Hunk-level accept/reject (FR-016 / SC-016)

`ChangeSet.files[].hunks[]` carry `depends_on` edges (a hunk that
semantically requires another, e.g. a signature change + its call-site
update). `apply()`:
1. Validates the selection.
2. If any accepted hunk has a `depends_on` pointing at a **rejected**
   hunk, returns warnings **before** application (SC-016) — the client
   may re-confirm or adjust.
3. Applies accepted hunks; rejected hunks are left out (staged) or
   reverted (direct).

## Recovery (FR-017 / FR-033)

- **Failed/cancelled/unwanted run** → `discard()` (safe; warns if the
  worktree/primary has unrelated user changes that would be touched).
- **Restart with valid checkpoint** → resume: the worktree already holds
  confirmed effects; only unconfirmed actions are skipped.
- **Restart with no valid checkpoint** → `recovery_failed`: preserve the
  worktree + transcript, report required action ("discard staging
  worktree" or "apply confirmed changes then re-run"). Never replay
  unconfirmed actions.

## Implementation note (research.md §3)

- Read/object side (HEAD, tree, index, diff, blobs, refs) → `gix` (pure
  Rust).
- Worktree lifecycle + `git apply --reject` → `git` CLI subprocess via the
  existing `commands.rs` `tokio::process::Command` helper (universally
  present wherever Spec-Kit runs).

## Non-goals

- No overlay filesystem (platform-specific, privileged, heavyweight).
- No scratch directory outside the repo (loses Git semantics, FR-016
  forbids it).
- No automatic commit/push/issue-publication (spec Assumptions — explicit
  user action + existing safety approvals only).
