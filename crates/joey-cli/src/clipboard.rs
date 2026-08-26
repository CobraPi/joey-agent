//! Clipboard helpers for the CLI and TUI surfaces (feature: TUI text copy).
//!
//! Strategy, most-compatible first:
//! 1. Native clipboard tool when present (`pbcopy` / `xclip` / `wl-copy`) —
//!    same chain the CLI `/copy` already used.
//! 2. OSC 52 escape sequence fallback — the terminal emulator itself copies
//!    to the LOCAL clipboard, which also works over SSH. Base64-encoded
//!    payload, sized defensively (many terminals cap ~100KB).
//!
//! In the TUI, mouse capture is enabled for scrolling, which intercepts
//! native click-drag text selection — these explicit copy paths (plus the
//! terminal's Shift-held native selection, which bypasses capture in most
//! emulators) restore copy functionality.
//!
//! T006 (specs/018 US1): the native-tool path runs the child via
//! `tokio::process` (`spawn` + async stdin write + `wait().await`), so a
//! clipboard copy never blocks a runtime worker thread while `pbcopy` &
//! friends run. The old sync entry point remains as a thin shim for the
//! existing sync callers (TUI pump, REPL `/copy`).

use std::io::Write;

/// Copy `text` to the clipboard. Returns a human-readable success or error
/// message suitable for a notice/transcript line.
///
/// Blocking compatibility shim over [`copy_to_clipboard_async`]. Safe to
/// call from sync code with or without an ambient tokio runtime (including
/// from inside async contexts such as the TUI pump, where
/// `Handle::block_on` would panic): when a runtime is ambient we drive the
/// async future on a dedicated thread with its own current-thread runtime.
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    if tokio::runtime::Handle::try_current().is_err() {
        // No ambient runtime: build a short-lived one and block on it.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("clipboard runtime build failed: {e}"))?;
        return rt.block_on(copy_to_clipboard_async(text));
    }
    // Ambient runtime (likely on a worker thread): `Handle::block_on` is not
    // allowed there, so run the future on a dedicated thread instead.
    let text = text.to_string();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("clipboard runtime build failed: {e}"))
            .and_then(|rt| rt.block_on(copy_to_clipboard_async(&text)))
    })
    .join()
    .unwrap_or_else(|_| Err("clipboard copy thread panicked".to_string()))
}

