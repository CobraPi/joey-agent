---

description: "Task list for feature 009-terminal-async-perf"
---

# Tasks: Terminal Async Performance & Streaming

**Input**: Design documents from `/specs/009-terminal-async-perf/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: Regression tests for the `ToolContext` public-surface change are MANDATORY per Constitution Principle VII. Implementation tests for new behavior are included alongside implementation per existing workspace convention.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

- Rust workspace under `crates/`. Paths are relative to repo root.
- `crates/joey-tools/src/` — terminal tool, process tool, tool context
- `crates/joey-agent-core/src/` — agent turn loop, events
- `crates/joey-cli/src/` — REPL (read-only for this feature)

---

## Phase 1: Setup

**Purpose**: Add the one new dependency and verify the workspace still builds.

- [X] T001 Add `tempfile` dependency to `crates/joey-tools/Cargo.toml` `[dependencies]` section (crate `tempfile`, features `["all"`] not needed — default features suffice). Run `cargo build -p joey-tools` to verify it compiles. See `specs/009-terminal-async-perf/research.md` R5 for justification.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: The progress channel that both US1 (streaming) and US3 (background notification) depend on. MUST be complete before any user-story work begins.

**⚠️ CRITICAL**: No user story work can begin until this phase is complete.

- [X] T002 Add optional `progress_sender: Option<tokio::sync::mpsc::UnboundedSender<String>>` field to `ContextInner` in `crates/joey-tools/src/context.rs`. Add builder method `with_progress_sender(self, sender: Option<UnboundedSender<String>>) -> Self` and accessor `progress_sender(&self) -> Option<&UnboundedSender<String>>`. Default to `None` in `ToolContext::new` and `with_interactive` so existing callers are unaffected. See `specs/009-terminal-async-perf/contracts/progress-channel.md`.
- [X] T003 Wire the progress sender into the agent turn loop in `crates/joey-agent-core/src/agent.rs::execute_tool_calls`. Before dispatching each tool call (both parallel and sequential branches), set a progress sender on the cloned `ToolContext` that forwards `String` progress to the main `AgentEvent` channel as `AgentEvent::ToolProgress { name, progress }` (attaching the current tool name). Create a dedicated `mpsc::unbounded_channel` for progress, spawn a forwarding task that maps incoming `String` → `AgentEvent::ToolProgress`, and pass the sender to `ctx.with_progress_sender(Some(sender))`. This reuses the existing `ToolProgress` variant in `events.rs:79` — NO new event variant needed.
- [X] T004 Regression test: verify `ToolContext::new(...)` still works without calling `with_progress_sender` and that `progress_sender()` returns `None`. Verify `ToolContext` is still `Clone` and `Send + Sync`. Add to `crates/joey-tools/src/context.rs` inline `#[cfg(test)]` module or `crates/joey-tools/tests/context_progress.rs`. This is MANDATORY per Constitution Principle VII (public-surface backward compatibility).

**Checkpoint**: Progress channel infrastructure ready — user story implementation can now begin.

---

## Phase 3: User Story 1 - Live Output During Long Commands (Priority: P1) 🎯 MVP

**Goal**: Replace the blocking `read_to_end` in `run_bash` with async chunked reads that stream `ToolProgress` events and write to a temp-file capture, so users see live output within ~1 second instead of a frozen screen.

**Independent Test**: Run `for i in $(seq 1 10); do echo "tick $i"; sleep 1; done` via the agent and confirm each line appears within ~1 second (not all at once at the end). See `specs/009-terminal-async-perf/quickstart.md` Scenario 1.

### Implementation for User Story 1

- [X] T005 [US1] Replace `run_bash` in `crates/joey-tools/src/tools/terminal_tool.rs` (currently lines 431-500) with an async streaming version. Preserve the same function signature `run_bash(command, cwd, timeout_secs) -> (String, i64, bool)` and the timeout contract (exit code 124 on timeout, `[Command timed out after Ns]` suffix appended by caller). Steps: (a) keep `os_pipe` to merge stderr→stdout on a single pipe and pass the writer to the tokio Command as `Stdio::from(writer)` (unchanged); (b) after spawn, `child.stdout` will be `None` (tokio doesn't populate it for `Stdio::from(fd)`) — instead wrap the `os_pipe` reader's raw FD in `tokio::io::AsyncFd` for native async read-readiness (see research.md R2 for rationale); (c) in an async loop, read chunks (≤ 64 KB) via `AsyncFd` readiness + `read()`; (d) for each chunk: write to a temp file (or in-memory `String` if total < 4096 bytes — see T006) AND emit a `ToolProgress` event via `ctx.progress_sender()` if set; (e) race against `tokio::time::timeout` for the timeout budget; (f) on EOF/timeout, read the full output back (from temp file or in-memory string); (g) run the existing post-processing pipeline (CWD marker extraction, truncation, ANSI stripping, redaction, exit-code interpretation) unchanged. Drop the separate `spawn_blocking(read_to_end)` entirely. See `specs/009-terminal-async-perf/research.md` R2 and `contracts/terminal-streaming.md`.
- [X] T006 [US1] Implement temp-file capture logic within the streaming `run_bash` in `crates/joey-tools/src/tools/terminal_tool.rs`: create a `tempfile::NamedTempFile` at start; append each chunk via `file.write_all(&chunk)`; track `total_bytes`. If `total_bytes` stays below 4096, skip temp-file I/O and use an in-memory `String` instead. On completion, seek to 0 and `read_to_string` from the temp file for the full output. The `NamedTempFile` auto-deletes on drop — no manual cleanup needed, but verify it drops even on early return/panic. On temp-file creation failure (disk full/permissions), fall back to in-memory `String` with head/tail truncation (same as current behavior). See `specs/009-terminal-async-perf/data-model.md` TempFileCapture entity.
- [X] T007 [US1] Implement output-chunk coalescing/throttling within the streaming read loop in `crates/joey-tools/src/tools/terminal_tool.rs`: if chunks arrive within a 50ms window, coalesce them into a single `ToolProgress` event to avoid flooding the event channel during output bursts (spec edge case: "output produced faster than displayed"). Track last-emit timestamp; if within 50ms, append to a pending buffer instead of emitting immediately; flush on next tick or when buffer exceeds 64 KB. See `specs/009-terminal-async-perf/contracts/terminal-streaming.md` Throttling section.
- [X] T008 [US1] Implement elapsed-time indicator for silent commands in the streaming read loop in `crates/joey-tools/src/tools/terminal_tool.rs`: wrap the read loop in `tokio::select!` with a 2-second interval timer. If no output chunk has arrived for ≥ 2 seconds, emit a `ToolProgress` event with the text `"running… Ns"` (where N = elapsed seconds since spawn). Reset the timer on each received chunk so chatty commands never emit this. This satisfies FR-011 and SC-008. See `specs/009-terminal-async-perf/research.md` R4.
- [X] T009 [US1] Regression test: verify the terminal tool result JSON schema is unchanged after streaming refactor — assert `{output, exit_code, error}` fields are present, `exit_code` is correct for a simple `echo hello` command, and `error` is null on success. Also assert that a command with no progress sender set (None) still returns the correct result (backward-compatible path). Add to `crates/joey-tools/tests/terminal_streaming.rs`. MANDATORY per Constitution Principle VII.
- [X] T010 [US1] Test streaming behavior: verify that `ToolProgress` events are emitted during a long-running command when a progress sender is set. Use a mock command (e.g. `echo line1; sleep 1; echo line2`) and assert at least 2 progress events are received before the tool returns. Verify the final result contains both lines. Add to `crates/joey-tools/tests/terminal_streaming.rs`.
- [X] T011 [US1] Test temp-file round-trip: run a command producing > 4 KB of output and verify the full output is available in the final result (read back from temp file), memory usage stays bounded (no holding full output in memory), and the temp file is cleaned up after readback. Add to `crates/joey-tools/tests/terminal_streaming.rs`.

**Checkpoint**: User Story 1 is fully functional — foreground commands stream live output with bounded memory and the same final result as before. The "silent freeze" is eliminated.

---

## Phase 4: User Story 2 - Agent Stays Responsive While a Command Runs (Priority: P2)

**Goal**: Verify and ensure that the UI render loop and Ctrl-C interrupt remain functional for the entire duration of any running terminal command. The async architecture from US1 (T005) is the primary fix — `spawn_blocking` no longer stalls the turn-driving task — but interrupt propagation and render-loop liveness must be verified.

**Independent Test**: Run `sleep 60` in the interactive REPL. During those 60 seconds confirm the UI spinner animates and Ctrl-C cancels within ~3 seconds. See `specs/009-terminal-async-perf/quickstart.md` Scenario 3.

### Implementation for User Story 2

- [X] T012 [US2] Verify and fix Ctrl-C interrupt propagation during streaming terminal commands. The REPL sets an atomic interrupt flag (repl.rs:711 `interrupt.store(true, ...)`) but the terminal tool's read loop (T005) must also check it so the command stops promptly. Add an interrupt check to the streaming read loop in `crates/joey-tools/src/tools/terminal_tool.rs`: check `ctx` interrupt state at each loop iteration (or add it to the `tokio::select!` alongside the timer from T008). On interrupt: stop reading, kill the child (`child.start_kill()`), preserve partial output captured so far (read from temp file/memory), and return the partial result with exit_code set appropriately. See `specs/009-terminal-async-perf/contracts/terminal-streaming.md` and agent.rs:1822-1831 for the existing pre-flight interrupt pattern.
- [X] T013 [US2] Test interrupt during streaming: start a long-running command (e.g. `sleep 30`), trigger the interrupt flag after ~1 second, and verify the tool returns within ~3 seconds with partial output preserved and the child process killed. Add to `crates/joey-tools/tests/terminal_streaming.rs`.

**Checkpoint**: User Stories 1 AND 2 are both functional — the program stays responsive and interruptible for the full duration of any running command.

---

## Phase 5: User Story 3 - Background Jobs Actually Notify on Completion (Priority: P3)

**Goal**: Fix the inert `notify_on_complete` flag by adding a reaper task that reads the child's pipes (filling the empty `RingBuffer`), awaits child exit, and fires a completion event when `notify_on_complete` is set.

**Independent Test**: Launch a background job with `notify_on_complete=true` that sleeps ~5 seconds, then continue a separate conversation turn. Confirm a completion notification appears automatically when the job finishes — no manual poll/wait needed. See `specs/009-terminal-async-perf/quickstart.md` Scenario 5.

### Implementation for User Story 3

- [X] T014 [US3] Add `ProcessOutcome` struct and new fields to `ProcessSession` in `crates/joey-tools/src/tools/process_tool.rs`: add `reaper_handle: Option<tokio::task::JoinHandle<()>>`, `completed: Option<ProcessOutcome>`, and `completion_notified: bool` fields. Define `ProcessOutcome { exit_code: i64, stdout_tail: String, stderr_tail: String, truncated: bool, elapsed_secs: f64 }`. Initialize new fields to `None`/`false` in `ProcessSession::new`. See `specs/009-terminal-async-perf/data-model.md` ProcessSession and ProcessOutcome entities.
- [X] T015 [US3] Implement the reaper task in `crates/joey-tools/src/tools/process_tool.rs`. The reaper is spawned by `execute_background` in `terminal_tool.rs` when a child is created. It: (a) takes ownership of `child.stdout` and `child.stderr` (both `tokio::process::ChildStdout`/`ChildStderr` — native `AsyncRead`); (b) reads chunks from both via `tokio::select!`, pushing bytes into the session's `RingBuffer` (locking the global registry, getting the session, pushing); (c) after EOF on both, awaits `child.wait()`; (d) on completion, sets `session.completed = Some(ProcessOutcome { ... })` using the exit code and ring-buffer tails; (e) if `session.notify_on_complete` is true and `session.completion_notified` is false, sends a completion event via the progress sender (from the `ToolContext` captured at spawn time) and sets `completion_notified = true`. Store the `JoinHandle` in `session.reaper_handle`. See `specs/009-terminal-async-perf/research.md` R3.
- [X] T016 [US3] Update `execute_background` in `crates/joey-tools/src/tools/terminal_tool.rs` to take `child.stdout` and `child.stderr` before storing the child (so the reaper owns them), capture a clone of `ctx` (with progress sender) for the reaper, and spawn the reaper task from T015. The stored `Child` in `ProcessSession` now has no stdout/stderr handles (both taken) — update the `action_kill` path in `process_tool.rs` to also abort the reaper handle if present (`reaper_handle.take()` + `abort()`).
- [X] T017 [US3] Update `action_wait` in `crates/joey-tools/src/tools/process_tool.rs` to check `session.completed` first (if set, return immediately with the outcome) before falling through to the poll loop. This ensures that once the reaper has recorded completion, a `process(action="wait")` call returns instantly instead of polling. Also update `action_poll` to check `completed` and include the exit code in its output if the process has finished.
- [X] T018 [US3] Wire the completion event into the agent's event flow in `crates/joey-agent-core/src/agent.rs`. The reaper sends a progress message (via the progress sender from T003) that the agent forwards as an `AgentEvent` — either reuse `AgentEvent::Notice(...)` with a formatted completion string, or add a minimal new variant. The completion must be non-interrupting (queued for the next turn, NOT preempting the current turn). The event must carry: session ID, exit code, bounded output tail. See `specs/009-terminal-async-perf/contracts/terminal-streaming.md` Background Process Completion Event section.
- [X] T019 [US3] Test reaper ring-buffer filling: spawn a background process that produces output (e.g. `echo hello; sleep 1; echo world`), wait for it to complete, then `action_poll` or `action_log` and verify the output is present in the `RingBuffer` (it was previously always empty — this is the core bug fix). Add to `crates/joey-tools/tests/process_reaper.rs`.
- [X] T020 [US3] Test completion notification: spawn a background process with `notify_on_complete=true`, verify the completion event fires exactly once when the process exits, and that `session.completed` is set with the correct exit code and output tail. Verify `completion_notified` prevents double-firing. Add to `crates/joey-tools/tests/process_reaper.rs`.
- [X] T021 [US3] Regression test: verify existing `action_list`, `action_kill`, `action_close` still work correctly with the modified `ProcessSession` (new fields don't break existing behavior). Verify `action_kill` properly cleans up the reaper task. Add to `crates/joey-tools/tests/process_reaper.rs`.

**Checkpoint**: All three user stories are independently functional. Background jobs now read their pipes and notify on completion.

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Full-workspace validation, edge-case coverage, and documentation.

- [X] T022 [P] Run `cargo build --workspace` and `cargo test --workspace` to verify the entire workspace is green (all ~520+ existing tests pass plus new tests). Fix any regressions. This is the constitutional acceptance bar.
- [X] T023 [P] Run the quickstart validation scenarios from `specs/009-terminal-async-perf/quickstart.md` manually (Scenarios 1-6) to verify end-to-end behavior.
- [X] T024 Code cleanup: remove any dead code from the old `spawn_blocking` path, ensure no warnings (`cargo build --workspace 2>&1 | grep warning`), verify temp files are cleaned up (check `/tmp` for leftover `joey-*` or `.tmp*` files after test runs).
- [X] T025 Update `PORTING.md` if any upstream-parity aspect of the terminal tool's behavior changed (it should not — output schema is unchanged — but verify and document).

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately.
- **Foundational (Phase 2)**: Depends on T001 (tempfile dep available for T005 later). BLOCKS all user stories.
- **US1 (Phase 3)**: Depends on Phase 2 (progress channel T002-T004). No dependency on other stories.
- **US2 (Phase 4)**: Depends on Phase 3 US1 (T005 — the streaming loop that T012 adds interrupt checks to). Cannot be done before US1's streaming loop exists.
- **US3 (Phase 5)**: Depends on Phase 2 (progress channel T002-T003 for reaper event delivery). Independent of US1/US2.
- **Polish (Phase 6)**: Depends on all user stories being complete.

### User Story Dependencies

- **User Story 1 (P1)**: Depends on Foundational only. No cross-story deps.
- **User Story 2 (P2)**: Depends on US1's streaming loop (T005) — the interrupt check (T012) is added to that loop.
- **User Story 3 (P3)**: Depends on Foundational only. Independent of US1/US2. Can proceed in parallel with US1 after Phase 2.

### Within Each User Story

- Implementation tasks before tests (except regression tests for public surfaces, which can be written alongside).
- T005 (streaming loop) before T006 (temp-file in that loop) before T007 (coalescing in that loop) before T008 (timer in that loop).
- T014 (ProcessSession fields) before T015 (reaper that sets those fields) before T016 (wire reaper into execute_background).

### Parallel Opportunities

- T002 and T003 are sequential (T003 uses T002's API) but both are in Phase 2.
- T004 (regression test for ToolContext) can run in parallel with T002/T003 if written test-first.
- US1 (Phase 3) and US3 (Phase 5) can proceed in parallel after Phase 2 — they touch different files (`terminal_tool.rs` vs `process_tool.rs`), though T016 in US3 modifies `terminal_tool.rs::execute_background` (conflict with T005's `run_bash` — coordinate).
- T019, T020, T021 (US3 tests) are independent test files that can be written in parallel.
- T022, T023 in Polish phase are parallel.

---

## Parallel Example: After Phase 2 Completes

```bash
# Developer A: User Story 1 (foreground streaming)
Task T005: streaming run_bash in terminal_tool.rs
Task T006: temp-file capture in terminal_tool.rs
Task T007: coalescing in terminal_tool.rs
Task T008: elapsed-time indicator in terminal_tool.rs

# Developer B: User Story 3 (background reaper) — in parallel
Task T014: ProcessSession fields in process_tool.rs
Task T015: reaper task in process_tool.rs
# T016 (wire into terminal_tool.rs) waits for Developer A's T005 to land
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (T001 — add tempfile dependency).
2. Complete Phase 2: Foundational (T002-T004 — progress channel + regression test).
3. Complete Phase 3: User Story 1 (T005-T011 — streaming + temp-file + tests).
4. **STOP and VALIDATE**: Run quickstart Scenarios 1, 2, 4, 6 — verify live streaming, large output, silent-command indicator, and backward compat.
5. The perceived "freeze" is now eliminated for the majority of cases.

### Incremental Delivery

1. Setup + Foundational → progress channel ready.
2. Add US1 → test independently → the silent freeze is gone (MVP!).
3. Add US2 → test independently → Ctrl-C works during long commands.
4. Add US3 → test independently → background jobs notify on completion.
5. Each story adds value without breaking previous stories.

---

## Notes

- [P] tasks = different files, no dependencies on incomplete tasks.
- [Story] label maps task to specific user story for traceability.
- Each user story is independently completable and testable.
- Regression tests for `ToolContext` (T004) and terminal result schema (T009) are MANDATORY per Constitution Principle VII — do not skip them.
- Commit after each task or logical group.
- Stop at any checkpoint to validate a story independently.
- Avoid: vague tasks, same-file conflicts without coordination, cross-story dependencies that break independence.

---

## Phase 7: Convergence

**Purpose**: Close the gap found by `/speckit-converge` between the US3 spec
intent and the current delivery path. Generated 2026-07-30.

- [ ] T026 Make background-process completion delivery survive the launching turn (FR-007, FR-008, US3/AC1, edge-case "owning session/turn already ended") (partial). Today the reaper routes the completion notice through `ctx.emit_progress` → the per-tool-call forwarding task in `Agent::ctx_for_tool` (`crates/joey-agent-core/src/agent.rs`), which captured the *per-turn* `mpsc::UnboundedSender<AgentEvent>`. The REPL creates a fresh channel per turn (`crates/joey-cli/src/repl.rs` `run_turn_interactive`) and drops it when the turn ends, so a background job that finishes in a LATER turn delivers nothing (the send fails silently via `let _ =`). Within-turn delivery works, and `ProcessOutcome` is still recorded/retrievable via `process(wait|poll)` — only automatic cross-turn delivery is missing. Fix: route the completion to a session-persistent surface (e.g. a session-scoped `Arc<Mutex<Vec<...>>>` pending-notice queue on the agent, or a long-lived channel distinct from the per-turn event channel) so that (a) the user still gets a visual `AgentEvent::Notice`/`BackgroundComplete` and (b) the result is non-interruptingly injected into the conversation for the agent's next turn (queued, never preempting the in-progress turn). The `ToolContext` captured by the reaper must reference this persistent surface, not the per-turn sender. Add a test that launches a background job, ends the launching turn, starts a new turn, and asserts the completion (exit code + bounded output tail) is surfaced without a manual `process(wait|poll)` call and without preempting the new turn.
