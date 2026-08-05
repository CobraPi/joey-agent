# Research: Terminal Async Performance & Streaming

**Feature**: 009-terminal-async-perf
**Date**: 2026-07-30

---

## R1: How to give tools access to emit progress events

### Decision

Add an optional `tokio::sync::mpsc::UnboundedSender<ToolProgress>` to
`ToolContext`'s inner struct, settable via a new builder method
`with_progress_sender()`. Default: `None` (existing callers unaffected).

### Rationale

- `ToolContext` is already `Clone` via `Arc<ContextInner>` and is cloned
  cheaply per dispatch (agent.rs:1929 clones it for concurrent fan-out).
- An `UnboundedSender` is `Clone + Send + Sync` — fits inside `Arc<ContextInner>`
  with no lifetime issues.
- The agent loop already owns the `mpsc::UnboundedSender<AgentEvent>` (repl.rs:691
  creates the channel, agent.rs:1820 receives `tx`). The agent can wrap it or
  create a dedicated progress sub-channel that it forwards to the main event
  channel.
- `Option<UnboundedSender<...>>` means tools that don't use it pay zero cost
  (a `None` check). Tools that do use it (terminal, process) check `.is_some()`
  and send progress deltas.
- The `Tool` trait signature (`async fn execute(&self, args: Value, ctx:
  &ToolContext) -> ToolResult`) does NOT change — the channel is accessed
  through `ctx`, preserving the trait contract (Constitution Principle VII).

### Alternatives considered

1. **Change `Tool` trait to take a progress callback parameter**: Rejected.
   Breaking change to a public trait (Principle VII). Would require updating
   every tool implementation and every test.

2. **Global static channel (like `PROCESS_REGISTRY`)**: Rejected. Couples
   tools to a global singleton, preventing multi-session isolation, and
   violates Principle VI (modularity — a tool should not depend on global
   mutable state for its event delivery).

3. **Store the sender in `SessionState` (the existing `Mutex`)**: Feasible but
   less clean — `SessionState` is mutable tool state (cwd, dedup caches),
   not an event-delivery concern. Mixing event plumbing into session state
   muddies the responsibility boundary.

---

## R2: Foreground streaming architecture

### Decision

Replace `run_bash`'s `spawn_blocking(|| reader.read_to_end(&mut buf))`
(terminal_tool.rs:468-472) with async chunked reading:

1. Create a `tempfile::NamedTempFile` at the start of execution.
2. Spawn the child with the existing `os_pipe` merged-stdout/stderr pipe.
3. In an async loop, read chunks (≤ 64 KB) from the pipe via
   `tokio::io::AsyncReadExt::read_buf`.
4. For each chunk: write to the temp file, decode as UTF-8 (lossy), and
   emit a `ToolProgress` event via `ctx.progress_sender()` if set.
5. Race the read loop against `tokio::time::timeout` for the timeout budget.
6. On completion/timeout/EOF: read the full output back from the temp file
   (for large outputs) or from an in-memory buffer (for small outputs —
   optimization: if total < threshold, skip temp-file readback).
7. Apply the existing post-processing pipeline (CWD marker extraction,
   truncation, ANSI stripping, redaction, exit-code interpretation) to the
   full output — unchanged.
8. Clean up the temp file.

### Rationale

**Key constraint**: the foreground path uses `os_pipe` to merge stderr→stdout
on a single pipe (upstream parity — documented in terminal_tool.rs:7), then
passes the os_pipe writer FD to the Command via `Stdio::from(writer)`
(terminal_tool.rs:454). This means `child.stdout` is **`None`** after spawn —
tokio only populates `child.stdout` when `Stdio::piped()` is used, not when
an arbitrary `Stdio::from(fd)` is passed. Reading must therefore go through
the `os_pipe` reader end, not through `child.stdout.take()`.

**Approach**: wrap the `os_pipe` reader's raw FD in `tokio::io::AsyncFd`
(available on Unix via `std::os::unix::io::AsRawFd`). This gives native async
read readiness notifications without `spawn_blocking`, keeping the single
merged pipe intact. Read chunks via `AsyncFd`'s readiness-based `readable()`
guard + `read()` (or use `tokio::io::AsyncReadExt` via a small adapter that
impls `AsyncRead` over the `AsyncFd`). Each chunk (≤ 64 KB) is then written
to the temp file and emitted as a `ToolProgress` event.

**Why not switch to `Stdio::piped()`?** That would require abandoning the
single merged-pipe contract or reading two separate pipes with interleaving
that doesn't match upstream's exact byte-order. The merged `os_pipe` approach
is the upstream-faithful design; we keep it and make the reader async.

### Temp-file strategy

- Use `tempfile::NamedTempFile` (auto-deleted on drop). Write chunks as they
  arrive. On completion, seek to 0 and `read_to_string` for the full output.
- For small outputs (< 4 KB threshold), skip temp file entirely — just use
  a `String` in memory (avoids filesystem overhead for the common case).
- This gives bounded memory (≤ 64 KB chunk buffer) while preserving full
  output for the model (read from temp file).

### Alternatives considered

1. **Keep `spawn_blocking` but emit progress from within it**: Rejected.
  `spawn_blocking` runs on a blocking thread and cannot send to async
  channels without additional plumbing. It also buffers everything in memory.

