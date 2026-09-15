# Feature Specification: Subagent Resource Governance

**Feature Branch**: `030-please-implement-features`

**Created**: 2026-09-11

**Status**: Draft

**Input**: User description: "please implement these features to optimize resource utilization" — in full: the culprit is usually not the subagents themselves but unbounded concurrency plus a retry death spiral; diagnose before fixing (aggregate load from many moderate subagents vs. a few stuck ones burning CPU in loops; retry amplification where timeouts trigger retries that add load and cause more timeouts, turning 2x overload into 10x; control-plane starvation where the orchestrator process itself is starved of CPU and can no longer even make load-shedding decisions). Then apply six fixes in order of impact: (1) bound concurrency with a worker pool — route all CPU-heavy work through a bounded executor sized near one worker per physical core, cap the waiting-queue depth, and return an explicit busy signal when full so the orchestrator re-plans or defers instead of accumulating backlog; (2) break the retry death spiral — per-task timeouts, exponential backoff with jitter, a global retry budget, and checkpointable/idempotent tasks so a timed-out task resumes rather than restarts from scratch; (3) deduplicate work — a persistent result cache keyed on task signature (hash of inputs plus operation) and single-flight (an identical in-flight task is awaited, not duplicated); (4) isolate heavy work from the control plane — run subagent compute away from the orchestrator's own process, with hard per-task limits so a runaway degrades throughput but cannot take down the system; (5) fix pathological work via per-subagent resource accounting (CPU-seconds per task logged alongside existing token telemetry) to find the few offending patterns (re-parsing the same inputs every step, brute-force loops, missing memoization); (6) if load still exceeds capacity, defer and degrade — priority queue so critical subtasks jump the line, batch low-priority work into idle windows, and graceful degradation (sample a bounded fraction instead of processing everything) as an explicit fallback mode. Starting point: bounded pool plus timeouts eliminates most exhaustion by itself; per-task accounting then finds the 2–3 patterns responsible for the bulk of CPU burn, feeding the same optimization loop used for tokens.

## Clarifications

### Session 2026-09-11

- Q: Must persisted governance data (result cache, checkpoints, resource records) be secret-sanitized before writing to disk? → A: No — no redaction; governance data stays within the local installation's trust boundary (explicitly accepted posture, diverging from the Context Economy redaction convention).
- Q: Must all six governance mechanisms ship before this feature is complete, or is a smaller core an acceptable completion cut? → A: All six required — feature complete only when every mechanism ships and all success criteria (SC-001 through SC-007) pass.
- Q: Is degraded mode engaged automatically by the system, or explicitly selected? → A: Explicitly selected — the system surfaces sustained-overload signals; the assistant (when re-planning) or user configuration selects degraded mode; never silently engaged.
- Q: Which resource dimensions must the per-task ceiling enforce as hard limits? → A: Hard: CPU time + wall-clock time. Memory is advisory only (tracked and reported, never enforced).

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Concurrency Is Bounded with Admission Control (Priority: P1)

As a user running the assistant on a busy machine, I want subagent work capped to what the machine can actually sustain, so that a large parallel delegation speeds up my task instead of exhausting the machine and dragging everything — including my own session — to a halt.

