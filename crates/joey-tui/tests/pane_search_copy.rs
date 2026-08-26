//! T014 / US3 (feature 017): search & copy PARITY between the focused
//! subagent pane and the orchestrator screen (FR-006/FR-007, design D5).
//!
//! TDD: written BEFORE pane search/copy exist (T015/T016/T017). The pane
//! search STATE fields (`SubagentPane::search_open/search_query/
//! search_has_match`) do not exist yet, so per the task brief these tests
//! drive the App-level mutators `Tui::handle_key` dispatches to (the
//! search-bar typing path: `search_open`/`search_query`/`run_search`/
//! `search_next`) and assert the OBSERVABLE contract D5 pins: with a pane
//! focused, search is pane-scoped (generalized `run_search`/`search_next`
//! operating on the pane transcript, main view untouched).
//!
//! Expected outcome against current code:
//!   - FAIL: every pane-scoping test — today `run_search`/`search_next`
//!     search `App::transcript` (main) only, so a search driven while a
//!     pane is focused moves the MAIN scroll (must stay `None`) and never
//!     pins the pane (`pane.scroll` stays `None`), and the match
//!     indicator reflects main-only matches.
//!   - FAIL: the pane match-indicator tests — `search_has_match` mirrors
//!     the main transcript, so a pane-only match shows "no matches" and a
//!     main-only match shows "match found" while the pane is focused.
//!   - PASS (legitimately): the format pins (empty-query title, prompt
//!     line, main-side indicator strings) — they mirror the orchestrator
//!     formats the pane view must reuse byte-for-byte.
//!   - PASS (legitimately): the no-in-text-highlight parity test — search
//!     only scrolls (never highlights) on the main screen; the pane
//!     renders the same item through the same `item_lines` path, so the
//!     matched row's styles must be identical and REVERSED-free.
//!   - PASS (pin + documented gap): the copy contract test — `y` on a
//!     focused pane must NOT resolve through the main transcript (T003
//!     routing). The `TuiAction::CopyPaneItem { pane, idx }` variant does
//!     not exist yet (T017), so its EMISSION cannot be referenced from an
//!     integration test; this test pins the routing precondition and the
//!     distinguishability of the two copy resolutions instead. T017's
//!     acceptance must add the emission assert once the variant lands.
//!
//! `Tui` itself needs a real TTY (`new_for_test` is `#[cfg(test)]`-gated),
//! so keys are driven through the exact App-level state/mutator sequence
//! the handlers use — same convention as `pane_scroll_parity.rs`.

mod common;

use common::*;
use joey_tui::state::{App, TranscriptItem};
use joey_tui::Theme;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;

const W: u16 = 80;
const H: u16 = 24;

// ── local helpers (common/mod.rs is frozen for this task) ─────────────

/// A User item with arbitrary text (needles/fillers that never collide
/// with the `message {i}` markers of the common constructors).
fn user_text(s: &str) -> TranscriptItem {
    TranscriptItem::User { text: s.to_string() }
}

/// An Assistant item with arbitrary text.
fn assistant_text(s: &str) -> TranscriptItem {
    TranscriptItem::Assistant { text: s.to_string() }
}

/// Build an App with ONE pane (child 1, "search child") holding
/// `pane_items` (pushed oldest-first), a main transcript holding
/// `main_items`, and the pane FOCUSED.
fn pane_app_with(pane_items: &[TranscriptItem], main_items: &[TranscriptItem]) -> App {
    let mut a = app();
    let idx = pane_with_transcript(&mut a, 1, "search child", 0);
    for it in pane_items {
        a.subagent_panes[idx].push_item(it.clone());
    }
    for it in main_items {
        a.push_item(it.clone());
    }
    a.focus_subagent(Some(idx));
    a
}

