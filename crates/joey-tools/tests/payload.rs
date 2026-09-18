//! US7 — pass-by-reference payloads (feature 034, quickstart.md US7,
//! contracts/tool-schemas.md).

use std::sync::Arc;

use joey_tools::{Tool, ToolContext, ToolRegistry, ToolResult};
use serde_json::{json, Value};

// ── Test harness ──────────────────────────────────────────────────────────

fn lock() -> std::sync::MutexGuard<'static, ()> {
    joey_core::constants::TEST_HOME_OVERRIDE_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Home override with the lock held for the guard's whole lifetime.
/// Field drop order matters: fields drop in DECLARATION order, so the
/// guard is declared first — it restores the home override BEFORE the
/// lock is released (no window where another test holds the lock while
/// this override is still active).
struct Home {
    _guard: joey_core::constants::HomeOverrideGuard,
    _lock: std::sync::MutexGuard<'static, ()>,
    dir: tempfile::TempDir,
}

fn home() -> Home {
    let lock = lock();
    let dir = tempfile::tempdir().unwrap();
    let guard = joey_core::constants::HomeOverrideGuard::new(dir.path().to_path_buf());
    Home { _guard: guard, _lock: lock, dir }
}

fn ctx(home: &Home, session: &str) -> ToolContext {
    ToolContext::new(home.dir.path().to_path_buf(), joey_core::Config::defaults(), session)
}

fn tool(name: &str) -> Arc<dyn Tool> {
    ToolRegistry::with_builtins()
        .get(name)
        .unwrap_or_else(|| panic!("{name} tool registered"))
}

async fn run(tool: Arc<dyn Tool>, args: Value, ctx: &ToolContext) -> ToolResult {
    tool.execute(args, ctx).await
}

/// Parse a Text/ToolResult payload as JSON; panic with the raw content on
/// failure. Error results render as {"error": "..."} via to_content_string,
/// so both shapes parse.
fn parse(result: &ToolResult) -> Value {
    let s = result.to_content_string();
    serde_json::from_str(&s)
        .unwrap_or_else(|e| panic!("result is not valid JSON: {e}\n---\n{s}\n---"))
}

fn error_text(v: &Value) -> String {
    v.get("error")
        .and_then(|e| e.as_str())
        .map(str::to_string)
        .unwrap_or_default()
}

// ── terminal: XOR validation ──────────────────────────────────────────────

#[tokio::test]
async fn terminal_xor_neither_errors() {
    let h = home();
    let c = ctx(&h, "s1");
    let v = parse(&run(tool("terminal"), json!({}), &c).await);
    assert_eq!(
        error_text(&v),
        "terminal: provide exactly one of 'command' or 'command_path'"
    );
    assert_eq!(v["exit_code"], json!(-1));
    assert_eq!(v["output"], json!(""));
    assert_eq!(v["status"], json!("error"));
}

#[tokio::test]
async fn terminal_xor_both_errors() {
    let h = home();
    let c = ctx(&h, "s1");
    let v = parse(&run(
        tool("terminal"),
        json!({"command": "echo hi", "command_path": "/tmp/x"}),
        &c,
    )
    .await);
    assert!(error_text(&v).contains("exactly one of 'command' or 'command_path'"), "{v}");
}

// ── terminal: inline persistence + path round-trip ───────────────────────

#[tokio::test]
async fn terminal_inline_persists_and_returns_path() {
    let h = home();
    let c = ctx(&h, "s1");
    let v = parse(&run(tool("terminal"), json!({"command": "echo hello-payload"}), &c).await);
    assert_eq!(v["exit_code"], json!(0), "{v}");
    let path = v["command_path"].as_str().expect("command_path in result").to_string();
    assert!(std::path::Path::new(&path).is_file(), "payload file exists at {path}");
    assert!(
        path.starts_with(h.dir.path().join("payloads").to_str().unwrap()),
        "payload lives under the session payloads dir: {path}"
    );

    // Round-trip: re-invoke with the returned path.
    let v2 = parse(&run(tool("terminal"), json!({"command_path": path}), &c).await);
    assert_eq!(v2["exit_code"], json!(0), "{v2}");
    assert!(v2["output"].as_str().unwrap().contains("hello-payload"), "{v2}");
}

