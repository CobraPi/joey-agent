# Data Model: Git Checkpoint Startup Performance

## Entities

### Shadow Store
The single shared bare git repository holding all checkpoint history for
all projects.

| Field | Type | Notes |
|---|---|---|
| `path` | `PathBuf` | `~/.joey/checkpoints/store` (honors `JOEY_HOME`) |
| `objects/`, `HEAD`, `config` | git-internal | Standard bare-repo internals; shared/deduplicated across all projects |
| `info/exclude` | file | Default exclude patterns (R4), written once at store creation |
| `indexes/` | dir | Contains one `<hash16>` file per project (Project Index) |
| `projects/` | dir | Contains one `<hash16>.json` file per project (Project Metadata) |
| `.last_prune` | file (in `~/.joey/checkpoints/`, sibling to `store/`) | Idempotency marker: last prune timestamp, used to throttle prune passes |

**Validation rules**: Store is considered initialized iff `store/HEAD`
exists. Creation is idempotent (safe to call `ensure_store_initialized()`
repeatedly). Store creation MUST NOT happen except when triggered lazily
(FR-001) — enforced structurally by only calling init from the
`checkpoint()` path, never from `CheckpointManager::new`.

### Project Ref
A per-project git ref pointing at that project's checkpoint history tip
within the shared store.

| Field | Type | Notes |
|---|---|---|
| `name` | `String` | `refs/joey/<hash16>` |
| `hash16` | `String` | First 16 hex chars of `sha256(canonicalized_absolute_workdir_path)` |
| tip commit | git commit | Points at most recent checkpoint for this project |

**Validation rules**: `hash16` MUST be derived from the canonicalized
(symlink-resolved, absolute) working directory path so the same project
always maps to the same ref regardless of how the path was originally
invoked (relative vs. absolute, trailing slash, etc.) — this is what
gives dedup/reuse across sessions of the same project (SC-002).

### Project Index
A per-project git index file used when staging that project's
checkpoint, avoiding cross-project index contention.

| Field | Type | Notes |
|---|---|---|
| `path` | `PathBuf` | `store/indexes/<hash16>` |

**Validation rules**: Must be set via `GIT_INDEX_FILE` env var on every
git invocation for that project's checkpoint operations; never shared
between two different projects' `hash16` values (this is what allows two
concurrent `joey` sessions in *different* projects to run checkpoint
operations without racing on the same index file — per Edge Case: "two
`joey` sessions run concurrently in different working directories").
Two sessions in the *same* project directory still share one index file
and one ref — git's own file locking on the index and ref serializes
concurrent writers safely (git index writes use lockfile-then-rename).

### Project Metadata
Small per-project record used for pruning and listing.

| Field | Type | Notes |
|---|---|---|
| `workdir` | `String` (absolute path) | Canonicalized working directory this project record tracks |
| `created_at` | `f64` / unix timestamp | Set once, preserved across updates |
| `last_touch` | `f64` / unix timestamp | Updated on every checkpoint; used for the 90-day stale-project retention window (FR-007) |

**Validation rules**: File path is `store/projects/<hash16>.json`; MUST
be updated (last_touch bumped) on every successful checkpoint. Used by
the prune pass (R7) to identify: orphaned records (`workdir` no longer
exists on disk) and stale records (`last_touch` older than 90 days).

### Checkpoint
A single commit within a project's history representing a snapshot of
the working tree at a point in time. **Unchanged concept from today** —
same externally-observable shape as the current `Checkpoint` struct.

| Field | Type | Notes |
|---|---|---|
| `number` | `usize` | Sequential per-project checkpoint number (parsed from commit message `[N] message`, unchanged format) |
| `commit_hash` | `String` | Full git commit SHA |
| `message` | `String` | User-supplied or auto-generated checkpoint message |
| `timestamp` | `String` | Git author date string |
| `files_changed` | `usize` | Count of files touched in that commit |

**Validation rules**: Numbering and lookup logic (`list()`, `revert()`)
are unchanged in *external behavior* (FR-006) — only the underlying git
plumbing (which ref/store they operate against) changes.

## State Transitions

```
CheckpointManager::new(work_tree)
  → cheap: resolve hash16, ref name, index path; probe `which git`
  → enabled = git_found (no store I/O yet — FR-001)

First checkpoint() call (mutating tool call OR explicit /checkpoint)
  → ensure_store_initialized()   [idempotent; creates store/ + info/exclude if absent]
  → ensure_project_registered()  [creates/updates projects/<hash16>.json]
  → stage + commit against refs/joey/<hash16> using indexes/<hash16>
  → opportunistic prune pass (throttled via .last_prune marker)

Subsequent checkpoint() calls
  → store already initialized (idempotent no-op check)
  → stage + commit (git dedupes unchanged blobs automatically)

revert(number)
  → unchanged behavior: checkout target commit's tree into work_tree,
    remove files added after that checkpoint
```

## Relationships

- One **Shadow Store** has many **Project Ref** / **Project Index** /
  **Project Metadata** entries (one triple per distinct `hash16`).
- One **Project Ref**'s history contains many **Checkpoint** commits.
- **Project Metadata.workdir** is the join key back to a real filesystem
  directory (used for orphan detection).
