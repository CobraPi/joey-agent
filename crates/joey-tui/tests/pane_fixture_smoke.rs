//! Smoke test for the shared pane-parity fixture (`tests/common/mod.rs`,
//! feature 017 T002). Proves the fixture compiles and its builders produce
//! the expected pane/transcript state and rendered output. Not one of the
//! four parity suites — those are T006/T010/T014/T019.

mod common;

use common::*;
use joey_tui::state::TranscriptItem;

/// The pane builder spawns one pane with every synthetic item kind present
/// (n=5 cycles Assistant/Tool/Reasoning/FileDiff/User exactly once each).
#[test]
fn pane_builder_populates_all_item_kinds_in_order() {
    let mut a = app();
    let idx = pane_with_transcript(&mut a, 1, "smoke goal", 5);
    assert_eq!(a.subagent_panes[idx].child_id, 1);
    assert_eq!(a.subagent_panes[idx].goal, "smoke goal");
    assert_eq!(a.subagent_panes[idx].transcript.len(), 5);
    let kinds: Vec<&str> = a.subagent_panes[idx]
        .transcript
        .iter()
        .map(|it| match it {
            TranscriptItem::Assistant { .. } => "assistant",
            TranscriptItem::Tool { .. } => "tool",
            TranscriptItem::Reasoning { .. } => "reasoning",
            TranscriptItem::FileDiff { .. } => "filediff",
            TranscriptItem::User { .. } => "user",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["assistant", "tool", "reasoning", "filediff", "user"]);
}

/// The focused-pane convenience yields a pane-focused App whose pane view
/// replaces the main transcript (pane title rendered, parent content hidden).
#[test]
fn focused_pane_app_replaces_main_transcript() {
    let mut a = focused_pane_app(5);
    a.push_item(TranscriptItem::User {
        text: "parent prompt".into(),
    });
    let text = render_frame(&a, 120, 30);
    assert!(text.contains("subagent: parity child"), "pane title shown");
    assert!(text.contains("assistant message 0"), "pane transcript rendered");
    assert!(
        !render_transcript_text(&a, 120, 30).contains("parent prompt"),
        "parent transcript hidden while pane focused"
    );
}

/// The transcript renderer scopes to the recorded text-area rect.
#[test]
fn transcript_renderer_scopes_to_text_area() {
    let a = focused_pane_app(5);
    let text = render_transcript_text(&a, 120, 30);
    assert!(text.contains("assistant message 0"), "pane content in area");
    assert!(!text.contains("subagents"), "rail not in the transcript area");
}

/// Larger payloads: tool results and diffs carry their per-line markers.
#[test]
fn item_constructors_carry_markers() {
    let tool = match tool_item(3, 10) {
        TranscriptItem::Tool { full_result: Some(r), .. } => r,
        _ => panic!("tool item"),
    };
    assert!(tool.contains("tool 3 output line 9"));
    let diff = match file_diff_item(2, 6) {
        TranscriptItem::FileDiff { lines, path, .. } => {
            assert_eq!(path, "src/file2.rs");
            lines
        }
        _ => panic!("diff item"),
    };
    assert_eq!(diff.len(), 6);
    assert!(diff[0].contains("+ item 2 diff line 0"));
    assert!(diff[5].contains("- item 2 diff line 5"));
    assert!(matches!(binary_diff_item(1), TranscriptItem::FileDiff { is_binary: true, .. }));
}
