# Data Model: Subagent Resource Governance

Entities keyed to FR numbers (see [spec.md](spec.md)). Fields follow the Rust structs to be added in `crates/joey-orchestration` (types indicative; exact Rust types are a tasks.md/implementation concern).

## Work Pool (FR-001/002/003)
- Existing `child_slots` semaphore (manager.rs) extended with: `max_concurrent` (usize, config/capacity-derived, ≥1), `queue: VecDeque<QueuedTask>` (bounded by max_queue_depth), `priority_of(task) -> Priority` lane ordering (critical > normal > background, FIFO within lane).
- Validation: queue.len() <= max_queue_depth at all times (invariant; busy refusal otherwise); admission picks highest-priority lane head.
- State transitions: submitted → queued → running → terminal(completed|failed|timeout|aborted_by_resource_limit|busy_refused).

## QueuedTask (queue entry)
- Fields: dispatch request, signature (R4c), priority, enqueued_at, sender for result.
- Validation: signature computed once at submission; priority defaults to normal; background waves enqueue as background.

## Task Signature (FR-007/008)
- Canonical deterministic JSON string of { goal, context, toolsets, model_override, role, budgets { timeout_secs, cpu_ceiling_secs } }.
- Validation: byte-for-byte equality defines a hit (cache and single-flight); any result-affecting field difference → different signature. Stable across restarts.

## Result Cache Entry (FR-007)
- File: `~/.joey/delegation/result-cache.json`; envelope { schema_version: 1, entries: [Entry] }.
- Entry fields: signature (string), result_json (serialized DelegationResult summary), created_at, last_used_at (ISO 8601).
- Validation: LRU eviction at max_entries (default 256); TTL 24h on read (expired → miss); successful outcomes only; exact signature compare.
- State transitions: miss → (execute) → hit thereafter until evicted/expired.

## Retry Budget (FR-005)
- Counting guard: `in_flight_retries` < retry_budget (default 2) required to start a retry; retry delay = jittered_backoff_with(attempt, backoff_base_secs, backoff_max_secs).
- Validation: budget never exceeded system-wide (invariant test); failures surface 'retry budget exhausted' fast-fail.

## Checkpoint / Resume Token (FR-006)
- Fields: last_completed_turn (usize), transcript_digest (short hash of turns so far), recorded_at.
- Validation: presented token must match current transcript digest prefix to be accepted (stale token → full restart, honestly counted).

## Resource Record (FR-011/012)
- File: `~/.joey/delegation/resource-records.jsonl` (append-only; one JSON object per line).
- Fields: record_id, task_signature, priority, outcome (completed|failed|timeout|aborted_by_resource_limit|busy_refused|cache_hit), queue_wait_ms, compute_ms, cpu_ms, memory_peak_kb (advisory), parent_starved_ms (advisory, sampled), retries, checkpoint (token|null), token_usage { prompt, completion, total, cache_read, cache_write, reasoning }, degraded (bool), created_at.
- Validation: one record per terminal task outcome (including busy_refused and cache_hit); sampled fields labeled by name (cpu_ms/memory_peak_kb/compute_ms are sampled).
- State transitions: appended at terminal outcome; immutable after append.

## Priority Class (FR-013)
- Enum: critical | normal | background (default normal). Critical = jump-the-line among queued work only; never preempts running work, never bumps queued work.
- Validation: critical starvation bound — normal lane makes progress under continuous critical load (test-enforced).

## Degraded Mode (FR-013)
- Explicitly selected via `delegation.degraded_mode.enabled` (default false) or runtime selection by the assistant on overload signals; sample rate 0.1 default applies to background+normal work only; outputs marked degraded=true in record + result text.
- Validation: critical work never sampled (full fidelity or explicit failure).

## Config keys (FR-014) → [contracts/config-keys.md](contracts/config-keys.md)

## State transitions (cross-entity)
- Delegation dispatch: submit → (cache hit? return) → (single-flight hit? await) → (queue full? busy refuse) → enqueue → admit (priority order) → run (timeout/watchdog armed) → terminal outcome → record append + cache store (success only) + single-flight release.
