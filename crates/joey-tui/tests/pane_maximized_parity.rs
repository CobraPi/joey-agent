//! T019 / US4 (feature 017): maximized-viewers PARITY between the focused
//! subagent pane and the orchestrator screen (Ctrl+O output viewer, live
//! reasoning panel, Ctrl+A stats page, FR-006..FR-008).
//!
//! TDD: written BEFORE T020/T021/T022 land. Expected outcome against
//! current code, test by test:
//!
//! EXPECTED-FAIL today (fail on ASSERTIONS, encoding the T020/T021
//! contract — they must turn green when the pane work lands):
//!   - `ctrl_o_from_focused_pane_opens_viewer_on_pane_tool`   (T020/D6:
//!     Ctrl+O is Main-gated at app.rs:1057 and the pane render branch
//!     never draws the output viewer, so no viewer chrome appears)
//!   - `pane_output_viewer_scrolls_full_range`                (T020/D6:
//!     same gating — the viewer never opens from a pane entry)
//!   - `pane_reasoning_panel_renders_pane_streaming_reasoning`(T021/D6:
//!     SubagentPane.streaming_reasoning is never rendered anywhere)
//!   - `pane_apply_flushes_reasoning_on_assistant_message`    (T021/D6:
//!     pane_apply accumulates ReasoningDelta but never flushes it to a
//!     Reasoning TranscriptItem)
//!   - `pane_apply_flushes_reasoning_on_tool_start`           (same)
//!
//! CONTRACT PINS (expected to PASS today — they lock already-landed
//! behavior so the pane work cannot regress it; constitution VII):
//!   - `ctrl_a_from_focused_pane_shows_pane_stats_not_main`   (T004:
//!     Ctrl+A is deliberately target-agnostic and the stats page self-
//!     retargets to the focused pane)
//!   - `pane_stats_expanded_context_survives_focus_switch`    (T004/FR-010)
//!   - `pane_stats_stream_scrolls_full_range`                 (T004)
//!   - `neurocode_explorer_not_reachable_from_plain_pane`     (FR-008
//!     negative half: the explorer never renders for a pane the mode did
//!     not spawn; the positive half landed with T022 — see
//!     `neurocode_explorer_reachable_from_mode_spawned_pane` and
//!     `neurocode_explorer_follows_focus_between_mode_and_plain_panes`)
//!   - `main_ctrl_o_with_panes_present_targets_main`          (Ctrl+O with
//!     focused_subagent == None still opens the MAIN viewer)
//!   - `main_ctrl_a_with_panes_present_targets_main`          (same for
//!     the main stats page)
//!
//! Key events cannot be driven from integration tests (`Tui::handle_key`
//! needs a real TTY), so each test calls the exact App mutator the key
//! arm dispatches to — the same convention as pane_scroll_parity.rs /
//! pane_expand_parity.rs:
//!   - Ctrl+O → `App::toggle_output_viewer(None)`   (app.rs:1057-1061)
//!   - Ctrl+A → `App::toggle_stats()`               (app.rs:1070-1073)
//!   - Ctrl+P → `App::focus_subagent(None)` +
//!              `App::set_focused_pane_stats_view(None)` (app.rs:1031-1040)
//!   - g/Home in pane stats → `App::set_focused_pane_stats_view(Some(0))`
//!   - viewer scroll → `App::output_viewer_scroll_up/down` (the arms at
//!     app.rs:831-871 while `output_viewer_open`)

mod common;

use common::*;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use joey_agent_core::events::{AgentEvent, ContextEntry};
use joey_tui::neurocode_viz::VizTab;
use joey_tui::state::{App, TranscriptItem};

const W: u16 = 100;
const H: u16 = 30;

// ── local helpers (common/mod.rs is frozen for this task) ─────────────

/// Route `ev` to the focused pane's child (id 1) exactly the way the
/// orchestration layer does: wrapped in a `SubagentEvent`.
fn child_event(a: &mut App, ev: AgentEvent) {
    a.apply(AgentEvent::SubagentEvent {
        id: 1,
        event: Box::new(ev),
    });
}

/// A context entry whose preview/full-content markers are pane-scoped
/// ("pane-ctx entry {i}" / "pane-ctx-full-{i}") so stats-page assertions
/// can discriminate the pane's stream from the main transcript's.
fn pane_ctx_entry(i: usize) -> ContextEntry {
    ContextEntry {
        role: if i % 2 == 0 { "user" } else { "assistant" }.to_string(),
        tokens: 100 + i as u64,
        preview: format!("pane-ctx entry {i}"),
        has_tool_calls: false,
        is_compressed_summary: false,
        full_content: format!("pane-ctx-full-{i}\nsecond body line {i}"),
    }
}

/// A main-transcript context entry with MAIN-scoped markers.
fn main_ctx_entry(i: usize) -> ContextEntry {
    ContextEntry {
        role: "user".to_string(),
        tokens: 50 + i as u64,
        preview: format!("main-ctx entry {i}"),
        has_tool_calls: false,
        is_compressed_summary: false,
        full_content: format!("main-ctx-full-{i}"),
    }
}

