# Convergence Findings: Claude Code-Style CLI Animations

**Feature**: 004-claude-code-cli-style
**Converge run**: 2026-07-25
**Spec**: [spec.md](spec.md) | **Plan**: [plan.md](plan.md) | **Tasks**: [tasks.md](tasks.md)

Produced by `/speckit-converge`: a fresh assessment of the current `joey-cli`
codebase against FR-001–FR-011 and the US1–US6 acceptance scenarios. Each row
cites file:line evidence read from the live source (not the task checkboxes,
which were stale relative to the code).

**Build/test state at assessment time**:
- `cargo build` (workspace) → green (only pre-existing baseline warnings).
- `cargo test -p joey-cli` → **89 passed; 0 failed.**
- `git diff --stat main -- crates/joey-tui` → **empty** (SC-005 PASS).

**Implementation pass (2026-07-25):** the open gaps below (CF-1…CF-7, except
the descoped FR-006) were implemented in this same converge/implement cycle.
Statuses marked ✅ below reflect the post-implementation state; the
"New findings" section retains the original evidence + fix references.

**Status legend**: ✅ PASS · 🟡 PARTIAL · ❌ FAIL · 🔵 RESOLVED-BY-DECISION · ⚪ DESCOPED

---

## Per-requirement findings

| ID | Requirement | Status | Evidence / Notes |
|---|---|---|---|
| FR-001 | Startup banner entrance animation | ✅ | `render::banner_animated` (`render.rs:964-1011`): Full = gradient shimmer wipe-in over the tick timer then full static banner; Reduced/NonInteractive delegate to `banner`. Bounded ≤ ~1.5 s. Wired at `repl.rs:455`. |
| FR-002 | Thinking spinner + static label while awaiting first token | ✅ | Spinner started on `ApiCallStart` (`render.rs:329-349`), repainted each tick (`render.rs:779-797`), finalized on first `ContentDelta`/`ToolStart` (`render.rs:403-413`, `:450-460`). Label is static `"Thinking…"`; no reasoning text in the spinner. Profile `SPINNER_FULL` accent color (`profile.rs:98-104`). |
| FR-003 (a) | Progressive raw reveal + **animated** caret | ✅ | Raw deltas print immediately (`render.rs`). Caret painted between deltas and erased on the next delta; **blink fixed (T045)**: `animation::tick_phase` now takes a monotonic `tick_count` so the `▌`/` ` frames alternate. Guarded by `tick_phase_advances_over_ticks`. |
| FR-003 (b) | Single markdown reflow on completion | ✅ | On `Done` with `streamed_any`, clears the streamed region and re-renders once via `markdown_to_ansi` (`render.rs:675-696`). `count_visual_lines` is now width-aware (`render.rs:176-198`), so the clear count no longer undercounts wrapped lines (T044 resolved). |
| FR-004 | Per-tool entry→running→resolved in-place animation | ✅ | Entry prints on `ToolStart` capturing `tool_row`; resolved line rewrites the same row on `ToolEnd`. **Running repaint now keeps the name + summary visible (T046)** (`active_tool` carries the summary; tick arm prints `  {frame} {name} ({summary})`). |
| FR-005 | Persistent in-flight usage indicator + turn-complete summary | ✅ | **AC1 (in-flight indicator):** implemented (T047) — accumulated `prompt`/`completion` tokens are appended to the spinner line via `usage_suffix(...)` on `ApiCallStart` and each tick repaint, reflecting usage while the agent works (esp. across agentic iterations); a separate anchored row was rejected as fragile on an append-only line CLI (Constitution VII). **AC2 (turn summary):** summary now shows `iterations · tokens in · tokens out · duration` (T048), with `turn_start` captured on `TurnStart`. |
| FR-006 | Polished prompt with idle caret blink | ⚪ | **Descoped** per `research.md` “T031 Spike Decision”: reedline owns a synchronous editor loop and cannot host an external tick-driven blink. Static colored prompt (DarkGrey → Cyan) shipped instead. `PromptCaret` profile retained for future use. Decision recorded; no action. |
| FR-007 | Steady frame rate, no flicker, resize safety | ✅ | Single `tokio::time::interval` tick loop drives all elements. Resize: lazy width re-read via `box_width()`/`term_width()` on each render — **T043 resolved-by-decision** (`research.md`). T033's mandated adaptation test added (T050): `reflow_line_count_adapts_to_width`. |
| FR-008 | Graceful degradation (reduced capability) | ✅ | `capability.rs` classifies Full/Reduced/NonInteractive; reduced profiles are ASCII-only (`profile.rs:138-181`); `reduced_profiles_use_only_ascii_safe_glyphs` test guards it. |
| FR-009 | Reuse Pantera palette only; no competing palette | ✅ | All rendered colors sourced from `Theme::pantera()`. NonInteractive profile constants now return `t.fg_base` (T051b) instead of the stray white literal. |
| FR-010 | Single interruptible tick loop | ✅ | One `interval` arm advances spinner, caret, tool spinner, and (intended) usage (`render.rs:775-862`); no element spawns its own timer. |
| FR-011 | Auto-disable animations on non-TTY | ✅ | `animations_on = opts.animations_enabled && opts.capability.is_interactive` (`render.rs:221`); NonInteractive short-circuits to plain text. `noninteractive_returns_final_text_without_hang` + regression tests green. |

