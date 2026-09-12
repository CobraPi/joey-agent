# Research: Subagent Resource Governance

Facts are file:line-sourced from the workspace (verified 2026-09-11). Each section: Decision / Rationale / Alternatives considered.

## R1: Execution substrate — extend the existing shared pool
**Decision**: Keep the existing child-slot semaphore pool (defaults `max_concurrent_children: 3`, manager.rs:62-63; admission at manager.rs:1030-1034; one pool shared across all dispatch calls per the comment at manager.rs:1454-1458) as the single admission point; add the waiting queue, priority lanes, and busy refusal at that boundary.
**Rationale**: The pool already exists, is shared across blocking singles, batches, and background waves, and has capacity-aware sizing via capacity.rs:94-114. Governing it rather than building a parallel executor is the minimal additive change (constitution VI, VIII).
**Alternatives considered**: (a) a new `joey-executor` crate with its own pool — rejected: duplicates existing machinery; (b) process-pool worker subprocesses — rejected: children are tokio tasks in-process (manager.rs:1508 JoinSet::spawn), and a subprocess layer would sever event taps and steering (subagent.rs:380,411; control_tool.rs:66-79) at high wiring cost; (c) queueing at the delegate_task tool layer only — rejected: background waves also consume slots and would bypass it.

## R2: Queue cap and busy signal
**Decision**: Bounded priority queue with default depth 2 × max_concurrent_children; when slots and queue are both full, dispatch returns a busy refusal: `[busy] delegation queue full (N waiting, cap M) — re-plan or defer` (exact contract in contracts/busy-and-outcomes.md). Critical priority may jump the line but never bumps queued work.
**Rationale**: Matches the request's own guidance (queue cap ≈ 2× pool size); explicit refusal lets the orchestrator re-plan instead of accumulating a hidden backlog.
**Alternatives considered**: (a) unbounded queue — rejected: silent backlog; (b) refuse-only with no queue — rejected: needlessly strict for bursts; (c) tool-layer queueing — rejected as in R1(c).

## R2b: Pool and queue sizing defaults
**Decision**: `delegation.max_concurrent_children` stays `auto` (capacity-derived); when it is auto or absent, queue cap = 2 × resolved child count; an explicit child count derives the queue cap from it. Both clamp to ≥ 1; the machine floor is one slot, never zero.
**Rationale**: capacity.rs already weighs CPU and memory, so `auto` remains the right default; deriving the queue from the resolved pool keeps the 2× relationship the request suggests.
**Alternatives considered**: (a) fixed 8-slot pool — rejected: ignores machine capacity; (b) CPU-count-only sizing — rejected: capacity.rs already weighs memory too; (c) separate pools for CPU-heavy vs IO-heavy work — deferred: no classification signal exists today.

## R2c: Busy-refusal shape
**Decision**: Busy is an immediate dispatch-level refusal, not a queued acknowledgment: the calling tool returns a busy ToolResult carrying current queue depth and cap; no side effects start, nothing is enqueued.
**Rationale**: FR-002 requires an explicit busy indication stating the work was not started; a side-effect-free refusal is trivially safe to retry or re-plan.
**Alternatives considered**: (a) queue-then-notify-busy later — rejected: violates 'refused, not queued'; (b) embedding a retry-after hint — deferred: the retry budget already governs retry pacing.

## R3: Wall-clock timeout and cancellation
**Decision**: Per-task wall-clock budget (default 600s) enforced with `tokio::time::timeout` around the child future inside dispatch; on expiry the child is cancelled via the existing interrupt path (interrupts always win, subagent.rs:333-336,445-465) and a timeout outcome is reported. A timeout consumes one retry attempt.
**Rationale**: tokio timeout is the portable in-process mechanism already used in this crate (control_tool.rs:429,479); cancellation reuses existing machinery instead of inventing kill paths.
**Alternatives considered**: (a) subprocess kill — rejected with R1(b); (b) soft deadline warnings only — rejected: the spec requires a hard stop; (c) CPU-time-based timeout — that is the ceiling (R5b), orthogonal to wall-clock.