/// Feed the pane (child 1) a ContextSnapshot with `n` pane-marked entries.
fn send_pane_context(a: &mut App, n: usize) {
    child_event(
        a,
        AgentEvent::ContextSnapshot {
            entries: (0..n).map(pane_ctx_entry).collect(),
            system_tokens: 400,
            history_tokens: 1200,
            context_window: 8000,
            compression_threshold: 6000,
            compactions: 0,
            model: "test-model".to_string(),
        },
    );
}

/// Feed the MAIN transcript a ContextSnapshot with main-marked entries.
fn send_main_context(a: &mut App, n: usize) {
    a.apply(AgentEvent::ContextSnapshot {
        entries: (0..n).map(main_ctx_entry).collect(),
        system_tokens: 300,
        history_tokens: 900,
        context_window: 8000,
        compression_threshold: 6000,
        compactions: 0,
        model: "test-model".to_string(),
    });
}

/// An App with ONE focused pane (child 1, "parity child") holding exactly
/// `items` (manual build, like pane_expand_parity's `focused_pane_with`).
fn focused_pane_with(items: Vec<TranscriptItem>) -> App {
    let mut a = app();
    a.apply(spawn(1, "parity child"));
    let idx = a
        .subagent_panes
        .iter()
        .position(|p| p.child_id == 1)
        .expect("spawn created the pane");
    for it in items {
        a.subagent_panes[idx].push_item(it);
    }
    a.focus_subagent(Some(idx));
    a
}

/// The pane fixture for the Ctrl+O tests: focused pane with two small
/// tools (tool1 @1, tool6 @6 from the 5-kind cycle), a 220-line tool31 as
/// the most recent pane tool, and a MAIN-transcript tool77 so any
/// main-leaking resolution is detectable.
fn pane_viewer_app() -> App {
    let mut a = focused_pane_with((0..10)
        .map(|i| match i % 5 {
            0 => assistant_item(i),
            1 => tool_item(i, 6),
            2 => reasoning_item(i),
            3 => file_diff_item(i, 4),
            _ => user_item(i),
        })
        .collect());
    // Most recent PANE tool: 220 result lines (full scroll range).
    a.focused_pane_mut().unwrap().push_item(tool_item(31, 220));
    // Main marker: if the viewer resolved against the main transcript it
    // would land on this item instead.
    a.push_item(tool_item(77, 6));
    a
}

// ── 1. Ctrl+O from a focused pane → output viewer on THAT pane ────────

/// With a pane focused, Ctrl+O (the `toggle_output_viewer(None)` the key
/// arm dispatches to) must open the maximized viewer on the PANE's most
/// recent tool item and render it through the shared draw_output_viewer
/// chrome — not the main transcript's tool. Pin: viewer chrome "output ·
/// finished · Ctrl+O or Esc to restore" + the PANE tool's tail content
/// (head off-screen at the follow-tail opening position).
#[test]
fn ctrl_o_from_focused_pane_opens_viewer_on_pane_tool() {
    let mut a = pane_viewer_app();

    // What the Ctrl+O arm dispatches to (app.rs:1057-1061). TARGET (T020,
    // D6): with a pane focused this resolves against the PANE transcript.
    a.toggle_output_viewer(None);
    assert!(
        a.output_viewer_open,
        "PARITY (fails until T020): Ctrl+O with a pane focused opens the output viewer"
    );

    let f = render_frame(&a, W, H);
    assert!(
        f.contains("output · finished"),
        "PARITY (fails until T020): shared draw_output_viewer chrome rendered while a pane is focused"
    );
    assert!(
        f.contains("Ctrl+O or Esc to restore"),
        "viewer title carries the restore affordance"
    );
    // The viewer targets the PANE's most recent tool (tool31), opened at
    // the live tail: line 219 visible, line 0 scrolled off.
    assert!(
        f.contains("tool 31 output line 219"),
        "PARITY: viewer shows the focused pane's most recent tool output (tail)"
    );
    assert!(
        !f.contains("tool 31 output line 0"),
        "220-line result opens at the tail, head scrolled off"
    );
    assert!(
        f.contains("end of output"),
        "follow-tail footer state shown for the finished tool"
    );
    // Toggling again docks it back (same key, same mutator).
    a.toggle_output_viewer(None);
    assert!(!a.output_viewer_open, "second Ctrl+O closes the viewer");
}

// ── 2. Pane live reasoning panel ──────────────────────────────────────