**SC-005 (joey-tui untouched)**: ✅ — `git diff --stat main -- crates/joey-tui` is empty.

---

## Stale task reconciliation (tasks.md Phase 10)

The Phase-10 checkboxes (T040–T044) predate the current code; reconcile:

| Task | Tasks.md says | Code reality | Action |
|---|---|---|---|
| T040 (streaming caret rendered) | `[ ]` open | Caret IS rendered (`render.rs:799-815`) | **Close** — but track the blink residual as **CF-1 / T045**. |
| T041 (per-tool in-place anim) | `[ ]` open | Implemented (`render.rs:466-513`, `:561-585`, `:817-837`) | **Close** — but track the name-drop residual as **CF-2 / T046**. |
| T042 (in-flight usage indicator) | `[ ]` open | Dead code — label never printed | **Keep open**, re-point to **CF-3 / T047**. |
| T043 (event-based resize) | `[ ]` open | Decision recorded: lazy width re-read | **Close as resolved-by-decision** (`research.md`). |
| T044 (markdown reflow line count) | `[ ]` open | `count_visual_lines` width-aware + tested | **Close**. |

---

## New findings (action items)

> Order = severity. Each becomes a task appended to `tasks.md` Phase 10.

### CF-1 / T045 — Streaming caret does not blink (FR-003(a), residual) — P2
`animation::tick_phase` (`animation.rs:70-81`) derives the phase from
`Instant::now().elapsed()`, which is always ~0, so the caret is permanently
frame 0 (`▌`). **Fix:** thread a real monotonic reference (e.g. a `tick_count`
incremented in the tick arm, or pass `started_at`/elapsed of the turn) into the
phase calculation so it toggles between `▌` and ` `. Add a unit test asserting
the phase advances over simulated ticks.

### CF-2 / T046 — Running tool spinner repaint drops the tool name (FR-004, residual) — P2
The tick arm clears the tool line and prints only `  {frame}`
(`render.rs:822-835`); the captured `_tool_name` is unused, so during execution
the tool name is invisible. **Fix:** reprint `  {frame} {name}` (and the
short summary if any) on each repaint, mirroring the entry print at
`render.rs:477-481`.

### CF-3 / T047 — Persistent in-flight usage indicator is dead code (FR-005/AC1) — P1
The tick-arm block (`render.rs:839-861`) builds `usage_label` but never emits
it; no in-flight usage is ever shown (quickstart Scenario E fails). **Fix:**
either (a) render the usage line on a dedicated anchored row repainted each
tick while `turn_in_progress` (positioned below the streaming region so it
cannot clobber streamed text — guard with a non-interference assertion
mirroring T027), or (b) formally descope AC1 with a recorded rationale in
`research.md` and update the spec/quickstart. Given AC1 is an explicit
acceptance scenario, prefer (a).

### CF-4 / T048 — Turn-complete summary omits turn duration (FR-005/US5/AC2) — P1
The summary (`render.rs:707-718`) shows `iterations · tokens in · tokens out`
but no duration. Spec/quickstart US5/AC2 requires “tokens used **and turn
duration**.” **Fix:** capture a turn-start `Instant` at `TurnStart` (or first
`ApiCallStart`) and append `fmt_duration(turn_start.elapsed())` to the summary.

### CF-5 / T049 — Documented done-gate test command is wrong (SC-004, T035) — P3
`quickstart.md` Scenario A and `tasks.md` T035 invoke `cargo test -p joey-cli --lib`,
which **fails** with “no library targets found in package `joey-cli`” (the crate
is a binary; tests are inline `#[cfg(test)]`). **Fix:** change both to
`cargo test -p joey-cli` (verified: 85 passed).

### CF-6 / T050 — Add resize-layout adaptation unit test (FR-007/T033) — P3
T033 mandated “an automated unit-level test that the layout function adapts to
a width change without emitting partial frames.” None exists. **Fix:** add a
test asserting a layout helper (`box_width`-dependent banner/summary builder,
or a factored `layout_for(width)`) produces width-appropriate output and no
partial-frame escapes when width changes, locking the lazy-re-read approach
chosen in `research.md`.

### CF-7 / T051 — Minor: fenced-code language label + NonInteractive color literal — P3
(a) `markdown_to_ansi` ignores the fenced-code language (Contract 2 expected a
language-label line) — `markdown.rs:30-33`. (b) NonInteractive profile constants
hardcode `Rgb(255,255,255)` instead of a Pantera color — `profile.rs` PLAIN
profiles (never rendered, but a stray non-Pantera literal per FR-009).
**Fix:** emit the language label for fenced blocks; replace the white literals
with a Pantera role (or `Theme`-derived) value. Both optional polish.

---

## Recommendation

Feature is **converged**: all FR-001–FR-011 requirements are met (FR-006
descoped by recorded decision) and SC-005 holds. The converge pass surfaced 7
gaps (CF-1…CF-7); the immediately-following implement pass closed all of
them (T045–T051). Final state: workspace build green, `cargo test -p joey-cli`
→ **89 passed**, `joey-tui` untouched. Remaining work is manual QA across
terminal emulators (quickstart Scenarios B–F), not code.
