# Contract: terminal.max_concurrent configuration key

Additive, backward-compatible configuration surface (constitution gate 2).

## Key
- Path: `terminal.max_concurrent`
- Layered resolution: existing YAML+env mechanism; must appear in default config documentation with value `auto`.
- Env override: `TERMINAL_MAX_CONCURRENT` (precedence: env > config > auto), mirroring `TERMINAL_TIMEOUT`.

## Value semantics
| Value | Resulting limit |
|-------|-----------------|
| absent / `auto` / `0` | clamp(CPU cores, 4, 16) |
| positive integer N | N |
| invalid | auto (with existing malformed-config warning path) |

## Compatibility
- Existing keys and defaults unchanged; absence of the key must not change any current behavior beyond enabling the cap with the auto default (lone-agent calls are sequential and never queue — SC-004).
- Reading follows the established `ctx.config().get_i64(...)` pattern; no new config subsystem.