/// The focused pane's streaming_reasoning must render through the shared
/// draw_reasoning widget (T021/D6): the panel header ("◆ thinking") and
/// the pane's live text are visible, tail-followed. The MAIN accumulator
/// must NOT leak into the pane view (focused-view isolation).
#[test]
fn pane_reasoning_panel_renders_pane_streaming_reasoning() {
    let mut a = focused_pane_with(vec![user_item(0)]);
    let pane_reasoning = (0..40)
        .map(|j| format!("pane think line {j}"))
        .collect::<Vec<_>>()
        .join("\n");
    child_event(&mut a, AgentEvent::ReasoningDelta(pane_reasoning));
    // Main-side live reasoning (must stay invisible while the pane is
    // focused — the pane's stream is what renders).
    a.streaming_reasoning = "main think marker".to_string();
    a.reasoning_open = true;

    let f = render_frame(&a, W, H);
    assert!(
        f.contains("pane think line 39"),
        "PARITY (fails until T021): the focused pane's streaming_reasoning renders live"
    );
    assert!(
        f.contains("thinking"),
        "PARITY: the shared draw_reasoning header renders for the pane"
    );
    assert!(
        !f.contains("main think marker"),
        "isolation: the MAIN reasoning accumulator never renders while a pane is focused"
    );

    // Counterpart pin: unfocused, the pane's reasoning is NOT rendered
    // anywhere (the pane panel is a focused-pane affordance).
    a.focus_subagent(None);
    let f = render_frame(&a, W, H);
    assert!(
        !f.contains("pane think line 39"),
        "unfocused: the pane's live reasoning stays in its pane"
    );
}

/// T034 (US4, FR-008, D6): the pane reasoning panel is fully pane-aware —
/// the docked title carries the SAME affordance as main ("click to
/// expand"), and expanding (the `toggle_focused_pane_reasoning_expanded`
/// mutator the click/Esc arms dispatch to) switches the title to the
/// collapse variant and renders the pane's stream through the shared
/// expanded-path chrome. (The click/Esc/wheel routing itself is pinned
/// inline in app.rs `reasoning_expand_tests` — `Tui::new_for_test` is
/// cfg(test)-gated, per this suite's header convention.)
#[test]
fn pane_reasoning_panel_title_and_expansion_match_main() {
    let mut a = focused_pane_with(vec![user_item(0)]);
    let pane_reasoning = (0..40)
        .map(|j| format!("pane think line {j}"))
        .collect::<Vec<_>>()
        .join("\n");
    child_event(&mut a, AgentEvent::ReasoningDelta(pane_reasoning));

    // Docked: same affordance segments as main's docked strip.
    let f = render_frame(&a, W, H);
    assert!(
        f.contains("reasoning · live · click to expand"),
        "PARITY (T034): the docked pane panel carries the expand affordance"
    );

    // Expanded (what the click dispatches to): collapse affordance + the
    // pane's live text still visible through the shared draw_reasoning.
    a.toggle_focused_pane_reasoning_expanded();
    assert!(a.focused_pane().unwrap().reasoning_expanded);
    let f = render_frame(&a, W, H);
    assert!(
        f.contains("reasoning · live · click or Esc to collapse"),
        "PARITY (T034): the expanded pane panel carries the collapse affordance"
    );
    assert!(
        f.contains("pane think line 39"),
        "the pane's stream renders through the expanded path"
    );
    // Docking back restores the docked title.
    a.toggle_focused_pane_reasoning_expanded();
    let f = render_frame(&a, W, H);
    assert!(f.contains("reasoning · live · click to expand"));
    assert!(!a.focused_pane().unwrap().reasoning_expanded);
}

// ── 3. pane_apply flushes streaming_reasoning on completion ───────────

/// Pure state (T021/D6): a pane's accumulated streaming_reasoning is
/// flushed to a `Reasoning` TranscriptItem when the child's message
/// completes (`AssistantMessage`) — mirroring the main loop's
/// flush_reasoning-on-boundary semantics. The pane's accumulator empties
/// and the committed item carries the full streamed text, Collapsed.
#[test]
fn pane_apply_flushes_reasoning_on_assistant_message() {
    let mut a = app();
    a.apply(spawn(1, "flush child"));

    child_event(&mut a, AgentEvent::ReasoningDelta("think A\nthink B".to_string()));
    child_event(&mut a, AgentEvent::AssistantMessage("final answer".to_string()));

    let pane = &a.subagent_panes[0];
    assert!(
        pane.streaming_reasoning.is_empty(),
        "PARITY (fails until T021): AssistantMessage flushes the pane's streaming_reasoning"
    );
    assert_eq!(pane.transcript.len(), 2, "Reasoning + Assistant committed");
    match &pane.transcript[0] {
        TranscriptItem::Reasoning { text, expand_state, .. } => {
            assert_eq!(text, "think A\nthink B", "flushed item carries the full stream");
            assert_eq!(*expand_state, joey_tui::state::ReasoningExpandState::Collapsed);
        }
        other => panic!("expected a Reasoning item, got {other:?}"),
    }
    assert!(matches!(
        &pane.transcript[1],
        TranscriptItem::Assistant { text, .. } if text == "final answer"
    ));
}

