//! T006 / US1 (feature 017): scroll-affordance PARITY between the focused
//! subagent pane and the orchestrator screen (FR-001/FR-002).
//!
//! TDD: written BEFORE the pane affordances exist (T008 renders scrollbar /
//! below-badge / header scroll-info in the pane). Expected outcome against
//! current code:
//!   - state/scroll-KEY tests PASS (T003's TranscriptTarget routing already
//!     routes g/G/Home/End/j/k/PgUp/PgDn to the focused pane), and
//!   - every RENDERING-parity test FAILs (the pane draws no scrollbar, no
//!     "↓ N lines below" badge, and no "N messages · P% from top"/"· live"
//!     scroll-info in its header).
//!
//! Each pane assertion is paired with the orchestrator-screen counterpart
//! (same fixture shape, same 80x24 geometry) so "parity" — not mere
//! presence — is what's pinned.

mod common;

use common::*;
use joey_tui::state::App;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

const W: u16 = 80;
const H: u16 = 24;

// ── local helpers (common/mod.rs is frozen for this task) ─────────────

/// Push the same Assistant/Tool/Reasoning/FileDiff/User cycle that
/// `pane_with_transcript` uses into the MAIN transcript, so orchestrator
/// counterparts hold item-for-item the same content as pane fixtures.
fn push_main_cycle(a: &mut App, n: usize) {
    for i in 0..n {
        let item = match i % 5 {
            0 => assistant_item(i),
            1 => tool_item(i, 6),
            2 => reasoning_item(i),
            3 => file_diff_item(i, 4),
            _ => user_item(i),
        };
        a.push_item(item);
    }
}

/// Orchestrator-screen counterpart: main transcript with `n` cycle items,
/// one spawned pane so the rail chrome matches the pane fixtures, main
/// view focused.
fn main_app_with_rail(n: usize) -> App {
    let mut a = app();
    pane_with_transcript(&mut a, 9, "chrome only", 1);
    push_main_cycle(&mut a, n);
    assert!(a.focused_subagent.is_none());
    a
}

/// Render one frame and return (full-frame text, scrollbar-column text).
/// The scrollbar column is the reserved column immediately right of the
/// recorded transcript text area (`last_pane_text_area` when a pane is
/// focused, `last_text_area` otherwise) — exactly where `draw_scrollbar`
/// paints `│`/`█` on the orchestrator screen.
fn render_with_scrollbar_column(a: &App, width: u16, height: u16) -> (String, String) {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let area = f.area();
            joey_tui::app::render_body_for_test(
                f,
                area,
                a,
                joey_tui::theme::Theme::aurora(),
                false,
                0.5,
            );
        })
        .unwrap();
    let (x, y, w, h) = if a.focused_subagent.is_some() {
        a.last_pane_text_area.get()
    } else {
        a.last_text_area.get()
    };
    assert!(w > 0 && h > 0, "text area rect recorded (frame drew a transcript)");
    let buf = terminal.backend().buffer();
    let mut frame = String::new();
    for c in buf.content.iter() {
        frame.push_str(c.symbol());
    }
    let col = x + w; // reserved scrollbar column
    let mut scrollbar = String::new();
    for row in y..y.saturating_add(h) {
        scrollbar.push_str(buf[(col, row)].symbol());
    }
    (frame, scrollbar)
}

/// Exactly what `Tui::handle_key`'s g/Home Pane arm does (app.rs, T003):
/// pin the pane to the render-time max bound.
fn pane_go_top(a: &mut App) {
    let max = a.last_pane_max_scroll.get();
    if let Some(p) = a.focused_pane_mut() {
        p.scroll = Some(max);
    }
}

/// Exactly what `Tui::handle_key`'s G/End Pane arm does: resume auto-follow.
fn pane_go_bottom(a: &mut App) {
    if let Some(p) = a.focused_pane_mut() {
        p.scroll = None;
    }
}

// ── T036 helpers: header scroll-segment PLACEMENT ─────────────────────

/// Render one frame and return the buffer as one String per terminal
/// row (row-major), for row-scoped placement assertions.
fn render_rows(a: &App, width: u16, height: u16) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let area = f.area();
            joey_tui::app::render_body_for_test(
                f,
                area,
                a,
                joey_tui::theme::Theme::aurora(),
                false,
                0.5,
            );
        })
        .unwrap();
    let buf = terminal.backend().buffer();
    let mut rows = Vec::with_capacity(height as usize);
    for row in 0..height {
        let mut s = String::new();
        for col in 0..width {
            s.push_str(buf[(col, row)].symbol());
        }
        rows.push(s);
    }
    rows
}

