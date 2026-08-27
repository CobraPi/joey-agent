# Research: Async Delegation & Subagent Control

**Date:** 2026-08-26
**Status:** All `NEEDS CLARIFICATION` items resolved prior to design via direct code exploration; all file:line references below verified against the working tree on 2026-08-26.

Each decision is recorded as: **Dn: Title** / Decision / Rationale / Alternatives considered.

---

**D1: Background execution model**
- **Decision:** Background requests spawn through the existing `SubagentManager` into a per-manager child-handle registry. The tool call returns immediately with a handle; completion is observed by a watcher consuming the existing orchestration tap + `JoinSet`, not by holding the tool future.
- **Rationale:** Reuses the dispatch path children already use (same semaphore, depth checks, toolset filtering); `JoinSet` futures are detached from the caller.
- **Alternatives considered:** Bare `tokio::spawn` per child (loses centralized limits/lifecycle); engine-owned job list (couples engine to delegation internals). Both rejected.

**D2: Completion notice delivery**
- **Decision:** Reuse `ToolContext::push_background_completion` (`joey-tools/src/context.rs:434`; queue cap 64, oldest dropped), drained at `run_turn` start (`agent.rs:2188`). Notices are formatted as distilled one-block strings: `[SUBAGENT COMPLETE|FAILED|STOPPED] <id> <goal> <outcome> <summary <=500 tokens> <tokens> <duration>`.
- **Rationale:** Identical mechanism to background terminal processes; zero new agent-core plumbing; FR-004 (distilled-only) satisfied by formatting at push time.
- **Alternatives considered:** A new dedicated mpsc channel into the agent (new plumbing, ordering risks); injecting `AgentEvent`s (events are observer-only, never model-visible). Both rejected.