/// Same contract at the ToolStart boundary: when the child starts a tool
/// after thinking, the pending reasoning commits BEFORE the tool item —
/// the main loop's flush ordering (state.rs App::apply ToolStart arm).
#[test]
fn pane_apply_flushes_reasoning_on_tool_start() {
    let mut a = app();
    a.apply(spawn(1, "flush child"));

    child_event(&mut a, AgentEvent::ReasoningDelta("deep thought".to_string()));
    child_event(
        &mut a,
        AgentEvent::ToolStart {
            name: "terminal".to_string(),
            emoji: "💻".to_string(),
            summary: "cargo build".to_string(),
        },
    );

    let pane = &a.subagent_panes[0];
    assert!(
        pane.streaming_reasoning.is_empty(),
        "PARITY (fails until T021): ToolStart flushes the pane's streaming_reasoning"
    );
    assert_eq!(pane.transcript.len(), 2, "Reasoning flushed before the Tool item");
    assert!(matches!(&pane.transcript[0], TranscriptItem::Reasoning { text, .. } if text == "deep thought"));
    assert!(matches!(&pane.transcript[1], TranscriptItem::Tool { .. }));
}

// ── 4. Ctrl+A from a focused pane → THAT pane's stats (PIN, T004) ─────

/// Ctrl+A is deliberately target-agnostic (app.rs:1063-1073): the stats
/// page self-retargets to the focused pane. With both a main and a pane
/// context snapshot present, the pane's stats page renders — pane goal,
/// pane context entries — and the MAIN page does not.
#[test]
fn ctrl_a_from_focused_pane_shows_pane_stats_not_main() {
    let mut a = focused_pane_with(vec![user_item(0)]);
    send_pane_context(&mut a, 3);
    send_main_context(&mut a, 3);

    // What the Ctrl+A arm dispatches to (app.rs:1070-1073).
    a.toggle_stats();
    assert!(a.stats_open);

    let f = render_frame(&a, W, H);
    assert!(f.contains("subagent stats"), "per-pane stats page rendered");
    assert!(f.contains("goal: parity child"), "dashboard shows the pane's goal");
    assert!(f.contains("pane-ctx entry"), "the pane's context stream renders");
    assert!(
        !f.contains("main-ctx entry"),
        "the MAIN context stream never renders while a pane is focused"
    );
    assert!(
        !f.contains("◆ agent stats"),
        "the MAIN stats title never renders while a pane is focused"
    );
}

// ── 5. Per-pane stats state survives focus switches (PIN, T004/FR-010) ─

/// An expanded context entry in the focused pane's stats page survives a
/// Ctrl+P focus switch away and back: expansions live on the pane, and
/// the focus switch only resets the departed pane's SCROLL anchor (the
/// exact Ctrl+P arm sequence from app.rs:1031-1040).
#[test]
fn pane_stats_expanded_context_survives_focus_switch() {
    let mut a = focused_pane_with(vec![user_item(0)]);
    send_pane_context(&mut a, 3);

    a.toggle_stats();
    let _ = render_frame(&a, W, H); // records stats geometry
    // What the pane-stats Space/click arm dispatches to.
    a.toggle_pane_context_entry(1);
    let f = render_frame(&a, W, H);
    assert!(
        f.contains("pane-ctx-full-1"),
        "expanded entry renders its full content inline"
    );
    assert!(a.subagent_panes[0].expanded_context.contains(&1));

    // Ctrl+P: back to the orchestrator (exact arm sequence).
    a.focus_subagent(None);
    a.set_focused_pane_stats_view(None);
    assert!(
        a.subagent_panes[0].expanded_context.contains(&1),
        "expansion survives the focus switch (per-pane state)"
    );

    // Focus the pane again (rail click equivalent) — still expanded.
    a.focus_subagent(Some(0));
    assert!(a.stats_open, "stats page stays open across focus switches");
    let f = render_frame(&a, W, H);
    assert!(
        f.contains("subagent stats"),
        "pane stats page re-renders after refocus"
    );
    assert!(
        f.contains("pane-ctx-full-1"),
        "the expanded entry is still expanded after the round trip"
    );
}

// ── 6. Pane stats stream scrolls its full range (PIN, T004) ───────────