/// Render one full frame the way `Tui::draw` composes it for search: the
/// real body layout, THEN the search-bar overlay (draw_search_bar is
/// invoked after render_body in Tui::draw; `render_body_for_test` alone
/// does not include it). Returns the whole buffer as a flat string.
fn render_frame_with_search(a: &App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let area = f.area();
            joey_tui::app::render_body_for_test(f, area, a, Theme::aurora(), false, 0.5);
            joey_tui::widgets::draw_search_bar(f, area, a, &Theme::aurora());
        })
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol().to_string())
        .collect()
}

/// Render one frame (no search overlay — the transcript layer only) and
/// return the focused transcript area's rows with per-cell styles, so
/// highlight-parity can be asserted on the buffer itself.
fn text_area_rows_with_styles(
    a: &App,
    width: u16,
    height: u16,
) -> Vec<(String, Vec<(Color, Color, Modifier)>)> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let area = f.area();
            joey_tui::app::render_body_for_test(f, area, a, Theme::aurora(), false, 0.5);
        })
        .unwrap();
    let (x, y, w, h) = if a.focused_subagent.is_some() {
        a.last_pane_text_area.get()
    } else {
        a.last_text_area.get()
    };
    assert!(w > 0 && h > 0, "text area rect recorded (frame drew a transcript)");
    let buf = terminal.backend().buffer();
    let mut rows = Vec::new();
    for row in y..y.saturating_add(h) {
        let mut text = String::new();
        let mut styles = Vec::new();
        for col in x..x.saturating_add(w) {
            let cell = &buf[(col, row)];
            text.push_str(cell.symbol());
            styles.push((cell.fg, cell.bg, cell.modifier));
        }
        rows.push((text, styles));
    }
    rows
}

/// Exactly what the search-bar typing path does per character (Tui::
/// handle_search_key): push the char, run the search.
fn type_search(a: &mut App, query: &str) {
    a.search_open = true;
    for c in query.chars() {
        a.search_query.push(c);
        a.run_search();
    }
}

// ── 1. unfocused pin: '/' opens MAIN search only ─────────────────────

/// With panes present but NONE focused, the '/' Main arm's mutators
/// (search_open = true, query typed, run_search) search the MAIN
/// transcript only: the main scroll moves and every pane's scroll stays
/// None (focused-view isolation, T005's pin from the search side).
#[test]
fn unfocused_search_moves_main_only_and_never_panes() {
    let mut a = app();
    let p0 = pane_with_transcript(&mut a, 1, "search child", 30);
    let p1 = pane_with_transcript(&mut a, 2, "other child", 30);
    a.push_item(user_text("main needle beta"));
    assert!(a.focused_subagent.is_none(), "precondition: orchestrator view");
    let _ = render_frame(&a, W, H); // record bounds like a real frame

    type_search(&mut a, "needle");
    assert!(a.search_open, "main search open");
    assert!(a.search_has_match, "main transcript matched");
    assert_eq!(a.scroll, Some(0), "main search pins the main view");
    assert_eq!(
        a.subagent_panes[p0].scroll, None,
        "pane 0 scroll untouched by main search"
    );
    assert_eq!(
        a.subagent_panes[p1].scroll, None,
        "pane 1 scroll untouched by main search"
    );
    let frame = render_frame_with_search(&a, W, H);
    assert!(
        frame.contains("search · match found (n=next N=prev)"),
        "main indicator format pinned: {frame:?}"
    );
}

// ── 2. pane-scoped search (FR-006) ───────────────────────────────────

