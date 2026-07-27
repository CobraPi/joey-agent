# Implementation Plan: Git Checkpoint Startup Performance

**Branch**: `004-git-checkpoint-perf` | **Date**: 2026-07-24 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/004-git-checkpoint-perf/spec.md`

## Summary

Joey's session-start checkpoint system (`joey-tools::vcs::CheckpointManager`)
currently allocates a brand-new bare git repo per session and synchronously
scans/adds/commits the entire working tree before the REPL prompt appears,
with no excludes, no dedup across sessions, and no retention limits. This
plan redesigns it as a **single shared shadow git store** under
`~/.joey/checkpoints/store` (per-project refs `refs/joey/<hash16>` + per-
project git index files), with a **default exclude list** applied via the
store's `info/exclude`, **fully lazy initialization** (no store/snapshot
work at startup — first mutating tool call or explicit `/checkpoint`
triggers it inline), a **5-second timeout** on every git subprocess call,
config isolation (`GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` = devnull), and
**retention/pruning** (50 snapshots/project, 2GB total cap, 50MB max file
size, 90-day stale-project window, orphan pruning, `git gc --prune=now`).
Old per-session shadow repos are discarded opportunistically during the
first pruning pass, never on the startup path. The public `/checkpoint`
and `/revert` behavior is unchanged; this is an internal architecture
rewrite of `crates/joey-tools/src/vcs.rs`, mirroring the proven design in
`hermes-agent/tools/checkpoint_manager.py` but implemented in Rust by
shelling out to the `git` binary (no new dependency), consistent with the
existing `which`-based git-availability check already in use.

## Technical Context

**Language/Version**: Rust 1.75+ (stable, edition 2021, per `rust-toolchain.toml`)

**Primary Dependencies**: `std::process::Command` (shell out to `git`
binary, matching current implementation and hermes-agent's approach) +
`sha2`/`hex` (already workspace deps, used to compute the 16-hex-char
project hash analogous to Python's `hashlib.sha256(...)[:16]`) + `which`
(already a dep, git-availability probe) + `serde`/`serde_json` (already
deps, for `projects/<hash>.json` metadata) + `tempfile` (already a dev-dep,
for tests). No new crate dependency required.

**Storage**: Filesystem — single shared bare git repo at
`~/.joey/checkpoints/store` (honoring `JOEY_HOME`), plus
`store/indexes/<hash16>` per-project git index files and
`store/projects/<hash16>.json` per-project metadata records. Not a
database; append-only git history is the storage mechanism itself.

**Testing**: `cargo test -p joey-tools` (existing `vcs.rs` inline tests
`checkpoint_lifecycle`, `checkpoint_noop_on_no_changes` must keep passing
unmodified in *behavior* — the plan updates the test setup to reflect the
new shared-store/lazy-init API surface without changing what they assert
about `/checkpoint` + `/revert` semantics) plus new tests for: lazy-init
(no store touched before first mutating call), exclude-list application,
dedup/reuse across two `CheckpointManager` instances for the same project,
retention pruning (size cap + orphan + stale), and timeout enforcement
(simulated via a slow/hanging git-replacement script on `PATH`).

**Target Platform**: Same as rest of workspace — macOS/Linux (POSIX);
Windows is out of scope for this pass (existing code has no Windows-
specific handling either) but changes should not regress Windows building
(pure `std::process`/`std::fs`, no POSIX-only syscalls introduced).

**Project Type**: CLI (single Rust workspace, backend library crate change)

**Performance Goals**: SC-001 — starting `joey` in a 10k+-file repo with a
large excluded-pattern directory adds ≤100ms of observable startup latency
vs. checkpoints disabled. SC-002 — repeated session starts in the same
project show near-zero incremental disk usage when unchanged. Per-git-call
budget: 5s hard timeout (FR-005); expected common-case latency for a lazy
first snapshot after excludes is well under 1s for typical projects.

**Constraints**: FR-001 (no full-repo scan/add/commit at startup — must be
literally absent from the startup code path, not just fast), FR-004 (full
git-config isolation), FR-005 (5s subprocess timeout, all call sites),
FR-010 (`~/.joey/checkpoints/...` rooted, `JOEY_HOME`-aware).

**Scale/Scope**: Single crate (`joey-tools::vcs`), one call site changed
(`joey-cli::repl`), no public API/CLI surface change (Principle II/VII —
`/checkpoint` and `/revert` commands keep identical external behavior).
Store must remain correct under concurrent sessions in the same project
directory (shared store, per-project index avoids index-file races; ref
updates use git's own ref-locking).

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

- **I. Workspace-First Rust**: PASS. All changes confined to the existing
  `joey-tools` crate (`src/vcs.rs` and its test module); no new crate
  needed. `cargo build -p joey-tools` / `cargo test -p joey-tools` remain
  the isolated verification unit.
- **II. CLI/TUI Parity**: PASS. No new user-facing surface; `/checkpoint`
  and `/revert` (already reachable identically through `joey-cli`) are
  unchanged in behavior per FR-006. No TUI-specific checkpoint UI exists
  or is being added.
- **III. Filesystem Is the Source of Truth**: N/A. This feature does not
  touch `.specify/` spec-kit artifacts or any UI that visualizes them —
  the checkpoint store itself *is* filesystem state and remains so.
- **IV. Test-First for New Crates**: PASS (adapted — no new crate, but
  same spirit applies to the rewritten module). New tests for lazy-init,
  excludes, dedup, pruning, and timeout behavior are written alongside the
  implementation in this plan's task breakdown, not deferred.
- **V. Incremental, Reviewable Delivery**: PASS. Task breakdown (see
  `/speckit-tasks`) will decompose this into independently buildable
  increments: (1) shared-store + project hash/ref plumbing, (2) default
  excludes + config isolation + timeout, (3) lazy/deferred init trigger
  wiring in `joey-cli::repl`, (4) retention/pruning + legacy discard, each
  landing with green `cargo test -p joey-tools`.
- **VI. Modularity and Decoupling**: PASS. `CheckpointManager`'s public
  API (`new`, `is_enabled`, `checkpoint`, `list`, `revert`, `cleanup`,
  `repo_path`) stays the same shape so `joey-cli::repl` needs no
  structural changes beyond removing the eager-init call — internals
  (store path resolution, git env construction, pruning) are private to
  `vcs.rs`.
- **VII. Backward Compatibility and Non-Regression (NON-NEGOTIABLE)**:
  PASS with a noted internal-format break, justified below. `/checkpoint`
  and `/revert` CLI-facing behavior (FR-006) and the `CheckpointManager`
  Rust API surface used by `joey-cli` are unchanged — no public-surface
  break. The *on-disk layout* under `~/.joey/checkpoints/` changes (new
  shared `store/` replaces per-session dirs), but per FR-009/Assumptions
  this is explicitly scoped as ephemeral, discardable data, not a
  persisted user-facing format — see Complexity Tracking for the formal
  justification note.
- **VIII. Performance Discipline and Lean Code**: PASS — this feature's
  entire purpose is a performance fix. No new dependency added (shells
  out to `git`, as today). Performance budget recorded above (SC-001:
  ≤100ms startup delta; FR-005: 5s per-call timeout ceiling). Pruning
  uses `git gc --prune=now` (git's own optimized reclamation), avoiding a
  custom reimplementation.

No unjustified violations. One entry recorded in Complexity Tracking for
the on-disk layout change (informational, not a gate failure — Principle
VII's "public surface" explicitly excludes internal, ephemeral cache
layout per the feature's own Assumptions section).

## Project Structure

### Documentation (this feature)

```text
specs/004-git-checkpoint-perf/
├── plan.md              # This file (/speckit-plan command output)
├── research.md          # Phase 0 output (/speckit-plan command)
├── data-model.md        # Phase 1 output (/speckit-plan command)
├── quickstart.md        # Phase 1 output (/speckit-plan command)
├── contracts/           # Phase 1 output (/speckit-plan command)
└── tasks.md             # Phase 2 output (/speckit-tasks command - NOT created by /speckit-plan)
```

### Source Code (repository root)

```text
crates/joey-tools/
├── src/
│   └── vcs.rs            # CheckpointManager rewrite: shared store, refs,
│                          # per-project index, excludes, timeouts, pruning
└── tests/                # (inline #[cfg(test)] in vcs.rs retained;
                           # additional scenarios added there, no new file
                           # needed since existing convention is inline)

