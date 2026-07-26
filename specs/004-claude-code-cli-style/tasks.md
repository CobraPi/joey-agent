---

description: "Task list for Claude Code-Style CLI Animations (joey-cli crate)"
---

# Tasks: Claude Code-Style CLI Animations

**Input**: Design documents from `/specs/004-claude-code-cli-style/`

**Prerequisites**: plan.md ✅, spec.md ✅, research.md ✅, data-model.md ✅, contracts/render-animation-seam.md ✅, quickstart.md ✅

**Tests**: Tests ARE included — the feature spec (plan.md "Testing", Constitution Principle IV, quickstart.md Scenario A) explicitly mandates seam-level tests as the done-gate (SC-004).

**Organization**: Tasks grouped by user story (US1–US6). Foundational infrastructure (capability/profile/state/render_turn refactor) is isolated in Phase 2 because every story depends on it.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies on incomplete tasks)
- **[Story]**: Which user story this task belongs to (e.g. US1, US2)
- Paths are relative to repo root; all source lives under `crates/joey-cli/src/`

## Path Conventions

- Single crate: `crates/joey-cli/src/` (the `joey` binary)
- Module manifest: `crates/joey-cli/Cargo.toml`, `crates/joey-cli/src/main.rs` / `lib.rs`
- No cross-crate changes (`joey-core`, `joey-agent-core`, `joey-tui`, etc. are read-only per Constitution Principle I / SC-005)

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Bring in the promoted dependency, create the new module skeletons, wire them into the crate manifest, and confirm the workspace still builds before any logic lands.

- [X] T001 Promote `pulldown-cmark = "0.12"` from workspace to a `joey-cli` workspace dependency in `crates/joey-cli/Cargo.toml` (plan R-003) using DEFAULT features only (`pulldown-cmark = { workspace = true }`; do NOT enable `simd`/`gen` — they add binary size for no CLI-renderer benefit). Verify it resolves with `cargo update -p joey-cli`. Record the feature-choice rationale (default-only, shared with joey-speckit-ui) in research.md R-003 per Constitution VIII.
- [X] T002 Create four new empty module files and register them in the crate root: `crates/joey-cli/src/capability.rs`, `crates/joey-cli/src/profile.rs`, `crates/joey-cli/src/animation.rs`, `crates/joey-cli/src/markdown.rs` — add `mod capability; mod profile; mod animation; mod markdown;` (declared `mod` at crate root, items `pub(crate)`) to `crates/joey-cli/src/main.rs` (confirmed owner of the module tree, L8–27; insert alphabetically near `mod omo_render;`/`mod render;`). Re-export the minimal entry points (`banner_animated`, `markdown_to_ansi`) from `render.rs`.
- [X] T003 Run `cargo build -p joey-cli` and confirm zero new warnings/errors beyond pre-existing baseline. Commit the empty-but-wired skeleton before Phase 2.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Implement the four data entities + the core `render_turn` tick-loop refactor that ALL user stories build on. No story work can begin until this phase is complete.

**⚠️ CRITICAL**: Blocks US1–US6. These modules are consumed by every animation.