/// Searching from a FOCUSED pane is pane-scoped (D5): the query matches
/// BOTH transcripts ("needle" hits the pane's "pane needle alpha" and the
/// main's "main needle beta"), yet the search must pin the PANE view to
/// the pane occurrence and never move the main scroll. Today run_search
/// searches main only → both assertions FAIL.
#[test]
fn focused_pane_search_pins_pane_view_and_leaves_main_alone() {
    let mut pane_items = vec![user_text("pane needle alpha")];
    pane_items.extend((0..29).map(user_item)); // overflow an 80x24 pane
    let mut a = pane_app_with(&pane_items, &[user_text("main needle beta")]);
    let _ = render_frame(&a, W, H); // record bounds like a real frame

    type_search(&mut a, "needle");
    assert!(
        a.focused_pane().unwrap().scroll.is_some(),
        "PARITY: pane search pins the pane view to the pane's match \
         (pane.scroll stays None today — run_search is main-only)"
    );
    assert_eq!(
        a.scroll, None,
        "PARITY: main scroll untouched by a pane-scoped search \
         (run_search sets Some(0) today because the main needle matches)"
    );
    let frame = render_frame_with_search(&a, W, H);
    assert!(
        frame.contains("subagent: search child"),
        "sanity: the pane view (not the orchestrator) is on screen"
    );
    assert!(
        frame.contains("/ needle▏"),
        "PARITY: the search bar (prompt + query) is drawn over the pane view"
    );
}

/// A query that matches ONLY the main transcript must be a NO-match from
/// the focused pane: the pane view stays put, the MAIN scroll stays None,
/// and the match indicator reads "no matches". Today all three FAIL in
/// the wrong direction (main scroll moves; indicator says "match found").
#[test]
fn focused_pane_search_ignores_main_only_matches() {
    let pane_items: Vec<_> = (0..30).map(user_item).collect(); // no needle
    let mut a = pane_app_with(&pane_items, &[user_text("main needle beta")]);
    let _ = render_frame(&a, W, H);

    type_search(&mut a, "needle");
    assert_eq!(
        a.scroll, None,
        "PARITY: a main-only match must not move the MAIN scroll while the \
         pane is focused (search is pane-scoped)"
    );
    assert_eq!(
        a.focused_pane().unwrap().scroll, None,
        "PARITY: no pane match → pane view stays at follow-tail"
    );
    let frame = render_frame_with_search(&a, W, H);
    assert!(
        frame.contains("search · no matches"),
        "PARITY: indicator must reflect the PANE transcript (no needle \
         there); today it mirrors the main match and says 'match found'"
    );
    assert!(
        !frame.contains("search · match found"),
        "PARITY: main-only matches must not report 'match found' in pane view"
    );
}

// ── 3. n/N navigation scrolls the OWNING view (FR-006) ───────────────

/// With two pane occurrences (an older and a newer "pane needle") and a
/// main occurrence, `run_search` lands on the pane's newest match, N
/// (search_next(false)) walks to the older one, n (search_next(true))
/// walks back — each step moves the PANE scroll and the main scroll never
/// leaves None. Today search_next is main-only: the pane-scroll expects
/// FAIL (pane.scroll is None throughout).
#[test]
fn focused_pane_search_next_prev_move_pane_only() {
    let mut pane_items: Vec<_> = (0..30).map(user_item).collect();
    pane_items[3] = user_text("pane needle old");
    pane_items[27] = user_text("pane needle new");
    let mut a = pane_app_with(&pane_items, &[user_text("main needle beta")]);
    let _ = render_frame(&a, W, H);

    type_search(&mut a, "needle");
    let first = a
        .focused_pane()
        .unwrap()
        .scroll
        .expect("PARITY: run_search pins the pane to its newest 'needle' match");
    assert_eq!(a.scroll, None, "PARITY: main scroll untouched (run_search)");

    a.search_next(false); // N → toward older messages
    let second = a
        .focused_pane()
        .unwrap()
        .scroll
        .expect("PARITY: N moves the pane to the older 'pane needle old' match");
    assert_ne!(
        first, second,
        "PARITY: N navigates — the pane offset targets the other pane match"
    );
    assert_eq!(a.scroll, None, "PARITY: main scroll untouched (N)");

    a.search_next(true); // n → back toward newer messages
    let third = a
        .focused_pane()
        .unwrap()
        .scroll
        .expect("PARITY: n moves the pane back to the newer match");
    assert_ne!(
        second, third,
        "PARITY: n navigates — the pane offset returns toward 'pane needle new'"
    );
    assert_eq!(a.scroll, None, "PARITY: main scroll untouched (n)");
}

