# Contract: `agent.goal_directed_guidance` Config Key

**Kind**: additive config key (boolean) | **Default**: `true` | **Since**: feature 031

## Semantics

- `true` (default): the system prompt's stable tier includes `GOAL_DIRECTED_GUIDANCE` — the agent plans before non-trivial work, executes steps in order, revises plans explicitly, and reports per-step outcomes.
- `false`: the constant is omitted; the assembled system prompt is byte-identical to the pre-feature prompt (same guarantee shape as `agent.adaptive_coding_guidance`, parity.rs:536-539 precedent).
- Accepted values: booleans via the layered YAML+env config (`config.yaml` under `agent.`, or `JOEY_AGENT_GOAL_DIRECTED_GUIDANCE`-style env override per existing env-config rules).
- The key is not secret (no `_KEY`/`_TOKEN` routing); it lives in `config.yaml`, not `.env`.

## Backward compatibility (Constitution VII)

- Absent key behaves as `true` (feature on) — new default behavior is the feature's purpose; users who want the old behavior set `false` and get the exact pre-feature prompt.
- No existing config key, CLI flag, exit code, or on-disk format changes.

## Verification

- On/off parity test: prompt with key=true contains `GOAL_DIRECTED_GUIDANCE`; key=false yields byte-identical pre-feature prompt (mirrors parity.rs adaptive-coding pattern).
- Default-resolution test: unset key resolves to true.
