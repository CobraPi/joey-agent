# Data Model: Concurrent Agent Terminal Performance & UI Responsiveness

Derived from spec.md Key Entities and research.md decisions D1-D8. No persistence: all state is in-process and ephemeral; the only durable artifact is the additive config key.

## Entities

### Terminal Execution Request
- Identity: a single agent-initiated terminal command execution flowing through `Terminal::execute`.
- Fields: command payload (existing), per-call timeout config, agent queue key (`Arc<str>`, from `ToolContext.queue_key`, default shared key when absent), lifecycle state.
- States & transitions: `queued` → `running` (governor admits; round-robin among agent keys) → `completed | failed | cancelled` (terminal states; slot released on all three via drop-guard).
- Validation: timeout deadline computed at admission, never during queue residence (FR-008); request must not be dropped while queued (FR-004) — it either runs or is cancelled via the cooperative interrupt.

### Execution Slot
- Identity: one unit of governor capacity; count-bounded, not individually addressable.
- Fields: limit (usize), active (usize).
- Invariants: `active <= limit` at all observable times (SC-002); acquire blocks (yields) when `active == limit`; release guaranteed on completion/failure/cancellation/panic (drop-guard).

### TerminalGovernor (internal, process-global)
- Identity: singleton in `joey-tools` (mirrors `process_registry()` precedent).
- Fields: `limit`, `active`, insertion-ordered map of agent-key → FIFO wait queue, rotating admission cursor, stats snapshot fn.
- Behavior: admission scans from cursor for next non-empty queue (one request per agent per turn — round-robin, no starvation); interrupt-aware acquire future; `stats() -> {active, queued}` for events/status.
- Sizing: `clamp(available_parallelism().unwrap_or(8), 4, 16)` unless overridden.

### Execution Event
- Identity: one additive `AgentEvent` variant carrying governor stats at queue-state changes.
- Fields: active count, queued count (throttled to existing 50ms producer budget; TUI coalesces per frame, last-value-wins).
- Consumers: CLI render (queued badge), TUI status bar span, `/status` in both UIs.

### Terminal Concurrency Setting
- Identity: config key `terminal.max_concurrent` (additive; absent = auto).
- Values: positive integer = fixed limit; `0`/`auto`/absent = auto-derived; env `TERMINAL_MAX_CONCURRENT` overrides config (mirrors `TERMINAL_TIMEOUT`).
- Validation: auto path clamps to [4,16]; invalid values fall back to auto with existing config-warning behavior.

## Relationships
- Terminal Execution Request —(1:1 during lifetime)→ Execution Slot (held while `running`).
- TerminalGovernor —(1:N)→ queued Requests, grouped by agent queue key.
- Running/queued Requests —(N:1)→ Execution Events emitted on governor state changes.
- Terminal Concurrency Setting —(configures)→ TerminalGovernor.limit at first use.

## State machine summary

queued → running → completed | failed | cancelled ; cancelled reachable from queued (interrupt) and running (kill + drop-guard release).
