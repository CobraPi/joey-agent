# Contract: context-economy config keys

11 additive keys in the DEFAULT_CONFIG_YAML + call-site getters pattern (config.rs:28+). None are secrets (no .env routing). Unknown keys ignored as today. All default ON except thresholds; per-mechanism disable = FR-013.

| Key | Kind | Default | Validation / clamp |
|-----|------|---------|--------------------|
| scratchpad.enabled | Bool | true | check()=false when disabled (tool absent from registry output but code registered) |
| scratchpad.max_entry_chars | Int | 8000 | 1000..=64000 clamp |
| state_block.enabled | Bool | true | false = no block ever rendered (parity) |
| state_block.max_chars | Int | 1200 | 200..=8000 clamp |
| compression.midturn_tool_hygiene | Bool | true | false = pressure branch untouched (parity) |
| compression.midturn_threshold | Int | 0.35 | read as float via ratio semantics; 0.10..=0.45 clamp, must be < compression.threshold |
| compression.boundary_trigger | Bool | true | false = exit paths untouched (parity) |
| compression.boundary_threshold | Int | 0. scratch | 0.35 | 0.10..=0.45 clamp |
| agent.context_economy_guidance | Bool | true | false = guidance absent from system prompt |
| agent.retrieval_verification_nudge | Bool | true | false = nudge line absent |
| scratchpad.retention_note | — | — | documentation-only key (no behavior) — OMIT; retention is existing policy |

Row correction: compression.midturn_threshold and compression.boundary_threshold are Float kind (0.35 default), and the scratchpad.retention_note row is REMOVED (10 keys, not 11; the contract table is authoritative: 10 keys).

## Threshold semantics (analysis F3, decision (a))

`compression.midturn_threshold` and `compression.boundary_threshold` are Float ratios (0.10..=0.45). At runtime each is multiplied by the compressor's `context_length` field to derive a token threshold, compared against `request_pressure_tokens` exactly as the existing pre-API gate compares `threshold_tokens` (crates/joey-agent-core/src/agent.rs:2872-2891; `should_compress` at crates/joey-agent-core/src/compression/compressor.rs:1260-1266). This keeps the keys unit-free while reusing the existing token-count comparison. The `midturn_threshold < compression.threshold` clamp is evaluated on the same scale (compression.threshold_tokens / context_length).
