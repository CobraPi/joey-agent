# Phase 0 Research: Git Checkpoint Startup Performance

## R1: Implementation approach — shell out to `git` vs. `git2` (libgit2)

- **Decision**: Continue shelling out to the `git` binary via
  `std::process::Command`, as the current implementation already does.
- **Rationale**: Zero new dependency (Principle VIII — no new binary size
  / compile time / transitive surface to justify). The upstream
  `hermes-agent` reference implementation also shells out to `git`
  (`subprocess.run([...])` throughout `checkpoint_manager.py`), so this
  keeps behavior/edge-cases (timeout handling, config isolation via env
  vars, `GIT_INDEX_FILE`/`GIT_WORK_TREE`) directly portable/comparable.
  `git2`/libgit2 would add a substantial dependency (FFI bindings, a C
  library, longer compile times) for a feature whose whole point is lean
  startup performance — the marginal benefit (mainly: avoiding process
  spawn overhead, ~1-3ms per invocation) doesn't offset that cost, and
  process-level `Command::spawn` timeouts are simple to enforce (kill
  child after wall-clock deadline), whereas libgit2 has no native
  per-operation timeout primitive and would require a similar workaround
  (thread + channel) anyway.
- **Alternatives considered**: `git2` crate (rejected: dependency weight,
  no timeout primitive, divergence from proven upstream design);
  reimplementing a minimal object-store writer from scratch (rejected:
  reinvents git, loses `git gc`/pack-file optimization for free, much
  higher risk of subtle corruption bugs).

## R2: Single shared store layout — refs + indexes vs. one repo per project

- **Decision**: One shared bare repo at `~/.joey/checkpoints/store`, with
  per-project git refs (`refs/joey/<hash16>`) and per-project index files
  (`store/indexes/<hash16>`), keyed by `sha256(canonical_abs_path)[:16]`.
- **Rationale**: Directly mirrors the proven hermes-agent v2 design
  (`_project_hash`, `_ref_name`, `_index_path` in
  `checkpoint_manager.py`). Git's content-addressable object store
  dedupes blobs/trees across all projects/refs automatically — no custom
  dedup logic needed. Per-project index files avoid two sessions in
  different projects racing on a single shared index (`GIT_INDEX_FILE`
  env var scopes each git invocation to its own index).
- **Alternatives considered**: One bare repo per project (today's design,
  minus per-session churn) — rejected because it forfeits cross-project
  blob dedup (FR-002) and multiplies the number of `.git`-style dirs to
  prune/gc individually instead of one shared store with one `gc`.

## R3: Lazy/deferred initialization trigger point