## R3b: Global retry budget
**Decision**: A system-wide in-flight retry cap (default 2) enforced by a counting guard in the manager; when exhausted, a failing task fails fast with reason `retry budget exhausted`. Retry delays use `jittered_backoff_with(attempt, base, max)` from joey-providers error.rs:424 (base 2s, max 60s). Existing subagent recovery retries (subagent.rs:333-336; `subagent_recovery_attempts: 1` default) count against this budget.
**Rationale**: This is the anti-death-spiral mechanism (stops 2x overload becoming 10x); the workspace's jittered backoff is reused verbatim; counting existing recovery retries prevents double budgeting.
**Alternatives considered**: (a) per-task caps only — rejected: system-wide retry concurrency stays unbounded; (b) cost-weighted budget — deferred: no per-retry cost signal yet; (c) disabling existing recovery retries — rejected: regresses resilience.

## R3c: Checkpoint and resume
**Decision**: Turn-boundary checkpoints: after each child turn completes, a resume token (last completed turn index plus a transcript digest) is recorded in the resource record; a re-dispatched timed-out or failed task may present the token to skip completed turns and continue at the next turn boundary. A restart with no valid token counts as a full restart against the retry budget (FR-006). A resume that itself times out re-checkpoints and may resume again within the budget.
**Rationale**: Turn boundaries are the natural checkpoint points for an LLM agent and map directly onto the existing per-turn loop (subagent.rs:311 run, run_with_tap:337); the digest is cheap to compute.
**Alternatives considered**: (a) op-level checkpoints — rejected: no op-level idempotency interface exists; (b) full-transcript replay — rejected: re-burns input tokens, defeating the purpose; (c) a separate durable checkpoint store — rejected: the token already lives in the resource record; no new store needed.

## R4: Persistent result cache
**Decision**: Cache at `~/.joey/delegation/result-cache.json`: envelope { schema_version: 1, entries: [ { signature, result_json, created_at, last_used_at } ] }; LRU eviction (default 256 entries), TTL 24h, exact-signature compare (the stored full signature string compared byte-for-byte; any hash is bucketing only); loaded and saved with the JobStore pattern (atomic_write_secure, temp + fsync + rename; tolerant load). Lookup happens before queue admission; a hit returns without consuming a slot. Successful results only.
**Rationale**: FR-007 requires persistence across restarts; exact compare eliminates collision risk entirely; the JobStore pattern is the workspace's proven atomic-persistence idiom; caching successes only keeps the cache small and lets the retry budget own failures.
**Alternatives considered**: (a) a SQLite table in the session store — rejected: the schema is Hermes-pinned (SCHEMA_VERSION 22); (b) in-memory only — rejected: FR-007; (c) hash-only compare — rejected: collision risk; (d) caching failures too — rejected: stale failure serving defeats the retry budget.

## R4b: Single-flight
**Decision**: An in-manager map from signature to in-flight task handle; a second dispatch with the same signature awaits the first's result and all requesters receive the same outcome; the entry is removed on completion. Order of checks: cache → single-flight → queue admission.
**Rationale**: Five identical requests must produce one execution (SC-003); trivially correct with one SubagentManager per process.
**Alternatives considered**: (a) dedup at the tool layer — rejected: bypasses background waves; (b) cross-process dedup — rejected: out of scope (single process).

## R4c: Signature definition
**Decision**: Signature = canonical (deterministic key order) JSON of { goal, context, toolsets, model_override, role, budgets { timeout_secs, cpu_ceiling_secs } } — every dispatch field that affects the result. Budget fields participate because they change what the result will be (a timed-out variant is a different task).
**Rationale**: Deterministic serialization keeps signatures stable across restarts (required for cache persistence); budget participation prevents serving a 600s-timeout result to a 60s-budget dispatch.
**Alternatives considered**: (a) goal-text-only — rejected: same goal with different context is a different task; (b) excluding budgets — rejected: cross-serving across budget variants is wrong; (c) a random UUID per dispatch — rejected: defeats dedup entirely.