#[tokio::test]
async fn terminal_path_rereads_edited_content() {
    let h = home();
    let c = ctx(&h, "s1");
    let v = parse(&run(tool("terminal"), json!({"command": "echo v1"}), &c).await);
    let path = v["command_path"].as_str().expect("command_path").to_string();
    std::fs::write(&path, "echo v2").unwrap();
    let v2 = parse(&run(tool("terminal"), json!({"command_path": path}), &c).await);
    assert_eq!(v2["exit_code"], json!(0), "{v2}");
    let out = v2["output"].as_str().unwrap();
    assert!(out.contains("v2") && !out.contains("v1"), "output should reflect edited content: {out}");
}

#[tokio::test]
async fn terminal_missing_path_file_errors() {
    let h = home();
    let c = ctx(&h, "s1");
    let v = parse(&run(
        tool("terminal"),
        json!({"command_path": "/definitely/not/a/real/path/x"}),
        &c,
    )
    .await);
    let e = error_text(&v);
    assert!(e.contains("command_path"), "error names command_path: {e}");
    assert!(e.contains("unreadable"), "error says unreadable: {e}");
}

#[tokio::test]
async fn terminal_prior_session_reference_copies() {
    let h = home();
    let c1 = ctx(&h, "s1");
    let v = parse(&run(tool("terminal"), json!({"command": "echo cross-session"}), &c1).await);
    let p = v["command_path"].as_str().expect("command_path").to_string();

    // New session referencing s1's payload: copy-into-current-session, then
    // the copy executes.
    let c2 = ctx(&h, "s2");
    let v2 = parse(&run(tool("terminal"), json!({"command_path": p}), &c2).await);
    assert_eq!(v2["exit_code"], json!(0), "{v2}");
    assert!(v2["output"].as_str().unwrap().contains("cross-session"), "{v2}");

    // s1's original still exists; a copy now exists under s2's payloads dir.
    // Dir naming grammar (payload_store): <sanitize(session)>-<fnv1a64 hex>
    // — "s1"/"s2" sanitize to themselves, so match on the prefix.
    assert!(std::path::Path::new(&p).exists(), "original payload preserved");
    let name = std::path::Path::new(&p).file_name().unwrap().to_str().unwrap().to_string();
    let s2_copy = std::fs::read_dir(h.dir.path().join("payloads"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir() && p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("s2-")))
        .find(|d| d.join(&name).exists());
    assert!(s2_copy.is_some(), "copy of payload exists under s2's payloads dir");
}

// ── write_file: content_path ──────────────────────────────────────────────

#[tokio::test]
async fn write_file_content_path_roundtrip() {
    let h = home();
    let c = ctx(&h, "s1");
    let out1 = h.dir.path().join("out1.txt");
    let v = parse(&run(
        tool("write_file"),
        json!({"path": out1.to_str().unwrap(), "content": "payload-body"}),
        &c,
    )
    .await);
    let ppath = v["content_path"].as_str().expect("content_path in result").to_string();
    assert!(std::path::Path::new(&ppath).is_file(), "payload persisted at {ppath}");
    assert_eq!(std::fs::read_to_string(&out1).unwrap(), "payload-body");

    // Edit the payload on disk; content_path re-reads current content (FR-013).
    std::fs::write(&ppath, "edited-body").unwrap();
    let out2 = h.dir.path().join("out2.txt");
    let v2 = parse(&run(
        tool("write_file"),
        json!({"path": out2.to_str().unwrap(), "content_path": ppath}),
        &c,
    )
    .await);
    assert!(v2.get("error").is_none() || v2["error"].is_null(), "{v2}");
    assert_eq!(std::fs::read_to_string(&out2).unwrap(), "edited-body");
}

