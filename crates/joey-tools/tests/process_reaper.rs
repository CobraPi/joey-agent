//! Feature 009 — background reaper tests (US3).
//!
//! - T019: the reaper fills the session `RingBuffer` (previously always empty
//!   because nobody read the child's pipes).
//! - T020: a `notify_on_complete=true` background job fires exactly one
//!   completion event, sets `session.completed`, and doesn't double-fire.
//! - T021: regression — `list` / `kill` / `close` still work with the modified
//!   `ProcessSession` (new fields don't break existing actions), and `kill`
//!   cleans up the reaper task.

use joey_tools::{Tool, ToolContext, ToolRegistry};
use serde_json::{json, Value};
use std::time::Duration;

fn terminal() -> std::sync::Arc<dyn Tool> {
    ToolRegistry::with_builtins()
        .get("terminal")
        .expect("terminal tool registered")
}

fn process() -> std::sync::Arc<dyn Tool> {
    ToolRegistry::with_builtins()
        .get("process")
        .expect("process tool registered")
}

fn ctx() -> ToolContext {
    ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "reaper-test")
}

/// Spawn a background process via the terminal tool and return its session id.
async fn spawn_background(ctx: &ToolContext, command: &str, notify: bool) -> String {
    let res = terminal()
        .execute(
            json!({
                "command": command,
                "background": true,
                "notify_on_complete": notify,
            }),
            ctx,
        )
        .await;
    assert!(!res.is_error(), "background spawn failed: {:?}", res);
    let v: Value =
        serde_json::from_str(&res.to_content_string()).expect("terminal result is JSON");
    v["session_id"]
        .as_str()
        .expect("session_id present")
        .to_string()
}

/// Look up a session in the global registry (returns a clone of its outcome).
fn session_outcome(sid: &str) -> Option<joey_tools::tools::process_tool::ProcessOutcome> {
    let reg = joey_tools::tools::process_tool::process_registry();
    let reg = reg.lock().unwrap_or_else(|p| p.into_inner());
    reg.get(sid).and_then(|s| s.completed.clone())
}

// ── T019: reaper fills the ring buffer ────────────────────────────────────

#[tokio::test]
async fn reaper_fills_ring_buffer_with_output() {
    let sid = spawn_background(&ctx(), "echo hello; sleep 1; echo world", false).await;

    // Wait for completion (the reaper drains the pipes and records the outcome).
    let outcome = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(o) = session_outcome(&sid) {
                return o;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("reaper did not record completion in time");

    // The ring buffer (now drained by the reaper into the outcome tail) must
    // contain the command's output — this is the core bug fix (it used to be
    // empty because nobody read the pipes).
    let combined = format!("{}\n{}", outcome.stdout_tail, outcome.stderr_tail);
    assert!(combined.contains("hello"), "stdout tail has 'hello': {combined:?}");
    assert!(combined.contains("world"), "stdout tail has 'world': {combined:?}");
    assert_eq!(outcome.exit_code, 0, "clean exit code");

    // Cleanup.
    kill(&sid).await;
}

#[tokio::test]
async fn reaper_output_visible_via_poll_after_completion() {
    let sid = spawn_background(&ctx(), "echo poll_marker", false).await;
    // Wait until completed.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if session_outcome(&sid).is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("did not complete");

    // poll after completion surfaces the drained output + the exit code.
    let res = process()
        .execute(json!({ "action": "poll", "session_id": sid }), &ctx())
        .await;
    let out = res.to_content_string();
    assert!(out.contains("poll_marker"), "poll shows captured output: {out}");
    assert!(out.contains("exited with code 0"), "poll shows exit code: {out}");

    kill(&sid).await;
}

// ── T020: completion notification fires exactly once ─────────────────────

#[tokio::test]
async fn completion_notification_fires_once() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let ctx = ctx().with_progress_sender(Some(tx));

    let sid = spawn_background(&ctx, "echo done_marker", true).await;

    // Wait for the one-shot completion notice on the progress channel.
    let notice = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match rx.recv().await {
                Some(msg) if msg.contains("completed") => return msg,
                Some(_) => continue,
                None => return String::new(),
            }
        }
    })
    .await
    .expect("no completion notice within 10s");
    assert!(notice.contains(&sid), "notice carries the session id: {notice}");
    assert!(notice.contains("exit 0"), "notice carries exit code: {notice}");
    assert!(
        notice.contains("done_marker"),
        "notice carries the output tail: {notice}"
    );

    // session.completed is set with the correct exit code.
    let outcome = session_outcome(&sid).expect("completed recorded");
    assert_eq!(outcome.exit_code, 0);

    // Give the reaper a moment and confirm no SECOND notice ever arrives.
    let second = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await;
    assert!(
        second.is_err() || !second.unwrap().unwrap_or_default().contains("completed"),
        "completion must fire exactly once (no double-fire)"
    );

    kill(&sid).await;
}