// ── 4. match-indicator bar parity (FR-006: mirror main's format) ─────

/// A pane-only match must show the SAME indicator title the orchestrator
/// shows on a match: " search · match found (n=next N=prev) ". Today
/// search_has_match mirrors the main transcript (no needle there) and the
/// bar reads "no matches" → FAIL.
#[test]
fn focused_pane_match_indicator_shows_match_found() {
    let mut pane_items: Vec<_> = (0..29).map(user_item).collect();
    pane_items.push(user_text("pane needle alpha")); // newest pane item
    let mut a = pane_app_with(&pane_items, &[user_text("no such marker")]);
    let _ = render_frame(&a, W, H);

    type_search(&mut a, "needle");
    let frame = render_frame_with_search(&a, W, H);
    assert!(
        frame.contains("subagent: search child"),
        "sanity: pane view on screen"
    );
    assert!(
        frame.contains("search · match found (n=next N=prev)"),
        "PARITY: pane match shows the orchestrator's exact indicator title; \
         today the bar mirrors main-only state and says 'no matches'"
    );
}

/// The orchestrator's three indicator titles + prompt line, pinned on the
/// main screen so the pane bar (T015) reuses them byte-for-byte. PASSes
/// today by design — it is the format contract, not the pane wiring.
#[test]
fn search_bar_formats_pinned_on_orchestrator() {
    let mut a = app();
    pane_with_transcript(&mut a, 9, "chrome only", 1); // rail parity
    a.push_item(user_text("main needle beta"));
    assert!(a.focused_subagent.is_none());
    let _ = render_frame(&a, W, H);

    // Empty query: the "Esc to close" title and the bare prompt.
    a.search_open = true;
    let frame = render_frame_with_search(&a, W, H);
    assert!(
        frame.contains("search (Esc to close)"),
        "empty-query title: {frame:?}"
    );
    assert!(frame.contains("/ ▏"), "bare prompt '/' + cursor ▏: {frame:?}");

    // No match: "no matches".
    a.search_query.push_str("zzz");
    a.run_search();
    let frame = render_frame_with_search(&a, W, H);
    assert!(
        frame.contains("search · no matches"),
        "no-match title: {frame:?}"
    );

    // Match: "match found" + the prompt carrying the query.
    a.search_query.clear();
    a.run_search();
    a.search_query.push_str("needle");
    a.run_search();
    let frame = render_frame_with_search(&a, W, H);
    assert!(
        frame.contains("search · match found (n=next N=prev)"),
        "match title: {frame:?}"
    );
    assert!(frame.contains("/ needle▏"), "prompt + query + cursor: {frame:?}");
}

// ── 5. NO in-text highlighting (FR-006 parity: search scrolls only) ──

