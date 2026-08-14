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

use std::io::Write;

/// Copy `text` to the clipboard. Returns a human-readable success or error
/// message suitable for a notice/transcript line.
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
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
        let mut child = match std::process::Command::new(cmd)
            .args(*args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
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
    #[test]
    fn copy_empty_fails_cleanly() {
        assert!(super::copy_to_clipboard("").is_err());
    }

    #[test]
    fn copy_nonempty_succeeds_or_reports_error() {
        // In a test env there may be no clipboard tool and no tty — either a
        // clean Ok or a descriptive Err is acceptable; it must not panic.
        let _ = super::copy_to_clipboard("hello from test");
    }
}
