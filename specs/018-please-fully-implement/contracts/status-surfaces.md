# Contract: on-demand status surfaces (FR-011)

Both interfaces expose identical information; parity is a constitution gate (4).

## CLI
- `/status` output gains one line: terminal execution `active: A, queued: Q`.
- Transient render shows a queued badge only while Q > 0.

## TUI
- `/status` notice includes `terminal active: A, queued: Q`.
- Status bar (visible when show_status_bar) gains a contention span only while Q > 0.

## Stability
- Format is a human-readable line; machine consumers should rely on the event contract (events.md), not scrape `/status`.