- [X] T004 [P] Implement `RenderCapability` struct + `Capability` enum + `detect()` + `level()` in `crates/joey-cli/src/capability.rs` per data-model Entity 1 and Contract 3. Fields: `is_interactive: bool`, `supports_truecolor: bool`, `supports_unicode: bool`, `term_width: usize`, `target_fps: u32`. `detect()` probes `std::io::stdout().is_terminal()`, `std::env::var("COLORTERM")` (truecolor/24bit), and `terminal_size::terminal_size()`. `level()` returns `NonInteractive` iff `!is_interactive`; else `Reduced` iff no truecolor OR no unicode OR `term_width < 60`; else `Full`. All items `pub(crate)`.
- [X] T005 [P] Implement `AnimationProfile` struct + `AnimationKind` enum (Banner, ThinkingSpinner, StreamingCaret, ToolLine, PromptCaret) + `for_kind(kind, cap) -> &'static AnimationProfile` data-registry lookup in `crates/joey-cli/src/profile.rs` per data-model Entity 2 and Contract 4. Fields: `frames: Vec<String>`, `interval_ticks: u32`, `color: Rgb`, `label: Option<String>`, `reduced: Option<Box<AnimationProfile>>`, `disabled_fallback: String`. Use a `const`/static table — NOT a central match with per-variant business logic (Constitution Principle II). All colors sourced from `joey_core::theme::Theme::pantera()` (FR-009). Leave per-kind frame data as TODO stubs filled in per story.
- [X] T006 [P] Implement `AnimationState` struct + `new()` / `advance(&mut self, profile)` / `current_frame(&self, profile) -> &str` / `finalize(&mut self)` in `crates/joey-cli/src/animation.rs` per data-model Entity 3 and Contract 5. Fields: `kind`, `frame_idx`, `ticks_to_next_frame`, `running`, `started_at: Option<Instant>`, `anchor_row: Option<u16>`. `advance` decrements countdown and wraps `frame_idx` mod `frames.len()`. Add cursor-control repaint helpers using crossterm (`cursor::MoveToColumn`, `terminal::Clear`, `cursor::Hide`/`Show`) — `pub(crate)` only.
- [X] T007 [P] Extend `RenderOptions` (crates/joey-cli/src/render.rs:110) with three new fields per data-model Entity 4: `animations_enabled: bool`, `animation_fps: u32` (default 12), `capability: RenderCapability`. Keep `#[derive(Clone)]`. When `animations_enabled == false` OR `capability.level() == NonInteractive`, the render path MUST short-circuit to plain text (FR-011).
- [X] T008 [P] Add seam tests for the foundational modules (Constitution Principle IV / SC-004): `RenderCapability::level()` parameterized classification (Full/Reduced/NonInteractive) in `crates/joey-cli/src/capability.rs` (inline `#[cfg(test)]`); `AnimationProfile::for_kind` returns non-empty frames under Full/Reduced and ASCII-only glyphs under Reduced in `crates/joey-cli/src/profile.rs`; `AnimationState::advance` wraps `frame_idx` correctly and never indexes out of bounds in `crates/joey-cli/src/animation.rs`. Confirm these FAIL first (TDD), then pass.
- [X] T009 Refactor `render_turn` in `crates/joey-cli/src/render.rs` (currently the blocking `recv().await` loop at render.rs:136) into a `tokio::select!` loop multiplexing `rx.recv()` with `tokio::time::interval(1000/animation_fps ms)` per Contract 1 and research R-001. Branch: when `opts.animations_enabled && opts.capability.is_interactive`, run the animated path (cursor-control repaints on each tick); else run the plain-text path identical to today plus the new persistent-info lines. Keep the external signature `pub async fn render_turn(rx, opts) -> String` unchanged (Contract 1). Preserve existing `total_prompt_tokens`/`total_completion_tokens` accumulators (render.rs:142-143). Depends on T004–T007.
- [X] T010 Wire capability detection at REPL startup: in `crates/joey-cli/src/repl.rs`, call `RenderCapability::detect()` once when constructing `RenderOptions` (around repl.rs:450 where `IsTerminal` is already probed) and populate the three new fields. Read `display.animation_fps` from config if present, else default 12. Depends on T004, T007.
- [X] T011 Add the plain-text fallback seam test (Constitution Principle IV / SC-004 / FR-011): with `Capability::NonInteractive`, `render_turn` output MUST contain no `\x1b[` cursor-control escapes and no `\r`. Use a synthetic `AgentEvent` stream driven to `Done`. Confirm it FAILS first, then passes. Depends on T009.
- [X] T011a [P] Reduced-capability end-to-end seam test (SC-004 / FR-008): drive `render_turn` to `Done` with a synthetic `AgentEvent` stream carrying Unicode-only glyphs (spinner frames, tool names, streamed text with box-drawing) under `Capability::Reduced` and assert the output contains ONLY ASCII-safe glyphs in the animated regions (no non-ASCII spinner frame, no non-ASCII tool icon) and uses ANSI-16 color codes (not truecolor `38;2;…`). Inline `#[cfg(test)]` in `crates/joey-cli/src/render.rs`. Confirm it FAILS first, then passes.

