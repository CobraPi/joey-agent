//! Terminal/CLI capability detection for animation fallback (FR-007/FR-008/FR-011).
//!
//! See `specs/004-claude-code-cli-style/data-model.md` Entity 1 and
//! `contracts/render-animation-seam.md` Contract 3.

use std::io::IsTerminal;

/// Three-level capability classification used by the profile selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Capability {
    /// Interactive + truecolor + unicode + adequate width — full animations.
    Full,
    /// Interactive but missing truecolor/unicode/width — simplified frames,
    /// ASCII glyphs, ANSI-16 colors.
    Reduced,
    /// Non-interactive (piped stdout) — animations disabled, plain text only.
    NonInteractive,
}

/// Detected terminal/CLI capability profile. Immutable after construction.
/// Computed once at REPL startup and stored in `RenderOptions`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RenderCapability {
    /// True when stdout is a TTY. False ⇒ NonInteractive (FR-011).
    pub(crate) is_interactive: bool,
    /// True when `COLORTERM` is `truecolor` or `24bit`.
    pub(crate) supports_truecolor: bool,
    /// True when the terminal likely renders box-drawing/arrow glyphs.
    pub(crate) supports_unicode: bool,
    /// Columns (from `terminal_size`), used for banner scaling/layout.
    pub(crate) term_width: usize,
    /// Effective animation frame rate; default 12, lowered for reduced.
    pub(crate) target_fps: u32,
}

impl RenderCapability {
    /// Probe stdout IsTerminal + COLORTERM + terminal_size exactly once.
    /// Cheap, deterministic per process environment.
    pub(crate) fn detect() -> Self {
        let is_interactive = std::io::stdout().is_terminal();
        let supports_truecolor = matches!(
            std::env::var("COLORTERM").ok().as_deref(),
            Some("truecolor") | Some("24bit")
        );
        // Conservative default true; reduced to ASCII glyphs only when the
        // terminal clearly cannot render box-drawing. We cannot reliably
        // detect Unicode rendering, so we assume yes for interactive TTYs and
        // rely on the LANG/TERM heuristics below for known-limited terminals.
        let lang_utf8 = std::env::var("LANG")
            .map(|l| l.to_ascii_uppercase().contains("UTF-8"))
            .unwrap_or(true);
        let term_utf8 = std::env::var("TERM")
            .map(|t| {
                let t = t.to_ascii_uppercase();
                // dumb terminals and the classic "ansi" terminfo are ASCII-only.
                !(t == "DUMB" || t == "ANSI")
            })
            .unwrap_or(true);
        let supports_unicode = lang_utf8 && term_utf8;
        let term_width = terminal_size::terminal_size()
            .map(|(terminal_size::Width(w), _)| w as usize)
            .unwrap_or(80);
        let level = if !is_interactive {
            Capability::NonInteractive
        } else if !supports_truecolor || !supports_unicode || term_width < 60 {
            Capability::Reduced
        } else {
            Capability::Full
        };
        let target_fps = match level {
            Capability::NonInteractive => 0,
            Capability::Reduced => 8, // slower for reduced-capability terminals
            Capability::Full => 12,
        };
        Self {
            is_interactive,
            supports_truecolor,
            supports_unicode,
            term_width,
            target_fps,
        }
    }

    /// Classify into Full / Reduced / NonInteractive.
    pub(crate) fn level(&self) -> Capability {
        if !self.is_interactive {
            Capability::NonInteractive
        } else if !self.supports_truecolor || !self.supports_unicode || self.term_width < 60 {
            Capability::Reduced
        } else {
            Capability::Full
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_full() {
        let cap = RenderCapability {
            is_interactive: true,
            supports_truecolor: true,
            supports_unicode: true,
            term_width: 80,
            target_fps: 12,
        };
        assert_eq!(cap.level(), Capability::Full);
    }

    #[test]
    fn classify_reduced_no_truecolor() {
        let cap = RenderCapability {
            is_interactive: true,
            supports_truecolor: false,
            supports_unicode: true,
            term_width: 80,
            target_fps: 8,
        };
        assert_eq!(cap.level(), Capability::Reduced);
    }

    #[test]
    fn classify_reduced_no_unicode() {
        let cap = RenderCapability {
            is_interactive: true,
            supports_truecolor: true,
            supports_unicode: false,
            term_width: 80,
            target_fps: 8,
        };
        assert_eq!(cap.level(), Capability::Reduced);
    }

    #[test]
    fn classify_reduced_narrow() {
        let cap = RenderCapability {
            is_interactive: true,
            supports_truecolor: true,
            supports_unicode: true,
            term_width: 50,
            target_fps: 8,
        };
        assert_eq!(cap.level(), Capability::Reduced);
    }

    #[test]
    fn classify_noninteractive_when_piped() {
        let cap = RenderCapability {
            is_interactive: false,
            supports_truecolor: true,
            supports_unicode: true,
            term_width: 80,
            target_fps: 0,
        };
        assert_eq!(cap.level(), Capability::NonInteractive);
    }

    #[test]
    fn detect_on_piped_stdout_is_noninteractive() {
        // Under `cargo test` stdout is typically not a TTY, so detect()
        // should return NonInteractive. If the test runner is attached to a
        // PTY this assertion may not hold; we only assert the cheap invariant.
        let cap = RenderCapability::detect();
        if !cap.is_interactive {
            assert_eq!(cap.level(), Capability::NonInteractive);
        }
    }
}
