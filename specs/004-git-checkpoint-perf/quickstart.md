# Quickstart: Validating the Git Checkpoint Startup Performance Fix

## Prerequisites

- Rust toolchain per `rust-toolchain.toml` (stable, already installed for
  this workspace).
- `git` installed and on `PATH` (required for checkpoints to be enabled
  at all — without it, checkpoints degrade gracefully per FR-008 and
  these scenarios are skipped, matching existing test pattern).
- A large test fixture directory to exercise startup-latency scenarios
  (e.g. `mkdir -p /tmp/big-repo/node_modules && (cd /tmp/big-repo &&
  for i in $(seq 1 20000); do : > node_modules/file_$i.txt; done)` or
  reuse an existing large `node_modules`/`target` tree already on disk).

## Setup

```bash
cd ~/Development/joey-agent
cargo build -p joey-tools
cargo build -p joey-cli
```

## Validation Scenario 1 — Startup is not blocked (SC-001, User Story 1)

```bash
# Baseline: checkpoints disabled.
time (cd /tmp/big-repo && JOEY_HOME=$(mktemp -d) \
  cargo run -p joey-cli -- --checkpoints=false --print-banner-and-exit)

# With checkpoints enabled (lazy init — should be within ~100ms of baseline).
time (cd /tmp/big-repo && JOEY_HOME=$(mktemp -d) \
  cargo run -p joey-cli -- --checkpoints=true --print-banner-and-exit)
```

**Expected outcome**: The two `time` measurements differ by no more than
~100ms (SC-001). Confirm via `strace`/`dtruss`/logging that no `git add`
or `git commit` subprocess runs before the prompt appears — only after an
explicit `/checkpoint` or a mutating tool call in a live session.

*(Note: `--print-banner-and-exit` is illustrative of the kind of
fast-exit flag needed to measure startup latency without an interactive
session; if no such flag exists yet, the task breakdown in `tasks.md`
should confirm/add a minimal non-interactive smoke path, or this
scenario is validated via `cargo test` timing assertions instead.)*

## Validation Scenario 2 — Repeated sessions reuse objects (SC-002, User Story 1)

```bash
export JOEY_HOME=$(mktemp -d)
cd /tmp/big-repo

# First session: triggers first-ever store + snapshot creation.
cargo run -p joey-cli -- --checkpoints=true <<< $'/checkpoint\n/exit\n'
du -sh "$JOEY_HOME/checkpoints/store"

# Second session, same project, no file changes: should add near-zero size.
cargo run -p joey-cli -- --checkpoints=true <<< $'/checkpoint\n/exit\n'
du -sh "$JOEY_HOME/checkpoints/store"
```

**Expected outcome**: Store size after the second session is
approximately unchanged (git dedupes identical blobs/trees automatically
since no files changed).

## Validation Scenario 3 — Checkpoint/revert semantics unchanged (User Story 2)

```bash
cargo test -p joey-tools checkpoint_lifecycle -- --nocapture
cargo test -p joey-tools checkpoint_noop_on_no_changes -- --nocapture
```

**Expected outcome**: Both existing tests pass unmodified in *assertions*
(file-state outcomes identical to before this feature), even though their
setup may be adjusted to reflect the new lazy-init/shared-store API.

## Validation Scenario 4 — Bounded disk usage via pruning (User Story 3, SC-003)

```bash
cargo test -p joey-tools --lib -- prune  # new pruning-focused tests added per tasks.md
```

**Expected outcome**: A test simulating >2GB of tracked content (or a
mocked size-cap threshold) shows oldest checkpoints dropped first until
under cap; a test simulating an orphaned project (workdir deleted) shows
its ref/metadata removed on the next prune pass; a test simulating a
project whose `last_touch` is >90 days old shows it pruned as well.

## Validation Scenario 5 — Git subprocess timeout enforcement (SC-005, Edge Cases)

```bash
cargo test -p joey-tools --lib -- timeout  # new timeout-focused test(s)
```

**Expected outcome**: A test that substitutes a slow/hanging fake `git`
script on `PATH` demonstrates the checkpoint operation returns
(failing gracefully, not hanging) within ~5 seconds, never blocking the
calling thread indefinitely.

## Full regression gate

```bash
cargo build --workspace
cargo test --workspace
```

**Expected outcome**: Both commands succeed with zero failures — this is
the constitution-mandated acceptance bar (Principle VII: `cargo build
--workspace` and `cargo test --workspace` MUST stay green on every
increment).
