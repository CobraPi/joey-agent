# Quickstart: Claude Code-Style CLI Animations

**Feature**: 002-claude-code-cli-style
**Date**: 2026-07-24
**Spec**: [spec.md](spec.md) | **Plan**: [plan.md](plan.md)

Runnable validation scenarios that prove the feature works end-to-end. These are manual QA + automated seam-test guides; full implementation/tests live in `tasks.md` (Phase 2).

---

## Prerequisites

- Rust toolchain (stable), the joey-agent workspace checked out at `/Users/jo110366/Development/joey-agent`.
- A working provider configured (`joey setup` done, or `JOEY_*` env vars set) so a real turn can run.
- A common terminal emulator (macOS Terminal, iTerm2, Alacritty, or Kitty) at ≥ 80×24 for interactive tests.

## Build

```sh
cd /Users/jo110366/Development/joey-agent
cargo build -p joey-cli --release      # fast release build of the joey binary
# binary appears at target/release/joey
```

If the build fails on `pulldown-cmark`, verify it was promoted to `crates/joey-cli/Cargo.toml` as a workspace dep (plan R-003).

---

## Scenario A — Automated seam tests (SC-004 fallback coverage)

**Validates**: FR-007 (degradation), FR-008 (reuse Pantera), FR-011 (non-TTY disable), Constitution Principle IV.

```sh
cd /Users/jo110366/Development/joey-agent
cargo test -p joey-cli --lib
```

**Expected outcomes**:
- `markdown_to_ansi` tests pass: heading/code/list/bold inputs produce ANSI output containing the expected Pantera color roles.
- `RenderCapability::level` tests pass: synthetic (non-TTY, no-truecolor, narrow) inputs classify to `NonInteractive`/`Reduced` correctly.
- `AnimationProfile::for_kind` tests pass: every kind returns a non-empty profile under `Full`/`Reduced`; `Reduced` profiles use only ASCII-safe glyphs.
- `AnimationState::advance` wraparound test passes.
- Plain-text fallback test passes: with `Capability::NonInteractive`, `render_turn` output contains no `\x1b[` cursor-control escapes and no `\r`.

**Pass condition**: all tests green.

---

## Scenario B — Startup banner entrance animation (FR-001, US1)

**Validates**: the first-impression claude-code feel on launch.

```sh
target/release/joey
```

**Steps**:
1. Launch the interactive REPL (no `--tui` flag).
2. Observe the startup banner.

**Expected**:
- A Joey-branded banner appears with a gradient wipe-in / entrance animation in Crush/Pantera colors, resolving to the full static banner (logo, model, cwd, session id, toolset summary) within ~1 second.
- The `❯` prompt becomes ready after the animation completes.
- If the terminal is narrowed (< 60 cols), the banner scales/wraps without overflow.

**Pass condition**: animated entrance is visible, resolves to a readable banner, prompt ready in ≤ ~1.5s.

---

## Scenario C — Thinking spinner + streaming reveal (FR-002, FR-003, US2/US3)

**Validates**: the core turn-time animations.

```sh
target/release/joey
# at the prompt, type a question that takes a few seconds, e.g.:
# "Explain how a hash map works in 3 paragraphs."
```

**Steps**:
1. Submit a prompt with a multi-second think time and a multi-paragraph reply.
2. Observe the processing phase, then the streaming phase, then completion.

**Expected**:
- While awaiting the first token: an animated spinner (Joey's own glyph set, not claude-code's) with a static "Thinking…"-style label, in Pantera colors, advancing smoothly without freezing.
- On first token: the spinner clears and assistant text streams in progressively (raw) with an animated caret at the current position.
- On completion: the streamed block reflows exactly once into formatted markdown (headings, code blocks, lists, bold) in Pantera colors; the caret disappears; no further reflow/flicker.

**Pass condition**: all three phases visible and smooth; finalize is a single reflow.

---

## Scenario D — Per-tool animated lines (FR-004, US4)

**Validates**: tool-call feedback animation.

```sh
target/release/joey
# at the prompt, trigger tool use, e.g.:
# "List the files in the current directory and read the first one."
```

**Expected**:
- Each tool call appears as its own line with a brief entry animation + running spinner glyph (Pantera colors).
- On tool completion: the same line transitions in place to a resolved ✓/✗ icon with a one-line summary and duration.
- No expandable/collapsible detail block (clarification Q4).
- Multiple rapid tool calls do not corrupt each other's lines (edge case).

**Pass condition**: per-tool running→resolved transition visible; no line tearing on rapid tools.

---

## Scenario E — Persistent usage + turn-complete summary (FR-005, US5)

**Validates**: claude-code-style persistent status.

**Expected during/after a turn**:
- A persistent token usage indicator updates in-flight (Pantera colors) as tokens are consumed.
- On turn completion, a summary line appears showing tokens used (in/out) and turn duration, positioned below the response without overwriting it.

**Pass condition**: usage visible during turn; summary line present and correct after turn.

---

## Scenario F — Non-TTY / piped plain-text fallback (FR-011, edge case)

**Validates**: animations disable when stdout is piped; content stays readable.

```sh
target/release/joey -q "say hello" | cat
# or pipe through a non-terminal consumer
```

**Expected**:
- No spinner, no caret, no banner animation, no cursor-control escapes.
- The final response text (and, where applicable, the turn-complete summary) print as plain readable text.

**Pass condition**: piped output is clean plain text (verifiable with `| cat -v` showing no `^[` escape sequences).

---

## Scenario G — `joey-tui` crate unchanged (SC-005)

**Validates**: the full-screen TUI is untouched by this feature.

```sh
cd /Users/jo110366/Development/joey-agent
# confirm no source files in joey-tui were modified by this feature:
git diff --stat main -- crates/joey-tui
# launch the TUI to confirm it still works:
target/release/joey --tui
```

**Expected**:
- `git diff --stat` for `crates/joey-tui` shows no changes attributable to this feature.
- `joey --tui` launches and behaves identically to before the feature.

**Pass condition**: no `joey-tui` diffs; TUI works as before.

---

## Notes

- Scenarios B–F are manual QA on a real terminal (the spec scopes fallback validation to automated tests for color/glyph logic; terminal-emulator-specific visual verification is best-effort per spec SC-003).
- The automated seam tests (Scenario A) are the primary done-gate; they must all pass for the feature to be considered complete (SC-004).
- Exact glyph designs and frame timings are implementation details (Phase 2); this guide validates behavior, not specific frame art.