**Checkpoint**: Foundation ready — capability detection, profile registry, animation state, and the tick-loop render_turn are in place with green seam tests. User-story work can now proceed.

---

## Phase 3: User Story 1 — Startup banner entrance animation (Priority: P1) 🎯 MVP

**Goal**: A Joey-branded banner renders with a polished entrance animation on `joey` launch, resolving to the ready `❯` prompt within ~1 second, in Crush/Pantera colors.

**Independent Test**: Launch `joey` cold and confirm an animated branded banner resolves to a ready prompt in ≤ ~1.5s (quickstart Scenario B).

### Tests for User Story 1

- [X] T012 [P] [US1] Seam test: under `Capability::NonInteractive`, `banner_animated` writes only plain text (no cursor escapes); under a mocked `Full` capability with a fake clock, it emits a bounded sequence of partial frames (Contract 6). Add inline `#[cfg(test)]` in `crates/joey-cli/src/render.rs`. Confirm it FAILS first.

### Implementation for User Story 1

- [X] T013 [P] [US1] Register the `Banner` profile data in `crates/joey-cli/src/profile.rs` (T005 stub): gradient wipe-in frames reusing `theme::gradient_fg`/`gradient_diagonal_field`, Pantera colors, `~600–900ms` duration via `interval_ticks`, ASCII-safe reduced variant (`reduced`), plain-text `disabled_fallback`. No non-Pantera colors (FR-009).
- [X] T014 [US1] Implement `pub fn banner_animated(info: &BannerInfo, opts: &RenderOptions)` in `crates/joey-cli/src/render.rs` per Contract 6 and research R-006. Full capability: bounded gradient wipe-in of the logo line + fade-in of info lines via the tick timer, then hand off to the existing `render::banner` static layout (render.rs:518). Reduced: print static banner. NonInteractive: plain text only. MUST complete in ≤ ~1.5s worst case and never block the prompt. Depends on T006, T009, T013.
- [X] T015 [US1] Replace the `render::banner(...)` call at `crates/joey-cli/src/repl.rs:412` with `render::banner_animated(&info, &st.ropts)`. Verify the `❯` prompt becomes ready after the animation completes. Depends on T010, T014.

**Checkpoint**: User Story 1 functional and testable independently — animated branded banner on launch, MVP shippable.

---

## Phase 4: User Story 2 — Thinking/processing spinner while awaiting first token (Priority: P1)

**Goal**: While the agent processes a submitted message, a claude-code-style spinner + static label animates at a steady frame rate until the first token arrives, then clears cleanly into streaming.

**Independent Test**: Submit a slow prompt and confirm the spinner animates during processing and transitions into streaming without flicker (quickstart Scenario C, first phase).

### Tests for User Story 2

- [X] T016 [P] [US2] Seam test: given the `ThinkingSpinner` profile, assert (a) `label` renders as the static "Thinking…" string alongside the frame (FR-002 — label is static, no reasoning text); (b) the profile's color is the Pantera `accent` role (FR-009); (c) NO live reasoning/thinking text is appended to the spinner output (FR-002 negative assertion). (Wraparound itself is covered by T008.) Inline in `crates/joey-cli/src/animation.rs` or `profile.rs`. Confirm it FAILS first.

### Implementation for User Story 2

