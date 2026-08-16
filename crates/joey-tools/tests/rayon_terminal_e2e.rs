// E2E: exercise the terminal tool's rayon post-processing path for real.
use joey_tools::{Tool, ToolContext};
use serde_json::json;

#[tokio::test]
async fn terminal_postprocessing_pipeline_e2e() {
    let ctx = ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "rayon-e2e");
    let tool = joey_tools::ToolRegistry::with_builtins().get("terminal").unwrap();

    // Colored output exercises the ANSI strip; a fake key exercises redaction.
    let r = tool
        .execute(
            json!({"command": "printf '\\033[32mgreen\\033[0m\\n'; echo 'OPENAI_API_KEY=sk-proj-abcdefghijklmnopqrstuv'"}),
            &ctx,
        )
        .await;
    let v: serde_json::Value = serde_json::from_str(&r.to_content_string()).unwrap();
    let out = v["output"].as_str().unwrap();
    assert!(out.contains("green"), "content survives: {out}");
    assert!(!out.contains("\x1b"), "ANSI stripped");
    assert!(!out.contains("sk-proj-abcdefghijklmnopqrstuv"), "secret redacted: {out}");
    assert_eq!(v["exit_code"], 0);
}

#[tokio::test]
async fn terminal_large_output_streams_and_postprocesses() {
    // 30k lines (~700KB) — enough to cross both parallel thresholds.
    let ctx = ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "rayon-e2e-big");
    let tool = joey_tools::ToolRegistry::with_builtins().get("terminal").unwrap();
    let r = tool
        .execute(json!({"command": "seq 1 30000", "timeout": 60}), &ctx)
        .await;
    let v: serde_json::Value = serde_json::from_str(&r.to_content_string()).unwrap();
    let out = v["output"].as_str().unwrap();
    assert!(out.contains("1\n"), "head present");
    assert!(out.contains("30000"), "tail present");
    assert_eq!(v["exit_code"], 0);
}