## R5: Control-plane isolation — dedicated child runtime
**Decision**: A manager-owned multi-thread tokio Runtime dedicated to subagent execution, sized to max_concurrent_children worker threads; children run inside it while the parent's turn loop and provider calls keep the main runtime. `parent_reserved_permits` continues to reserve provider capacity for the parent.
**Rationale**: FR-009 demands that saturation cannot starve orchestrator decisions; a separate runtime gives the parent's scheduler its own threads that saturated children cannot occupy, at zero new dependencies and modest cost (pool ≤ 8 threads).
**Alternatives considered**: (a) subprocess per child — rejected (R1b); (b) same-runtime yielding — rejected: one runtime shares one scheduler, so saturation still delays the parent; (c) OS nice/priority — rejected: not portable and needs privileges; (d) cgroups — rejected: Linux-only; (e) nothing — rejected: FR-009 unmet.

## R5b: CPU ceiling — sampled watchdog with hard abort
**Decision**: Per-child CPU ceiling (default 300 CPU-seconds) enforced by a sampling watchdog: the manager samples child CPU time at a fixed interval (default 1s) using a portable mechanism (the exact mechanism — /proc, sysctl, or equivalent — is an implementation-level choice to be settled in tasks.md, with cross-platform support as a hard requirement); when cumulative CPU exceeds the ceiling the child is aborted via the same interrupt path as timeouts and reported `aborted_by_resource_limit`. The sampling interval is recorded in resource records.
**Rationale**: Hard abort satisfies FR-010's 'cannot exceed'; sampling yields the CPU-time attribution FR-011 needs at negligible cost (1s cadence vs multi-second LLM turns).
**Alternatives considered**: (a) cgroup CPU quota — rejected: Linux-only; (b) POSIX rlimit on CPU — rejected: signals the whole process, killing the parent too (children are in-process tasks); (c) application-level self-accounting — deferred: depends on turns self-reporting; (d) wall-clock-only enforcement — rejected: clarification Q4 requires the CPU dimension to be hard.

## R6: Memory — advisory only
**Decision**: Memory stays advisory (clarification Q4, answer A): peak process RSS is sampled by the same watchdog while a child runs, attributed to that child in the resource record, labeled sampled — and never enforced.
**Rationale**: Q4 explicitly made memory advisory; reuse of the watchdog keeps it cheap; per-child memory isolation would require subprocesses, already rejected.
**Alternatives considered**: (a) hard memory limit — rejected: contradicts Q4; (b) per-child subprocess for exact attribution — rejected (R1b); (c) skip memory entirely — rejected: FR-011 requires the dimension recorded.

## R7: Resource records — JSONL
**Decision**: Append-only `~/.joey/delegation/resource-records.jsonl`, one JSON object per terminal task outcome (completed, failed, timeout, aborted_by_resource_limit, busy_refused, cache_hit): { record_id, task_signature, priority, outcome, queue_wait_ms, compute_ms, cpu_ms, memory_peak_kb, retries, checkpoint, token_usage, degraded, created_at }. Sampled fields (compute_ms, cpu_ms, memory_peak_kb) are labeled by name. Appended at completion and at timeout/abort; retained under existing retention policies; no SQLite.
**Rationale**: JSONL append is O(1) and restart-tolerant; the field set is exactly what FR-012's three pathologies need (aggregate vs stuck: per-task cpu_ms; retry amplification: retries per record; starvation: queue_wait_ms plus parent-latency sampling); token_usage mirrors joey-providers Usage so records join with token telemetry.
**Alternatives considered**: (a) SQLite table — rejected (pinned schema); (b) per-run files — rejected: cross-run aggregation is the point; (c) in-memory only — rejected: today's DelegationResult.token_usage is discarded at session end (types.rs:332 'In-memory only').