/// Parity pin: the orchestrator screen never highlights matched text
/// in-place (search only scrolls); the pane must match. Both views hold
/// the SAME items, the query matches both, and the matched row's
/// per-cell styles in the pane must equal the orchestrator's — with no
/// REVERSED cells in either. PASSes today (item rendering is shared);
/// it exists so T015 cannot add pane-side in-text highlighting.
#[test]
fn matched_row_renders_without_highlight_pane_equals_main() {
    let needle = "needle highlight probe";
    let build = |fill_before: usize, fill_after: usize| -> Vec<TranscriptItem> {
        let mut v: Vec<_> = (0..fill_before).map(user_item).collect();
        v.push(assistant_text(needle));
        v.extend((0..fill_after).map(user_item));
        v
    };
    // Needle 3rd-from-last: at follow-tail (and at the post-T015 scroll,
    // which lands at ≈0) the row is on screen in BOTH views.
    let items = build(9, 2);

    // Orchestrator counterpart.
    let mut m = app();
    pane_with_transcript(&mut m, 9, "chrome only", 1); // same rail chrome
    for it in &items {
        m.push_item(it.clone());
    }
    assert!(m.focused_subagent.is_none());
    let _ = render_frame(&m, W, H);
    type_search(&mut m, "needle");
    assert!(m.search_has_match, "main counterpart matched");
    let main_rows = text_area_rows_with_styles(&m, W, H);
    let (_, main_styles) = main_rows
        .iter()
        .find(|(t, _)| t.contains(needle))
        .expect("main: matched row visible after run_search");
    assert!(
        !main_styles.iter().any(|(_, _, mo)| mo.contains(Modifier::REVERSED)),
        "main parity: matched text is never REVERSED (no in-text highlight)"
    );

    // Pane parity: same items, pane focused, same query.
    let mut a = pane_app_with(&items, &[]);
    let _ = render_frame(&a, W, H);
    type_search(&mut a, "needle");
    let pane_rows = text_area_rows_with_styles(&a, W, H);
    let (_, pane_styles) = pane_rows
        .iter()
        .find(|(t, _)| t.contains(needle))
        .expect("PARITY: pane: matched row visible (pane search scrolls the pane)");
    assert!(
        !pane_styles.iter().any(|(_, _, mo)| mo.contains(Modifier::REVERSED)),
        "PARITY: pane matched text is never REVERSED (no in-text highlight)"
    );
    assert_eq!(
        pane_styles, main_styles,
        "PARITY: matched row styles identical to the orchestrator's"
    );
}

// ── 6. copy contract (FR-007, T017) ──────────────────────────────────

/// `y` in transcript mode copies the last assistant message; with a pane
/// focused it must resolve against the PANE transcript (T017's
/// `TuiAction::CopyPaneItem { pane, idx }`), never the main one. The
/// variant does not exist yet, so its emission cannot be referenced from
/// an integration test; this test pins the routing precondition (T003:
/// the Pane arm no-ops, so no main `CopyItem` may fire) and that the two
/// resolutions are distinguishable — the pane's last assistant text
/// differs from the main's, so a wrongly-main-routed copy is detectable.
/// T017's acceptance must extend this with the emission assert.
#[test]
fn focused_pane_y_copy_never_resolves_through_main() {
    let pane_items = vec![
        assistant_text("pane secret answer"),
        user_item(0),
    ];
    let main_items = vec![
        assistant_text("main secret answer"),
        user_item(1),
    ];
    let a = pane_app_with(&pane_items, &main_items);
    assert!(
        a.focused_subagent.is_some(),
        "precondition: pane focused — y routes to the Pane arm (T003), so \
         the main-transcript resolution below must NOT fire"
    );

    // Exactly the resolution the y Main arm performs (app.rs): rposition
    // over the MAIN transcript. Guarded by target==Main it never runs
    // while a pane is focused.
    let main_last_assistant = a
        .transcript
        .iter()
        .rev()
        .find(|i| matches!(i, TranscriptItem::Assistant { .. }))
        .map(|i| match i {
            TranscriptItem::Assistant { text } => text.clone(),
            _ => unreachable!(),
        });
    let pane_last_assistant = a
        .focused_pane()
        .unwrap()
        .transcript
        .iter()
        .rev()
        .find(|i| matches!(i, TranscriptItem::Assistant { .. }))
        .map(|i| match i {
            TranscriptItem::Assistant { text } => text.clone(),
            _ => unreachable!(),
        });

    assert_eq!(
        main_last_assistant.as_deref(),
        Some("main secret answer"),
        "the main resolution exists and would copy MAIN text if mis-routed"
    );
    assert_eq!(
        pane_last_assistant.as_deref(),
        Some("pane secret answer"),
        "the pane resolution (T017's CopyPaneItem payload) exists and is \
         indexed inside the pane transcript"
    );
    assert_ne!(
        main_last_assistant, pane_last_assistant,
        "the two resolutions are distinguishable: CopyPaneItem must carry \
         the pane id + pane-relative idx (T017), a bare CopyItem(idx) would \
         copy the wrong (main) text"
    );
}
