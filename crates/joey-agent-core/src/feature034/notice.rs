//! US2 — unified system-notice channel (quickstart.md US2).

use super::support::{fixture, fixture_with_tools};

fn last_msg(req: &joey_providers::ProviderRequest) -> String {
    req.messages.last().and_then(|m| m.content.clone()).unwrap_or_default()
}

const CHANNEL_ON: &str = "notice_channel:\n  enabled: true\n";

#[tokio::test]
async fn gauge_wrapped_when_channel_on() {
    let mut f = fixture(CHANNEL_ON, vec![]);
    f.turn("hello").await;
    let reqs = f.transport.requests.lock().unwrap();
    let text = last_msg(reqs.last().unwrap());
    assert!(text.starts_with("<system-notice>\n"), "gauge must open the wrapper: {text}");
    assert!(text.ends_with("</system-notice>"), "gauge must close the wrapper: {text}");
    assert!(text.contains("<total_tokens>"), "gauge tag inside wrapper: {text}");
}

#[tokio::test]
async fn gauge_bare_when_channel_off() {
    let mut f = fixture("\n", vec![]);
    f.turn("hello").await;
    let reqs = f.transport.requests.lock().unwrap();
    for m in &reqs.last().unwrap().messages {
        if let Some(c) = &m.content {
            assert!(!c.contains("<system-notice>"), "no wrapper when channel off: {c}");
        }
    }
    let text = last_msg(reqs.last().unwrap());
    assert!(text.contains("<total_tokens>"), "bare gauge still present: {text}");
}

#[tokio::test]
async fn system_prompt_blocks_wrapped_when_on() {
    let f = fixture(CHANNEL_ON, vec![]);
    let sp = f.agent.effective_system_prompt();
    assert!(sp.contains("<system-notice>"), "wrappers must appear: {sp}");
    assert!(sp.contains("</system-notice>"));
    assert!(sp.contains("## System notices"), "trust note must ride when channel on");
    // The project-context header must sit INSIDE a wrapper when present.
    if sp.contains("# Project Context") {
        let h = sp.find("# Project Context").unwrap();
        let open = sp.rfind("<system-notice>").filter(|&o| o < h);
        let close = sp[h..].find("</system-notice>").map(|c| h + c);
        assert!(open.is_some() && close.is_some(), "project context must be wrapped");
    }
}

#[tokio::test]
async fn no_wrapper_in_system_prompt_when_off() {
    let f = fixture("\n", vec![]);
    let sp = f.agent.effective_system_prompt();
    assert!(!sp.contains("<system-notice>"), "channel off must not wrap: {sp}");
    assert!(!sp.contains("## System notices"), "trust note must not ride when channel off");
}

#[tokio::test]
async fn steer_note_never_wrapped() {
    // Tools must be loaded or the steer channel note never rides — the
    // plain fixture registers zero tools, so seed one real tool.
    let f = fixture_with_tools(CHANNEL_ON, vec![], vec!["read_file"]);
    let sp = f.agent.effective_system_prompt();
    let steer = sp.find("## Mid-turn user steering").expect("steer note present");
    // No wrapper span may contain the steer note.
    let mut idx = 0;
    while let Some(o) = sp[idx..].find("<system-notice>") {
        let o = idx + o;
        let c = sp[o..].find("</system-notice>").map(|c| o + c).expect("unbalanced wrapper");
        assert!(!(o < steer && steer < c), "steer note must never sit inside the wrapper");
        idx = c;
    }
}

#[tokio::test]
async fn legacy_history_notice_formats_tolerated() {
    // Pre-feature history carrying a bare gauge-shaped line must replay
    // unchanged — never re-wrapped, never an error.
    let mut f = fixture(CHANNEL_ON, vec![]);
    let history = vec![joey_providers::Message::user(
        "<total_tokens>99999 tokens left</total_tokens>\nold-style notice text",
    )];
    f.agent.set_history(history);
    f.turn("go").await;
    let reqs = f.transport.requests.lock().unwrap();
    let req = reqs.last().unwrap();
    let legacy = req.messages.iter().find(|m| {
        m.content.as_deref().unwrap_or("").contains("old-style notice text")
    }).expect("legacy message replayed");
    let c = legacy.content.as_deref().unwrap();
    assert_eq!(c, "<total_tokens>99999 tokens left</total_tokens>\nold-style notice text");
    assert!(!c.contains("<system-notice>"), "history is never re-wrapped");
}