/// State-level full scroll in the pane's maximized stats view: a long
/// context stream overflows, follow-tail shows the newest entry, g/Home
/// (set_focused_pane_stats_view(Some(0))) pins the top with the frozen
/// indicator, G/End resumes follow, and the mutator walk
/// (pane_stats_scroll_up/down) freezes/clamps exactly like the main
/// stats page's stats_scroll_up/down.
#[test]
fn pane_stats_stream_scrolls_full_range() {
    let mut a = focused_pane_with(vec![user_item(0)]);
    send_pane_context(&mut a, 24);
    a.toggle_stats();

    let _ = render_frame(&a, W, H); // records pane.last_stats_max_anchor
    let max = a.focused_pane().unwrap().last_stats_max_anchor.get();
    assert!(max > 0, "24 entries overflow the pane stats viewport (max={max})");

    // Follow-tail: newest visible, oldest off-screen.
    let tail = render_frame(&a, W, H);
    assert!(tail.contains("pane-ctx entry 23"), "newest entry visible at follow-tail");
    assert!(!tail.contains("pane-ctx entry 0"), "oldest entry scrolled off at follow-tail");

    // g / Home → top (frozen).
    a.set_focused_pane_stats_view(Some(0));
    let top = render_frame(&a, W, H);
    assert!(top.contains("pane-ctx entry 0"), "g pins the stream to the top");
    assert!(
        top.contains("below · scroll"),
        "frozen-state footer indicator shown while pinned"
    );

    // G / End → back to follow.
    a.set_focused_pane_stats_view(None);
    let bot = render_frame(&a, W, H);
    assert!(bot.contains("pane-ctx entry 23"), "G re-pins to the live tail");
    assert!(!bot.contains("pane-ctx entry 0"), "top scrolled off again");

    // Mutator walk: 3 up from the tail anchor, 1 down, then a huge down
    // clamps back to follow (mirrors stats_scroll_up/down semantics).
    a.pane_stats_scroll_up(3);
    assert_eq!(a.focused_pane().unwrap().stats_view, Some(max - 3));
    a.pane_stats_scroll_down(1);
    assert_eq!(a.focused_pane().unwrap().stats_view, Some(max - 2));
    a.pane_stats_scroll_down(usize::MAX);
    assert_eq!(
        a.focused_pane().unwrap().stats_view,
        None,
        "scrolling past the tail resumes auto-follow"
    );
}

// ── 7. Viewer from a pane entry scrolls its full range (T020) ─────────

/// The maximized output viewer opened from a pane entry (Ctrl+O) must
/// scroll its FULL range with the shared viewer scroll state: a 220-line
/// result overflows, the frozen anchor walks to the very first line, and
/// scrolling back down resumes follow ("end of output").
#[test]
fn pane_output_viewer_scrolls_full_range() {
    let mut a = pane_viewer_app();
    a.toggle_output_viewer(None);
    assert!(
        a.output_viewer_open,
        "PARITY (fails until T020): viewer opens from the focused pane"
    );

    let _ = render_frame(&a, W, H); // records last_output_viewer_max_anchor
    let max = a.last_output_viewer_max_anchor.get();
    assert!(max > 0, "220-line viewer content overflows the viewport (max={max})");

    // Frozen to the very top: the first output line becomes visible.
    a.output_viewer_scroll_up(usize::MAX);
    assert_eq!(a.output_viewer_view, Some(0), "huge scroll-up clamps at the top anchor");
    let top = render_frame(&a, W, H);
    assert!(
        top.contains("tool 31 output line 0"),
        "PARITY: the pane viewer scrolls all the way to the first line"
    );
    assert!(top.contains("↓"), "frozen state shows the below-tail indicator");

    // Back down past the tail resumes follow.
    a.output_viewer_scroll_down(usize::MAX);
    assert_eq!(a.output_viewer_view, None, "scrolling past the tail resumes follow");
    let bot = render_frame(&a, W, H);
    assert!(bot.contains("tool 31 output line 219"), "tail visible again");
    assert!(bot.contains("end of output"), "follow-tail footer restored");
}

// ── 8. FR-008: mode-specific explorer reachability (PIN, negative) ────

/// The NeuroCode explorer is reachable ONLY when that mode spawned the
/// focused pane (FR-008). Negative half: a plain subagent pane (spawned
/// by delegation, not by NeuroCode) never shows the explorer — even with
/// the engine active and its expanded flag set. Positive contrast: the
/// same flags on the orchestrator view DO render the explorer.
#[test]
fn neurocode_explorer_not_reachable_from_plain_pane() {
    let mut a = focused_pane_with(vec![user_item(0)]);
    a.neurocode_active = true;
    a.neurocode_expanded = true;
    let f = render_frame(&a, W, H);
    assert!(
        !f.contains("neurocode explorer"),
        "FR-008: the explorer never renders for a pane NeuroCode did not spawn"
    );
    assert!(
        f.contains("subagent: parity child"),
        "the focused pane still renders its own view"
    );

    // Contrast (pins that the flag pair itself works on the main view).
    let mut m = app();
    m.neurocode_active = true;
    m.neurocode_expanded = true;
    let mf = render_frame(&m, W, H);
    assert!(
        mf.contains("neurocode explorer"),
        "contrast: the explorer DOES render from the orchestrator view"
    );
}

// ── 9. FR-008: mode-specific explorer reachability (T022, positive) ───

/// Mode attribution is snapshotted at SubagentSpawn from
/// `App::neurocode_active` (RunMode-family mode flag on App) — the exact
/// production path `AgentEvent::SubagentSpawn` takes through `App::apply`.
fn mode_pane_app(goal: &str) -> App {
    let mut a = app();
    a.neurocode_active = true; // the mode is live BEFORE the delegation
    a.apply(spawn(1, goal));
    let idx = a
        .subagent_panes
        .iter()
        .position(|p| p.child_id == 1)
        .expect("spawn created the pane");
    assert!(
        a.subagent_panes[idx].spawned_by_neurocode,
        "spawn under an active NeuroCode mode attributes the pane to it"
    );
    a.subagent_panes[idx].push_item(user_item(0));
    a.focus_subagent(Some(idx));
    a.neurocode_expanded = true; // request the explorer (same flag as main)
    a
}

