# Contract: /llm-selector Slash Command

**Feature**: 011-dynamic-llm-selector | **Surface**: chat slash command + CLI text surface
**Owning crate**: `joey-cli` (handler) → consumes `joey-llm-selector` query API
**Stability**: CLI flag/exit-code contract (Constitution II, VII) — additive only.

Constitution II parity: every capability is reachable as text in/out from the
REPL slash command AND from the CLI; no UI-only affordance.

---

## Registration

- `crates/joey-cli/src/slash.rs`: one new entry in the `static REGISTRY`
  (`&[CommandDef]`, built via `cmd!` macro), inserted near the `/model` entry
  at line 69.
- `crates/joey-cli/src/repl.rs`: one new dispatch arm at line 825
  (`"llm-selector" => llm_selector_slash(st, args)`).
- Prefix abbreviation (`slash.rs:153` `resolve()`) handles `/llm-s`
  automatically — no separate alias wiring needed (FR-001's "alias" is
  satisfied by the prefix resolver).

## Subcommands & exit codes

All output is plain text to stdout; errors to stderr; exit 0 on success,
non-zero on error. `/llm-selector` with no subcommand defaults to `status`.

| Subcommand | Args | Effect | Exit |
|---|---|---|---|
| `status` (default) | — | Print enabled/disabled state, candidate pool size, active diagnoser model, budget usage (FR-001). | 0 |
| `pool` | — | List every chat-capable model in the active catalog: id, tier, context window, tools/vision flags, cost (FR-003, SC-005). | 0 |
| `allocations` | — | Print the full allocation map: per module → model, pinned/implicit-pin flags, reason, estimated p_j, updated_at (FR-011, SC-008). | 0 |
| `diagnostics` | `[-n <count>]` | Print the last N (default 20) diagnoser judgments: module, signal, implicated model, rationale, timestamp (FR-018, SC-008). | 0 |
| `pin` | `<module> <model_id>` | Pin a specific model to a module; persisted, applied immediately, exempt from reallocation (FR-012). | 0 / 1 (model not in catalog) |
| `unpin` | `<module>` | Remove a user pin (FR-012). | 0 / 1 (not pinned) |
| `budget` | `<n>` | Set the learning budget (`model.selector.budget`); `0` disables learning (FR-009). | 0 |
| `diagnoser` | `[<model_id>]` | Show or set the diagnoser model (FR-008). | 0 / 1 (model not versatile-tier-eligible) |
| `enable` | — | Enable dynamic allocation (engaged when `auto` is the active model). | 0 |
| `disable` | — | Disable dynamic allocation; `auto` falls back to the literal configured model for all modules (FR-002). | 0 |
| `refresh` | — | Force-refresh the candidate pool from the live catalog. | 0 / 1 (catalog fetch failed → degraded) |
| `help` | — | Print this table. | 0 |

## Module argument grammar

`<module>` accepts the snake_case enum ids — `main_turn`, `compression`,
`subagent` — or a `custom:<name>` for an additive module. The handler rejects
unknown modules with exit 1 and a helpful message.

## `status` output shape (canonical)

```
LLM Selector: enabled
Active model: auto (dynamic allocation engaged)
Candidate pool: 24 chat-capable models (source: copilot)
Diagnoser model: gpt-4.1 (versatile)
Learning budget: 8 (0 used this cycle)
Allocations:
  main_turn    -> gpt-4.1           (pinned, reason: "user pin")
  compression  -> claude-haiku-4-5  (reason: "diagnoser +0.12 p_j")
  subagent     -> gpt-4.1           (reason: "cold-start")
Run /llm-selector help for the full command list.
```

When disabled:
```
LLM Selector: disabled
Active model: gpt-4o (concrete model; selector inactive)
Enable by selecting the `auto` model, then /llm-selector enable.
```

When the provider exposes no catalog:
```
LLM Selector: unavailable
The active provider does not expose a live model catalog.
Dynamic selection requires a catalog-exposing provider (copilot/openrouter).
```

## CLI reachability (Constitution II)

The handler `llm_selector_slash` is callable from the REPL slash surface AND
from a `joey llm-selector <subcommand>` CLI text path (wired in
`crates/joey-cli/src/main.rs` command tree). Both paths use the same handler,
so output is identical. No visual/interactive UI is required for any
capability.

## Backward compatibility

- The command is additive — `/llm-selector` does not exist today, so adding it
  breaks nothing. The prefix resolver ensures it doesn't shadow existing
  commands (checked against the existing registry at registration).
- Exit codes follow the existing `joey-cli` convention (0 success, 1 usage /
  not-found, 2+ reserved). Documented above per subcommand.
- The `enable`/`disable` subcommands only mutate the on-disk map's `enabled`
  flag (atomic write); they never touch conversation state (FR-016).
