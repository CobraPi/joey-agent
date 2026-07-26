//! Animation profile data registry (Constitution Principle II).
//!
//! See `specs/004-claude-code-cli-style/data-model.md` Entity 2 and
//! `contracts/render-animation-seam.md` Contract 4.
//!
//! Adding a new animation = adding one `AnimationKind` variant + one table
//! entry in `for_kind`. No central `match` with per-variant business logic.

use crate::capability::Capability;
use joey_core::theme::{Rgb, Theme};

/// Kinds of animated CLI elements. Each maps to one `AnimationProfile` via
/// `AnimationProfile::for_kind`.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnimationKind {
    Banner,
    ThinkingSpinner,
    StreamingCaret,
    ToolLine,
    PromptCaret,
}

/// A named set of parameters defining one animation. Pure data; looked up via
/// the registry below.
#[allow(dead_code)]
pub(crate) struct AnimationProfile {
    /// The glyph sequence cycled per tick.
    pub(crate) frames: &'static [&'static str],
    /// Ticks elapsed before advancing to the next frame (1 = every tick).
    pub(crate) interval_ticks: u32,
    /// Pantera theme color applied to the animating glyph.
    pub(crate) color: fn(&Theme) -> Rgb,
    /// Static status label rendered alongside (e.g. "Thinking…").
    pub(crate) label: Option<&'static str>,
    /// Plain-text rendering used under `Capability::NonInteractive`.
    pub(crate) disabled_fallback: &'static str,
}

impl AnimationProfile {
    /// Data-registry lookup. Returns a pre-built `&'static AnimationProfile`
    /// for the given kind. For `Reduced`, the returned profile already uses
    /// ASCII-safe frames and slower timing; for `NonInteractive`, callers use
    /// the `disabled_fallback` string and never render frames.
    pub(crate) fn for_kind(kind: AnimationKind, cap: Capability) -> &'static AnimationProfile {
        match cap {
            Capability::Full => full(kind),
            Capability::Reduced => reduced(kind),
            Capability::NonInteractive => non_interactive(kind),
        }
    }
}

// ---------------------------------------------------------------------------
// Full-capability profiles — Pantera colors, polished glyph sets.
// ---------------------------------------------------------------------------

fn full(kind: AnimationKind) -> &'static AnimationProfile {
    match kind {
        AnimationKind::Banner => &BANNER_FULL,
        AnimationKind::ThinkingSpinner => &SPINNER_FULL,
        AnimationKind::StreamingCaret => &CARET_FULL,
        AnimationKind::ToolLine => &TOOL_FULL,
        AnimationKind::PromptCaret => &PROMPT_FULL,
    }
}

// Reduced-capability profiles: ASCII-safe glyphs, slower intervals.
fn reduced(kind: AnimationKind) -> &'static AnimationProfile {
    match kind {
        AnimationKind::Banner => &BANNER_REDUCED,
        AnimationKind::ThinkingSpinner => &SPINNER_REDUCED,
        AnimationKind::StreamingCaret => &CARET_REDUCED,
        AnimationKind::ToolLine => &TOOL_REDUCED,
        AnimationKind::PromptCaret => &PROMPT_REDUCED,
    }
}

// NonInteractive: frames are never rendered; callers use `disabled_fallback`.
fn non_interactive(kind: AnimationKind) -> &'static AnimationProfile {
    match kind {
        AnimationKind::Banner => &BANNER_PLAIN,
        AnimationKind::ThinkingSpinner => &SPINNER_PLAIN,
        AnimationKind::StreamingCaret => &CARET_PLAIN,
        AnimationKind::ToolLine => &TOOL_PLAIN,
        AnimationKind::PromptCaret => &PROMPT_PLAIN,
    }
}

// --- Full ---
static BANNER_FULL: AnimationProfile = AnimationProfile {
    frames: &["⠁", "⠃", "⠋", "⠏", "⠹", "⠼"],
    interval_ticks: 1,
    color: |t| t.primary,
    label: None,
    disabled_fallback: "",
};
static SPINNER_FULL: AnimationProfile = AnimationProfile {
    frames: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
    interval_ticks: 1,
    color: |t| t.accent,
    label: Some("Thinking…"),
    disabled_fallback: "Thinking…",
};
static CARET_FULL: AnimationProfile = {
    // alternate the accent and most-subtle to blink the streaming caret
    static FRAMES: [&str; 2] = ["▌", " "];
    AnimationProfile {
        frames: &FRAMES,
        interval_ticks: 2,
        color: |t| t.accent,
        label: None,
        disabled_fallback: "",
    }
};
static TOOL_FULL: AnimationProfile = {
    static FRAMES: [&str; 3] = ["⠋", "⠙", "⠹"];
    AnimationProfile {
        frames: &FRAMES,
        interval_ticks: 1,
        color: |t| t.info,
        label: None,
        disabled_fallback: "",
    }
};
static PROMPT_FULL: AnimationProfile = {
    static FRAMES: [&str; 2] = ["❯", " "];
    AnimationProfile {
        frames: &FRAMES,
        interval_ticks: 6, // ~0.5s blink at 12fps
        color: |t| t.primary,
        label: None,
        disabled_fallback: "❯",
    }
};

