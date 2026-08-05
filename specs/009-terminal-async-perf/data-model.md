# Data Model: Terminal Async Performance & Streaming

**Feature**: 009-terminal-async-perf
**Date**: 2026-07-30

---

## Entities

### OutputChunk

A bounded chunk of bytes read from the child process's merged stdout/stderr
pipe during an async read loop. Never persisted as a standalone entity — it
is a transient value processed on-the-fly.

| Field | Type | Description |
|-------|------|-------------|
| bytes | `Vec<u8>` (≤ 64 KB) | Raw bytes from the pipe. |
| text | `String` (lossy UTF-8) | Decoded text for display/delta. |

**Lifecycle**: read from pipe → written to temp file (or appended to in-memory string for small outputs) → emitted as `ToolProgress` delta → discarded.

---

### TempFileCapture

The on-disk backing store for a command's complete output, created at the
start of a foreground terminal execution and cleaned up after the final
result is read back.

| Field | Type | Description |
|-------|------|-------------|
| file | `tempfile::NamedTempFile` | Auto-deleted on drop. Written incrementally. |
| total_bytes | `u64` | Running total of bytes written. |
| threshold | `usize` (const: 4096) | Outputs below this size skip the temp file and use an in-memory `String` instead. |

**Lifecycle**: created at execution start → chunks appended during read loop → on process exit, full output read back via `seek(0) + read_to_string` → temp file dropped (deleted).

**State transition**: open → writing → reading-back → deleted.

**Validation rule**: if the temp file cannot be created (permissions, disk full), fall back to in-memory `String` with head/tail truncation (same as current behavior). The command still runs; only the "full output available to model" guarantee is relaxed.

---

### ProcessSession (MODIFIED)

Existing entity in `process_tool.rs:76`. Changes are additive.

| Field | Type | Status | Description |
|-------|------|--------|-------------|
| session_id | `String` | unchanged | Unique identifier. |
| child | `Option<Child>` | changed | Now stored WITHOUT stdout/stderr handles (taken by reaper). |
| stdout_buf | `RingBuffer` | unchanged | 256 KB ring buffer for output. |
| stderr_buf | `RingBuffer` | unchanged | 256 KB ring buffer for output. |
| command | `String` | unchanged | Original command string. |
| cwd | `String` | unchanged | Working directory. |
| started_at | `Instant` | unchanged | Start time for elapsed display. |
| notify_on_complete | `bool` | unchanged | Flag read by reaper (was inert, now functional). |
| last_poll_pos | `usize` | unchanged | Incremental read position. |
| **reaper_handle** | `Option<tokio::task::JoinHandle<()>>` | **NEW** | Handle to the reaper task; awaited on kill, cancelled on close. |
| **completed** | `Option<ProcessOutcome>` | **NEW** | Set by reaper on completion; read by poll/wait/list. |
| **completion_notified** | `bool` | **NEW** | Ensures the completion event fires exactly once. |

---

### ProcessOutcome (NEW)

The final outcome of a background process, stored in `ProcessSession.completed`.

| Field | Type | Description |
|-------|------|-------------|
| exit_code | `i64` | Process exit code (same semantics as foreground: signal as negative, timeout as 124). |
| stdout_tail | `String` | Bounded tail of stdout (from ring buffer, truncated for display). |
| stderr_tail | `String` | Bounded tail of stderr. |
| truncated | `bool` | Whether output was truncated by the ring buffer. |
| elapsed_secs | `f64` | Total wall-clock duration. |

---

## Event Flow (not an entity — documents the data flow)

```
┌─────────────────────────────────────────────────────────────┐
│ FOREGROUND                                                   │
│                                                              │
│  child.stdout ──(async read chunks)──► TempFileCapture      │
│                                   ├──► ToolProgress event ──► AgentEvent channel ──► renderer
│                                   └──► (elapsed-time tick if silent > 2s)
│                                                              │
│  on exit: read full output from TempFileCapture              │
│         → post-process (CWD/truncate/ANSI/redact)            │
│         → ToolEnd with full_result                           │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│ BACKGROUND                                                   │
│                                                              │
│  child.stdout ──┐                                            │
│                 ├──► reaper task ──► RingBuffer (stdout/stderr)
│  child.stderr ──┘                      │                     │
│                                        ├──► completion event │
│                                        │     (if notify)     │
│                                        └──► ProcessOutcome   │
└─────────────────────────────────────────────────────────────┘
```

---

## Validation Rules (from spec requirements)

1. **FR-002**: Output delta latency ≤ 1s → enforced by async read loop (yields on each chunk, no buffering).
2. **FR-005/FR-006**: Full output to model + bounded memory → enforced by temp-file capture (disk-backed, chunk buffer ≤ 64 KB).
3. **FR-007**: Background notify on completion → enforced by reaper task setting `completed` + firing event.
4. **FR-009**: Backward-compatible result schema → enforced by reusing the existing post-processing pipeline on the full output.
5. **FR-011**: Elapsed-time indicator → enforced by `tokio::select!` tick in the read loop (2s interval, reset on chunk).
