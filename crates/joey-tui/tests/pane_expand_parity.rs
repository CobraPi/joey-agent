//! T010 / US2 (feature 017): expand/collapse PARITY between the focused
//! subagent pane and the orchestrator screen (FR-003/FR-004/FR-005).
//!
//! TDD: written BEFORE the pane expand routing exists. Expected outcome
//! against current code:
//!   - FAIL: the Ctrl+E / Ctrl+G retarget tests — the App-level mutators
//!     `cycle_focused_reasoning_expand` / `toggle_focused_tool_expand`
//!     operate on `App::transcript` (main) only, so with a pane focused the
//!     pane's entries never cycle (T012 wires the retarget).
//!   - PASS (legitimately): the Space/x cycle and click hit-test tests —
//!     they drive the exact resolution + mutator machinery the handlers
//!     dispatch to (`transcript_hit_test_core` + `SubagentPane::
//!     toggle_item_expand`), which already exists and is target-agnostic;
//!     T011's work is the ROUTING in `Tui::handle_key`, which integration
//!     tests cannot construct (`new_for_test` is `#[cfg(test)]`-gated).
//!   - PASS (legitimately): FileDiff rendering parity — `draw_pane_
//!     transcript` renders through the SAME shared `item_lines` FileDiff
//!     arm the orchestrator uses, so identical items + geometry produce
//!     identical frames (these pin the parity so T013's event mapping
//!     cannot regress it).
//!   - PASS: the negative/unfocused pins (main-only routing with panes
//!     present — T003/T005 behavior that must not regress).

mod common;

use common::*;
use joey_tui::state::{
    expand_state_for_test, App, ReasoningExpandState, TranscriptItem, ToolStatus,
    LIVE_OUTPUT_CAPACITY,
};
use joey_tui::widgets;
use joey_tui::Theme;
use std::time::Duration;

const W: u16 = 100;
const H: u16 = 30;

// ── local helpers (common/mod.rs is frozen for this task) ─────────────

/// Collapsed == the fixture default; alias for readable cycle asserts.
use ReasoningExpandState::{Collapsed, Full, TailWindow};

/// A Reasoning item with `n` lines (>200 to exercise all three states of
/// the cycle distinctly: Collapsed → TailWindow → Full → Collapsed).
fn long_reasoning_item(n: usize) -> TranscriptItem {
    let text = (0..n).map(|j| format!("think line {j:03}")).collect::<Vec<_>>().join("\n");
    TranscriptItem::Reasoning {
        text,
        expand_state: Collapsed,
        thought_duration: Some(Duration::from_secs(2)),
    }
}

/// A completed Tool item with an `n`-line result (>200 for the full cycle).
fn long_tool_item(n: usize) -> TranscriptItem {
    let result = (0..n).map(|j| format!("tool out line {j:03}")).collect::<Vec<_>>().join("\n");
    TranscriptItem::Tool {
        name: "longtool".to_string(),
        emoji: "🔧".to_string(),
        summary: "long tool summary".to_string(),
        status: ToolStatus::Done,
        duration_secs: Some(0.5),
        result_preview: result.clone(),
        expand_state: Collapsed,
        full_args: Some("{}".to_string()),
        full_result: Some(result),
        is_terminal: false,
        exit_code: Some(0),
        live_output: String::new(),
        live_output_capacity: LIVE_OUTPUT_CAPACITY,
    }
}

/// A FileDiff with real unified-diff structure: meta lines, a hunk header
/// with ranges, and context/deletion/insertion lines (exercises the dual
/// old/new gutters, the +/-/@ markers, and hunk-header rendering).
fn rich_diff_item() -> TranscriptItem {
    let lines = vec![
        "--- a/src/lib.rs".to_string(),
        "+++ b/src/lib.rs".to_string(),
        "@@ -10,4 +10,5 @@ fn main".to_string(),
        " context line ten".to_string(),
        "-removed line eleven".to_string(),
        "+added line twelve".to_string(),
        "+added line thirteen".to_string(),
        " kept line fourteen".to_string(),
    ];
    TranscriptItem::FileDiff {
        path: "src/lib.rs".to_string(),
        stat: "+2 -1".to_string(),
        lines,
        is_binary: false,
        expand_state: Collapsed,
    }
}

