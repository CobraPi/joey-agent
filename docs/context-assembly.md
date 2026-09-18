# Dynamic Context Assembly (opt-in)

Dynamic context assembly is an **opt-in** layer that budget-assembles each
outgoing LLM request: it filters the tool-schema list by relevance to the
current step, pins an always-keep tool set, caps the deterministic state
block at a character budget, and optionally logs the exact assembled
context per step. The whole layer is **off by default**
(`context_assembly.enabled = false`) and every hook is an identity when
disabled — the request wire format is byte-identical to a build without
the feature.

It complements [context-economy.md](context-economy.md) (feature 028).
Feature 028 is default-on and provides the just-in-time retrieval
affordances the assembly layer builds on: the scratchpad tool, session
search, the deterministic state block, mid-turn tool-output hygiene, and
boundary-aligned cleanup. This layer (opt-in) adds **budgeted assembly**
on top: relevance-ranked tool-schema selection (the "tool schema
retrieval" pattern), always-keep pinning, a state-block budget cap, and
per-step assembly logging ("log the exact assembled context per step").

Implementation: `crates/joey-agent-core/src/context_assembly.rs`
(`relevance_scores`, `select_tools`, `cap_state_block`, JSONL logger) and
the tail-only hook in `build_request`.

## Mechanisms

### Tool-schema retrieval

When `context_assembly.tool_schema_retrieval` is on, the tool schemas sent
with a request are ranked against the current step by a **deterministic
term-overlap scorer** (`relevance_scores`): each term found in a tool's
name scores **+3**; each term found in its description scores **+1**.
`select_tools` then keeps at most `context_assembly.tool_top_k` tools:
the always-keep set is pinned first, and the remaining slots are filled by
score descending, ties broken by name ascending. When the registry holds
`tool count <= top_k` tools, selection is a **no-op** — the tool list is
untouched, so small registries never churn.

Config keys: `context_assembly.tool_schema_retrieval`,
`context_assembly.tool_top_k`.

### Always-keep set

`context_assembly.always_keep_tools` lists tools that are **always
included regardless of score**, pinned ahead of the ranked fill. The
default set keeps the core file/terminal/todo workflow usable even under
an aggressive budget: `read_file`, `write_file`, `patch`, `search_files`,
`terminal`, `todo`, `scratchpad`.

Config key: `context_assembly.always_keep_tools`.

### State-block budget

The deterministic state block (see [context-economy.md](context-economy.md))
is capped at `context_assembly.budget_state_chars` characters per request.
`cap_state_block` truncates **line-preserving** — whole lines are dropped
from the tail and a `[truncated]` marker is emitted — so the block that
reaches the model always remains structurally valid.

Config key: `context_assembly.budget_state_chars`.

### Assembly log

With `context_assembly.log_assembly` on, each assembled request is
recorded as one JSON object per line in a per-session JSONL file (see
[Assembly log](#assembly-log) below for the path pattern and schema).
Logging is **best-effort**: a write failure is logged and never fails the
turn.

Config key: `context_assembly.log_assembly`.

### Context gauge (feature 034)

Every assembled request appends one tail gauge line:

```
<total_tokens>N tokens left</total_tokens>
```

When the remaining budget is at or below the compression threshold, the
line carries a `— LOW` suffix. The figure is
`context_window − max(last real prompt tokens, rough estimate)`
(research D3/D4). The gauge is gated by `context_gauge.enabled`
(default `true`) and rides as the **last message after all stable
content**, so the cached prefix stays byte-stable; it is request-only and
never persisted to history. When `notice_channel.enabled` is `true`, the
line is wrapped in `<system-notice>` markers (see
[System-notice channel (feature 034)](#system-notice-channel-feature-034)).

### System-notice channel (feature 034)

System-injected notices — the gauge line, the skills index, environment
hints, and the project-context attachment block — ride a unified
`<system-notice>...</system-notice>` wrapper when `notice_channel.enabled`
is on (default `false`; with it off, wire bytes are identical to the
pre-feature build). The mid-turn steer marker
(`[OUT-OF-BAND USER MESSAGE ...]`) is **never** wrapped (FR-003).
Untrusted tool output containing the literal markers is defused
(backtick-wrapped) by the tools layer before it can reach history
(FR-015).

### Tool-list stability guard (feature 034)

When `context_assembly` + `tool_schema_retrieval` are enabled, the dynamic
selection is snapshotted and replayed byte-identically on subsequent turns
unless the registered toolset changes (input-hash keyed cache). Rationale
(FR-004): re-ranking the tool list per turn would churn the tool-schema
section of the prompt and defeat prompt-prefix cache warmth.

## Configuration

All six keys live under the `context_assembly` block in `config.yaml`
(and are read via `joey-core` named getters). **All are off/false by
default.**

| Key | Default | Clamp | Meaning |
|---|---|---|---|
| `context_assembly.enabled` | `false` | — | Master switch for the whole layer; when `false`, every hook is an identity (byte parity) |
| `context_assembly.tool_schema_retrieval` | `false` | — | Enable relevance-ranked tool-schema selection |
| `context_assembly.tool_top_k` | `15` | `5..=60` | Max tool schemas kept per request; selection is a no-op when the registry has this many tools or fewer |
| `context_assembly.always_keep_tools` | `[read_file, write_file, patch, search_files, terminal, todo, scratchpad]` | — | Tools pinned ahead of ranking regardless of score |
| `context_assembly.budget_state_chars` | `1500` | `200..=8000` | Character budget for the state block; line-preserving truncation with a `[truncated]` marker |
| `context_assembly.log_assembly` | `false` | — | Write one JSON object per assembled request to the per-session JSONL log |
| `context_gauge.enabled` | `true` | — | Feature 034: append the tail context-gauge line to every assembled request |
| `compaction.calm_framing` | `true` | — | Feature 034: calm-framing sentence on the compression continuation |
| `notice_channel.enabled` | `false` | — | Feature 034: wrap system-injected notices in `<system-notice>` markers |
| `reasoning_prune.enabled` | `false` | — | Feature 034: strip thinking blocks from completed turns on the request copy |

The last four rows are the feature-034 keys: they live outside the
`context_assembly` block, in the feature-034 key group documented in
[state-and-config.md](state-and-config.md), and two of them default to
`true`.

Enabling the full layer:

```yaml
context_assembly:
  enabled: true
  tool_schema_retrieval: true
  tool_top_k: 15
  always_keep_tools: [read_file, write_file, patch, search_files, terminal, todo, scratchpad]
  budget_state_chars: 1500
  log_assembly: true
```

## REPL command

The layer is controlled at runtime from the REPL:

```
/context-assembly [on|off|status]      (alias: /ctxasm)
```

- `on` — enable assembly (sets `context_assembly.enabled = true`)
- `off` — disable assembly (sets `context_assembly.enabled = false`)
- `status` — report whether the layer is currently enabled
- no argument — **toggle** the current state

The change **persists** through the `context_assembly.enabled` config key,
so it survives across sessions.

## Assembly log

When `context_assembly.log_assembly` is on, every assembled request
appends one JSON object per line to a per-session file:

```
~/.joey/context-assembly/<sanitized-session-key>-<fnv1a-hex8>/assembly.jsonl
```

Schema (11 fields per line):

| Field | Meaning |
|---|---|
| `ts` | Timestamp of the assembly step |
| `request_turn` | Turn ordinal within the session |
| `tools_total` | Tools visible to the selector before filtering |
| `tools_kept` | Tools actually sent in the request |
| `tools_dropped` | Tools filtered out by the budget |
| `state_block_truncated` | Whether the state block was capped (`true`/`false`) |
| `request_messages` | Message count in the assembled request |
| `gauge_remaining` | Remaining-token figure carried by the context gauge line |
| `gauge_low` | Whether the gauge flagged `— LOW` (at/below the compression threshold) |
| `notice_channel_on` | Whether `notice_channel.enabled` was on for this request |
| `tool_list_hash` | Input-hash key of the snapshotted tool list (stability-guard cache) |

Logging is best-effort: failures are reported but never fail or alter the
turn.

## Caching & parity guarantees

1. **Byte parity when disabled.** With
   `context_assembly.enabled = false`, the request wire format is
   byte-identical to the pre-feature build — every hook (tool selection,
   state-block cap, logging) is an identity when the layer is off.
2. **Cache stability when enabled.** Assembly only affects the request
   **tail** — the tool list and the state block, applied to the request
   clone only. The system prompt and the message prefix are never
   mutated, so provider prompt-prefix caches stay warm; and when
   `tool count <= tool_top_k` the tool list is untouched, so there is no
   schema churn under budget.
3. **Feature 034 defaults.** The default-on keys (`context_gauge.enabled`,
   `compaction.calm_framing`) change the wire by exactly one tail line /
   one appended sentence; the default-off keys (`notice_channel.enabled`,
   `reasoning_prune.enabled`) leave the request byte-identical to the
   pre-feature build while disabled. When `reasoning_prune.enabled` is on,
   reasoning prune strips thinking blocks from completed turns on the
   **request copy only** — history stays append-only.

## See also

- [context-economy.md](context-economy.md) — the default-on feature 028
  mechanisms (scratchpad, deterministic state block, tool-output hygiene,
  boundary cleanup) this layer builds on.
- [PORTING.md](../PORTING.md) — upstream parity tracker, including the
  ledger entry for this Joey-only addition.
- [state-and-config.md](state-and-config.md) — the feature-034 config key
  group (`context_gauge.enabled`, `compaction.calm_framing`,
  `notice_channel.enabled`, `reasoning_prune.enabled`).
- [PORTING.md](../PORTING.md) deliberate deviations — calm framing and
  the truncation marker are recorded as deliberate deviations from
  upstream wording/behavior.