2. **Use `tokio::io::ReaderStream` + `StreamExt`**: Viable but adds a
  `tokio-util` dependency for `ReaderStream`. Simple async `read_buf` in a
  loop achieves the same with no new dependency.

3. **Pipe to temp file via shell redirection (`> /tmp/xxx`)**: Rejected.
  Changes the command semantics, breaks the merged-pipe contract, and loses
  the ability to emit live progress (output goes to file, not to our reader).

---

## R3: Background reaper task

### Decision

When `execute_background` spawns a child with `notify_on_complete=true`,
also spawn a tokio task ("reaper") that:

1. Takes ownership of `child.stdout` and `child.stderr` (`ChildStdout` /
   `ChildStderr` — both are `AsyncRead`).
2. Reads chunks from both in an async loop (via `tokio::select!`),
   pushing bytes into the session's `RingBuffer` (fixing the "pipes never
   read" bug).
3. After reading EOF from both, awaits `child.wait()`.
4. On completion: if `notify_on_complete` was set, sends a completion
   `AgentEvent` (new variant or `Notice`) via the progress channel.

The reaper stores the `JoinHandle` in the `ProcessSession` so it can be
awaited/cancelled on kill.

### Rationale

- The reaper must own the `ChildStdout`/`ChildStderr` handles. Currently
  `execute_background` stores the `Child` in `ProcessSession` with stdout/stderr
  still piped — but nobody reads them. The reaper takes them out before storing
  the child.
- `RingBuffer` is behind a `Mutex` (inside `ProcessSession` inside the global
  registry `Mutex`). The reaper locks the registry, gets the session, pushes to
  its ring buffers. This is the same lock `action_poll` / `action_log` /
  `action_wait` already use — no new locking concerns.
- For the completion notification to reach the agent, the reaper needs the
  progress sender from `ToolContext`. Since `ToolContext` is `Clone` (Arc), the
  reaper captures a clone at spawn time.

### Non-interrupting injection

Per spec FR-007: the completion result must be injected into the conversation
on the next turn, NOT preempt the current turn. The reaper sends a completion
event through the event channel; the agent loop queues it and processes it at
the next turn boundary (after the current turn's `Done`/`Failed`).

### Alternatives considered

1. **Poll-based reaper (periodic `try_wait`)**: Rejected. The existing
  `action_wait` already polls at 100ms intervals (process_tool.rs:331). A
  dedicated reaper task that `await`s the child is cleaner and more efficient.

2. **OS-level SIGCHLD handler**: Rejected. Platform-specific, fragile,
  unnecessary complexity when tokio already handles child process futures.

---

## R4: Elapsed-time indicator for silent commands

### Decision

In the foreground streaming loop (R2), if no output chunk has arrived for
a configurable interval (default: 2s), emit a `ToolProgress` event with
elapsed-time text (e.g. `"running… 12s"`).

Implemented via `tokio::select!` in the read loop:
```text
loop {
    tokio::select! {
        chunk = reader.read_buf(&mut buf) => { /* process chunk, reset timer */ }
        _ = interval.tick() => { /* emit elapsed progress */ }
    }
}
```

### Rationale

- Uses the existing `ToolProgress` event variant — no new event type.
- The render loop already handles `ToolProgress` (render.rs:744), so the
  elapsed indicator appears automatically in both CLI and TUI.
- The interval resets on each received chunk, so chatty commands don't spam
  elapsed indicators.

### Alternatives considered

1. **Separate timer task**: Rejected. Adds a second spawned task for something
  the read loop can handle inline with `select!`.

2. **Render-loop-side timer**: Rejected. The renderer doesn't know when a
  tool started (it only gets `ToolStart`/`ToolEnd`); a renderer-side timer
  would need new state tracking. Simpler to emit from the tool.

---

## R5: New dependency — `tempfile`

### Decision

Add `tempfile` as a dependency of `joey-tools`.

### Justification (Constitution Principle VIII)

| Criterion | Assessment |
|-----------|------------|
| Binary size | ~20 KB (the crate is small; no transitive deps beyond `cfg-if`, `fastrand`, `remove_dir_all` — several already in the tree via other deps). |
| Compile time | Negligible (< 1s incremental). |
| Why needed | Temp-file capture for large command output — required by spec FR-005/FR-006 (bounded memory + full output available to model). |
| Alternatives | (a) `std::fs::File` + manual temp path generation + manual cleanup — more code, less robust (no auto-delete-on-drop, race-prone path generation). (b) `tokio::fs::File` — async but doesn't auto-delete and requires manual path management. (c) Write to the existing SQLite session store — wrong abstraction (blob storage, not temp capture). |
| Verdict | `tempfile::NamedTempFile` is the standard, well-audited Rust crate for this exact use case. Its auto-delete-on-drop semantics guarantee cleanup even on panic/early-return, which is critical for not leaking temp files. |

### Alternatives considered (no dependency)

If adding a dependency is rejected, the fallback is: cap the in-memory buffer
at the existing `max_result_chars` limit (100 KB) and truncate with head+tail
markers (same as today's truncation). This violates spec clarification Q1
(user chose Option B: full output available to the model), so it is only a
fallback if the dependency is blocked.
