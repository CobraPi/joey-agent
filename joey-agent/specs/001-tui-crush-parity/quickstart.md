# Quickstart: Validating TUI Crush Parity

This guide runs the automated checks and manual smoke scenarios that prove this feature works,
without requiring the reader to have read `plan.md`/`data-model.md`/`contracts/` in full.

## Prerequisites

- Rust toolchain per `rust-toolchain.toml` (already installed if you can build the workspace).
- Both repos checked out side by side (already true in this environment):
  `/Users/jo110366/Development/joey-agent` and `/Users/jo110366/Development/crush` (reference
  only — not built or run as part of validation).
- On branch `001-tui-crush-parity` with this feature's implementation applied (post `/speckit-
  tasks` + `/speckit-implement`, or manually for a partial build).

## 1. Build the workspace

```bash
cd /Users/jo110366/Development/joey-agent
cargo build -p joey-tui -p joey-cli
```

Expected: builds cleanly with the new `pulldown-cmark`/`syntect` dependencies resolved (research.md R1).

## 2. Run the automated seam and unit tests

```bash
cargo test -p joey-tui
```

Expected, at minimum, these tests pass (new for this feature):
- `dialog_seam` — proves `DialogStack` routes keys/close-outcomes generically (contracts/dialog-trait.md).
- `transcript_renderer_seam` — proves new `TranscriptItem` kinds render via the registry, not a
  hardcoded match (contracts/transcript-renderer-trait.md).
- `capabilities` — table-driven truecolor/Unicode detection + downsampling (contracts/capabilities-api.md);
  this is the full verification bar for FR-011/SC-004 per the Clarifications — no manual
  multi-terminal pass is required here.
- Extended `smoke.rs` cases covering: compact-mode layout at <120x30 (FR-002), tool-status
  icon/color per state (FR-005), diff view rendering (FR-006), sidebar section empty-states
  (FR-009a edge case).

## 3. Manual interactive smoke pass (User Stories 1–4)

Launch the TUI against a real or mock provider:

```bash
cargo run -p joey-cli -- 
```

Walk through, confirming against spec.md's acceptance scenarios:

1. **Layout** (User Story 1): header/status bar, transcript, sidebar, and bottom editor are all
   visible at a normal terminal size; resize the terminal below 120 columns or 30 rows and
   confirm the layout collapses to compact mode (FR-002).
2. **Transcript rendering** (User Story 2): send a prompt that triggers at least one tool call
   producing a diff (e.g. ask the agent to edit a file). Confirm: assistant text streams then
   finalizes as styled markdown (FR-004); the tool block shows Running → Done/Failed status
   icons (FR-005); the diff renders with colored additions/deletions (FR-006).
3. **Theme** (User Story 3): visually compare the running session against a Crush screenshot (or
   `cargo run` inside `/Users/jo110366/Development/crush` if available locally) for background/
   accent/status color parity (FR-007) and confirm the startup banner/logo uses a smooth
   multi-stop gradient (FR-008).
4. **Dialogs** (User Story 4): open the model picker, session picker, and command/slash
   completions; trigger a tool permission prompt; trigger quit (confirm dialog appears); open
   the reasoning-level picker. Confirm each opens as a keyboard-navigable overlay and closes
   correctly (FR-009/FR-009a), and that the sidebar shows files/LSP/MCP/skills sections
   (rendering their empty states where applicable).

## 4. Success criteria checkpoints

Map back to spec.md's Success Criteria:
- **SC-001**: A reviewer unfamiliar with this feature's internals, but who knows Crush, should
  complete steps 1–4 above without consulting this document beyond the four numbered steps.
- **SC-002**: Take screenshots at the same three points as step 3 in both TUIs (if Crush is
  built locally) and confirm ≥95% visually-distinct-element color/layout match.
- **SC-003**: Repeat steps 1–4 in at least 3 terminal emulators (e.g. Terminal.app, iTerm2, a
  tmux session) with no crashes/corruption.
- **SC-004**: Confirmed entirely by the `capabilities` automated test suite in step 2 — no
  additional manual step required per the Clarifications.

## Out of scope for this quickstart

Per spec.md Assumptions: CLI subcommands, cron scheduler, MCP client wire behavior, in-TUI OAuth
flows, and a file picker are not covered here — they are explicitly out of scope for this
feature.
