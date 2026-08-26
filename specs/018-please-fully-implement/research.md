# Research: Concurrent Agent Terminal Performance & UI Responsiveness

Phase 0 consolidation. Each entry: Decision / Rationale / Alternatives considered. All Technical Context unknowns resolved; no NEEDS CLARIFICATION remains.

## D1 — Where the terminal concurrency governor lives

- **Decision**: Process-global `TerminalGovernor` singleton in `crates/joey-tools/src/tools/terminal_governor.rs`, initialized lazily on first terminal execution, limit read once from config.
- **Rationale**: All agent-initiated terminal executions flow through `Terminal::execute` (`terminal_tool.rs:406`); a process-global mirrors the established `process_registry()` singleton precedent (`process_tool.rs:165`) and requires no changes to `register_all` construction, tool trait, or agent plumbing. One process hosts exactly one session's agents (CLI `main.rs`, TUI `spawn_engine`), so process scope == session scope as required by FR-003.
- **Alternatives**: (a) `Arc<Semaphore>` injected via constructor — rejected: touches `builtins.rs:19` and every construction site, no isolation benefit; (b) governor carried in `ToolContext` — rejected: per-dispatch context is rebuilt per call (`ctx_for_tool`), forcing a separate global registry anyway to share state; (c) governor in `joey-agent-core` — rejected: bypasses direct tool executions (tests, gateway paths) and inverts the DAG.

## D2 — Admission policy implementation (per-agent round-robin)

- **Decision**: Governor keeps `limit`, `active`, an insertion-ordered registry of per-agent FIFO wait queues, and a rotating cursor. On release, the next agent key after the cursor with a non-empty queue is admitted first (one request per turn), guaranteeing each waiting agent's next request runs within one admission cycle (spec clarification Q4). Agent key comes from `ToolContext.queue_key` (D6).
- **Rationale**: A plain `tokio::sync::Semaphore` admits FIFO globally, which lets one agent flooding 40 requests starve another agent's single request — exactly the failure User Story 2 forbids.
- **Alternatives**: (a) global FIFO Semaphore — rejected (starvation under burst); (b) interactive-vs-background priority tiers — rejected: spec Q4 selected round-robin; tiers add config surface without a spec'd need; (c) per-agent sub-caps — rejected: violates the single global cap semantics of FR-003.

## D3 — Default limit sizing (auto, clamped 4-16)

- **Decision**: Absent config or `0`/`auto` → `clamp(std::thread::available_parallelism().unwrap_or(8), 4, 16)`. Explicit positive integer → fixed. Env `TERMINAL_MAX_CONCURRENT` overrides config, mirroring the `TERMINAL_TIMEOUT` pattern (`terminal_tool.rs:40-44`). Clamp logic is a pure unit-testable function.
- **Rationale**: std-only (no new dep); matches spec clarification Q3. The clamp precedent in `joey-orchestration/src/capacity.rs:51,82` lives in a higher crate than `joey-tools` (DAG forbids reuse), so the ~10-line clamp is duplicated locally and cited.
- **Alternatives**: (a) fixed default 8 — rejected by spec Q4/Q3 decision; (b) reuse `SystemCapacity` — rejected: DAG violation (joey-tools cannot depend on joey-orchestration); (c) `num_cpus` crate — rejected: new dependency for one std call.

## D4 — Admission point and timeout semantics

- **Decision**: In `Terminal::execute`, acquire a slot BEFORE the platform spawn path (`run_command_unix:695` / windows twin `:894`); compute the per-call deadline AFTER admission succeeds, then pass it into `stream_output` unchanged.
- **Rationale**: FR-008 and the queue-wait edge case pin timeout to execution time, not queue residence; moving the deadline computation (currently before spawn) is the minimal change preserving all existing timeout behavior post-admission. Admission at the single choke point covers foreground, and is a no-op fast path when capacity is free (lone-agent ≤5% goal, SC-004).
- **Alternatives**: (a) admit inside `stream_output` — rejected: background/process-tool spawns bypass it; (b) deadline before admission — rejected: queue wait would consume call timeout, contradicting spec.

## D5 — Cancellation and cleanup