/// (top_title_row, bottom_title_row) of the focused pane's transcript
/// block, derived from the recorded text area: the top title is the
/// border row above the inner area, the bottom title the border row
/// below it. Panes are always focused when this is called.
fn pane_title_rows(a: &App) -> (usize, usize) {
    let (_x, y, _w, h) = a.last_pane_text_area.get();
    (y.saturating_sub(1) as usize, (y + h) as usize)
}

// ── 1. overflow precondition ──────────────────────────────────────────

/// With 40 synthetic items on an 80x24 terminal the focused pane MUST
/// overflow its viewport (recorded max scroll > 0) and, at follow-tail,
/// show the NEWEST item — the overflow is real, not assumed.
#[test]
fn focused_pane_overflows_small_terminal_shows_tail() {
    let a = focused_pane_app(40);
    let frame = render_frame(&a, W, H);
    assert!(frame.contains("subagent: parity child"), "pane title shown");
    assert!(
        a.last_pane_max_scroll.get() > 0,
        "40 items overflow an 80x24 pane viewport (max={})",
        a.last_pane_max_scroll.get()
    );
    let text = render_transcript_text(&a, W, H);
    assert!(text.contains("user message 39"), "tail item visible at follow");
    assert!(
        !text.contains("assistant message 0"),
        "head item scrolled off — overflow confirmed via render output"
    );
}

// ── 2. scrollbar glyph parity ─────────────────────────────────────────

/// Orchestrator screen: the reserved scrollbar column paints `│` track +
/// `█` thumb (draw_scrollbar) when content overflows, thumb at the BOTTOM
/// while auto-following. The focused pane must paint the same glyphs in
/// its own reserved column for equivalent content/geometry.
#[test]
fn scrollbar_glyphs_match_orchestrator_when_overflowing() {
    // Orchestrator counterpart (passes today — pins the format).
    let m = main_app_with_rail(40);
    let (_mf, mcol) = render_with_scrollbar_column(&m, W, H);
    assert!(m.last_max_scroll.get() > 0, "main counterpart overflows");
    assert!(!mcol.is_empty(), "main scrollbar column drawn");
    assert!(
        mcol.chars().all(|c| c == '│' || c == '█'),
        "main scrollbar column is pure track/thumb glyphs: {mcol:?}"
    );
    assert!(mcol.contains('│'), "main track glyph present");
    assert!(mcol.contains('█'), "main thumb glyph present");
    assert!(mcol.ends_with('█'), "main thumb at bottom while live (auto-follow)");

    // Pane parity (FAILS until T008: pane reserves the column but never
    // paints it).
    let a = focused_pane_app(40);
    let (_pf, pcol) = render_with_scrollbar_column(&a, W, H);
    assert!(
        a.last_pane_max_scroll.get() > 0,
        "pane counterpart overflows too"
    );
    assert!(
        pcol.chars().all(|c| c == '│' || c == '█'),
        "pane scrollbar column is pure track/thumb glyphs: {pcol:?}"
    );
    assert!(pcol.contains('│'), "PARITY: pane track glyph present");
    assert!(pcol.contains('█'), "PARITY: pane thumb glyph present");
    assert!(pcol.ends_with('█'), "PARITY: pane thumb at bottom while live");
}

/// Without overflow neither view draws scrollbar glyphs (draw_scrollbar
/// early-returns; the pane must match once it grows one).
#[test]
fn scrollbar_absent_without_overflow_in_both_views() {
    let m = main_app_with_rail(3);
    let (_mf, mcol) = render_with_scrollbar_column(&m, W, H);
    assert_eq!(m.last_max_scroll.get(), 0, "main counterpart fits");
    assert!(!mcol.contains('█') && !mcol.contains('│'), "no main scrollbar");

    let a = focused_pane_app(3);
    let (_pf, pcol) = render_with_scrollbar_column(&a, W, H);
    assert_eq!(a.last_pane_max_scroll.get(), 0, "pane fits");
    assert!(
        !pcol.contains('█') && !pcol.contains('│'),
        "PARITY: no pane scrollbar without overflow"
    );
}

// ── 3. "↓ N lines below" badge parity ────────────────────────────────

