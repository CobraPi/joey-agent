//! Per-element runtime animation state, advanced each tick by the central
//! tick loop in `render_turn` (FR-010).
//!
//! See `specs/004-claude-code-cli-style/data-model.md` Entity 3 and
//! `contracts/render-animation-seam.md` Contract 5.

use std::time::Instant;

use crate::profile::{AnimationKind, AnimationProfile};

/// Mutable, per-element runtime state of an active animation. Advanced each
/// tick by the single tick loop in `render_turn`.
#[allow(dead_code)]
pub(crate) struct AnimationState {
    pub(crate) kind: AnimationKind,
    /// Current index into `profile.frames`.
    pub(crate) frame_idx: usize,
    /// Countdown until `frame_idx` advances (reset to `interval_ticks`).
    pub(crate) ticks_to_next_frame: u32,
    /// Whether this animation is currently active.
    pub(crate) running: bool,
    /// Wall-clock start, for duration display (tool line, turn summary).
    pub(crate) started_at: Option<Instant>,
}

impl AnimationState {
    pub(crate) fn new(kind: AnimationKind, _now: Instant) -> Self {
        Self {
            kind,
            frame_idx: 0,
            ticks_to_next_frame: 1,
            running: true,
            started_at: Some(_now),
        }
    }

    /// Called per tick. Decrements countdown; at zero, increments `frame_idx`
    /// mod `frames.len()` and resets the countdown.
    pub(crate) fn advance(&mut self, profile: &AnimationProfile) {
        if profile.frames.is_empty() {
            return;
        }
        if self.ticks_to_next_frame > 1 {
            self.ticks_to_next_frame -= 1;
            return;
        }
        // countdown elapsed → advance and reset
        self.frame_idx = (self.frame_idx + 1) % profile.frames.len();
        self.ticks_to_next_frame = profile.interval_ticks.max(1);
    }

    /// Returns `profile.frames[self.frame_idx]`, or empty string if no frames.
    pub(crate) fn current_frame(&self, profile: &AnimationProfile) -> &str {
        if profile.frames.is_empty() {
            return "";
        }
        let idx = self.frame_idx % profile.frames.len();
        profile.frames[idx]
    }

    /// Stop animating.
    pub(crate) fn finalize(&mut self) {
        self.running = false;
    }
}

/// T045: Compute the frame index for a profile at a given tick count.
/// Used by the streaming caret, which blinks based on the monotonic tick
/// counter incremented once per tick-arm firing in `render_turn` (FR-010).
/// Previously this used `Instant::now().elapsed()` which is always ~0, so the
/// caret never advanced past frame 0. Threading the real tick count fixes the
/// blink.
pub(crate) fn tick_phase(profile: &AnimationProfile, tick_count: u64) -> u8 {
    if profile.frames.is_empty() {
        return 0;
    }
    let interval = profile.interval_ticks.max(1) as u64;
    let phase = tick_count / interval;
    (phase % profile.frames.len() as u64) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Capability;
    use joey_core::theme::Theme;

    #[test]
    fn advance_wraps_correctly() {
        let kind = AnimationKind::ThinkingSpinner;
        let profile = AnimationProfile::for_kind(kind, Capability::Full);
        let now = Instant::now();
        let mut state = AnimationState::new(kind, now);
        // advance enough times to guarantee a wrap regardless of frame count
        let n = profile.frames.len() * 4 + 3;
        for _ in 0..n {
            state.advance(profile);
            // frame_idx must always be in bounds
            assert!(state.frame_idx < profile.frames.len());
        }
        // after N >= len advances, index should be N % len (with countdown
        // reset semantics — advance happens when countdown hits zero).
    }

    #[test]
    fn current_frame_never_out_of_bounds() {
        let kind = AnimationKind::StreamingCaret;
        let profile = AnimationProfile::for_kind(kind, Capability::Full);
        let now = Instant::now();
        let mut state = AnimationState::new(kind, now);
        // force frame_idx artificially high to confirm mod indexing
        state.frame_idx = 9999;
        let f = state.current_frame(profile);
        assert!(!f.is_empty() || profile.frames.is_empty());
    }

    #[test]
    fn finalize_stops_animation() {
        let kind = AnimationKind::PromptCaret;
        let now = Instant::now();
        let mut state = AnimationState::new(kind, now);
        assert!(state.running);
        state.finalize();
        assert!(!state.running);
    }

    #[test]
    fn thinking_spinner_color_is_accent() {
        // covers the T016-style assertion at the animation layer
        let t = Theme::pantera();
        let p = AnimationProfile::for_kind(AnimationKind::ThinkingSpinner, Capability::Full);
        assert_eq!((p.color)(&t), t.accent);
        assert_eq!(p.label, Some("Thinking…"));
    }

    // T045: tick_phase advances the frame index as the tick count grows, so
    // the streaming caret actually blinks (previously it was stuck on frame 0
    // because it derived the phase from `Instant::now().elapsed()` ≈ 0).
    #[test]
    fn tick_phase_advances_over_ticks() {
        let profile = AnimationProfile::for_kind(AnimationKind::StreamingCaret, Capability::Full);
        let len = profile.frames.len() as u64;
        assert!(len >= 2, "caret must have >= 2 frames to blink");
        let interval = profile.interval_ticks.max(1) as u64;

        // At tick 0 → frame 0.
        assert_eq!(tick_phase(profile, 0), 0);
        // After one full interval → frame 1 (blink visible).
        assert_eq!(tick_phase(profile, interval), 1);
        // After two intervals → wraps back to frame 0 for a 2-frame profile.
        assert_eq!(tick_phase(profile, 2 * interval), (2 % len) as u8);
        // Arbitrary large tick count still lands in bounds.
        let big = tick_phase(profile, 10_000 * interval);
        assert!((big as u64) < len);
    }

    #[test]
    fn tick_phase_empty_profile_is_zero() {
        // NonInteractive profiles have empty frames; tick_phase must be safe.
        let profile = AnimationProfile::for_kind(
            AnimationKind::StreamingCaret,
            crate::capability::Capability::NonInteractive,
        );
        assert_eq!(tick_phase(profile, 0), 0);
        assert_eq!(tick_phase(profile, 999), 0);
    }
}