- **Decision**: The acquire future races the existing cooperative interrupt (`Arc<AtomicBool>` polled as a future); on interrupt: queued waiters deregister; running children get the existing `start_kill()` treatment (`terminal_tool.rs:784`). Slot release uses a drop-guard so completion, failure, cancellation, and panic all release capacity (SC-003's 2s / zero-orphan bound rides existing kill + 5s EOF-grace timeout at `:792`).
- **Rationale**: Reuses the only cancellation mechanism the codebase has (no `CancellationToken` dep); avoids inventing a second interrupt pathway.
- **Alternatives**: (a) tokio `CancellationToken` — rejected: new pattern not present upstream, adds surface; (b) no queue cancellation (wait it out) — rejected: violates FR-006/SC-003.

## D6 — Agent identity for round-robin keys

- **Decision**: Additive `ToolContext` field `queue_key: Option<Arc<str>>` + `with_queue_key()` builder; `joey-agent-core` `ctx_for_tool` sets it from the agent's stable identity (main agent id, subagent child id, background task id). Absent key (direct `tool.execute` calls, tests, gateway paths) falls back to a single shared default key — preserves behavior and never breaks back-compat.
- **Rationale**: `ctx_for_tool` (`agent.rs:3103`) already decorates per-call contexts (interrupt flag); one more additive Option is the established pattern; unit-struct `Terminal` needs no constructor change.
- **Alternatives**: (a) governor API keyed by raw caller thread/task id — rejected: unstable identity across retries; (b) new trait method on `Tool` — rejected: breaking-ish surface for metadata only.

## D7 — Events, indicator, and coalescing (FR-007/FR-011)

- **Decision**: Governor exposes `stats() -> {active, queued}`. Emissions ride the existing event channel as ONE additive `AgentEvent` variant (queued-state change, throttled to the existing 50ms producer throttle budget `terminal_tool.rs:1114`). CLI: `/status` (`repl.rs:2290`) prints active/queued; `render.rs` ToolStart vicinity gains a queued badge when queue depth > 0. TUI: `draw_status` (`widgets.rs:3059`) gains a contention span rendered ONLY when queued > 0 (no persistent chrome — spec Q2); `/status` (`tui.rs:1417`) extended. Bursts coalesce via existing producer throttle + TUI `frame_budget()` last-value-wins rendering; no new debounce machinery.
- **Rationale**: The additive-event pattern is documented at `events.rs:96-110`; both UIs already have the two surfaces FR-011 needs; reuse avoids new UI architecture. Verification task: confirm `AgentEvent` is (or is made) `#[non_exhaustive]`; update any workspace-internal exhaustive matches (constitution gate 2).
- **Alternatives**: (a) new global telemetry channel — rejected: parallel plumbing for one number; (b) per-chunk queue-depth events — rejected: floods the very UI this feature protects.

## D8 — Residual blocking process calls (FR-002)

- **Decision**: Fix the four UI-reachable sites: `joey-cli/src/clipboard.rs:34` (async `tokio::process` + `.wait().await`); `joey-cli/src/repl.rs:1462` and `joey-cli/src/tui.rs:2235-2240` (`/paste` osascript — move off the hot path via `tokio::process` await or `spawn_blocking`, following the `repl.rs:1050,2623` precedent; the TUI site is the critical one — it blocks the single pump task, stalling ALL rendering/input); `joey-omo/src/team.rs:341,475-491` (wrap `TmuxVisualizer` ops in `spawn_blocking` or convert `run_tmux` to async). `joey-providers/src/copilot.rs:283-304` (`gh auth token`) is setup-flow only, not UI-reachable — classified non-blocking-concern, optional hardening, out of FR-002's mandatory scope.
- **Rationale**: Each fix uses the narrowest established pattern at that site (spec Assumptions: async preferred, `spawn_blocking` for legacy-shaped code); FR-002 requires only interface-reachable paths.
- **Alternatives**: (a) leave tmux sync (team mode is opt-in) — rejected: it runs on the engine path during concurrent delegation, the exact scenario this feature targets; (b) blanket-convert every `std::process` in the workspace — rejected: out of scope, churn without user-visible gain.
