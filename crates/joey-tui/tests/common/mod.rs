//! Shared test-fixture module for the pane parity suites (feature 017).
//!
//! Included from each parity suite via `mod common;`:
//!
//! - `tests/pane_scroll_parity.rs`   (T006 / US1)
//! - `tests/pane_expand_parity.rs`   (T010 / US2)
//! - `tests/pane_search_copy.rs`     (T014 / US3)
//! - `tests/pane_maximized_parity.rs`(T019 / US4)
//!
//! Provides builders that spawn a `SubagentPane` populated with N synthetic
//! `TranscriptItem`s (cycling Assistant/Tool/Reasoning/FileDiff/User, each
//! carrying a deterministic `item {i}` marker so buffer assertions and
//! ordering checks stay unambiguous), a focused-pane convenience, and a
//! TestBackend full-frame renderer matching the conventions of
//! `tests/subagent_panes.rs`.
//!
//! Items are pushed DIRECTLY into the pane transcript (not via wrapped
//! `AgentEvent`s) so the fixture stays forward-compatible: e.g. panes do
//! not map `FileChange` events to `FileDiff` items until T013, but parity
//! suites need diff items in panes from T010 on.
//!
//! Each integration-test target compiles its own copy of this module, so
//! different suites use different helper subsets — unused helpers are
//! intentionally allowed.

#![allow(dead_code)]

use joey_agent_core::events::AgentEvent;
use joey_tui::state::{
    App, ReasoningExpandState, ToolStatus, TranscriptItem, LIVE_OUTPUT_CAPACITY,
};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use std::time::Duration;

/// A fresh App, matching the convention of `tests/subagent_panes.rs`.
pub fn app() -> App {
    App::new("s", "m")
}

/// A `SubagentSpawn` orchestration event for `child_id` with a unique goal.
pub fn spawn(child_id: u64, goal: &str) -> AgentEvent {
    AgentEvent::SubagentSpawn {
        id: child_id,
        goal: goal.to_string(),
        model: "test-model".to_string(),
        toolset_summary: "file, web".to_string(),
        depth: 0,
    }
}

// ── Synthetic TranscriptItem constructors ──────────────────────────────
//
// Every constructor bakes a deterministic `item {i}` marker into each text
// field so tests can assert presence, ordering, and scroll position in the
// rendered buffer without ambiguity.

/// A `User` item: "user message {i}".
pub fn user_item(i: usize) -> TranscriptItem {
    TranscriptItem::User {
        text: format!("user message {i}"),
    }
}

/// An `Assistant` item: "assistant message {i}".
pub fn assistant_item(i: usize) -> TranscriptItem {
    TranscriptItem::Assistant {
        text: format!("assistant message {i}"),
    }
}

/// A `Reasoning` item: three lines "reasoning {i} line 0..2", collapsed.
pub fn reasoning_item(i: usize) -> TranscriptItem {
    TranscriptItem::Reasoning {
        text: format!("reasoning {i} line 0\nreasoning {i} line 1\nreasoning {i} line 2"),
        expand_state: ReasoningExpandState::Collapsed,
        thought_duration: Some(Duration::from_secs(2)),
    }
}

/// A completed `Tool` item named `tool{i}` with a `result_lines`-line
/// result ("tool {i} output line 0.." in both preview and full_result),
/// collapsed — the expand cycle (T010) starts from `Collapsed`.
pub fn tool_item(i: usize, result_lines: usize) -> TranscriptItem {
    let result = (0..result_lines)
        .map(|j| format!("tool {i} output line {j}"))
        .collect::<Vec<_>>()
        .join("\n");
    TranscriptItem::Tool {
        name: format!("tool{i}"),
        emoji: "🔧".to_string(),
        summary: format!("item {i} summary"),
        status: ToolStatus::Done,
        duration_secs: Some(0.5),
        result_preview: result.clone(),
        expand_state: ReasoningExpandState::Collapsed,
        full_args: Some(format!("{{\"arg\": \"item {i}\"}}")),
        full_result: Some(result),
        is_terminal: false,
        exit_code: Some(0),
        live_output: String::new(),
        live_output_capacity: LIVE_OUTPUT_CAPACITY,
    }
}