- **Decision**: `CheckpointManager` construction becomes cheap/instant
  (just resolve paths, probe `git` on `PATH` via `which`, no filesystem
  mutation). The store directory, refs, and first snapshot are only
  created the first time `checkpoint()` (or equivalent "ensure a
  checkpoint exists for this turn" call) is invoked — i.e. on the first
  mutating tool call or explicit `/checkpoint`, per the spec's
  Clarification session 2026-07-24 answer to Q1 ("fully lazy").
- **Rationale**: This is the direct fix for FR-001/SC-001. It requires no
  background thread (simpler lifecycle, no shutdown races, no error
  reporting problem for a detached task) while still guaranteeing the
  interactive prompt is never blocked. `joey-cli::repl` today calls
  `CheckpointManager::new(...)` synchronously right on the startup path
  (`repl.rs` — commented "Initialize session-scoped filesystem
  checkpoints (fresh every session)"); this plan changes that call site
  to only build the struct (cheap) and defers store creation into the
  turn loop's existing `maybe_auto_checkpoint`/checkpoint-triggering
  logic.
- **Alternatives considered**: Background thread at startup (rejected per
  clarification answer — adds lifecycle/race complexity for no added
  benefit once init is already cheap enough to be inline-on-first-use);
  hybrid store-existence check at startup (rejected — even a "cheap"
  filesystem `stat` scan of a store plus ref existence check was judged
  unnecessary complexity vs. fully lazy, and the clarification session
  explicitly chose the simpler fully-lazy option).

## R4: Default exclude list contents

- **Decision**: Port hermes-agent's `DEFAULT_EXCLUDES` list verbatim
  (node_modules/, dist/, build/, target/, out/, .next/, .nuxt/,
  __pycache__/, *.pyc/*.pyo, .cache/, .pytest_cache/, .mypy_cache/,
  .ruff_cache/, coverage/, .coverage, .venv/, venv/, env/, .git/, .hg/,
  .svn/, .worktrees/, *.so/*.dylib/*.dll/*.o/*.a/*.jar/*.class/*.exe/
  *.obj, media/archive formats (*.mp4, *.mov, *.mkv, *.webm, *.zip, *.tar,
  *.tar.gz, *.tgz, *.7z, *.rar, *.iso), secrets (.env, .env.*,
  .env.local, .env.*.local), OS junk (.DS_Store, Thumbs.db), and *.log —
  applied via the shared store's `info/exclude` file (same mechanism as
  a repo-local `.git/info/exclude`, which git honors for `git add` /
  status without needing a tracked `.gitignore` in the user's tree).
- **Rationale**: This exact list is the reference implementation's
  battle-tested set (FR-003); reusing it verbatim avoids re-deriving
  which patterns matter and keeps behavior parity with the tool this
  feature explicitly references as the model to follow.
- **Alternatives considered**: A smaller/curated subset (rejected — no
  reason to diverge, spec explicitly cites hermes-agent's list as the
  standard to match); user-configurable list from day one (rejected —
  Assumptions section explicitly scopes exclude-list customization as an
  optional stretch goal, not required for this feature).

## R5: Git subprocess timeout enforcement mechanism

- **Decision**: Wrap every `git` invocation with a wall-clock timeout of
  5 seconds (per clarification), implemented by spawning the child
  process and polling/waiting with a deadline, killing the child process
  if the deadline is exceeded before the process exits (no data races —
  `std::process::Child::wait` in a loop with `try_wait()` + sleep, or a
  helper that spawns a watchdog thread that kills after the deadline).
- **Rationale**: Rust's `std::process::Command` has no built-in timeout;
  the standard pattern is `try_wait()` polled with a short sleep interval
  up to the deadline, then `child.kill()` — this requires no new
  dependency. This mirrors hermes-agent's use of
  `subprocess.run(..., timeout=_GIT_TIMEOUT)` (Python's `subprocess`
  timeout raises `TimeoutExpired` and kills the process for you; the Rust
  equivalent must be hand-rolled but is a well-known ~15-line pattern).
- **Alternatives considered**: `wait-timeout` crate (rejected — small,
  but avoidable; the hand-rolled poll loop is simple enough not to justify
  a new dependency per Principle VIII); async/tokio-based timeout
  (rejected — `joey-tools` already depends on `tokio` for other tools,
  but the checkpoint manager's call sites in `repl.rs` are synchronous;
  introducing async here would ripple into the REPL's control flow for
  no benefit — the poll-loop approach is simpler and self-contained).

## R6: Config isolation

- **Decision**: Every git invocation sets `GIT_CONFIG_GLOBAL=/dev/null`
  and `GIT_CONFIG_SYSTEM=/dev/null` (already present in the current
  implementation) — retained unchanged, applied consistently to *all*
  call sites including store init, per FR-004.
- **Rationale**: Already proven in the current codebase; hermes-agent
  additionally sets `GIT_CONFIG_NOSYSTEM=1` as "belt-and-suspenders" for
  older git versions — this plan adopts that too for parity, since it's a
  zero-cost env var addition.
- **Alternatives considered**: None — this is a settled, low-risk pattern
  already validated in production by both codebases.

## R7: Retention/pruning strategy

- **Decision**: On each triggered checkpoint (not startup), after
  committing, run a lightweight prune pass: (a) drop project metadata /
  refs for projects whose `workdir` no longer exists on disk (orphan), (b)
  drop project data untouched for >90 days (stale, per clarification),
  (c) if total store size (measured via directory walk of
  `store/objects`) exceeds 2GB, drop oldest checkpoints per project
  (walking each project's ref history oldest-first) until under cap, (d)
  run `git gc --prune=now` after any ref deletions to reclaim space, (e)
  cap snapshots per project at 50 by trimming the oldest checkpoint
  commits' reachability once exceeded to avoid unbounded ref history.
  Prune passes themselves are subject to the same 5s-per-git-call timeout
  and are only triggered opportunistically (e.g. throttled via a
  `.last_prune` marker file, not on every single checkpoint) so pruning
  itself never becomes a new startup-adjacent cost.
- **Rationale**: Directly matches hermes-agent's `prune_checkpoints`
  design (orphan + stale sweep, `git gc --prune=now`, size-cap pass
  dropping oldest first) — proven approach, satisfies FR-007/SC-002/SC-003.
- **Alternatives considered**: Pruning on every checkpoint call (rejected
  — adds latency to the already-latency-sensitive per-turn checkpoint
  path; a throttled marker-file approach, as hermes-agent uses, avoids
  this); external cron-based pruning (rejected — adds an OS-level
  scheduling dependency Joey doesn't otherwise need; in-process throttled
  pruning is simpler and self-contained).

## R8: Legacy per-session shadow repo handling

- **Decision**: On first store initialization, sweep
  `~/.joey/checkpoints/` for old per-session directories (non-`store`
  entries) and simply delete them (per clarification: "Discard — ...
  removed opportunistically ... never on the startup critical path").
  This sweep runs lazily as part of the throttled prune pass (R7), not
  eagerly at startup, so it never adds startup latency.
- **Rationale**: hermes-agent archives old data into a `legacy-<ts>/` dir
  rather than deleting it outright — but the clarification session for
  this feature explicitly chose the simpler "discard" behavior (old
  per-session repos are documented in the current `vcs.rs` as ephemeral
  by design, "cleaned up when the session ends"), so archiving would add
  unnecessary complexity/disk usage for data that was never meant to be
  durable.
- **Alternatives considered**: Archive-then-manual-clear (hermes-agent's
  approach) — rejected per explicit clarification answer favoring the
  simpler discard semantics for this codebase.

## Dependency Weight Summary (Principle VIII compliance)

No new crate dependencies are introduced by this feature. All
functionality uses already-present workspace deps: `std::process`,
`std::fs` (stdlib), `sha2`/`hex` (project hashing, already deps of
`joey-tools`), `which` (git availability probe, already a dep),
`serde`/`serde_json` (project metadata JSON, already deps), `tempfile`
(already a dev-dep for tests).