/// A FileDiff with a hunk header + 220 content lines: collapsed caps at 50
/// rendered lines, TailWindow at 200, Full shows all 221 — every state
/// renders a distinct affordance/window (line-cap parity, FR-005).
fn big_diff_item() -> TranscriptItem {
    let mut lines = vec!["@@ -1,110 +1,111 @@".to_string()];
    for j in 0..220 {
        lines.push(if j % 2 == 0 {
            format!("+ cap line {j:03}")
        } else {
            format!("- cap line {j:03}")
        });
    }
    TranscriptItem::FileDiff {
        path: "src/big.rs".to_string(),
        stat: "+110 -110".to_string(),
        lines,
        is_binary: false,
        expand_state: Collapsed,
    }
}

/// Set a FileDiff item's expand state directly (fixture push + state pin,
/// independent of pane_apply which drops FileChange events until T013).
fn set_diff_state(item: &mut TranscriptItem, s: ReasoningExpandState) {
    if let TranscriptItem::FileDiff { expand_state, .. } = item {
        *expand_state = s;
    }
}

/// An App with ONE pane (child 1, "expand child") holding exactly `items`,
/// focused — built manually (not the 5-kind cycle) for deterministic
/// expand targets.
fn focused_pane_with(items: Vec<TranscriptItem>) -> App {
    let mut a = app();
    a.apply(spawn(1, "expand child"));
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

/// Pane-side `item_is_expandable` (same kinds as App's main-transcript
/// version: tool / file-diff / reasoning).
fn pane_item_is_expandable(item: &TranscriptItem) -> bool {
    matches!(
        item,
        TranscriptItem::Tool { .. } | TranscriptItem::FileDiff { .. } | TranscriptItem::Reasoning { .. }
    )
}

/// Exactly what T011's Space/x Pane arm will do (mirroring the Main arm's
/// strategy at app.rs:1280-1312, but reading the pane): hit-test the
/// viewport-CENTER row via the shared `transcript_hit_test_core`, fall
/// back to the first expandable item at/below the top visible row, then
/// `SubagentPane::toggle_item_expand`. Re-renders first so geometry is
/// fresh — one press per frame, like a real user.
fn pane_space_x_press(a: &mut App) {
    let _ = render_frame(a, W, H); // records last_pane_text_area + max scroll
    let (tx, ty, tw, th) = a.last_pane_text_area.get();
    assert!(th > 0 && tw > 0, "pane text area recorded (render first)");
    let center_row = ty + th / 2;
    let resolved = {
        let pane = a.focused_pane().expect("pane focused");
        let area = (tx, ty, tw, th);
        let max = a.last_pane_max_scroll.get();
        let idx = widgets::transcript_hit_test_core(
            &pane.transcript,
            &pane.streaming_assistant,
            pane.scroll,
            max,
            area,
            Theme::aurora(),
            center_row,
        );
        match idx {
            Some(i) if pane_item_is_expandable(&pane.transcript[i]) => Some(i),
            _ => {
                // Top-visible fallback: first expandable at/below the top row.
                let top = widgets::transcript_hit_test_core(
                    &pane.transcript,
                    &pane.streaming_assistant,
                    pane.scroll,
                    max,
                    area,
                    Theme::aurora(),
                    ty,
                );
                top.and_then(|t0| {
                    (t0..pane.transcript.len()).find(|&i| pane_item_is_expandable(&pane.transcript[i]))
                })
            }
        }
    };
    if let Some(i) = resolved {
        if let Some(p) = a.focused_pane_mut() {
            p.toggle_item_expand(i);
        }
    }
}

/// Pin the pane to the top of its content (what 'g'/Home do), iterating
/// until the render-time bound is stable — static content converges fast.
fn pin_pane_top(a: &mut App) {
    for _ in 0..10 {
        let _ = render_frame(a, W, H);
        let m = a.last_pane_max_scroll.get();
        if let Some(p) = a.focused_pane_mut() {
            p.scroll = Some(m);
        }
        let _ = render_frame(a, W, H);
        if a.last_pane_max_scroll.get() == m {
            break;
        }
    }
}

/// Same for the main transcript (App::scroll_to_top pins to last_max_scroll).
fn pin_main_top(a: &mut App) {
    for _ in 0..10 {
        let _ = render_frame(a, W, H);
        a.scroll_to_top();
        let _ = render_frame(a, W, H);
        if a.scroll == Some(a.last_max_scroll.get()) {
            break;
        }
    }
}

/// Orchestrator-screen counterpart: main transcript with the SAME items,
/// one spawned pane so the rail chrome matches, main view focused. The
/// spawn notice the pane's builder leaves in the MAIN transcript is
/// cleared first so both sides hold item-for-item identical transcripts
/// (byte-parity frames require identical content, not just identical
/// expandables).
fn main_app_with(items: Vec<TranscriptItem>) -> App {
    let mut a = app();
    pane_with_transcript(&mut a, 9, "chrome only", 1);
    a.transcript.clear();
    for it in items {
        a.push_item(it);
    }
    assert!(a.focused_subagent.is_none());
    a
}

/// Expand state of the pane transcript item at `idx` (focused pane).
fn pane_state(a: &App, idx: usize) -> ReasoningExpandState {
    expand_state_for_test(&a.focused_pane().unwrap().transcript[idx])
}

/// Assert every MAIN expandable is still Collapsed.
fn main_all_collapsed(a: &App, ctx: &str) {
    for (i, it) in a.transcript.iter().enumerate() {
        assert_eq!(
            expand_state_for_test(it),
            Collapsed,
            "{ctx}: main item {i} untouched while a pane is focused"
        );
    }
}

// ── 1. Space/x three-state cycle on pane entries (FR-003) ─────────────

/// A pane holding one 220-line Tool item: Space/x resolves the
/// viewport-center entry through the shared hit-test and cycles it
/// Collapsed → TailWindow → Full → Collapsed. The main transcript's own
/// expandables never move (focused-view isolation).
#[test]
fn space_x_cycles_pane_tool_three_state() {
    let mut a = focused_pane_with(vec![long_tool_item(220)]);
    a.push_item(reasoning_item(90)); // main markers — must stay Collapsed
    a.push_item(tool_item(91, 6));
    assert_eq!(pane_state(&a, 0), Collapsed, "starts Collapsed");

    pane_space_x_press(&mut a);
    assert_eq!(pane_state(&a, 0), TailWindow, "press 1: Collapsed → TailWindow");
    pane_space_x_press(&mut a);
    assert_eq!(pane_state(&a, 0), Full, "press 2: TailWindow → Full (220 > 200)");
    pane_space_x_press(&mut a);
    assert_eq!(pane_state(&a, 0), Collapsed, "press 3: Full → Collapsed");

    main_all_collapsed(&a, "space/x on pane tool");
}

/// Same cycle for a 220-line Reasoning entry in the pane.
#[test]
fn space_x_cycles_pane_reasoning_three_state() {
    let mut a = focused_pane_with(vec![long_reasoning_item(220)]);
    a.push_item(reasoning_item(90));
    a.push_item(tool_item(91, 6));

    pane_space_x_press(&mut a);
    assert_eq!(pane_state(&a, 0), TailWindow, "press 1: Collapsed → TailWindow");
    pane_space_x_press(&mut a);
    assert_eq!(pane_state(&a, 0), Full, "press 2: TailWindow → Full");
    pane_space_x_press(&mut a);
    assert_eq!(pane_state(&a, 0), Collapsed, "press 3: Full → Collapsed");

    main_all_collapsed(&a, "space/x on pane reasoning");
}

// ── 2. Click hit-test expansion in panes (FR-003) ──────────────────────

/// Clicking the row that renders a pane FileDiff's header resolves through
/// the SAME `transcript_hit_test_core` the mouse handler uses
/// (app.rs handle_mouse_click's pane branch) to that item index, and
/// toggling it cycles the diff — neighboring expandables stay Collapsed
/// (per-item isolation).
#[test]
fn click_hit_test_expands_pane_diff_item() {
    let mut a = focused_pane_app(5); // 0 assistant, 1 tool, 2 reasoning, 3 diff, 4 user
    a.push_item(tool_item(80, 6)); // main marker — must stay Collapsed
    let text = render_transcript_text(&a, W, H); // renders + records geometry
    assert!(text.contains("◆ src/file3.rs"), "pane renders the diff header");

    // The screen row displaying the diff header (rows in `text` start at ty).
    let (tx, ty, tw, th) = a.last_pane_text_area.get();
    let row_off = text
        .lines()
        .position(|l| l.contains("◆ src/file3.rs"))
        .expect("diff header row found in the pane transcript area");
    let row = ty + row_off as u16;
    assert!(row >= ty && row < ty + th, "row inside the pane text area");

    // Exactly what handle_mouse_click's pane branch does (app.rs:2012-2032).
    let pane = a.focused_pane().unwrap();
    let hit = widgets::transcript_hit_test_core(
        &pane.transcript,
        &pane.streaming_assistant,
        pane.scroll,
        a.last_pane_max_scroll.get(),
        (tx, ty, tw, th),
        Theme::aurora(),
        row,
    );
    assert_eq!(hit, Some(3), "click row resolves to the pane's FileDiff item");
    if let Some(i) = hit {
        if let Some(p) = a.focused_pane_mut() {
            p.toggle_item_expand(i);
        }
    }
    assert_ne!(pane_state(&a, 3), Collapsed, "the clicked diff expanded");
    // Per-item isolation: neighbors and main untouched.
    assert_eq!(pane_state(&a, 1), Collapsed, "pane tool untouched");
    assert_eq!(pane_state(&a, 2), Collapsed, "pane reasoning untouched");
    main_all_collapsed(&a, "click on pane diff");
}

// ── 3. Ctrl+E / Ctrl+G retarget to the focused pane (FR-004) ──────────
// These FAIL until T012: the mutators currently operate on the MAIN
// transcript regardless of focus, so the pane entry never cycles.

/// With a pane focused, Ctrl+E (the `cycle_focused_reasoning_expand` call
/// the key arm dispatches to) must cycle the PANE's most-recent reasoning
/// entry — and leave the main transcript's expandables Collapsed.
#[test]
fn ctrl_e_cycles_focused_pane_reasoning_not_main() {
    let mut a = focused_pane_app(5); // pane reasoning @2, tool @1
    a.push_item(reasoning_item(90)); // main reasoning marker
    a.push_item(tool_item(91, 6)); // main tool marker

    // What the Ctrl+E arm dispatches to (app.rs KeyCode::Char('e') if ctrl).
    a.cycle_focused_reasoning_expand();

    assert_ne!(
        pane_state(&a, 2),
        Collapsed,
        "PARITY (fails until T012): Ctrl+E cycles the focused pane's reasoning entry"
    );
    assert_eq!(pane_state(&a, 1), Collapsed, "pane tool untouched by Ctrl+E");
    main_all_collapsed(&a, "ctrl+e while pane focused");
}

/// With a pane focused, Ctrl+G (`toggle_focused_tool_expand`) must toggle
/// the PANE's most-recent tool entry — main stays Collapsed.
#[test]
fn ctrl_g_toggles_focused_pane_tool_not_main() {
    let mut a = focused_pane_app(5); // pane tool @1, reasoning @2
    a.push_item(reasoning_item(90));
    a.push_item(tool_item(91, 6));

    // What the Ctrl+G arm dispatches to (app.rs KeyCode::Char('g') if ctrl).
    a.toggle_focused_tool_expand();

    assert_ne!(
        pane_state(&a, 1),
        Collapsed,
        "PARITY (fails until T012): Ctrl+G toggles the focused pane's tool entry"
    );
    assert_eq!(pane_state(&a, 2), Collapsed, "pane reasoning untouched by Ctrl+G");
    main_all_collapsed(&a, "ctrl+g while pane focused");
}

// ── 4. FileDiff rendering parity (FR-005) ─────────────────────────────

/// A pane rendering a structured diff + a binary diff draws byte-identical
/// transcript-area text to the orchestrator screen rendering the SAME
/// items at the SAME geometry — gutters, +/-/@ markers, the
/// `@@ -10,4 +10,5 @@` hunk header, and the `binary file changed`
/// placeholder included.
#[test]
fn pane_filediff_rendering_identical_to_orchestrator() {
    let items = || {
        vec![
            assistant_item(0),
            rich_diff_item(),
            binary_diff_item(7),
            user_item(9),
        ]
    };
    let pane_app = focused_pane_with(items());
    let main_app = main_app_with(items());

    let pt = render_transcript_text(&pane_app, W, H);
    let mt = render_transcript_text(&main_app, W, H);

    // Marker pins on the pane side (equality below covers the orchestrator).
    assert!(pt.contains("src/lib.rs"), "diff path header");
    assert!(pt.contains("@@ -10,4 +10,5 @@"), "hunk header rendered verbatim");
    assert!(pt.contains("context line ten"), "context row (dual gutter)");
    assert!(pt.contains("removed line eleven"), "- row content");
    assert!(pt.contains("added line twelve"), "+ row content");
    assert!(pt.contains("kept line fourteen"), "kept row content");
    assert!(pt.contains("bin/blob7.png"), "binary diff path header");
    assert!(pt.contains("binary file changed"), "binary placeholder");

    // THE parity pin: identical items + geometry → identical frames.
    assert_eq!(
        pt, mt,
        "PARITY: pane FileDiff rendering is byte-identical to the orchestrator's"
    );
}

/// Line-cap parity across the three expand states: a 221-line diff renders
/// its last 50 lines collapsed ("… (171 earlier lines hidden)"), its last
/// 200 in the tail window ("… (21 earlier lines hidden)"), and everything
/// when Full — and each state's pane frame equals the orchestrator's.
#[test]
fn pane_filediff_line_caps_parity_across_expand_states() {
    let items = || vec![assistant_item(0), big_diff_item(), user_item(9)];

    for state in [Collapsed, TailWindow, Full] {
        let mut pane_app = focused_pane_with(items());
        let mut main_app = main_app_with(items());
        // Same starting state on both sides (diff is the middle item).
        for a in [&mut pane_app, &mut main_app] {
            if a.focused_subagent.is_some() {
                set_diff_state(&mut a.focused_pane_mut().unwrap().transcript[1], state);
            } else {
                set_diff_state(&mut a.transcript[1], state);
            }
        }
        // Pin both views to the top so the affordance/window rows are on
        // screen (tail-anchored views of a tall block look identical in
        // every state — the cap only shows at the top of the block).
        pin_pane_top(&mut pane_app);
        pin_main_top(&mut main_app);

        let pt = render_transcript_text(&pane_app, W, H);
        let mt = render_transcript_text(&main_app, W, H);

        match state {
            Collapsed => {
                assert!(pt.contains("171 earlier lines hidden"), "collapsed caps at 50 of 221");
                assert!(pt.contains("cap line 170"), "window starts at parsed[171]");
                assert!(!pt.contains("cap line 020"), "early lines hidden while collapsed");
            }
            TailWindow => {
                assert!(pt.contains("21 earlier lines hidden"), "tail window caps at 200 of 221");
                assert!(pt.contains("cap line 020"), "window starts at parsed[21]");
                assert!(!pt.contains("cap line 000"), "head still hidden in tail window");
            }
            Full => {
                assert!(!pt.contains("earlier lines hidden"), "no cap when Full");
                assert!(pt.contains("cap line 000"), "very first line visible when Full");
            }
        }
        assert_eq!(
            pt, mt,
            "PARITY at {state:?}: pane diff frame equals the orchestrator's"
        );
    }
}

// ── 5. Negative/unfocused pins (main-only routing, no pane leakage) ────

/// With `focused_subagent == None` (panes exist and carry expandables),
/// Space/x, Ctrl+E and Ctrl+G act on the MAIN transcript only; every pane
/// item stays Collapsed (lean pane-expand-focused complement to T005's
/// main-screen pins).
#[test]
fn unfocused_space_and_ctrl_expand_keys_touch_main_only() {
    fn neg_app() -> App {
        let mut a = app();
        pane_with_transcript(&mut a, 1, "neg one", 5);
        pane_with_transcript(&mut a, 2, "neg two", 5);
        // Drop the spawn notices the pane builders leave in the MAIN
        // transcript so the expandable indices below are exact.
        a.transcript.clear();
        // main: 0 assistant, 1 reasoning (3 lines), 2 tool (6 lines), 3 user
        a.push_item(assistant_item(0));
        a.push_item(reasoning_item(1));
        a.push_item(tool_item(2, 6));
        a.push_item(user_item(3));
        assert!(a.focused_subagent.is_none());
        a
    }
    fn panes_all_collapsed(a: &App, ctx: &str) {
        for (pi, pane) in a.subagent_panes.iter().enumerate() {
            assert_eq!(pane.scroll, None, "{ctx}: pane {pi} scroll untouched");
            for (j, it) in pane.transcript.iter().enumerate() {
                assert_eq!(
                    expand_state_for_test(it),
                    Collapsed,
                    "{ctx}: pane {pi} item {j} stays Collapsed"
                );
            }
        }
    }

    // Space/x (Main-arm mirror: center-row hit-test + top-item fallback).
    let mut a = neg_app();
    let _ = render_frame(&a, W, H);
    let (_tx, ty, _tw, th) = a.last_text_area.get();
    assert!(th > 0, "main text area recorded");
    let center_row = ty + th / 2;
    let idx = widgets::transcript_hit_test(&a, Theme::aurora(), center_row, 4);
    let resolved = match idx {
        Some(i) if a.item_is_expandable(i) => Some(i),
        _ => {
            let top = widgets::transcript_item_at_top(&a, Theme::aurora());
            top.and_then(|t0| (t0..a.transcript.len()).find(|&i| a.item_is_expandable(i)))
        }
    };
    let i = resolved.expect("Space resolves a MAIN expandable");
    a.toggle_item_expand_by_index(i);
    assert_ne!(expand_state_for_test(&a.transcript[i]), Collapsed, "main item left Collapsed");
    panes_all_collapsed(&a, "space/x");

    // Ctrl+E: main reasoning (3 lines) Collapsed → Full; panes untouched.
    let mut a = neg_app();
    a.cycle_focused_reasoning_expand();
    assert_eq!(expand_state_for_test(&a.transcript[1]), Full, "main reasoning cycled");
    assert_eq!(expand_state_for_test(&a.transcript[2]), Collapsed, "Ctrl+E touches only reasoning");
    panes_all_collapsed(&a, "ctrl+e");

    // Ctrl+G: main tool (6 lines) Collapsed → Full; panes untouched.
    let mut a = neg_app();
    a.toggle_focused_tool_expand();
    assert_eq!(expand_state_for_test(&a.transcript[2]), Full, "main tool cycled");
    assert_eq!(expand_state_for_test(&a.transcript[1]), Collapsed, "Ctrl+G touches only the tool");
    panes_all_collapsed(&a, "ctrl+g");
}
