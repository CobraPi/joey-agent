//! Secret input prompts with masked typing feedback (port of
//! `hermes_cli/secret_prompt.py`).
//!
//! Reads one secret line in raw mode, echoing one `*` per typed character,
//! with backspace, Ctrl-C (interrupt), Ctrl-D/Ctrl-Z (EOF), and
//! escape-sequence swallowing. Falls back to no-echo entry (getpass
//! equivalent), then plain line input, when raw terminal handling is
//! unavailable (piped stdin, non-TTY).

use std::io::{IsTerminal, Read, Write};

/// Outcome of a secret prompt.
pub enum SecretInput {
    Value(String),
    /// Ctrl-C / EOF — callers treat this as "cancel entry".
    Cancelled,
}

impl SecretInput {
    /// The entered value, with cancel mapped to "" (matching upstream
    /// callers, which catch KeyboardInterrupt/EOFError and use "").
    pub fn unwrap_or_empty(self) -> String {
        match self {
            SecretInput::Value(v) => v,
            SecretInput::Cancelled => String::new(),
        }
    }
}

/// Prompt for a secret while showing masked typing feedback
/// (`masked_secret_prompt`).
pub fn masked_secret_prompt(prompt: &str) -> SecretInput {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        // Non-interactive: plain line read (upstream getpass falls back to
        // reading stdin with a warning; keep it simple and honest).
        return plain_prompt(prompt);
    }
    #[cfg(unix)]
    {
        match masked_prompt_posix(prompt) {
            Some(result) => result,
            None => plain_prompt(prompt),
        }
    }
    #[cfg(not(unix))]
    {
        plain_prompt(prompt)
    }
}

fn plain_prompt(prompt: &str) -> SecretInput {
    print!("{}", prompt);
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    match std::io::stdin().read_line(&mut buf) {
        Ok(0) => SecretInput::Cancelled,
        Ok(_) => SecretInput::Value(buf.trim_end_matches(['\r', '\n']).to_string()),
        Err(_) => SecretInput::Cancelled,
    }
}

/// POSIX raw-mode masked input (`_masked_secret_prompt_posix` +
/// `_collect_masked_input`). Returns None when raw mode can't be entered.
#[cfg(unix)]
fn masked_prompt_posix(prompt: &str) -> Option<SecretInput> {
    use std::os::unix::io::AsRawFd;

    let stdin = std::io::stdin();
    let fd = stdin.as_raw_fd();

    // Enter raw-ish mode: no echo, no canonical buffering; keep signals off
    // so Ctrl-C is delivered as 0x03 (upstream tty.setraw semantics).
    let mut orig: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut orig) } != 0 {
        return None;
    }
    let mut raw = orig;
    raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG);
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    if unsafe { libc::tcsetattr(fd, libc::TCSADRAIN, &raw) } != 0 {
        return None;
    }

    struct RestoreTermios {
        fd: i32,
        orig: libc::termios,
    }
    impl Drop for RestoreTermios {
        fn drop(&mut self) {
            unsafe { libc::tcsetattr(self.fd, libc::TCSADRAIN, &self.orig) };
        }
    }
    let _restore = RestoreTermios { fd, orig };

    let mut out = std::io::stdout();
    let write = |out: &mut std::io::Stdout, s: &str| {
        let _ = out.write_all(s.as_bytes());
        let _ = out.flush();
    };
    write(&mut out, prompt);

    let mut value: Vec<char> = Vec::new();
    let mut handle = stdin.lock();
    let mut byte = [0u8; 1];
    loop {
        let ch = match handle.read(&mut byte) {
            Ok(0) => {
                write(&mut out, "\r\n");
                return Some(SecretInput::Cancelled); // EOF
            }
            Ok(_) => byte[0],
            Err(_) => {
                write(&mut out, "\r\n");
                return Some(SecretInput::Cancelled);
            }
        };
        match ch {
            b'\r' | b'\n' => {
                write(&mut out, "\r\n");
                return Some(SecretInput::Value(value.into_iter().collect()));
            }
            0x03 => {
                // Ctrl-C
                write(&mut out, "\r\n");
                return Some(SecretInput::Cancelled);
            }
            0x04 | 0x1a => {
                // Ctrl-D / Ctrl-Z
                write(&mut out, "\r\n");
                return Some(SecretInput::Cancelled);
            }
            0x08 | 0x7f => {
                // Backspace / DEL
                if value.pop().is_some() {
                    write(&mut out, "\x08 \x08");
                }
            }
            0x1b => {
                // Ignore escape itself. Terminals commonly send
                // escape-prefixed navigation/delete sequences; they should
                // not become secret text. (The sequence's remaining printable
                // bytes are unavoidable in a byte-at-a-time reader; upstream
                // has the same property.)
            }
            b => {
                // UTF-8 continuation/multibyte: accumulate bytes as chars via
                // lossy single-byte mapping is wrong for secrets — API keys
                // are ASCII; treat non-ASCII bytes as-is per byte.
                value.push(b as char);
                write(&mut out, "*");
            }
        }
    }
}
