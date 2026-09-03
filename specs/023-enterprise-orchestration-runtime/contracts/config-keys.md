# Contract: Configuration Keys

New config keys (dotted paths, `~/.joey/config.yaml`). All additive; all have safe defaults; none rename or remove existing keys. Read via existing `Config::get_bool`/`get_i64`.

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| `hypercode.execution_graph.enabled` | bool | `false` | Enables the typed TaskGraph runtime (validation, scheduler, isolation, gates, router). When false: byte-identical legacy behavior. |
| `hypercode.execution_graph.max_concurrent_workers` | int | `16` | Maximum concurrently executing workers in a conflict-free wave; excess ready tasks queue deterministically. |
| `hypercode.execution_graph.max_repair_attempts` | int | `3` | Repair attempts per capability tier before escalation (exhaustion ladder). |
| `neurocode.enterprise_context.enabled` | bool | `false` | Enables the enterprise analysis plane (EnterpriseTaskAnalyzer, combined policy resolution, extended classifier signals, outcome memory consultation). When false: legacy context assembly. |

Rules:
- Flags are independent; the execution graph may run with the analysis plane off (planner supplies node fields) and vice versa.
- Flip-to-default of both flags is the final delivery step of this feature, executed only after the SC-001 parity check passes in CI.
- `_config_version` in the default config text is bumped additively (34); existing configs migrate forward silently, never backward.