/// Orchestrator: scrolled up 3 lines the transcript shows the exact badge
/// " ↓ 3 lines below " (plural). The focused pane must show the same badge
/// with the same N for the same scroll state.
#[test]
fn below_badge_when_scrolled_up_matches_orchestrator() {
    // Orchestrator counterpart (passes today — pins format + N).
    let mut m = main_app_with_rail(40);
    let _ = render_frame(&m, W, H); // records last_max_scroll
    m.scroll_up(3);
    assert_eq!(m.scroll, Some(3));
    let mt = render_transcript_text(&m, W, H);
    assert!(mt.contains("↓ 3 lines below"), "orchestrator badge shown: {mt:?}");

    // Pane parity (FAILS until T008: no badge is drawn).
    let mut a = focused_pane_app(40);
    let _ = render_frame(&a, W, H); // records last_pane_max_scroll
    a.pane_scroll_up(3);
    assert_eq!(a.focused_pane().unwrap().scroll, Some(3), "pane scrolled up 3");
    let pt = render_transcript_text(&a, W, H);
    assert!(
        pt.contains("↓ 3 lines below"),
        "PARITY: pane shows the same '↓ N lines below' badge"
    );
}

/// At follow-tail (scroll None) neither view shows a below badge.
#[test]
fn below_badge_absent_at_follow_tail_in_both_views() {
    let m = main_app_with_rail(40);
    let mt = render_transcript_text(&m, W, H);
    assert!(!mt.contains("below"), "no orchestrator badge while live");

    let a = focused_pane_app(40);
    let pt = render_transcript_text(&a, W, H);
    assert!(!pt.contains("below"), "PARITY: no pane badge while live");
}

// ── 4. header scroll-info parity ──────────────────────────────────────

/// Orchestrator header at follow-tail reads " {N} messages · live " with
/// N = transcript item count. The pane header must carry the same segment.
#[test]
fn pane_header_live_segment_matches_orchestrator() {
    // Orchestrator counterpart (passes today — pins the exact format).
    let mut m = app(); // no pane chrome: msg_count is exactly 40
    push_main_cycle(&mut m, 40);
    let mf = render_frame(&m, W, H);
    assert!(
        mf.contains("40 messages · live"),
        "orchestrator live header, N = transcript.len()"
    );

    // Pane parity (FAILS until T008: pane title has status '· live' but no
    // 'N messages' scroll-info segment).
    let a = focused_pane_app(40); // pane transcript holds exactly 40 items
    let pf = render_frame(&a, W, H);
    assert!(
        pf.contains("40 messages · live"),
        "PARITY: pane header shows '40 messages · live' at follow-tail"
    );
}

/// Orchestrator header when scrolled reads " {N} messages · {P}% from top ";
/// pinned to the top (offset == recorded max) P is exactly 0. The pane must
/// match after the same 'g' press.
#[test]
fn pane_header_pct_segment_matches_orchestrator() {
    // Orchestrator counterpart (passes today — pins format + 0% at top).
    let mut m = app();
    push_main_cycle(&mut m, 40);
    let _ = render_frame(&m, W, H); // records last_max_scroll
    m.scroll_to_top();
    let mf = render_frame(&m, W, H);
    assert!(
        mf.contains("40 messages · 0% from top"),
        "orchestrator scrolled header at top"
    );

    // Pane parity (FAILS until T008). Same two-render sequence so the
    // header is evaluated against the same render-time bound the 'g' arm
    // used (mirrors orchestrator semantics exactly).
    let mut a = focused_pane_app(40);
    let _ = render_frame(&a, W, H); // records last_pane_max_scroll
    pane_go_top(&mut a); // what handle_key's g/Home Pane arm does
    let pf = render_frame(&a, W, H);
    assert!(
        pf.contains("40 messages · 0% from top"),
        "PARITY: pane header shows '40 messages · 0% from top' when pinned to top"
    );
}

// ── 5. follow-tail semantics ──────────────────────────────────────────

/// Pure state: pane.scroll None (pinned) stays None across appends; a
/// scrolled-up offset does NOT jump when items are appended.
#[test]
fn follow_tail_state_pinned_follows_scrolled_stays_stable() {
    let mut a = focused_pane_app(40);
    let _ = render_frame(&a, W, H);

    // Pinned: appends keep auto-follow.
    assert_eq!(a.focused_pane().unwrap().scroll, None);
    a.focused_pane_mut().unwrap().push_item(user_item(200));
    a.focused_pane_mut().unwrap().push_item(user_item(201));
    assert_eq!(
        a.focused_pane().unwrap().scroll,
        None,
        "pinned pane keeps following the tail across appends"
    );

    // Scrolled up: appends must not move the offset.
    a.pane_scroll_up(5);
    assert_eq!(a.focused_pane().unwrap().scroll, Some(5));
    a.focused_pane_mut().unwrap().push_item(user_item(202));
    a.focused_pane_mut().unwrap().push_item(user_item(203));
    a.focused_pane_mut().unwrap().push_item(user_item(204));
    assert_eq!(
        a.focused_pane().unwrap().scroll,
        Some(5),
        "scrolled-up pane does not jump when new items arrive"
    );
}