All subagent work passes through a single bounded execution pathway with a maximum number of concurrently executing tasks (by default tied to the machine's capacity) and a capped waiting queue. Beyond those bounds, new work is refused with an explicit busy signal that tells the assistant the work was not started, so it can re-plan, defer, or run less in parallel. The backlog never grows silently.

**Why this priority**: This is the single biggest lever — the request itself notes that a bounded pool plus timeouts eliminates most exhaustion by itself. Every other fix depends on work flowing through this governed pathway.

**Independent Test**: Can be fully tested by requesting many more concurrent CPU-heavy subagents than the machine has processing units and observing that only the configured maximum run at once, the rest wait in a queue no deeper than its cap or are explicitly deferred, and the machine and the assistant remain usable throughout.

**Acceptance Scenarios**:

1. **Given** a delegation of more CPU-heavy subagents than the concurrent-task maximum, **When** they are dispatched, **Then** at most the configured maximum execute concurrently and all remaining work waits in a queue no deeper than its configured cap.
2. **Given** all execution slots busy and the queue at its cap, **When** another subagent task is submitted, **Then** the submitter receives an explicit busy indication stating the work was not started, rather than the work being silently queued or spawned.
3. **Given** a fresh installation with no user configuration, **When** subagent work is delegated, **Then** safe bounded defaults derived from the machine's capacity apply with no setup.

---

### User Story 2 - Retries Are Bounded, Spaced, and Resumable (Priority: P1)

As a user whose subagents occasionally time out or fail, I want retries to be limited in number, spaced out, and covered by a system-wide budget, and timed-out work to resume from where it stopped, so that one bad task cannot amplify into a self-inflicted overload that starves everything else.

Every subagent task carries a time budget; when it is exceeded the task is stopped and reported. Retries wait using exponential backoff with random jitter, and a system-wide retry budget caps how many retries may be in flight at once — beyond the budget, failures surface as failures instead of generating more load. Progress within a task is checkpointed, so a re-dispatched timed-out or failed task continues from its latest checkpoint instead of repeating completed work.

**Why this priority**: Retry amplification is the second half of "the big one" — the feedback loop that turns a 2x overload into a 10x one. Together with Story 1 it removes the two mechanisms that actually exhaust machines.

**Independent Test**: Can be fully tested by dispatching a mixed workload containing tasks that always fail, tasks that exceed their time budget once and then succeed, and tasks that time out mid-way through checkpointable progress: retries observed are bounded and spaced, the system-wide budget is never exceeded, and the resumed task does not repeat its completed steps.

**Acceptance Scenarios**:

1. **Given** a task that exceeds its time budget, **When** it is stopped, **Then** a timeout is reported and any retry honors exponential backoff with jitter before re-execution.
2. **Given** a system-wide retry budget of K retries in flight, **When** concurrent failures push retry demand past K, **Then** further retries are refused and surfaced as failures rather than accumulating load.
3. **Given** a task with recorded progress checkpoints that timed out mid-run, **When** it is re-dispatched, **Then** it resumes from its latest checkpoint instead of repeating completed work.

---

### User Story 3 - The Control Plane Stays Responsive Under Saturation (Priority: P2)

As a user, I want the orchestrating assistant itself to stay responsive even while its subagents saturate the machine, so that it can still make decisions — shedding load, deferring work, answering me — instead of freezing while the overload cascades.

Subagent execution is isolated from the orchestrator's own decision-making capacity, so that saturating subagent work cannot starve the assistant that manages it. A single runaway subagent is cut off by a hard per-task resource ceiling: it degrades that one task's outcome, never the stability of the rest of the system.

**Why this priority**: Control-plane starvation is what turns overload into total collapse — an orchestrator that cannot think cannot shed load. It ranks just below Stories 1–2 because it protects against their residual failure modes.

**Independent Test**: Can be fully tested by saturating all subagent capacity with heavy work while including one deliberately runaway task, then verifying the assistant still admits, refuses, and answers within a short bounded time and the runaway task is stopped by its ceiling while everything else continues.

**Acceptance Scenarios**:

1. **Given** all subagent execution capacity in use, **When** the assistant must make a scheduling decision or respond to the user, **Then** it does so within a bounded time rather than starving.
2. **Given** a subagent task that consumes resources without bound, **When** it exceeds its per-task resource ceiling, **Then** it is stopped and reported as failed-by-resource-limit while other tasks and the assistant continue unaffected.
3. **Given** subagent work running at full saturation, **When** new user input arrives, **Then** the user is not blocked waiting for subagent compute.

---

### User Story 4 - Identical Work Runs Exactly Once (Priority: P2)

As a cost-conscious user, I want identical subagent work executed once — not five times — so that I do not pay repeatedly, in time and in CPU, for the same answer whether the duplicates arrive simultaneously or across runs.

Before any subagent task executes, its signature — the identity of its operation plus every input that affects its result — is checked against a persistent result cache (a hit returns the recorded result without execution) and against work already in flight (an identical running task is awaited rather than duplicated).

**Why this priority**: Deduplication is a large saving with small risk, but it is a secondary pathology — it presumes the governed execution pathway of Stories 1–2 already exists to route through.

**Independent Test**: Can be fully tested by submitting several identical requests simultaneously and then the same request again after a restart: the first set produces exactly one execution with every requester receiving its result, and the post-restart request returns the recorded result without re-execution.

**Acceptance Scenarios**:

1. **Given** several identical subagent requests submitted near-simultaneously, **When** they are processed, **Then** exactly one executes and every requester receives its result.
2. **Given** a task whose signature completed and was recorded earlier, **When** the same task is requested again including after a restart, **Then** the recorded result is returned without re-execution.
3. **Given** two tasks that differ in any input that affects their result, **When** their signatures are computed, **Then** they are treated as different tasks, with no cross-contamination of recorded results.

---

### User Story 5 - Every Task's Cost Is Accounted and Diagnosable (Priority: P2)

As a user optimizing how my assistant uses the machine, I want each subagent task's resource consumption — compute time, waiting time, retries, and outcome — recorded alongside the token telemetry I already have, so that I can see which few task patterns burn the bulk of the CPU and fix those specifically instead of guessing.

Every subagent task carries a resource record produced next to the existing token accounting, individually attributable and aggregatable. Together the records answer the three diagnostic questions from the request without extra instrumentation: is load aggregate (many moderate tasks) or pathological (a few stuck ones); is retry amplification occurring; is the orchestrating process itself being starved.

**Why this priority**: Accounting is where the compounding wins are — it converts scheduler tuning into targeted fixes for the 2–3 offending patterns and feeds the same optimization loop already used for tokens. It requires the governed pipeline of Stories 1–2 to attribute costs per task, so it follows them.

**Independent Test**: Can be fully tested by running a mixed workload seeded with a few deliberately heavy, stuck, and retrying tasks, then identifying from the records alone which tasks consumed the most compute and which pathology occurred.

**Acceptance Scenarios**:

1. **Given** any completed subagent task, **When** its record is inspected, **Then** compute time, queue wait, retry count, and outcome are present and joinable with the task's token telemetry.
2. **Given** a workload dominated by a few stuck CPU-burning tasks, and separately a workload of many moderate tasks, **When** their records are compared, **Then** the aggregate-versus-pathological distinction is evident from the records alone.
3. **Given** a run in which retries amplified load, **When** records are reviewed, **Then** the retry-driven growth is visible, with retries counted and attributed per task.

---

### User Story 6 - Overload Defers and Degrades Gracefully (Priority: P3)

As a user whose workload genuinely exceeds capacity, I want critical subtasks to jump the line, low-priority work deferred to idle windows, and an explicit clearly-marked degraded mode as the fallback, so that the system stays useful and honest under overload instead of thrashing or silently cutting corners.

Work carries a priority class: higher-priority queued work is admitted ahead of lower-priority work as capacity frees, and deferrable work can wait for idle capacity. When load still exceeds capacity, degraded mode is available as an explicit, deliberately selected fallback: the system surfaces sustained-overload signals, and the assistant — when re-planning — or user configuration makes the selection; it is never engaged silently. Degraded mode processes an explicit bounded sample of the work instead of everything, and every output produced under it is marked as degraded so samples are never mistaken for complete results.

**Why this priority**: This is the last resort by design — needed only when load still exceeds capacity after Stories 1–5 have bounded, budgeted, deduplicated, and accounted for work.

**Independent Test**: Can be fully tested by submitting more work than capacity with mixed priorities: higher-priority work completes first, deferrable work runs in idle windows, and enabling degraded mode yields clearly-marked bounded-sample results while critical work still runs at full fidelity.

**Acceptance Scenarios**:

1. **Given** a saturated system with queued work of mixed priority, **When** execution capacity frees, **Then** higher-priority work is admitted before lower-priority work.
2. **Given** sustained overload, **When** degraded mode is selected, **Then** the system processes an explicit bounded sample and every degraded output is marked as degraded.
3. **Given** degraded mode active, **When** critical-priority work arrives, **Then** it is never silently sampled or dropped — it runs at full fidelity or fails with an explicit reason.

### Edge Cases

- All execution slots busy and the queue full when critical-priority work arrives — the priority policy lets critical work jump the line; any lower-priority work displaced by the jump is explicitly reported as deferred rather than silently dropped.
- A task times out before recording any checkpoint — restart is permitted, but it is counted as a full restart against the retry budget, not as a resume.
- The retry budget is exhausted while tasks are still failing — those tasks fail fast with a clear reason; nothing is silently dropped and nothing is re-queued without bound.
- A runaway task hits its resource ceiling mid-execution — its partial result is discarded or marked failed-by-resource-limit, and the orchestrator is informed; the rest of the system is unaffected.
- Two tasks with nearly identical inputs but different outcomes-relevant details — signatures must distinguish any input that affects the result, so a recorded result is never served across differing signatures.
- Outputs produced under degraded mode flow into later work — the degraded marking travels with them so downstream consumers cannot mistake samples for complete results.
- The system restarts with work queued or checkpointed — queued work is either safely re-dispatched or explicitly reported as dropped, checkpointed tasks resume, and nothing half-runs silently.
- A machine with very few processing units (or a tightly constrained environment) — defaults remain bounded and safe, respecting a minimum of at least one execution slot.
- All governance mechanisms disabled — the system behaves exactly as before the feature existed, with no residual overhead or telemetry side effects.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST route all delegated subagent execution through a single bounded execution pathway with a configurable maximum number of concurrently executing tasks; beyond that maximum, no additional subagent work starts.
- **FR-002**: The waiting queue for subagent work MUST have a configurable depth cap; when execution slots and queue are both full, newly submitted work MUST be refused with an explicit busy indication stating the work was not started.
- **FR-003**: Default bounds MUST be derived from the machine's available capacity — on the order of one concurrent execution slot per processing unit for CPU-heavy work — and MUST require no user configuration to be safe.
- **FR-004**: Every subagent task MUST carry a configurable time budget; exceeding it MUST stop the task and report a timeout.
- **FR-005**: Retries MUST be spaced with exponential backoff including random jitter, and a system-wide retry budget MUST cap the number of retries permitted in flight; once the budget is exhausted, further retries MUST be refused and reported as failures rather than adding load.
- **FR-006**: The system MUST support resumable tasks: progress checkpoints recorded during execution let a re-dispatched timed-out or failed task continue from its latest checkpoint instead of repeating completed work; a task restarted without any checkpoint MUST be counted as a full restart against the retry budget.
- **FR-007**: The system MUST consult a persistent result cache keyed on task signature — the operation plus every input that affects the result — before executing subagent work; on a hit, the recorded result MUST be returned without execution. The cache MUST survive restarts and MUST NOT serve a recorded result across differing signatures.
- **FR-008**: The system MUST implement single-flight deduplication: an incoming task whose signature matches work already executing MUST await that work's result rather than launch a duplicate.
- **FR-009**: Subagent execution MUST be isolated from the orchestrator's own decision-making capacity such that full subagent saturation cannot prevent the orchestrator from scheduling, shedding load, and responding to the user within a bounded time.
- **FR-010**: Each subagent task MUST be subject to a hard per-task resource ceiling on CPU time and wall-clock time; memory MUST remain advisory only — tracked and reported, never enforced. Enforcement MUST degrade that one task's result, not the stability of the rest of the system.
- **FR-011**: Every subagent task MUST produce a resource record — compute time, queue wait, retry count, and outcome — alongside the existing token telemetry, individually attributable and aggregatable across tasks.
- **FR-012**: Resource records MUST collectively make the three overload pathologies distinguishable without extra instrumentation: (a) many moderate tasks versus a few stuck tasks, (b) retry amplification, and (c) starvation of the orchestrating process itself.
- **FR-013**: Subagent work MUST support priority classes: higher-priority work MUST be admitted ahead of lower-priority queued work, lower-priority work MUST be deferrable to idle capacity, and a degraded mode processing an explicit bounded sample MUST exist as a deliberately selected fallback — surfaced by sustained-overload signals and selected by the assistant or user configuration, never engaged silently — whose outputs are always marked as degraded; critical-priority work MUST never be silently sampled or dropped.
- **FR-014**: All governance mechanisms MUST be active by default on a fresh installation, each individually disableable through the existing configuration surface; with any mechanism disabled, everything it governs MUST behave exactly as it did before the feature existed.
- **FR-015**: The feature MUST NOT alter existing public surfaces: existing configuration keys, on-disk formats and their versions, command behavior, and delegation result semantics keep working unchanged; any new settings are strictly additive.

### Key Entities *(include if feature involves data)*

- **Work Pool**: the single bounded admission and execution pathway for subagent tasks, defined by its maximum concurrent tasks, queue depth cap, and busy indication behavior.
- **Task Signature**: the deterministic identity of a subagent task — its operation plus every input that affects its result — used for cache lookups and single-flight matching.
- **Result Cache Entry**: a recorded task result keyed by task signature, persisting across restarts.
- **Retry Budget**: the system-wide cap on retries permitted in flight at any moment.
- **Checkpoint**: recorded progress within a task that enables resuming a re-dispatched task instead of restarting it.
- **Resource Record**: the per-task accounting of compute time, queue wait, retries, and outcome, joinable with token telemetry.
- **Priority Class**: an admission-ordering label on subagent work governing queue jumping and idle-window deferral.
- **Degraded Mode**: the deliberate, explicitly selected fallback (chosen by the assistant or user configuration on sustained-overload signals, never auto-engaged) in which a bounded sample of work is processed and all resulting outputs are marked degraded.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Under a delegated workload of at least 3x the concurrent-task maximum, 100% of admissible work completes or fails-with-explicit-reason without the orchestrator hanging, crashing, or becoming unresponsive to the user.
- **SC-002**: For tasks that fail permanently, total retries per task never exceed the configured allowance, and system-wide concurrent retries never exceed the retry budget — 100% adherence in verification runs.
- **SC-003**: When 5 identical subagent requests are submitted simultaneously, exactly 1 execution occurs and all 5 requesters receive the result; re-requesting the same task after a restart returns the recorded result with 0 additional executions.
- **SC-004**: While subagent execution is fully saturated, at least 95% of orchestrator scheduling decisions and user-visible responses complete within twice their unsaturated latency.
- **SC-005**: 100% of completed subagent tasks have resource records joinable with token telemetry, and from records alone an operator correctly identifies seeded pathologies (stuck-task burn versus aggregate load; retry amplification; control-plane starvation) in at least 90% of seeded diagnostic runs.
- **SC-006**: Under sustained overload with mixed priorities, higher-priority queued work is admitted before lower-priority work in at least 95% of admission decisions, and 100% of degraded-mode outputs carry the degraded marking.
- **SC-007**: With every governance mechanism disabled, behavior across the standard regression scenarios is 100% identical to the pre-feature system.
- **SC-008**: Persisted governance data (cache entries, checkpoints, resource records) stays within the local installation — 100% readable only by the local user context in verification checks — with no network exfiltration and no redaction machinery applied to it.

## Assumptions

- "Subagents" refers to the assistant's existing delegated child-agent machinery; this feature governs and instruments that existing pathway — it does not introduce a new kind of subagent.
- The request's "spend an hour instrumenting first" is rollout practice, not runtime behavior; the runtime obligation it translates to is that the diagnostic telemetry exists (covered by the resource-record requirements), not that the system enforces any diagnosis delay.
- Defaults follow the request's guidance: execution slots on the order of one per processing unit, queue depth on the order of twice the slots, and a small-integer system-wide retry budget; exact default values, cache scope, and eviction behavior are planning decisions.
- Enforcement of per-task resource ceilings may use platform resource-control facilities; the choice of mechanism is a planning decision and must work on every platform the project supports.
- Governance ships enabled by default with per-mechanism switches through the existing configuration surface, matching the repository's established default-on convention; disabled means exact pre-feature behavior.
- The result cache is local to the installation; no new services, accounts, or network dependencies are introduced.
- Checkpoint granularity depends on what each task's work naturally exposes; this spec requires resume support and honest accounting of full restarts, not a specific checkpoint format.
- The waiting queue is in-memory and process-scoped: subagent dispatchers are in-process callers, so a process restart ends callers and queued work together — nothing is silently lost across a restart, and checkpointed tasks resume via their resume tokens.