- [X] T017 [P] [US2] Register the `ThinkingSpinner` profile data in `crates/joey-cli/src/profile.rs` (T005 stub): Joey's own glyph frames (NOT claude-code's literal frames), Pantera `accent` color, `label: Some("Thinking…")` (FR-002), reduced ASCII frames (`|/-\`), empty `disabled_fallback` (label printed once). Depends on T005.
- [X] T018 [US2] In the refactored `render_turn` tick loop (`crates/joey-cli/src/render.rs`), instantiate `ThinkingSpinner` `AnimationState` on turn start (first `ApiCallStart` or equivalent pre-streaming event), advance + repaint the spinner each tick, and `finalize()` (clear the spinner line via cursor control) when the first `ContentDelta` arrives. MUST keep animating smoothly on slow connections without spawning overlapping indicators (edge case). Depends on T009, T017.

**Checkpoint**: User Stories 1 AND 2 work independently — banner + thinking spinner visible on a real turn.

---

## Phase 5: User Story 3 — Streaming assistant text reveal (Priority: P1)

**Goal**: Assistant text streams token-by-token (raw) with an animated caret, then on completion reflows exactly once into formatted markdown in Pantera colors.

**Independent Test**: Send a multi-paragraph prompt and confirm progressive raw reveal + caret, then a single markdown reflow on completion (quickstart Scenario C).

### Tests for User Story 3

- [X] T019 [P] [US3] Seam test for `markdown_to_ansi` (Contract 2 / Principle IV): feed representative markdown (heading H1–H6, fenced code block, bullet list, ordered list, bold, italic, inline code, blockquote, horizontal rule) and assert the output contains the expected Pantera ANSI color sequences per role. Inline `#[cfg(test)]` in `crates/joey-cli/src/markdown.rs`. Confirm it FAILS first.
- [X] T020 [P] [US3] Seam test for the StreamingCaret profile: assert frames non-empty under Full/Reduced, ASCII-only under Reduced. Inline in `crates/joey-cli/src/profile.rs`. Confirm it FAILS first.

### Implementation for User Story 3

- [X] T021 [P] [US3] Implement `pub(crate) fn markdown_to_ansi(input: &str, theme: &Theme) -> String` in `crates/joey-cli/src/markdown.rs` per Contract 2 and research R-003. Parse via `pulldown-cmark` event stream; emit Pantera-styled ANSI: headings → bold + gradient per level (`primary`/`secondary`/`accent`/`info`); bold/italic → ANSI attrs; inline + fenced code → `theme.accent` (fenced gets a language-label line + preserved newlines); lists → indented with Pantera markers; blockquotes → `│` marker in `fg_more_subtle`; horizontal rules → `gradient_diagonal_field`; links → text + URL in `fg_more_subtle`. Pure function, deterministic.
- [X] T022 [P] [US3] Register the `StreamingCaret` profile data in `crates/joey-cli/src/profile.rs` (T005 stub): Joey's own caret glyph set, Pantera color, reduced ASCII variant (`_`), empty `disabled_fallback`. Depends on T005.
- [X] T023 [US3] In `render_turn` (`crates/joey-cli/src/render.rs`), implement the two-phase reveal per research R-002: (a) on `ContentDelta`, append raw text with the `StreamingCaret` frame repainted each tick (capture the start row for in-place caret updates); (b) on `AssistantMessage` OR `Done { final_text, .. }`, perform exactly ONE controlled reflow — move cursor up to the streamed region, clear streamed lines, then print `markdown_to_ansi(final_text, &theme)`. The reflow MUST NOT repeat on subsequent events (FR-003). Depends on T009, T021, T022.

**Checkpoint**: User Stories 1–3 work — the core claude-code feel (banner, spinner, streaming+finalize) is complete and independently testable.

---

## Phase 6: User Story 4 — Tool-call progress with animated status transitions (Priority: P2)

**Goal**: Each tool call renders as its own line with an entry animation, a running spinner while executing, and a resolved done/failed icon + one-line summary + duration, transitioning in place.

**Independent Test**: Trigger a turn that calls tools and confirm each tool line enters → runs → resolves coherently, even on rapid succession (quickstart Scenario D).

### Tests for User Story 4

- [X] T024 [P] [US4] Seam test: assert the `ToolLine` profile has entry/running/resolved frame sets (non-empty under Full/Reduced, ASCII-only under Reduced). Inline in `crates/joey-cli/src/profile.rs`. Confirm it FAILS first.

### Implementation for User Story 4

- [X] T025 [P] [US4] Register the `ToolLine` profile data in `crates/joey-cli/src/profile.rs` (T005 stub): entry reveal frames (2–3 frame gradient build), running spinner frames, resolved done/failed icon glyphs, Pantera colors, reduced ASCII variants, plain-text `disabled_fallback` (e.g. "[tool] name — done"). Depends on T005.
- [X] T026 [US4] In `render_turn` (`crates/joey-cli/src/render.rs`), enhance `ToolStart`/`ToolEnd` handling (currently render.rs:240–300) per research R-007: on `ToolStart`, print the entry animation + start a running spinner on the SAME logical line (capture `anchor_row`); advance the spinner each tick; on `ToolEnd`, rewrite the SAME line in place (cursor-up + clear-line) to the resolved icon + one-line summary + duration. NO expandable/collapsible detail (clarification Q4). Per-tool granularity (not aggregated). Rapid-succession tools MUST NOT corrupt each other's lines (edge case). Depends on T009, T025.

**Checkpoint**: User Stories 1–4 work independently — full agentic feedback loop visible.

---

## Phase 7: User Story 5 — Persistent token/cost line + turn-complete summary (Priority: P2)

**Goal**: A persistent in-flight usage indicator updates during a turn, and a turn-complete summary line (tokens used + duration) appears after each response, without overwriting the streamed text.

**Independent Test**: Run a turn to completion and confirm the usage indicator updates during the turn and a summary line appears after (quickstart Scenario E).

### Tests for User Story 5

- [X] T027 [P] [US5] Seam test: given accumulated token counts + a duration, assert (a) the summary line formatter emits the expected Pantera-colored "tokens in/out + duration" string with no cursor escapes; AND (b) NON-INTERFERENCE — for a fixed event stream that produces N lines of streamed assistant text followed by the summary, assert the summary line is positioned BELOW the streamed region (the streamed N lines are intact and unmodified in the output; no cursor-up escape crosses from the summary into the streamed block). Inline `#[cfg(test)]` in `crates/joey-cli/src/render.rs`. Confirm it FAILS first.

### Implementation for User Story 5

- [X] T028 [US5] In `render_turn` (`crates/joey-cli/src/render.rs`), add a persistent in-flight usage indicator updated via the tick loop: read `total_prompt_tokens`/`total_completion_tokens` (accumulated from `ApiCallEnd { usage }`, render.rs:142–143) and repaint a usage line on each tick while a turn is in flight (position so it does NOT overwrite streamed text). On `Done`, stop the indicator and emit the turn-complete summary line (tokens in/out + duration sourced from `session_start`/`started_at`) in Pantera colors. Restyle the existing turn summary at render.rs:365–376 rather than duplicating it. Cost estimation is out of scope unless a price table exists in config (research R-004). Depends on T009.

**Checkpoint**: User Stories 1–5 work — claude-code-style persistent status is live.

---

## Phase 8: User Story 6 — Polished prompt with subtle idle animation (Priority: P3)

**Goal**: The reedline `❯` prompt has a subtle idle caret blink and smooth multiline expansion, in Pantera colors.

**Independent Test**: Focus the idle prompt and confirm a blinking caret with no flicker; type a multiline message and confirm smooth expansion (quickstart, US6 acceptance scenarios).

### Tests for User Story 6

- [X] T029 [P] [US6] Seam test: assert the `PromptCaret` profile blinks on the tick interval (non-empty frames under Full/Reduced, ASCII-only under Reduced). Inline in `crates/joey-cli/src/profile.rs`. Confirm it FAILS first.

### Implementation for User Story 6

- [X] T030 [P] [US6] Register the `PromptCaret` profile data in `crates/joey-cli/src/profile.rs` (T005 stub): two-frame blink cycle, Pantera color, reduced ASCII variant (`_`), empty `disabled_fallback`. Depends on T005.
- [X] T031 [US6] **[SPIKE FIRST — DESCOPED]** reedline owns a synchronous blocking editor loop and cannot support an external tick-driven blink. US6 descoped to static colored prompt (DarkGrey → Cyan). Decision recorded in research.md. In `crates/joey-cli/src/repl.rs`, integrate the `PromptCaret` blink with the reedline prompt. reedline owns its own synchronous editor loop, so the blink mechanism must be confirmed feasible before full implementation: either (a) embed the blink in the reedline prompt color escape (re-painted by reedline on its own redraw — no separate thread), or (b) use reedline's `Highlighter`/prompt-render hook driven by the CLI's tick loop. Drive the blink from the SAME tick loop (FR-010). Ensure multiline expansion grows smoothly without a visible jump. MUST remain plain-text (no escapes) under NonInteractive. If the spike shows reedline cannot support an external tick-driven blink, DESCOPE US6 to "static colored prompt, no blink" and record the decision in research.md. Depends on T030, T010.

**Checkpoint**: All user stories (1–6) independently functional and testable.

---

## Phase 9: Polish & Cross-Cutting Concerns

**Purpose**: Cross-story hardening, fallback coverage, resize safety, and final validation.

- [X] T032 [P] Audit reduced-capability fallback across all five animation kinds (Banner/Spinner/Caret/ToolLine/PromptCaret): every `reduced` variant uses only ASCII-safe glyphs and ANSI-16 (via existing `theme::Rgb::ansi()`), and every `disabled_fallback` is correct plain text. Update profile.rs as needed. (SC-004 / FR-007 / FR-008.)
- [X] T033 [P] Verify resize safety (FR-007, edge case "terminal resized during animation"): use crossterm's event-based resize detection (`event::read()` matching `Event::Resize`), polled in a dedicated arm of the `tokio::select!` tick loop (NOT a SIGWINCH signal handler, NOT a per-tick terminal_size poll — the event is lower-overhead and deterministic). On resize, re-read width and re-layout the banner, tool, and usage lines without leaving partial frames or forcing a redraw flash of unrelated content. Add an automated unit-level test that the layout function adapts to a width change without emitting partial frames. Manual QA in at least 3 terminal emulators (SC-003).
- [X] T034 Verify SC-005 (joey-tui untouched): run `git diff --stat main -- crates/joey-tui` and confirm no changes attributable to this feature; launch `target/release/joey --tui` and confirm identical behavior.
- [X] T035 Run the full automated seam-test suite: `cargo test -p joey-cli` (quickstart Scenario A). All seam tests (T008, T011, T011a, T012, T016, T019, T020, T024, T027, T029, T035a) MUST be green — this is the SC-004 done-gate.
- [X] T035a [P] Regression-coverage test (Constitution VII): add a characterization test in `crates/joey-cli/src/render.rs` (inline `#[cfg(test)]`) that drives `render_turn` with a fixed synthetic `AgentEvent` stream (ToolStart/ToolEnd, ContentDelta, ApiCallEnd, Done) under `Capability::NonInteractive` and asserts the output is byte-equivalent to the pre-feature plain-text shape (same line order, same content, no new cursor escapes, no removed lines). This locks the existing public behavior against drift. Confirm it FAILS against the old code shape only if behavior intentionally changed; otherwise it MUST stay green on every increment.
- [X] T036 Run the workspace build + tests: `cargo build` and `cargo test` at repo root. Confirm zero new warnings/errors beyond baseline.
- [X] T037 Performance check: confirm idle CPU overhead is negligible (tick timer dormant except during active turn/banner) and the ~12 fps default introduces no measurable latency on `ContentDelta`. Document the default tick interval in code comments.
- [X] T038 Run quickstart.md manual validation end-to-end (Scenarios B–F) on a real terminal. Explicit checklist: (a) no `\r`-tearing on any animated line (verify with `| cat -v` showing no stray `^M` outside controlled repaints); (b) no overlapping/corrupted frames when multiple tool calls fire in rapid succession; (c) spinner never freezes on a multi-second think; (d) finalize reflow happens exactly once per message. Record any visual regressions and file follow-ups.
- [X] T039 [P] Update `crates/joey-cli/src/render.rs` module-level doc comment + any user-facing docs to describe the new animation behavior, the `display.animation_fps` config knob, and the non-TTY plain-text fallback.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — start immediately. T003 gates Phase 2.
- **Foundational (Phase 2)**: Depends on Phase 1. T009 depends on T004–T007; T011 depends on T009. **BLOCKS all user stories.**
- **User Stories (Phases 3–8)**: All depend on Phase 2 completion.
  - US1 (Phase 3), US2 (Phase 4), US3 (Phase 5) are P1 — highest value, do first.
  - US4 (Phase 6), US5 (Phase 7) are P2.
  - US6 (Phase 8) is P3.
  - Within a story, tests → profile data → render integration, in that order.
- **Polish (Phase 9)**: Depends on all desired user stories being complete. T035 (full test run) is the final done-gate.

### User Story Dependencies

- **US1 (P1)**: Depends on Phase 2 only. No cross-story deps.
- **US2 (P1)**: Depends on Phase 2 only. Independent of US1.
- **US3 (P1)**: Depends on Phase 2 only. Independent of US1/US2.
- **US4 (P2)**: Depends on Phase 2 only. Independent of US1–US3.
- **US5 (P2)**: Depends on Phase 2 only. Independent of US1–US4.
- **US6 (P3)**: Depends on Phase 2 only. Independent of US1–US5.

All six stories are mutually independent at the data level; they share the Phase 2 foundation (capability/profile/state/render_turn) but do not import each other's logic.

### Within Each User Story

- Seam tests written FIRST and confirmed to FAIL.
- Profile data (if story-specific) registered in profile.rs.
- Render/render_turn integration last (depends on the tick loop from T009).

### Parallel Opportunities

- Phase 2: T004, T005, T006, T007, T008 are all independent files — run in parallel. T009 serializes on all four.
- Within each story: the profile-data task and the seam-test task are different files and can run in parallel before the render-integration task.
- Different user stories can be worked in parallel by different developers once Phase 2 lands (no cross-story file conflicts: profile.rs entries are additive, render.rs edits are in distinct code regions).
- Polish tasks T032, T033, T039 are parallelizable.

---

## Parallel Example: User Story 3

```bash
# Launch the independent pieces of US3 together (different files):
Task: "Seam test for markdown_to_ansi in crates/joey-cli/src/markdown.rs"   # T019
Task: "Seam test for StreamingCaret profile in crates/joey-cli/src/profile.rs" # T020
Task: "Implement markdown_to_ansi in crates/joey-cli/src/markdown.rs"       # T021
Task: "Register StreamingCaret profile in crates/joey-cli/src/profile.rs"   # T022

# Then serialize on the render_turn integration (depends on T009 + T021 + T022):
Task: "Two-phase streaming reveal in crates/joey-cli/src/render.rs"         # T023
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (T001–T003).
2. Complete Phase 2: Foundational (T004–T011) — CRITICAL, blocks all stories.
3. Complete Phase 3: User Story 1 (T012–T015).
4. **STOP and VALIDATE**: Launch `joey`, confirm animated branded banner + ready prompt (quickstart Scenario B); confirm `cargo test -p joey-cli` is green.
5. Ship/demo the MVP.

### Incremental Delivery

1. Setup + Foundational → foundation ready with green seam tests.
2. + US1 → animated banner (MVP) → validate → demo.
3. + US2 → thinking spinner → validate.
4. + US3 → streaming + markdown finalize → validate (now the core claude-code feel is complete).
5. + US4 → tool-call lines → validate.
6. + US5 → persistent usage + summary → validate.
7. + US6 → polished prompt → validate.
8. Polish phase → full quickstart run → feature complete.

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational together (Phase 2 is the serialization point).
2. Once Foundational lands:
   - Dev A: US1 (banner) + US5 (usage line) — distinct render.rs regions.
   - Dev B: US2 (spinner) + US4 (tool lines).
   - Dev C: US3 (streaming/markdown) + US6 (prompt).
3. Stories integrate independently; Phase 9 ties them together.

---

## Notes

- Every animation color MUST come from `joey_core::theme::Theme::pantera()` (FR-009). No hardcoded non-Pantera colors — add a lint-style review in T032.
- The single tick loop (FR-010) lives in the refactored `render_turn` (T009). US6's prompt blink reuses the same loop, not a second timer.
- `joey-tui` is read-only (SC-005). T034 verifies this with a git diff.
- No cross-crate API changes (Constitution Principle I): `AgentEvent`, `Usage`, `Theme` are consumed read-only.
- All seams are `pub(crate)` — no new cross-crate public surface (Constitution Principle III).

---

## Phase 10: Convergence

> Re-assessed by `/speckit-converge` on 2026-07-25 against the live codebase.
> Full per-FR evidence in [convergence.md](convergence.md). Build green, 85/85
> tests pass, `joey-tui` untouched (SC-005). Original T040–T044 entries below
> are reconciled to their true code status; new findings follow as T045–T051.

### Reconciled (prior convergence pass)

- [X] T040 ~~Render the streaming caret animation during `ContentDelta`~~ — **DONE in code** (`render.rs:799-815` paints the caret; `:414-423`/`:651-660` erase it). The `StreamingCaret` profile is now instantiated. Residual blink defect tracked as **T045**.
- [X] T041 ~~Implement per-tool entry→running→resolved in-place animation~~ — **DONE in code** (`render.rs:466-513` entry + `anchor_row` capture, `:561-585` in-place resolved rewrite, `:817-837` tick repaint). Residual name-drop defect tracked as **T046**.
- [X] T042 ~~Add a persistent in-flight usage indicator per FR-005 / US5/AC1~~ — **RESOLVED via T047**: in-flight usage is rendered on the spinner line (`usage_suffix` on `ApiCallStart` + tick repaint). A separate anchored row was rejected as fragile on an append-only line-based CLI (Constitution VII). See T047 for the implemented fix.
- [X] T043 ~~Event-based resize detection~~ — **RESOLVED-BY-DECISION**: lazy width re-read via `box_width()` on each render call; rationale recorded in `research.md` ("T043 Decision"). No further code work; only the missing unit test (T050) remains.
- [X] T044 ~~Fix the markdown reflow line-count to account for terminal wrapping~~ — **DONE in code**: `count_visual_lines` (`render.rs:176-198`) now uses `unicode-width`-aware wrapping; covered by `count_visual_lines_wraps_long_lines`.

### New convergence findings (2026-07-25)

- [X] T045 [US3] Fix the streaming caret blink (FR-003(a), CF-1, P2). DONE: `animation::tick_phase` now takes a monotonic `tick_count` (`animation.rs`), incremented each tick in `render_turn`; the caret alternates `▌`/` `. Unit tests `tick_phase_advances_over_ticks` + `tick_phase_empty_profile_is_zero` added.
- [X] T046 [US4] Keep the tool name visible while the tool spinner runs (FR-004, CF-2, P2). DONE: `active_tool` now carries the summary too; the tick-arm repaint prints `  {frame} {name} ({summary})` (`render.rs`), mirroring the entry print.
- [X] T047 [US5] Make the persistent in-flight usage indicator actually render (FR-005/AC1, CF-3, P1). DONE: in-flight usage is appended to the spinner line via `usage_suffix(...)` on both the `ApiCallStart` initial print and the tick repaint (`render.rs`), reflecting accumulated tokens while the agent works. A separate anchored row was rejected as fragile on an append-only line-based CLI (Constitution VII); the obsolete dead-code block was removed.
- [X] T048 [US5] Add turn duration to the turn-complete summary (FR-005/US5/AC2, CF-4, P1). DONE: `turn_start: Option<Instant>` captured on `TurnStart`; the `Done` summary appends `fmt_duration(turn_start.elapsed())` (`render.rs`).
- [X] T049 Fix the documented done-gate test command (SC-004, CF-5, P3). DONE: `quickstart.md` Scenario A, `tasks.md` T035, and the MVP-validate step now invoke `cargo test -p joey-cli` (`joey-cli` is a binary with inline `#[cfg(test)]` tests; `--lib` failed with "no library targets found"). Verified: 85 passed.
- [X] T050 [P] Add a resize-layout adaptation unit test (FR-007/T033, CF-6, P3). DONE: `reflow_line_count_adapts_to_width` test added in `render.rs`, asserting `count_visual_lines` (the width-aware reflow layout helper) is monotonic in width — locks the lazy width re-read chosen in `research.md`.
- [X] T051 [P] Minor polish (CF-7, P3). DONE: (a) `markdown_to_ansi` now emits a `╭─[ lang ]` label line for fenced code blocks (`markdown.rs`); (b) NonInteractive profile `color` closures return `t.fg_base` (Pantera) instead of a hardcoded white literal (`profile.rs`).