/// Rendering side of follow-tail while PINNED: after appends the pane
/// header still reads "N messages · live" (N grew) and no badge appears.
#[test]
fn pane_pinned_append_keeps_live_header_and_no_badge() {
    let mut a = focused_pane_app(40);
    a.focused_pane_mut().unwrap().push_item(user_item(210));
    a.focused_pane_mut().unwrap().push_item(user_item(211));
    a.focused_pane_mut().unwrap().push_item(user_item(212));
    assert_eq!(a.focused_pane().unwrap().transcript.len(), 43);
    assert_eq!(a.focused_pane().unwrap().scroll, None);
    let pf = render_frame(&a, W, H);
    assert!(
        pf.contains("43 messages · live"),
        "PARITY: pinned pane header stays live (N grew to 43) after appends"
    );
    assert!(
        !render_transcript_text(&a, W, H).contains("below"),
        "still no badge while pinned"
    );
}

/// Rendering side of follow-tail while SCROLLED UP: after appends the
/// offset is unchanged and the badge still shows the SAME distance-to-live.
#[test]
fn pane_scrolled_append_keeps_stable_offset_and_badge() {
    let mut a = focused_pane_app(40);
    let _ = render_frame(&a, W, H); // records pane max scroll
    a.pane_scroll_up(5);
    a.focused_pane_mut().unwrap().push_item(user_item(220));
    a.focused_pane_mut().unwrap().push_item(user_item(221));
    assert_eq!(
        a.focused_pane().unwrap().scroll,
        Some(5),
        "offset stable across appends"
    );
    let pt = render_transcript_text(&a, W, H);
    assert!(
        pt.contains("↓ 5 lines below"),
        "PARITY: badge persists with stable N=5 after appends (view did not jump)"
    );
    assert!(
        !pt.contains("messages · live"),
        "scrolled-up pane is not live-pinned"
    );
}

// ── 6. scroll keys on the focused pane (T003 routing — expected PASS) ──

/// g/Home → top (render-time bound), G/End → bottom/live, j/k/PgUp/PgDn
/// move and clamp against last_max_scroll — the exact mutators
/// `Tui::handle_key` dispatches to in its Pane arms (Tui itself needs a
/// real TTY; same convention as tests/subagent_panes.rs). Main scroll is
/// untouched throughout (reverse of the T005 pin).
#[test]
fn pane_scroll_keys_top_bottom_move_and_clamp() {
    let mut a = focused_pane_app(40);
    let _ = render_frame(&a, W, H);
    let max1 = a.last_pane_max_scroll.get();
    assert!(max1 > 0, "precondition: pane overflows");

    // g / Home → top.
    pane_go_top(&mut a);
    assert_eq!(a.focused_pane().unwrap().scroll, Some(max1));
    // G / End → bottom / auto-follow.
    pane_go_bottom(&mut a);
    assert_eq!(a.focused_pane().unwrap().scroll, None);
    // k / Up → one line up; j / Down → back to follow.
    a.pane_scroll_up(1);
    assert_eq!(a.focused_pane().unwrap().scroll, Some(1));
    a.pane_scroll_down(1);
    assert_eq!(a.focused_pane().unwrap().scroll, None);
    // PgUp → 10 up; PgDn → 4 down leaves 6; past the bottom clamps to live.
    a.pane_scroll_up(10);
    assert_eq!(a.focused_pane().unwrap().scroll, Some(10));
    a.pane_scroll_down(4);
    assert_eq!(a.focused_pane().unwrap().scroll, Some(6));
    a.pane_scroll_down(100);
    assert_eq!(a.focused_pane().unwrap().scroll, None, "clamps to follow at bottom");
    // Huge scroll-up clamps against the render-recorded bound.
    a.pane_scroll_up(usize::MAX);
    assert_eq!(
        a.focused_pane().unwrap().scroll,
        Some(a.last_pane_max_scroll.get()),
        "scroll-up clamps to last_max_scroll"
    );

    // Main transcript scroll is never touched by pane keys.
    assert_eq!(a.scroll, None, "main scroll untouched while pane focused");

    // Repeated 'g' (one per rendered frame, like a real user) walks the
    // render-time bound up to the true top: the head item becomes visible
    // and the tail item scrolls off.
    let mut stable = 0;
    for _ in 0..20 {
        let m = a.last_pane_max_scroll.get();
        pane_go_top(&mut a);
        let _ = render_frame(&a, W, H);
        let m2 = a.last_pane_max_scroll.get();
        stable = m2;
        if m2 == m {
            break;
        }
    }
    assert_eq!(a.focused_pane().unwrap().scroll, Some(stable));
    let top_text = render_transcript_text(&a, W, H);
    assert!(
        top_text.contains("assistant message 0"),
        "g pins the pane to the top (head item visible)"
    );
    assert!(
        !top_text.contains("user message 39"),
        "tail item scrolled off at top"
    );
    // G returns to the live tail.
    pane_go_bottom(&mut a);
    let tail_text = render_transcript_text(&a, W, H);
    assert!(tail_text.contains("user message 39"), "G re-pins to the tail");
}