**D3: Idle wake**
- **Decision:** Engine-layer wake: a completion watcher sends a new `EngineCommand` (`DelegationNoticePending`) into the engine's select loop when notices are queued and no orchestrator turn is active. The TUI `pump_one` (`tui.rs:904-967`) already selects over command channels — add one arm. The line REPL cannot be woken (reedline performs a synchronous stdin read, `repl.rs:732`): it degrades to next-interaction delivery; this is recorded as a Complexity Tracking deviation (Constitution II).
- **Rationale:** Honors FR-003 (proactive wake) on event-driven surfaces (incl. HyperCode engine) without replacing the line editor.
- **Alternatives considered:** Replace reedline with an async reader (rejected: core UX risk); poll timer printing to stdout (rejected: can't start a turn mid-read — only cosmetic).

**D4: Per-child control registry**
- **Decision:** `SubagentManager` keeps `HashMap<child_id, ChildHandle{interrupt: Arc<AtomicBool> (clone of the child Agent's), steer: Arc<Mutex<String>> (from Agent::steer_handle, agent.rs:698), spec, budgets, accumulated usage, started_at}>`; `steer_child`/`stop_child` operate on it.
- **Rationale:** Reuses existing mid-turn drain points (pre-API `agent.rs:2237`, post-tool-batch `:2492`) and per-iteration interrupt checks (`agent.rs:2220`); zero changes to the child Agent loop.
- **Alternatives considered:** `tokio::CancellationToken` (would require new check sites in agent-core; `AtomicBool` is already polled); `JoinHandle::abort` (abrupt — no partial summary, violates FR-010). Both rejected.

**D5: Stop reasons**
- **Decision:** The manager records a pending `StopReason` on the child record **before** setting its interrupt flag; when the child winds down (the `TurnAbort::Interrupted` path already produces a summary), `DelegationResult.stop_reason` is filled and a new `AgentEvent::SubagentStopped{id, goal, reason, summary_preview}` is emitted.
- **Rationale:** Distinguishes the FR-016 states; `AgentEvent` is `#[non_exhaustive]` (`events.rs:74`) and external consumers use wildcard arms — additive variant verified safe.
- **Alternatives considered:** Encoding the reason in an error string (unparseable, lossy); a separate callback registry (over-engineering). Both rejected.

**D6: Parent-enforced budgets**
- **Decision:** A budget watcher per running child aggregates tap events (`SubagentEvent{id, IterationStart{iteration,max_iterations}}` → turns; `ApiCallEnd{usage}` → cumulative tokens; `started_at` → wall clock); on breach it calls `stop_child(BudgetExceeded)`. Caps: `max_turns` (default: existing `delegation.default_max_turns`), `max_tokens`, `max_wall_clock_secs` (no default = unbounded unless set).
- **Rationale:** Parent-enforced per spec; no child-side changes; reuses events the tap already receives.
- **Alternatives considered:** Enforce inside child `run_turn` (couples agent-core to delegation semantics; a child could lie about its own usage); post-hoc accounting only (violates SC-004 "no further actions after breach detected"). Both rejected.

**D7: Reserved orchestrator capacity**
- **Decision:** Two-pool semaphore: the parent keeps the original N-permit semaphore; children acquire from a dedicated (N − reserve)-permit pool (`reserve` default 1, config `delegation.parent_reserved_permits`). A lightweight grant-back watcher honors FR-018's release clause: while the parent is idle (no parent provider call in flight) it temporarily `add_permits(1)` to the child pool, and reclaims + forgets one permit when parent activity resumes (permits are per-call, `agent.rs:1618-1622`, so a reclaimer always completes).
- **Rationale:** No priority queue in the provider hot path; the parent can never starve (SC-007); the reservation genuinely releases when idle.
- **Alternatives considered:** Static split only (violates FR-018 release clause); priority semaphore inside `transport_call` (touches hot path, complex ordering). Both rejected.

**D8: Control/inspection tool**
- **Decision:** One action-based tool `subagent_control` (toolset: `delegation`, joining `delegate_task` in `toolsets.rs:194-201`) mirroring the background process tool. Actions: `list` (running + completed-this-session records), `status{id}`, `log{id, last=N}` (bounded recent tap events), `wait{ids, timeout_secs}`, `steer{id, message}`, `stop{id}`. Also: `delegate_task` gains `background:boolean` (default `false`) and optional `budgets{max_turns,max_tokens,max_wall_clock_secs}`; name-lists that special-case `delegate_task` by name must gain the sibling: `guardrails.rs:54`, `compressor.rs:534`, `breakdown.rs:22`; the `delegate_task` description text (`delegation_tool.rs:238-249`) is updated to teach background mode + the control tool.
- **Rationale:** One registration path (`register_orchestration_inner`, `lib.rs:94`), familiar action UX, progressive disclosure protects orchestrator context.
- **Alternatives considered:** Five separate tools (registration/name-list churn, schema sprawl); exposing raw manager methods via existing tools (no schema home). Both rejected.

**D9: Operator controls (TUI)**
- **Decision:** When a subagent pane has focus: keybinding `x` = stop, `s` = steer (opens the existing text-input overlay). Actions flow `TuiAction::StopSubagent{id}` / `SteerSubagent{id,text}` → `EngineCommand` → `manager.stop_child(OperatorRequested)` / `steer_child` — the same path as the existing Esc interrupt (`app.rs:1112` → `tui.rs:1189` → `engine.rs:369`).
- **Rationale:** Reuses the focus model (`focus_subagent` `app.rs:2223`) and the proven action→command→manager pipeline; satisfies FR-017.
- **Alternatives considered:** A modal command palette (bigger UX surface); direct manager calls from the TUI (bypasses engine serialization of actions). Both rejected.

**D10: Session-end wind-down**
- **Decision:** New `SubagentManager::shutdown(timeout)` — signals every running child with `SessionEnd` reason, awaits bounded completion (config `delegation.wind_down_timeout_secs`, default 10), records final statuses; called from the REPL end_session path (`repl.rs:762`) and TUI exit.
- **Rationale:** FR-015; nothing exists today (no `Drop` impls in joey-orchestration); bounded wait can't hang exit.
- **Alternatives considered:** `Drop`-based cleanup (async in `Drop` is unsound; no timeout control); process exit without wind-down (violates FR-015, loses final statuses). Both rejected.

**D11: Retention**
- **Decision:** The delegation overview (running + completed records) is in-memory only, session lifetime (FR-019), discarded at wind-down/session end; durable copies exist only via the existing opt-in persist (subagent session SQLite).
- **Rationale:** Principle III (no new on-disk formats) untouched; matches the ephemeral-by-default subagent model.
- **Alternatives considered:** A new SQLite table (new on-disk format — violates this feature's no-new-formats constraint); JSON files (same problem). Both rejected.

**D12: HyperCode**
- **Decision:** No `hypercode.rs` changes are required for core behavior: it shares the engine agent's `SubagentManager` (`hypercode.rs:349-361`), so background dispatch, steer/stop, budgets, reserved capacity, and notices flow automatically; only an optional prompt-text mention of the new tool.
- **Rationale:** The feature is manager-level, so sharing the manager shares the feature.
- **Alternatives considered:** A parallel HyperCode-specific job system (duplicate surface, drift risk). Rejected.