#[tokio::test]
async fn write_file_xor_both_errors() {
    let h = home();
    let c = ctx(&h, "s1");
    let r = run(
        tool("write_file"),
        json!({"path": "/tmp/never.txt", "content": "a", "content_path": "/tmp/y"}),
        &c,
    )
    .await;
    let v = parse(&r);
    let e = error_text(&v);
    assert_eq!(e, "write_file: provide exactly one of 'content' or 'content_path'");
}

#[tokio::test]
async fn write_file_xor_neither_errors() {
    let h = home();
    let c = ctx(&h, "s1");
    let v = parse(&run(tool("write_file"), json!({"path": "/tmp/never.txt"}), &c).await);
    assert_eq!(
        error_text(&v),
        "write_file: provide exactly one of 'content' or 'content_path'"
    );
}

// ── browser_cdp: params_path ──────────────────────────────────────────────

#[tokio::test]
async fn browser_cdp_params_path_both_errors() {
    let h = home();
    let c = ctx(&h, "s1");
    let r = run(
        tool("browser_cdp"),
        json!({"method": "Runtime.evaluate", "params": {"expression": "1"}, "params_path": "/tmp/z"}),
        &c,
    )
    .await;
    // ToolResult::Error("browser_cdp: provide at most one of 'params' or 'params_path'")
    let v = parse(&r);
    let e = error_text(&v);
    assert_eq!(e, "browser_cdp: provide at most one of 'params' or 'params_path'");
}

#[tokio::test]
async fn browser_cdp_bad_params_path_json_errors() {
    let h = home();
    let c = ctx(&h, "s1");
    let bad = h.dir.path().join("bad-params.json");
    std::fs::write(&bad, "not json").unwrap();
    let v = parse(&run(
        tool("browser_cdp"),
        json!({"method": "X", "params_path": bad.to_str().unwrap()}),
        &c,
    )
    .await);
    let e = error_text(&v);
    assert!(e.contains("not a JSON object"), "error mentions JSON object: {e}");
}

// ── parameters() schema shape ─────────────────────────────────────────────

#[test]
fn terminal_parameters_shape() {
    let h = home();
    let _ = ctx(&h, "schema");
    let p = tool("terminal").parameters();
    assert_eq!(p["required"], json!([]));
    assert!(p["properties"].get("command_path").is_some(), "command_path property present");
    assert_eq!(
        p["properties"]["command_path"]["description"],
        "Path to a persisted command script; the file's current content is used as the command. Provide exactly one of command or command_path."
    );
    assert_eq!(
        p["properties"]["command"]["description"],
        "The command to execute on the VM (or provide command_path instead — exactly one of the two)"
    );
}

#[test]
fn write_file_parameters_shape() {
    let h = home();
    let _ = ctx(&h, "schema");
    let p = tool("write_file").parameters();
    assert_eq!(p["required"], json!(["path"]));
    assert!(p["properties"].get("content_path").is_some(), "content_path property present");
    assert_eq!(
        p["properties"]["content_path"]["description"],
        "Path to a persisted payload file; its current content is written. Provide exactly one of content or content_path."
    );
    assert_eq!(
        p["properties"]["content"]["description"],
        "Complete content to write to the file (or provide content_path instead — exactly one of the two)"
    );
}

#[test]
fn browser_cdp_parameters_shape() {
    let h = home();
    let _ = ctx(&h, "schema");
    let p = tool("browser_cdp").parameters();
    assert_eq!(p["required"], json!(["method"]));
    assert!(p["properties"].get("params_path").is_some(), "params_path property present");
    assert_eq!(
        p["properties"]["params_path"]["description"],
        "Path to a persisted params JSON file; its current content is parsed as the CDP params object."
    );
}