// ── 7. T036: header scroll-info PLACEMENT parity (FR-001/FR-009/SC-003) ─

/// WIDE terminal (140 cols): the composed pane top title
/// " ◆ subagent: {goal} [{model}] {status} {scroll segment} " fits, so the
/// scroll-info rides the TOP title row exactly like `draw_transcript`'s
/// single top title — placement parity, not just string parity.
#[test]
fn pane_header_scroll_info_rides_top_title_when_it_fits() {
    let mut a = focused_pane_app(40);
    let _ = render_frame(&a, 140, 30); // records pane geometry
    let rows = render_rows(&a, 140, 30);
    let (top, _bottom) = pane_title_rows(&a);
    assert!(
        rows[top].contains("40 messages · live"),
        "PARITY (T036): scroll-info composes into the pane's TOP title row"
    );
    assert!(rows[top].contains("subagent: parity child"), "same row carries the pane identity");
}

/// The same wide geometry SCROLLED: the " {N} messages · {P}% from top "
/// segment rides the top title too (the composition is scroll-state
/// aware, like the orchestrator header).
#[test]
fn pane_header_scrolled_segment_rides_top_title_when_it_fits() {
    let mut a = focused_pane_app(40);
    let _ = render_frame(&a, 140, 30); // records last_pane_max_scroll
    pane_go_top(&mut a);
    let rows = render_rows(&a, 140, 30);
    let (top, _bottom) = pane_title_rows(&a);
    assert!(
        rows[top].contains("40 messages · 0% from top"),
        "PARITY (T036): scrolled header segment composes into the TOP title"
    );
}

/// NARROW terminal (the 80-col suite geometry): the composed title would
/// be clipped, so the segment falls back to the block's BOTTOM-right
/// corner — the pre-T036 placement — keeping it fully visible.
#[test]
fn pane_header_scroll_info_falls_back_to_bottom_corner_when_clipped() {
    let a = focused_pane_app(40);
    let rows = render_rows(&a, W, H);
    let (top, bottom) = pane_title_rows(&a);
    assert!(
        !rows[top].contains("messages · live"),
        "top row stays the bare pane title at narrow widths"
    );
    assert!(
        rows[bottom].contains("40 messages · live"),
        "T036 fit fallback: the segment rides the bottom-right corner fully visible"
    );
}

/// MINIMUM geometry (quickstart.md ≥96 cols): through the real layout the
/// pane is 43 cols wide (96 − 19 rail − 34 sidebar) and its title row
/// holds only 41 usable columns — even the bare 47-col pane title clips,
/// let alone the composed one. The bottom-corner fallback preserves the
/// pre-T036 rendering there; the residual (segment placement differs
/// from the orchestrator at min geometry) is the sanctioned deviation
/// Wave 4 records in checklists/parity.md.
#[test]
fn pane_header_min_geometry_keeps_bottom_fallback() {
    let a = focused_pane_app(40);
    let rows = render_rows(&a, 96, 30);
    let (top, bottom) = pane_title_rows(&a);
    let _ = top;
    assert!(
        rows[bottom].contains("40 messages · live"),
        "min-geometry (96col) pane keeps the bottom-corner scroll segment"
    );
}