/// Async clipboard copy (T006): spawns the first available native clipboard
/// tool via `tokio::process`, writes the payload through the piped stdin
/// asynchronously, and reaps the child with `wait().await` — the calling
/// task yields instead of blocking its OS thread.
pub async fn copy_to_clipboard_async(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Err("nothing to copy".to_string());
    }

    // 1. Native clipboard tool chain.
    let candidates: &[(&str, &[&str])] = &[
        ("pbcopy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("wl-copy", &[]),
    ];
    for (cmd, args) in candidates {
        if which::which(cmd).is_err() {
            continue;
        }
        let mut child = match tokio::process::Command::new(cmd)
            .args(*args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Some(mut stdin) = child.stdin.take() {
            // Async write; large payloads yield to the runtime instead of
            // stalling the thread. Dropping `stdin` closes the pipe so the
            // child sees EOF before we `wait()`.
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(text.as_bytes()).await;
            let _ = stdin.shutdown().await;
        }
        let _ = child.wait().await;
        return Ok(());
    }

    // 2. OSC 52 fallback: write the escape sequence to stderr (never stdout —
    // stdout may be a pipe in oneshot mode; the TUI's alternate screen uses
    // the same tty). Most modern terminals (iTerm2, kitty, alacritty,
    // WezTerm, Windows Terminal, macOS Terminal with enabled preference)
    // honor it, including over SSH.
    osc52_copy(text).then_some(()).ok_or_else(|| {
        "no clipboard command (pbcopy/xclip/wl-copy) and OSC 52 unavailable".to_string()
    })
}

/// Emit an OSC 52 copy sequence to the controlling terminal.
fn osc52_copy(text: &str) -> bool {
    use base64::Engine;
    // Guard: very large payloads can hang some terminals.
    if text.len() > 100_000 {
        return false;
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    let seq = format!("\x1b]52;c;{}\x07", b64);
    // stderr is the tty-adjacent stream that survives alternate-screen mode.
    let mut err = std::io::stderr();
    let _ = err.write_all(seq.as_bytes());
    let _ = err.flush();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_empty_fails_cleanly() {
        assert!(copy_to_clipboard("").is_err());
    }

    #[test]
    fn copy_nonempty_succeeds_or_reports_error() {
        // In a test env there may be no clipboard tool and no tty — either a
        // clean Ok or a descriptive Err is acceptable; it must not panic.
        let _ = copy_to_clipboard("hello from test");
    }

    #[tokio::test]
    async fn async_copy_empty_fails_cleanly() {
        assert!(copy_to_clipboard_async("").await.is_err());
    }

    // T006 non-blocking proof #1 — structural: runs the exact
    // spawn → piped-stdin async write → `wait().await` shape used by
    // `copy_to_clipboard_async` against `sleep`, so it passes on hosts with
    // NO clipboard tool at all. A sibling ticker task must keep advancing
    // while the child is being waited on: with a blocking `wait()` (or a
    // blocking stdin write) the current-thread test runtime could never
    // poll the ticker and the counter would stay near zero.
    #[cfg(unix)]
    #[tokio::test]
    async fn clipboard_spawn_wait_structure_is_non_blocking() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::time::Duration;
        use tokio::io::AsyncWriteExt;

        let ticks = Arc::new(AtomicUsize::new(0));
        let ticker = {
            let ticks = ticks.clone();
            tokio::spawn(async move {
                let mut iv = tokio::time::interval(Duration::from_millis(1));
                loop {
                    iv.tick().await;
                    ticks.fetch_add(1, Ordering::SeqCst);
                }
            })
        };

        let mut child = tokio::process::Command::new("sleep")
            .arg("0.1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn sleep");
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(b"joey clipboard non-blocking probe").await;
            let _ = stdin.shutdown().await;
        }
        let status = child.wait().await.expect("wait for sleep");
        assert!(status.success(), "sleep child should exit cleanly");

        let advanced = ticks.load(Ordering::SeqCst);
        ticker.abort();
        assert!(
            advanced >= 10,
            "ticker only advanced {advanced} times during the ~100ms child wait — \
             wait()/write must be blocking the runtime",
        );
    }

    // T006 non-blocking proof #2 — full round-trip through the real
    // helper when a native clipboard tool exists; skips gracefully (with a
    // note) on hosts that have none, where proof #1 covers the structure.
    #[tokio::test]
    async fn async_copy_round_trip_does_not_block_sibling_tasks() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        let has_tool = ["pbcopy", "xclip", "wl-copy"]
            .iter()
            .any(|c| which::which(c).is_ok());
        if !has_tool {
            eprintln!("skipping: no clipboard tool (pbcopy/xclip/wl-copy) on this host");
            return;
        }

        let ticks = Arc::new(AtomicUsize::new(0));
        let ticker = {
            let ticks = ticks.clone();
            tokio::spawn(async move {
                let mut iv = tokio::time::interval(Duration::from_millis(1));
                loop {
                    iv.tick().await;
                    ticks.fetch_add(1, Ordering::SeqCst);
                }
            })
        };

        let res = copy_to_clipboard_async("joey clipboard non-blocking proof").await;
        let advanced = ticks.load(Ordering::SeqCst);
        ticker.abort();

        assert!(res.is_ok(), "native-tool copy path should succeed: {res:?}");
        assert!(
            advanced >= 1,
            "sibling ticker made no progress during the clipboard copy — \
             the copy path appears to block the runtime",
        );
    }
}