/// Positive half: a pane the NeuroCode mode spawned DOES render the
/// explorer (same `draw_explorer` chrome as the orchestrator view) —
/// FR-008's "mode-specific explorers only when that mode spawned the
/// pane", the reachable direction.
#[test]
fn neurocode_explorer_reachable_from_mode_spawned_pane() {
    let a = mode_pane_app("mode child");
    let f = render_frame(&a, W, H);
    assert!(
        f.contains("neurocode explorer"),
        "PARITY (T022): a NeuroCode-spawned pane renders the shared explorer"
    );
    assert!(
        f.contains("subagent: mode child"),
        "the pane's transcript strip stays visible above the explorer"
    );
    // Full chrome needs a wider column (the 100-col pane column truncates
    // the shared title) — same draw_explorer title as the orchestrator.
    let wide = render_frame(&a, 160, H);
    assert!(
        wide.contains("click title or Esc to dock"),
        "the shared draw_explorer chrome renders in the pane view"
    );
}

/// Focus toggling between a mode-attributed pane and a plain pane: the
/// same global `neurocode_expanded` request must surface the explorer on
/// the mode pane and NEVER on the plain pane — per-pane mode attribution
/// governs reachability, not the global flag (FR-008).
#[test]
fn neurocode_explorer_follows_focus_between_mode_and_plain_panes() {
    // Production ordering: the plain pane spawns OUTSIDE the mode, the
    // mode pane after the mode activates (mode stays live — the arm
    // mirrors the main branch's `neurocode_active` guard).
    let mut a = app();
    a.apply(spawn(1, "plain child")); // mode off → plain delegation pane
    a.neurocode_active = true;
    a.apply(spawn(2, "mode child")); // mode live → attributed pane
    let plain_idx = a
        .subagent_panes
        .iter()
        .position(|p| p.child_id == 1)
        .expect("plain spawn created its pane");
    let mode_idx = a
        .subagent_panes
        .iter()
        .position(|p| p.child_id == 2)
        .expect("mode spawn created its pane");
    assert!(!a.subagent_panes[plain_idx].spawned_by_neurocode);
    assert!(a.subagent_panes[mode_idx].spawned_by_neurocode);
    a.neurocode_expanded = true; // the explorer request (global)

    // Focused on the mode pane: explorer visible.
    a.focus_subagent(Some(mode_idx));
    let f = render_frame(&a, W, H);
    assert!(
        f.contains("neurocode explorer"),
        "mode pane focused: the explorer renders"
    );

    // Switch to the plain pane (rail-click equivalent). The explorer
    // request stays set but must NOT render — FR-008 negative.
    a.focus_subagent(Some(plain_idx));
    let f = render_frame(&a, W, H);
    assert!(
        !f.contains("neurocode explorer"),
        "plain pane focused: the explorer never renders (FR-008)"
    );
    assert!(
        f.contains("subagent: plain child"),
        "the plain pane renders its own view instead"
    );

    // Back to the mode pane: the explorer returns.
    a.focus_subagent(Some(mode_idx));
    let f = render_frame(&a, W, H);
    assert!(
        f.contains("neurocode explorer"),
        "refocus the mode pane: the explorer renders again"
    );
}

// ── 9b. T037 (FR-008): the explorer's KEY gate follows the spawner ─────

