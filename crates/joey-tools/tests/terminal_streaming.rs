//! Feature 009 — terminal async streaming & performance tests.
//!
//! - T009: regression — the terminal tool result schema is unchanged after the
//!   streaming refactor (`{output, exit_code, error}` present, correct values,
//!   and works with NO progress sender set → backward-compatible path).
//! - T010: streaming behavior — `ToolProgress` deltas are emitted during a
//!   long-running command when a progress sender is set.
//! - T011: temp-file round-trip — a command producing > 4 KB of output yields
//!   the FULL output in the final result (read back from the temp-file capture).

use joey_tools::{Tool, ToolContext, ToolRegistry};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn terminal() -> std::sync::Arc<dyn Tool> {
    ToolRegistry::with_builtins()
        .get("terminal")
        .expect("terminal tool registered")
}

fn ctx() -> ToolContext {
    ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "streaming-test")
}

/// Parse the terminal tool's JSON result string.
fn parse_result(content: &str) -> Value {
    serde_json::from_str::<Value>(content).unwrap_or_else(|e| {
        panic!("terminal result is not valid JSON: {e}\n---\n{content}\n---")
    })
}

// ── T009: result schema regression ───────────────────────────────────────

#[tokio::test]
async fn terminal_result_schema_has_required_fields() {
    let tool = terminal();
    let result = tool.execute(json!({ "command": "echo hello" }), &ctx()).await;
    assert!(!result.is_error(), "echo should not error");
    let v = parse_result(&result.to_content_string());
    assert!(v.get("output").is_some(), "output field present");
    assert!(v.get("exit_code").is_some(), "exit_code field present");
    assert!(v.get("error").is_some(), "error field present");
}

#[tokio::test]
async fn terminal_result_exit_code_zero_on_success() {
    let tool = terminal();
    let result = tool.execute(json!({ "command": "echo hello" }), &ctx()).await;
    let v = parse_result(&result.to_content_string());
    assert_eq!(v["exit_code"], json!(0));
    assert_eq!(v["error"], Value::Null);
    assert!(
        v["output"].as_str().unwrap().contains("hello"),
        "output should contain 'hello': {:?}",
        v["output"]
    );
}

#[tokio::test]
async fn terminal_result_nonzero_exit_code() {
    let tool = terminal();
    let result = tool.execute(json!({ "command": "exit 3" }), &ctx()).await;
    let v = parse_result(&result.to_content_string());
    assert_eq!(v["exit_code"], json!(3));
}

#[tokio::test]
async fn terminal_works_without_progress_sender_backward_compat() {
    // The backward-compatible path: never call with_progress_sender.
    // The tool must still return a correct result (no panic, exit_code 0).
    let tool = terminal();
    let result = tool
        .execute(json!({ "command": "echo backward_compat" }), &ctx())
        .await;
    let v = parse_result(&result.to_content_string());
    assert_eq!(v["exit_code"], json!(0));
    assert!(
        v["output"].as_str().unwrap().contains("backward_compat"),
        "output should contain marker"
    );
}

// ── T010: streaming emits ToolProgress deltas ────────────────────────────

#[tokio::test]
async fn terminal_emits_progress_during_long_command() {
    let tool = terminal();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let ctx = ctx().with_progress_sender(Some(tx));

    // line1 now, line2 after a 1s sleep — each should be its own delta.
    let result = tool
        .execute(
            json!({ "command": "echo line1; sleep 1; echo line2" }),
            &ctx,
        )
        .await;
    let v = parse_result(&result.to_content_string());
    let out = v["output"].as_str().unwrap();
    assert!(out.contains("line1"), "result has line1");
    assert!(out.contains("line2"), "result has line2");

    // Drain the progress channel — expect at least two distinct deltas
    // (one per line) rather than everything dumped at once at the end.
    let mut deltas = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        deltas.push(msg);
    }
    assert!(
        deltas.len() >= 2,
        "expected >= 2 progress deltas, got {}: {:?}",
        deltas.len(),
        deltas
    );
    let joined = deltas.concat();
    assert!(joined.contains("line1"), "a delta carries line1");
    assert!(joined.contains("line2"), "a delta carries line2");
}

#[tokio::test]
async fn terminal_progress_arrives_before_completion() {
    // A progress delta must arrive DURING the run, not only at the end.
    // We race the command against a timer: if streaming works, we observe a
    // delta well before the (3s) command finishes.
    let tool = terminal();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let ctx = ctx().with_progress_sender(Some(tx));

    let run = tokio::spawn(async move {
        tool.execute(json!({ "command": "echo early; sleep 3; echo late" }), &ctx)
            .await
    });

    // Within 1.5s we should have seen the "early" delta.
    let got_early = tokio::time::timeout(
        std::time::Duration::from_millis(1500),
        async {
            loop {
                match rx.recv().await {
                    Some(msg) if msg.contains("early") => return true,
                    Some(_) => continue,
                    None => return false,
                }
            }
        },
    )
    .await;
    assert!(
        got_early.unwrap_or(false),
        "expected an 'early' progress delta before the command finished"
    );

    // Cancel the long command so the test does not wait the full 3s+.
    run.abort();
}