crates/joey-cli/
└── src/
    └── repl.rs            # Startup call site: replace eager
                            # `CheckpointManager::new(...)` construction
                            # with lazy construction deferred to first
                            # mutating tool call / `/checkpoint` command
```

**Structure Decision**: Single-project structure (this repo is already a
Cargo workspace, not a web/mobile split). All logic changes are confined
to `crates/joey-tools/src/vcs.rs` (the module being rewritten) with one
call-site adjustment in `crates/joey-cli/src/repl.rs` to move
`CheckpointManager` construction off the eager startup path. No new
crate, no new top-level directory.

## Complexity Tracking

> Recorded per Principle VII for transparency, not because a gate failed.

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|---------------------------------------|
| On-disk checkpoint layout changes (`~/.joey/checkpoints/<session_id>/` per-session dirs → single `store/` + refs/indexes/projects) | FR-002/FR-003/FR-007 require a single deduplicated store with per-project retention; a per-session-dir layout cannot dedupe objects or apply a global size cap without re-scanning every session dir | Keeping the old per-session layout and only adding excludes/timeouts would fix startup latency (FR-001/FR-004/FR-005) but not FR-002 (dedup) or FR-007 (bounded storage) — spec explicitly requires both; per FR-009/Assumptions the old layout is documented as ephemeral/discardable so this is not a persisted-format break |