// ── T021: regression — list / kill / close still work ─────────────────────

async fn kill(sid: &str) {
    let _ = process()
        .execute(json!({ "action": "kill", "session_id": sid }), &ctx())
        .await;
}

#[tokio::test]
async fn action_list_shows_background_process() {
    let sid = spawn_background(&ctx(), "sleep 5", false).await;
    let res = process().execute(json!({ "action": "list" }), &ctx()).await;
    let out = res.to_content_string();
    assert!(out.contains(&sid), "list includes the session: {out}");
    kill(&sid).await;
}

#[tokio::test]
async fn action_kill_cleans_up_and_aborts_reaper() {
    let sid = spawn_background(&ctx(), "sleep 30", false).await;

    let res = process()
        .execute(json!({ "action": "kill", "session_id": sid }), &ctx())
        .await;
    let out = res.to_content_string();
    assert!(out.contains("killed"), "kill reports cleanup: {out}");

    // The session must be gone from the registry.
    let reg = joey_tools::tools::process_tool::process_registry();
    let reg = reg.lock().unwrap_or_else(|p| p.into_inner());
    assert!(reg.get(&sid).is_none(), "session removed after kill");
}

#[tokio::test]
async fn action_close_works_on_background_process() {
    let sid = spawn_background(&ctx(), "sleep 30", false).await;
    let res = process()
        .execute(json!({ "action": "close", "session_id": sid }), &ctx())
        .await;
    assert!(!res.is_error(), "close should succeed: {:?}", res);
    kill(&sid).await;
}

#[tokio::test]
async fn action_wait_returns_promptly_after_completion() {
    let sid = spawn_background(&ctx(), "echo wait_marker", false).await;
    // Wait until the reaper records completion.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if session_outcome(&sid).is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("did not complete");

    // A wait AFTER completion must return near-instantly (within 2s) — proving
    // it consults `completed` instead of re-polling the dead child.
    let res = tokio::time::timeout(Duration::from_secs(2), async {
        process()
            .execute(json!({ "action": "wait", "session_id": sid, "timeout": 30 }), &ctx())
            .await
    })
    .await
    .expect("wait did not return promptly after completion");
    let out = res.to_content_string();
    assert!(out.contains("completed"), "wait reports completion: {out}");
    assert!(out.contains("wait_marker"), "wait shows output: {out}");

    kill(&sid).await;
}

// ── T026: cross-turn delivery via session-persistent queue ───────────────

#[tokio::test]
async fn completion_pushed_to_persistent_queue() {
    // When notify_on_complete=true, the reaper must push a BackgroundCompletion
    // into the context's session-persistent queue (not just the per-turn
    // progress channel). This is what survives the launching turn.
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let ctx = ctx().with_progress_sender(Some(tx));

    let sid = spawn_background(&ctx, "echo queue_marker", true).await;

    // Wait for the reaper to finalize and push to the queue.
    let completion = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let drained = ctx.drain_pending_completions();
            if let Some(c) = drained.into_iter().next() {
                return c;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("completion not pushed to persistent queue within 10s");

    assert_eq!(completion.session_id, sid, "carries session id");
    assert_eq!(completion.exit_code, 0, "correct exit code");
    assert!(
        completion.output_tail.contains("queue_marker"),
        "carries output tail: {:?}",
        completion.output_tail
    );
    assert!(completion.elapsed_secs >= 0.0, "carries elapsed time");

    // The queue must be empty after draining.
    assert!(ctx.drain_pending_completions().is_empty(), "drained exactly once");

    kill(&sid).await;
}

#[tokio::test]
async fn persistent_queue_survives_drop_of_progress_sender() {
    // Simulate cross-turn: spawn with a progress sender, then drop the
    // receiver (as the REPL does at turn end). The completion must still
    // arrive in the persistent queue — the dropped progress channel is
    // best-effort, the queue is the delivery guarantee. The reaper captured a
    // clone of the ToolContext (sharing the same Arc<ContextInner>), so the
    // original context's queue sees the push.
    let ctx = ctx();
    let sid = {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let bg_ctx = ctx.clone().with_progress_sender(Some(tx));
        let sid = spawn_background(&bg_ctx, "echo cross_turn", true).await;
        drop(rx); // simulate turn-end: event channel gone
        sid
    };

    let completion = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let drained = ctx.drain_pending_completions();
            if let Some(c) = drained.into_iter().next() {
                return c;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("completion not in persistent queue after channel drop");

    assert_eq!(completion.session_id, sid);
    assert!(
        completion.output_tail.contains("cross_turn"),
        "output preserved despite dropped event channel"
    );

    kill(&sid).await;
}