// ── Live output channel (realtime terminal view) ─────────────────────────

#[tokio::test]
async fn terminal_emits_raw_output_chunks_during_long_command() {
    // The dedicated output channel mirrors progress: raw chunks arrive
    // DURING the run (not only at completion), carrying the same text.
    let tool = terminal();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let ctx = ctx().with_output_sender(Some(tx));

    let run = tokio::spawn(async move {
        tool.execute(json!({ "command": "echo early; sleep 3; echo late" }), &ctx)
            .await
    });

    // Within 1.5s we should have seen the "early" raw chunk.
    let got_early = tokio::time::timeout(
        std::time::Duration::from_millis(1500),
        async {
            loop {
                match rx.recv().await {
                    Some(msg) if msg.contains("early") => return true,
                    Some(_) => continue,
                    None => return false,
                }
            }
        },
    )
    .await;
    assert!(
        got_early.unwrap_or(false),
        "expected an 'early' raw output chunk before the command finished"
    );

    run.abort();
}

#[tokio::test]
async fn terminal_output_channel_noop_without_sender() {
    // Backward compatibility: no output sender wired → the tool still works.
    let tool = terminal();
    let result = tool
        .execute(json!({ "command": "echo no_sender" }), &ctx())
        .await;
    let v = parse_result(&result.to_content_string());
    assert_eq!(v["exit_code"], json!(0));
    assert!(v["output"].as_str().unwrap().contains("no_sender"));
}

// ── T011: temp-file round-trip for > 4 KB output ─────────────────────────

#[tokio::test]
async fn terminal_full_output_available_for_large_output() {
    // ~20 KB of output — well above the 4 KB in-memory threshold, so the
    // capture spills to a temp file and is read back. The full output must
    // be present in the result (head AND tail).
    let tool = terminal();
    let result = tool
        .execute(json!({ "command": "seq 1 5000" }), &ctx())
        .await;
    let v = parse_result(&result.to_content_string());
    assert_eq!(v["exit_code"], json!(0));
    let out = v["output"].as_str().unwrap();
    assert!(out.contains("1\n"), "head present");
    assert!(
        out.contains("5000"),
        "tail present (proves full read-back from temp file): tail={:?}",
        out.chars().rev().take(40).collect::<String>()
    );
    // Every number 1..=5000 should appear — confirms no data was lost.
    let count = (1..=5000).filter(|n| out.contains(&n.to_string())).count();
    // Allow a little slack for substring overlaps (e.g. "1" in "10"), but the
    // count must be high — at minimum the distinct large numbers.
    assert!(count >= 4900, "expected ~5000 numbers present, got {count}");
}

// ── T013: interrupt cancels a long-running command promptly ──────────────

#[tokio::test]
async fn terminal_interrupt_cancels_long_command() {
    let tool = terminal();
    let flag = Arc::new(AtomicBool::new(false));
    let ctx = ctx().with_interrupt_flag(Some(flag.clone()));

    // Start a 30s command.
    let handle = {
        let tool = tool.clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            tool.execute(json!({ "command": "sleep 30" }), &ctx).await
        })
    };

    // Request an interrupt after ~1s.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    flag.store(true, Ordering::SeqCst);

    // The tool must return within ~5s of the interrupt — well under the
    // remaining ~29s of the command. This proves the streaming loop honored
    // the interrupt instead of blocking until process exit.
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("terminal tool did not return within 5s of interrupt")
        .expect("terminal task panicked");

    let v = parse_result(&result.to_content_string());
    assert!(
        v["output"]
            .as_str()
            .unwrap()
            .contains("[Command interrupted by user]"),
        "output should mark the interruption: {:?}",
        v["output"]
    );
}

#[tokio::test]
async fn terminal_without_interrupt_flag_runs_to_completion() {
    // No interrupt flag wired (backward-compatible path): is_interrupted()
    // reports false forever, so the command runs normally and is NOT cut off.
    let tool = terminal();
    let result = tool.execute(json!({ "command": "echo not_interrupted" }), &ctx()).await;
    let v = parse_result(&result.to_content_string());
    assert_eq!(v["exit_code"], json!(0));
    assert!(
        !v["output"].as_str().unwrap().contains("[Command interrupted by user]"),
        "no interrupt flag => command must complete normally"
    );
}

// ── T008 / Scenario 4: silent-command elapsed-time indicator ─────────────

#[tokio::test]
async fn terminal_silent_command_emits_elapsed_heartbeat() {
    // A command that produces NO output for several seconds should emit a
    // "running… Ns" heartbeat so the user knows it's still working.
    let tool = terminal();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let ctx = ctx().with_progress_sender(Some(tx));
    let result = tool.execute(json!({ "command": "sleep 4" }), &ctx).await;
    let v = parse_result(&result.to_content_string());
    assert_eq!(v["exit_code"], json!(0));

    let mut deltas = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        deltas.push(msg);
    }
    assert!(
        deltas.iter().any(|d| d.starts_with("running…") && d.contains('s')),
        "expected a 'running… Ns' heartbeat, got {:?}",
        deltas
    );
}
