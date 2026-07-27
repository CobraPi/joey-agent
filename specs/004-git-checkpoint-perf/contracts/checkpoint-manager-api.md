# Contract: `CheckpointManager` internal API (joey-tools::vcs)

This is not an external network/CLI contract — it is the Rust module
boundary `joey-cli` (and any future caller) depends on. Per Principle VI
(Modularity and Decoupling), this is documented as the stable interface
other crates may rely on; internal helpers (store paths, git env
construction, pruning) are private implementation details.

## Public API (unchanged method signatures from today)

```rust
impl CheckpointManager {
    /// Construct a manager for `work_tree`. Cheap: resolves the project
    /// hash, probes `which git`. Performs NO filesystem/store mutation.
    /// (Behavior change: today this eagerly creates a shadow repo and
    /// commits the whole tree; after this feature, it does neither.)
    pub fn new(session_id: &str, work_tree: &Path) -> Self;

    /// Whether git was found on PATH (does NOT imply the store has been
    /// initialized yet — that happens lazily on first checkpoint()).
    pub fn is_enabled(&self) -> bool;

    /// Create a checkpoint. On first call for this manager, lazily
    /// initializes the shared store + this project's ref/index/metadata
    /// if not already present, then commits current file state.
    /// Unchanged return contract: Some(number) on success, None if
    /// disabled or the operation failed (bounded by the 5s git timeout).
    pub fn checkpoint(&mut self, message: &str) -> Option<usize>;

    /// List all checkpoints for this project (newest first). Unchanged
    /// return shape (`Vec<Checkpoint>`).
    pub fn list(&self) -> Result<Vec<Checkpoint>>;

    /// Revert the working directory to checkpoint `number`. Unchanged
    /// externally-observable behavior (file state restored exactly).
    pub fn revert(&self, number: usize) -> Result<()>;

    /// Session-end cleanup. Behavior change: today this deletes the
    /// per-session shadow repo directory entirely. After this feature,
    /// there is no per-session directory to delete (the shared store
    /// persists across sessions) — this becomes a no-op or is removed;
    /// callers (`joey-cli::repl`) must not assume it deletes checkpoint
    /// history.
    pub fn cleanup(&self);

    /// Path exposed for debugging/tests. Behavior change: now returns
    /// the shared store path, not a per-session directory.
    pub fn repo_path(&self) -> &Path;
}
```

## Behavioral contract changes (call-site impact)

| Aspect | Before | After | Caller impact |
|---|---|---|---|
| `new()` cost | Synchronous full repo init + whole-tree commit | Cheap struct construction only | `joey-cli::repl` startup call site becomes non-blocking; no code restructuring needed since `new()` was already called synchronously — it just becomes fast |
| `cleanup()` | Deletes per-session shadow repo | No-op (shared store persists) | `repl.rs`'s session-end call to `cp.cleanup()` remains a no-op-safe call; no compile-time API break |
| `repo_path()` | Per-session dir path | Shared store path | Any test/debug code reading this path must expect the shared store layout, not a session-scoped dir |
| First-use latency | N/A (already done at construction) | First `checkpoint()` call after `new()` pays the one-time store/project-init cost (excludes applied, so bounded even on large repos) | Turn loop calling `checkpoint()` after the first mutating tool call may see a small first-call latency bump vs. subsequent calls — acceptable per SC-001 (measured at startup, not at first tool call) |

## External CLI contract (`/checkpoint`, `/revert`) — UNCHANGED

Per FR-006 / spec Assumptions, the `/checkpoint` and `/revert` REPL slash
commands keep byte-identical externally observable behavior: same output
formatting, same numbering scheme, same revert semantics (added/modified/
deleted files restored exactly). No contract file needed for these since
they are not changing — regression coverage is via the existing
`checkpoint_lifecycle` / `checkpoint_noop_on_no_changes` tests plus new
tests added per the plan's Testing section, run against the rewritten
internals.