/// A `FileDiff` item for `src/file{i}.rs` with `diff_lines` marker lines
/// ("+ item {i} diff line 0.." etc.), collapsed.
pub fn file_diff_item(i: usize, diff_lines: usize) -> TranscriptItem {
    let lines = (0..diff_lines)
        .map(|j| {
            if j % 2 == 0 {
                format!("+ item {i} diff line {j}")
            } else {
                format!("- item {i} diff line {j}")
            }
        })
        .collect();
    TranscriptItem::FileDiff {
        path: format!("src/file{i}.rs"),
        stat: format!("+{a} -{b}", a = diff_lines.div_ceil(2), b = diff_lines / 2),
        lines,
        is_binary: false,
        expand_state: ReasoningExpandState::Collapsed,
    }
}

/// A binary `FileDiff` item (exercises the binary placeholder rendering).
pub fn binary_diff_item(i: usize) -> TranscriptItem {
    TranscriptItem::FileDiff {
        path: format!("bin/blob{i}.png"),
        stat: "binary".to_string(),
        lines: Vec::new(),
        is_binary: true,
        expand_state: ReasoningExpandState::Collapsed,
    }
}

// ── Pane builders ───────────────────────────────────────────────────────

/// Spawn a pane for `child_id` and push `n` synthetic items into it,
/// cycling kinds by index: `i % 5` → Assistant(0), Tool(1), Reasoning(2),
/// FileDiff(3), User(4). With `n >= 4` every kind is present. Tools get
/// 6 result lines; diffs get 4 lines — enough to render distinctly while
/// staying small; suites needing larger payloads call the item
/// constructors directly.
///
/// Returns the pane's index in `App::subagent_panes`.
pub fn pane_with_transcript(a: &mut App, child_id: u64, goal: &str, n: usize) -> usize {
    a.apply(spawn(child_id, goal));
    let idx = a
        .subagent_panes
        .iter()
        .position(|p| p.child_id == child_id)
        .expect("spawn created the pane");
    for i in 0..n {
        let item = match i % 5 {
            0 => assistant_item(i),
            1 => tool_item(i, 6),
            2 => reasoning_item(i),
            3 => file_diff_item(i, 4),
            _ => user_item(i),
        };
        a.subagent_panes[idx].push_item(item);
    }
    idx
}

/// Convenience for the common parity-suite setup: an App with ONE pane
/// (child 1, goal "parity child") holding `n` synthetic items, already
/// focused so the pane view replaces the main transcript. Panes keep
/// per-pane state on `App::focused_pane()`.
pub fn focused_pane_app(n: usize) -> App {
    let mut a = app();
    let idx = pane_with_transcript(&mut a, 1, "parity child", n);
    a.focus_subagent(Some(idx));
    a
}

// ── Rendering (TestBackend) ─────────────────────────────────────────────

/// Render one full frame of the real body layout (rail + panes + sidebar,
/// same `render_body_for_test` path as `tests/subagent_panes.rs`) and
/// return the entire buffer as a flat symbol string. Use for substring
/// assertions on chrome/affordances that appear anywhere in the frame.
pub fn render_frame(a: &App, width: u16, height: u16) -> String {
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
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol().to_string())
        .collect()
}

/// Render one frame and return ONLY the transcript area's buffer text
/// (row-major, one terminal row per line), using the text-area rect the
/// frame recorded. When a pane is focused that is `App::last_pane_text_area`
/// (the pane transcript replaces the main view); otherwise it is
/// `App::last_text_area`. For assertions that must be scoped to the
/// transcript column (e.g. pane-vs-sidebar disambiguation).
pub fn render_transcript_text(a: &App, width: u16, height: u16) -> String {
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
    let mut out = String::new();
    for row in y..y.saturating_add(h) {
        for col in x..x.saturating_add(w) {
            out.push_str(buf[(col, row)].symbol());
        }
        out.push('\n');
    }
    out
}