/// Test-side replica of `Tui::handle_key`'s explorer arm: feed `key` to
/// the explorer ONLY when the T037 predicate `neurocode_explorer_owns_keys`
/// says the explorer owns routing; otherwise fall through to the pane
/// arms the real dispatcher reaches next (scroll keys → `pane_scroll_*`,
/// Tab → the agent-picker arm's `agent_picker_open`, Enter → the
/// transcript arm's focus return). Integration tests cannot construct
/// `Tui` (`new_for_test` is `#[cfg(test)]`-gated and `Tui::enter` needs a
/// real TTY), so — the suite's standing convention — this drives the
/// exact predicate + mutators the key arm dispatches to.
fn explorer_key_routed(a: &mut App, key: KeyCode) {
    if joey_tui::app::neurocode_explorer_owns_keys(a) {
        let ev = KeyEvent {
            code: key,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        let _ = joey_tui::neurocode_viz::explorer_key(a, &ev);
        return;
    }
    // Fall-through arms (Focus::Transcript target resolution, T003).
    match key {
        KeyCode::Down | KeyCode::Char('j') => a.pane_scroll_down(1),
        KeyCode::Up | KeyCode::Char('k') => a.pane_scroll_up(1),
        KeyCode::Tab => a.agent_picker_open = true,
        KeyCode::Enter => { /* focus → Input; no transcript-side state */ }
        _ => {}
    }
}

/// T037 case 3, scroll keys: with the mode active + expanded AND a plain
/// (non-neurocode) pane focused, an explorer-claimed key (Down / j) must
/// act on the PANE, not the explorer. Observable effects: the pane's
/// scroll offset moves (drives `pane_scroll_*`, not
/// `neurocode_scroll`), and the explorer state (viz tab / feed scroll)
/// is untouched.
#[test]
fn explorer_scroll_keys_route_to_plain_pane_not_explorer() {
    // Plain pane (spawned OUTSIDE the mode), mode live, explorer requested.
    let mut a = app();
    a.apply(spawn(1, "plain child"));
    a.neurocode_active = true;
    a.neurocode_expanded = true;
    let idx = a
        .subagent_panes
        .iter()
        .position(|p| p.child_id == 1)
        .expect("spawn created the pane");
    assert!(!a.subagent_panes[idx].spawned_by_neurocode);
    a.focus_subagent(Some(idx));

    // A long pane transcript + one render records last_pane_max_scroll.
    for i in 0..40 {
        a.subagent_panes[idx].push_item(user_item(i));
    }
    let _ = render_frame(&a, W, H);
    let max = a.last_pane_max_scroll.get();
    assert!(max > 0, "40 items overflow the pane viewport (max={max})");

    // Down: the pane pins its scroll offset (pane_scroll_down at the
    // follow-tail bottom is a no-op, so pin from the top first via the
    // same fall-through the dispatcher reaches).
    explorer_key_routed(&mut a, KeyCode::Up);
    explorer_key_routed(&mut a, KeyCode::Up);
    assert_eq!(
        a.focused_pane().unwrap().scroll,
        Some(2.min(max)),
        "plain pane focused: Down/Up drive the PANE transcript, not the explorer"
    );
    // The explorer's own scroll state never moved.
    assert_eq!(a.neurocode_scroll, 0, "neurocode_scroll untouched");
    assert_eq!(a.neurocode_viz.tab, VizTab::Graph, "viz tab untouched");

    // j/k behave identically (the same arms, vim aliases).
    explorer_key_routed(&mut a, KeyCode::Char('j'));
    assert_eq!(
        a.focused_pane().unwrap().scroll,
        Some(1.min(max)),
        "j scrolls the pane down one"
    );
    explorer_key_routed(&mut a, KeyCode::Char('k'));
    assert_eq!(a.focused_pane().unwrap().scroll, Some(2.min(max)));
}

/// T037 case 3, Tab/Enter: same state (mode active + expanded, plain pane
/// focused) — Tab must reach the pane-handling path (the global Tab arm
/// opens the agent picker), never the explorer's tab cycler, and Enter
/// must fall through to the transcript arm's focus return, never the
/// explorer's Graph↔Nodes jump.
#[test]
fn explorer_tab_enter_route_to_plain_pane_not_explorer() {
    let mut a = app();
    a.apply(spawn(1, "plain child"));
    a.neurocode_active = true;
    a.neurocode_expanded = true;
    let idx = a
        .subagent_panes
        .iter()
        .position(|p| p.child_id == 1)
        .expect("spawn created the pane");
    a.focus_subagent(Some(idx));

    // Tab: pane path (agent picker opens — the KeyCode::Tab arm at the
    // global match), NOT the explorer's cycle_tab.
    explorer_key_routed(&mut a, KeyCode::Tab);
    assert!(
        a.agent_picker_open,
        "Tab with a plain pane focused opens the agent picker (pane routing)"
    );
    assert_eq!(
        a.neurocode_viz.tab,
        VizTab::Graph,
        "the explorer's tab cycler never ran (FR-008: not fed)"
    );

    // Enter: falls through the explorer (no Graph→Nodes jump — list
    // cursor stays put) into the transcript arm's focus→Input return,
    // which leaves no transcript-side state to observe beyond "the
    // explorer did not consume it".
    let cursor_before = a.neurocode_viz.list_cursor;
    let sel_before = a.neurocode_viz.selected;
    explorer_key_routed(&mut a, KeyCode::Enter);
    assert_eq!(a.neurocode_viz.tab, VizTab::Graph, "Enter did not jump to Nodes");
    assert_eq!(a.neurocode_viz.list_cursor, cursor_before);
    assert_eq!(a.neurocode_viz.selected, sel_before);
}

/// T037 cases 1+2 (pins, unchanged): the explorer still owns keys when
/// NO pane is focused (orchestrator view — byte-identical to the old
/// two-flag gate) and when the focused pane was spawned by the mode.
/// Drives the REAL `explorer_key` through the T037 predicate and asserts
/// its effects land on the explorer state.
#[test]
fn explorer_still_owns_keys_without_pane_and_on_mode_pane() {
    // Case 1: no pane focused → explorer gets keys (old behavior).
    let mut m = app();
    m.apply(AgentEvent::NeuroCodeActive { active: true });
    m.neurocode_expanded = true;
    assert!(m.focused_subagent.is_none());
    assert!(joey_tui::app::neurocode_explorer_owns_keys(&m));
    // Down on the Feed-less explorer scrolls neurocode_scroll... it has
    // no snapshot yet, so the raw-feed fallback claims Down. Instead
    // assert via Tab (always claimed): the viz tab cycles.
    explorer_key_routed(&mut m, KeyCode::Tab);
    assert_eq!(m.neurocode_viz.tab, VizTab::Nodes, "no pane: explorer ate Tab");
    explorer_key_routed(&mut m, KeyCode::Tab);
    assert_eq!(m.neurocode_viz.tab, VizTab::Feed, "no pane: explorer cycles again");
    assert!(!m.agent_picker_open, "the global Tab arm never ran");

    // Case 2: mode-spawned pane focused → explorer still gets keys.
    let mut p = app();
    p.apply(AgentEvent::NeuroCodeActive { active: true });
    p.apply(spawn(1, "mode child"));
    let idx = p
        .subagent_panes
        .iter()
        .position(|c| c.child_id == 1)
        .expect("spawn created the pane");
    assert!(p.subagent_panes[idx].spawned_by_neurocode);
    p.focus_subagent(Some(idx));
    p.neurocode_expanded = true;
    assert!(joey_tui::app::neurocode_explorer_owns_keys(&p));
    explorer_key_routed(&mut p, KeyCode::Tab);
    assert_eq!(p.neurocode_viz.tab, VizTab::Nodes, "mode pane: explorer ate Tab");
    assert!(!p.agent_picker_open, "the global Tab arm never ran");
}

/// Consistency pin (T037): key routing and drawing agree — for a plain
/// pane focused while the flags are set, the explorer is not drawn AND
/// `neurocode_explorer_owns_keys` is false; for a mode pane (or no pane)
/// it is drawn AND owns keys. Guards against the two gates drifting.
#[test]
fn explorer_key_gate_agrees_with_draw_gate() {
    // Plain pane: not drawn (test 8 pins the render side), not fed.
    let mut plain = focused_pane_with(vec![user_item(0)]);
    plain.neurocode_active = true;
    plain.neurocode_expanded = true;
    assert!(!joey_tui::app::neurocode_explorer_owns_keys(&plain));
    let f = render_frame(&plain, W, H);
    assert!(!f.contains("neurocode explorer"), "not drawn for a plain pane");

    // Mode pane: drawn AND owns keys.
    let mode = mode_pane_app("mode child");
    assert!(joey_tui::app::neurocode_explorer_owns_keys(&mode));
    let f = render_frame(&mode, W, H);
    assert!(f.contains("neurocode explorer"), "drawn for a mode pane");

    // No pane (orchestrator): drawn AND owns keys.
    let mut m = app();
    m.neurocode_active = true;
    m.neurocode_expanded = true;
    assert!(m.focused_subagent.is_none());
    assert!(joey_tui::app::neurocode_explorer_owns_keys(&m));
    let f = render_frame(&m, W, H);
    assert!(f.contains("neurocode explorer"), "drawn on the orchestrator view");
}

// ── 10. Non-regression: main viewers still open with panes present ────

/// With `focused_subagent == None` (panes exist and hold tools), Ctrl+O
/// still opens the MAIN output viewer on the main transcript's most
/// recent tool, with the shared chrome, and toggles closed (constitution
/// VII — pane work must not regress the orchestrator screen).
#[test]
fn main_ctrl_o_with_panes_present_targets_main() {
    let mut a = app();
    pane_with_transcript(&mut a, 9, "chrome only", 5); // pane holds tools
    a.transcript.clear(); // drop the spawn notice — exact main indices
    a.push_item(tool_item(77, 40)); // main's most recent (only) tool
    assert!(a.focused_subagent.is_none());

    a.toggle_output_viewer(None);
    assert!(a.output_viewer_open, "main viewer opens (pin)");
    assert_eq!(a.output_viewer_index, Some(0), "targets the MAIN transcript's tool");

    let f = render_frame(&a, W, H);
    assert!(f.contains("output · finished"), "main viewer chrome renders");
    assert!(f.contains("tool 77 output line 39"), "main tool tail content in the viewer");

    a.toggle_output_viewer(None);
    assert!(!a.output_viewer_open, "toggle closes the main viewer");
    let f = render_frame(&a, W, H);
    assert!(!f.contains("output · finished"), "viewer chrome gone after close");
}

/// Same pin for Ctrl+A: with no pane focused, the MAIN stats page opens
/// (its own title + the main context stream), never the pane's.
#[test]
fn main_ctrl_a_with_panes_present_targets_main() {
    let mut a = app();
    pane_with_transcript(&mut a, 9, "chrome only", 5);
    send_pane_context(&mut a, 3); // pane stream must NOT render
    a.transcript.clear();
    send_main_context(&mut a, 3);
    assert!(a.focused_subagent.is_none());

    a.toggle_stats();
    assert!(a.stats_open, "main stats page opens (pin)");

    let f = render_frame(&a, W, H);
    assert!(f.contains("◆ agent stats"), "main stats title renders");
    assert!(f.contains("main-ctx entry"), "main context stream renders");
    assert!(
        !f.contains("pane-ctx entry"),
        "the pane's context stream never renders on the main stats page"
    );

    a.toggle_stats();
    assert!(!a.stats_open, "toggle closes the main stats page");
}
