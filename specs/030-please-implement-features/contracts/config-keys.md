# Contract: delegation governance config keys

All keys live under the existing `delegation.*` namespace (dotted paths, `joey-core` layered config). Defaults are declared in `joey-core` `DEFAULT_CONFIG_YAML` (merged DEFAULTS ⊕ user). Dotted keys never route to `.env` (routing rule, config.rs:1140-1149). All keys additive — no existing key changes meaning or default.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| delegation.resource_governance.enabled | bool | true | Master switch; false = exact pre-feature behavior (all mechanisms off, zero residual overhead) |
| delegation.max_queue_depth | int\|"auto" | auto | Waiting-queue cap; auto = 2 × max_concurrent_children |
| delegation.task_timeout_secs | u64 | 600 | Per-task wall-clock budget; 0 = disabled |
| delegation.retry_budget | usize | 2 | Max retries in flight system-wide; 0 = retries refused (fail fast) |
| delegation.backoff_base_secs | f64 | 2.0 | Retry backoff base (jittered, exponential) |
| delegation.backoff_max_secs | f64 | 60.0 | Retry backoff ceiling |
| delegation.checkpointing.enabled | bool | true | Turn-boundary resume tokens on; false = timeouts always full-restart |
| delegation.result_cache.enabled | bool | true | Persistent result cache on/off |
| delegation.result_cache.max_entries | usize | 256 | LRU bound |
| delegation.result_cache.ttl_hours | u64 | 24 | Entry TTL |
| delegation.single_flight.enabled | bool | true | In-flight dedup on/off |
| delegation.cpu_ceiling_secs | u64 | 300 | Hard per-task CPU ceiling; 0 = disabled |
| delegation.watchdog_interval_secs | u64 | 1 | CPU/memory sampling cadence |
| delegation.memory_tracking.enabled | bool | true | Advisory memory tracking on/off |
| delegation.priority.enabled | bool | true | Priority lanes on/off |
| delegation.degraded_mode.enabled | bool | false | Explicit degraded-mode selection (never auto) |
| delegation.degraded_mode.sample_rate | f64 | 0.1 | Fraction of background+normal work processed under degraded mode; critical never sampled |

## Notes

- Per-task retry allowance (SC-002): the pre-existing `delegation.subagent_recovery_attempts` key (default 1) caps retries per task; `delegation.retry_budget` caps retries in flight system-wide. Both are enforced together.
- Clamps: `task_timeout_secs`, `retry_budget`, `backoff_base_secs`, `backoff_max_secs`, `cpu_ceiling_secs`, `watchdog_interval_secs`, `result_cache.max_entries`, `result_cache.ttl_hours` clamp to >= 0 (0 disables the mechanism where noted); `degraded_mode.sample_rate` clamps to [0.0, 1.0]; `max_queue_depth` and the pool clamp to >= 1 after auto-resolution.