// --- Reduced (ASCII-safe glyphs, slower intervals) ---
static BANNER_REDUCED: AnimationProfile = AnimationProfile {
    frames: &["*", "+", "*"],
    interval_ticks: 2,
    color: |t| t.primary,
    label: None,
    disabled_fallback: "",
};
static SPINNER_REDUCED: AnimationProfile = AnimationProfile {
    frames: &["|", "/", "-", "\\"],
    interval_ticks: 1,
    color: |t| t.accent,
    label: Some("Thinking…"),
    disabled_fallback: "Thinking…",
};
static CARET_REDUCED: AnimationProfile = {
    static FRAMES: [&str; 2] = ["_", " "];
    AnimationProfile {
        frames: &FRAMES,
        interval_ticks: 2,
        color: |t| t.accent,
        label: None,
        disabled_fallback: "",
    }
};
static TOOL_REDUCED: AnimationProfile = {
    static FRAMES: [&str; 3] = ["|", "/", "-"];
    AnimationProfile {
        frames: &FRAMES,
        interval_ticks: 1,
        color: |t| t.info,
        label: None,
        disabled_fallback: "",
    }
};
static PROMPT_REDUCED: AnimationProfile = {
    static FRAMES: [&str; 2] = [">", " "];
    AnimationProfile {
        frames: &FRAMES,
        interval_ticks: 6,
        color: |t| t.primary,
        label: None,
        disabled_fallback: ">",
    }
};

// --- NonInteractive (frames never rendered; disabled_fallback is the content) ---
static BANNER_PLAIN: AnimationProfile = AnimationProfile {
    frames: &[],
    interval_ticks: 0,
    color: |t| t.fg_base,
    label: None,
    disabled_fallback: "",
};
static SPINNER_PLAIN: AnimationProfile = AnimationProfile {
    frames: &[],
    interval_ticks: 0,
    color: |t| t.fg_base,
    label: Some("Thinking…"),
    disabled_fallback: "Thinking…",
};
static CARET_PLAIN: AnimationProfile = AnimationProfile {
    frames: &[],
    interval_ticks: 0,
    color: |t| t.fg_base,
    label: None,
    disabled_fallback: "",
};
static TOOL_PLAIN: AnimationProfile = AnimationProfile {
    frames: &[],
    interval_ticks: 0,
    color: |t| t.fg_base,
    label: None,
    disabled_fallback: "",
};
static PROMPT_PLAIN: AnimationProfile = AnimationProfile {
    frames: &[],
    interval_ticks: 0,
    color: |t| t.fg_base,
    label: None,
    disabled_fallback: ">",
};

#[cfg(test)]
mod tests {
    use super::*;
    use joey_core::theme::Theme;

    fn theme() -> Theme {
        Theme::pantera()
    }

    #[test]
    fn full_profiles_have_nonempty_frames() {
        let t = theme();
        for kind in [
            AnimationKind::Banner,
            AnimationKind::ThinkingSpinner,
            AnimationKind::StreamingCaret,
            AnimationKind::ToolLine,
            AnimationKind::PromptCaret,
        ] {
            let p = AnimationProfile::for_kind(kind, Capability::Full);
            assert!(!p.frames.is_empty(), "Full profile for {:?} has empty frames", kind);
            // color closure must resolve to a real Pantera color.
            let _ = (p.color)(&t);
        }
    }

    #[test]
    fn reduced_profiles_use_only_ascii_safe_glyphs() {
        for kind in [
            AnimationKind::Banner,
            AnimationKind::ThinkingSpinner,
            AnimationKind::StreamingCaret,
            AnimationKind::ToolLine,
            AnimationKind::PromptCaret,
        ] {
            let p = AnimationProfile::for_kind(kind, Capability::Reduced);
            assert!(!p.frames.is_empty(), "Reduced profile for {:?} has empty frames", kind);
            for frame in p.frames {
                for ch in frame.chars() {
                    assert!(
                        ch.is_ascii(),
                        "Reduced profile for {:?} contains non-ASCII glyph {:?}",
                        kind,
                        ch
                    );
                }
            }
        }
    }

    #[test]
    fn noninteractive_profiles_have_disabled_fallback_or_empty() {
        // Under NonInteractive, frames are never rendered; the caller uses
        // disabled_fallback. The profile must still be addressable.
        for kind in [
            AnimationKind::Banner,
            AnimationKind::ThinkingSpinner,
            AnimationKind::StreamingCaret,
            AnimationKind::ToolLine,
            AnimationKind::PromptCaret,
        ] {
            let _ = AnimationProfile::for_kind(kind, Capability::NonInteractive);
        }
    }

    #[test]
    fn thinking_spinner_label_is_thinking() {
        let p = AnimationProfile::for_kind(AnimationKind::ThinkingSpinner, Capability::Full);
        assert_eq!(p.label, Some("Thinking…"));
    }

    #[test]
    fn thinking_spinner_color_is_pantera_accent() {
        let t = theme();
        let p = AnimationProfile::for_kind(AnimationKind::ThinkingSpinner, Capability::Full);
        assert_eq!((p.color)(&t), t.accent);
    }
}