## R8: Priority lanes and degraded mode
**Decision**: Three priority classes — critical / normal / background (default normal) — set per dispatch via an additive request field; background waves enqueue as background. When a slot frees, the highest-priority lane head is admitted; critical never preempts running work and never bumps queued work (jump-the-line only). Degraded mode is explicitly selected (clarification Q3): config key `delegation.degraded_mode.enabled` (default false) plus a sustained-overload signal (busy refusals ≥ 3 in 60s) surfaced in busy text so the assistant can select degraded mode when re-planning. Under degraded mode, background and normal work is sampled at `delegation.degraded_mode.sample_rate` (default 0.1) and every degraded output is marked; critical work is never sampled (contract in contracts/busy-and-outcomes.md).
**Rationale**: The classes map cleanly onto existing dispatch kinds; explicit selection follows Q3 answer B; the 10% default comes from the user's own request text.
**Alternatives considered**: (a) automatic degradation — rejected (Q3); (b) numeric priorities — rejected: unbounded values need clamping; (c) preemptive scheduling — rejected: wastes completed work; (d) skipping degraded mode — rejected: FR-013 requires it.

## R9: Config keys
**Decision**: All keys additive under the existing `delegation.*` namespace with defaults declared in joey-core's DEFAULT_CONFIG_YAML (full table in contracts/config-keys.md): resource_governance.enabled (true), max_queue_depth (auto), task_timeout_secs (600), retry_budget (2), backoff_base_secs (2.0), backoff_max_secs (60.0), checkpointing.enabled (true), result_cache.enabled / max_entries (256) / ttl_hours (24), single_flight.enabled (true), cpu_ceiling_secs (300), watchdog_interval_secs (1), memory_tracking.enabled (true), priority.enabled (true), degraded_mode.enabled (false) / sample_rate (0.1).
**Rationale**: Follows the exact joey-core pattern (defaults in DEFAULT_CONFIG_YAML, read via cfg.get_* dotted paths; the context_economy keys are precedent); dotted keys never route to .env (config.rs:1140-1149), so there are no secret-routing interactions.
**Alternatives considered**: (a) a top-level `resource_governance.*` namespace — rejected: every mechanism lives in the delegation path, so one namespace is cleaner; (b) defaults in joey-orchestration — rejected: joey-core owns DEFAULT_CONFIG_YAML; (c) per-mechanism switches at the tool layer — rejected: the manager is the single admission point.

## R10: Governance observability
**Decision**: Reuse the existing AgentEvent fan-out: new additive event variants (DelegationBusy, DelegationTimeout, DelegationRetryBudgetExhausted, DelegationCacheHit, DelegationDegradedOutput) plus a periodic capacity snapshot event; they flow through the existing event_tap (manager.rs:588) so TUI and CLI see them. tracing spans are unchanged.
**Rationale**: Additive enum variants are backward-compatible; the existing tap wiring already carries delegation events to UIs (constitution II parity).
**Alternatives considered**: (a) log-only — rejected: FR-011/012 need structured records; (b) a new event channel — rejected: additive variants are cheaper; (c) tracing-only — rejected: UIs would not see governance events.

## R11: PORTING.md duty
**Decision**: Feature 030 records a Joey-only-additions section in PORTING.md (in-process pool extension, dedicated child runtime, sampled CPU watchdog, JSONL records, persistent result cache — no upstream equivalent to port), with date, per AGENTS.md's expectation that PORTING.md tracks parity work.
**Rationale**: PORTING.md is the living audit document; repo convention requires the update.
**Alternatives considered**: none — it is a repo duty.

## R12: Risks (carry to tasks)
- Sampling attribution error (R5b, R6): CPU and memory samples are process-level and attributed to the running child — records must label them sampled; tests use generous tolerances.
- Runtime split (R5): the dedicated runtime must not break event taps or steering — mitigated by reusing existing tap wiring; isolation tests must prove parent responsiveness under child saturation.
- Cache correctness (R4c): the signature must include every result-affecting field; regression tests must pin signature stability across restarts.
- Parity risk (SC-007): governance disabled must equal pre-feature behavior — golden tests compare outputs with all governance off against the pre-feature baseline.
- Windows portability of CPU sampling (R5b): the chosen mechanism must be validated on Windows (CI or a manual validation note).
- Queue-jump fairness (R8): critical jump-the-line must not starve normal work — a bounded-wait test under continuous critical load is required.
