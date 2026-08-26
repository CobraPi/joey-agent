//! Animation engine for the joey TUI.
//!
//! Everything that moves lives here. The key behavioral contract: **animation
//! speed scales with the number of active agents.** More agents ⇒ faster
//! spinners, denser particles, more energetic bars. When the system is idle,
//! motion gracefully slows to a calm shimmer.

use std::time::{Duration, Instant};

use crate::theme::{sample_stops, Rgb, Theme};

/// Global pacing signal derived from active agent count. All animators read
/// from one of these so the whole UI speeds up / slows down in lockstep.
#[derive(Clone, Copy, Debug)]
pub struct Activity {
    /// Smoothed active-agent count (float so it eases in/out).
    pub agents: f32,
    /// 0 = idle, grows toward 1 with active work; decays when idle.
    pub intensity: f32,
}

impl Activity {
    pub fn idle() -> Self {
        Self { agents: 0.0, intensity: 0.0 }
    }

    /// Advance one tick: blend toward the target agent count and ease intensity.
    pub fn update(&mut self, target_agents: usize, dt: Duration) {
        let dt = dt.as_secs_f32();
        let target = target_agents as f32;
        // Exponential smoothing toward the target count.
        let k_agents = 1.0 - (-dt * 4.0).exp();
        self.agents += (target - self.agents) * k_agents;

        // Intensity rises toward a cap driven by agent count; decays to a low
        // shimmer baseline when idle so motion never fully stops.
        let target_intensity = if target_agents > 0 {
            (0.35 + 0.65 * (target_agents as f32 / 4.0).min(1.0)).min(1.0)
        } else {
            0.12
        };
        let k_int = 1.0 - (-dt * 2.0).exp();
        self.intensity += (target_intensity - self.intensity) * k_int;
    }

    /// Effective animation speed multiplier. Toned down from the original
    /// synthwave build (crush-style: motion should be a quiet status signal,
    /// not a light show) — 0.8 baseline, up to ~1.5x when busy.
    pub fn speed(self) -> f32 {
        0.8 + self.intensity * 0.7
    }

    /// FPS target for the render loop, scaled by activity. Lower ceiling
    /// keeps CPU usage down and avoids visual restlessness while idle.
    pub fn target_fps(self) -> u32 {
        (12.0 + self.intensity * 12.0).round() as u32
    }
}

// ── Spinner ────────────────────────────────────────────────────────────────

/// A multi-phase gradient spinner whose rotation speed tracks activity.
pub struct Spinner {
    frames: &'static [&'static str],
    phase: f32,
}

impl Spinner {
    const DOTS: &'static [&'static str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    const ORBIT: &'static [&'static str] = &["◐", "◓", "◑", "◒"];
    #[allow(dead_code)]
    const ARC: &'static [&'static str] = &["◜", "◠", "◝", "◞", "◟", "◞"];

    pub fn dots() -> Self {
        Self { frames: Self::DOTS, phase: 0.0 }
    }
    pub fn orbit() -> Self {
        Self { frames: Self::ORBIT, phase: 0.0 }
    }

    /// Advance the spinner. `speed_mult` is the activity speed().
    pub fn tick(&mut self, dt: Duration, speed_mult: f32) {
        // ~10 fps baseline; scales with activity.
        let advance = dt.as_secs_f32() * 10.0 * speed_mult;
        self.phase += advance;
    }

    pub fn glyph(&self) -> &'static str {
        let n = self.frames.len() as f32;
        let idx = (self.phase % n) as usize;
        // Safe guard in case frames contains an empty string.
        self.frames.get(idx).copied().unwrap_or("·")
    }

    /// Render the spinner glyph in a theme-gradient color cycling over time.
    pub fn styled_glyph(&self, theme: Theme) -> ratatui::text::Span<'static> {
        use ratatui::style::{Modifier, Style};
        let stops = [theme.grad_0, theme.grad_1, theme.grad_2, theme.grad_3];
        let t = ((self.phase * 0.15) % 1.0).abs();
        let col = sample_stops(&stops, t).to_color();
        ratatui::text::Span::styled(
            self.glyph().to_string(),
            Style::default().fg(col).add_modifier(Modifier::BOLD),
        )
    }
}

// ── Particle field ─────────────────────────────────────────────────────────
//
// A field of drifting glowing particles used in the header / status backdrop.
// Particle count and drift velocity scale with activity — idle shows a sparse,
// slow twinkle; busy shows a dense, fast starfield.

#[derive(Clone, Copy)]
pub struct Particle {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub life: f32,
    pub max_life: f32,
    pub size: f32,
    pub stop_idx: u8,
}

pub struct ParticleField {
    particles: Vec<Particle>,
    width: f32,
    height: f32,
    rng: Rng,
    spawn_accum: f32,
}

impl ParticleField {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            particles: Vec::new(),
            width: width as f32,
            height: height as f32,
            rng: Rng::seeded(0xA11CE),
            spawn_accum: 0.0,
        }
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width as f32;
        self.height = height as f32;
    }

    pub fn particles(&self) -> &[Particle] {
        &self.particles
    }

    /// Advance the field. Density and speed scale with `activity`.
    pub fn tick(&mut self, dt: Duration, activity: Activity, theme: Theme) {
        let dt = dt.as_secs_f32();
        let speed = activity.speed();
        let intensity = activity.intensity;

        // Spawn rate scales with intensity and screen area.
        let area = self.width * self.height;
        // Toned down considerably: the particle backdrop is now a faint
        // ambient signal rather than a dense starfield.
        let spawn_rate = 0.15 + intensity * 1.5; // particles / sec baseline scaled
        self.spawn_accum += dt * spawn_rate * (area / 2000.0).clamp(1.0, 4.0);
        while self.spawn_accum >= 1.0 {
            self.spawn_accum -= 1.0;
            self.spawn_one(theme);
        }

        // Update + cull.
        self.particles.retain_mut(|p| {
            p.life += dt;
            p.x += p.vx * dt * speed;
            p.y += p.vy * dt * speed;
            // gentle drift acceleration with intensity
            p.vy += dt * intensity * 0.5;
            p.life < p.max_life
                && p.x > -2.0
                && p.x < self.width + 2.0
                && p.y > -2.0
                && p.y < self.height + 2.0
        });
    }

    fn spawn_one(&mut self, theme: Theme) {
        let r = self.rng.next();
        let side = (r * 4.0).floor() as u8 % 4;
        let (x, y, vx, vy) = match side {
            0 => (self.rng.next() * self.width, -1.0, (self.rng.next() - 0.5) * 4.0, 2.0 + self.rng.next() * 6.0),
            1 => (self.width + 1.0, self.rng.next() * self.height, -(2.0 + self.rng.next() * 6.0), (self.rng.next() - 0.5) * 4.0),
            2 => (self.rng.next() * self.width, self.height + 1.0, (self.rng.next() - 0.5) * 4.0, -(2.0 + self.rng.next() * 6.0)),
            _ => (-1.0, self.rng.next() * self.height, 2.0 + self.rng.next() * 6.0, (self.rng.next() - 0.5) * 4.0),
        };
        let max_life = 1.5 + self.rng.next() * 3.5;
        let stop_idx = (self.rng.next() * 4.0) as u8 % 4;
        let _ = theme; // stop palette referenced by caller via stop_idx
        self.particles.push(Particle {
            x,
            y,
            vx,
            vy,
            life: 0.0,
            max_life,
            size: 0.5 + self.rng.next() * 0.8,
            stop_idx,
        });
    }

    /// Particle color from the theme gradient.
    pub fn particle_color(p: &Particle, theme: Theme) -> Rgb {
        let stops = [theme.grad_0, theme.grad_1, theme.grad_2, theme.grad_3];
        sample_stops(&stops, (p.stop_idx as f32 + (p.life / p.max_life.max(0.001)) * 0.5) / 4.0)
    }
}

// ── Activity equalizer bars ────────────────────────────────────────────────
//
// A row of vertical bars whose heights + oscillation speed track activity.
// Inspired by crush's scrambled-char spinner energy but rendered as a live
// spectrum analyzer across the gradient palette.

pub struct Equalizer {
    bars: Vec<f32>,
    phases: Vec<f32>,
}

impl Equalizer {
    pub fn new(n: usize) -> Self {
        Self {
            bars: vec![0.0; n],
            phases: (0..n).map(|i| i as f32 * 0.7).collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.bars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }

    /// Advance the bars. Amplitude and frequency scale with activity.
    pub fn tick(&mut self, dt: Duration, activity: Activity) {
        let dt = dt.as_secs_f32();
        let speed = activity.speed();
        let intensity = activity.intensity;
        for (i, (b, ph)) in self.bars.iter_mut().zip(self.phases.iter_mut()).enumerate() {
            *ph += dt * (1.5 + (i as f32 % 3.0)) * speed;
            // target is a layered sine; amplitude scales with intensity.
            let s = (*ph).sin() * 0.5 + 0.5;
            let s2 = (*ph * 1.7 + i as f32).sin() * 0.5 + 0.5;
            // Toned-down amplitude range: calmer at rest, less jittery when busy.
            let target = (0.06 + intensity * 0.55) * (0.4 * s + 0.6 * s2);
            // smooth toward target
            let k = 1.0 - (-dt * 12.0).exp();
            *b += (target - *b) * k;
        }
    }

    /// (index, normalized height 0..1)
    pub fn heights(&self) -> impl Iterator<Item = (usize, f32)> + '_ {
        self.bars.iter().copied().enumerate()
    }
}

// ── Pulse ──────────────────────────────────────────────────────────────────
//
// A single oscillating value (0..1) for glow / breathing effects. Used for
// the header logo glow and panel focus rings.

pub struct Pulse {
    phase: f32,
}

impl Pulse {
    pub fn new() -> Self {
        Self { phase: 0.0 }
    }

    pub fn tick(&mut self, dt: Duration, activity: Activity) {
        self.phase += dt.as_secs_f32() * activity.speed();
    }

    /// Current value in 0..=1.
    pub fn value(&self) -> f32 {
        self.phase.sin() * 0.5 + 0.5
    }
}

impl Default for Pulse {
    fn default() -> Self {
        Self::new()
    }
}

// ── HeaderFlow ───��──────────────────────────────────────────────────────────
//
// The header's gradient underline animator — the "agent is running" status
// indicator. While a turn is busy the underline becomes a slow, elegant
// traveling wave: a soft brightness pulse glides across the bar (gradient
// colors themselves stay fixed — only brightness moves), and the whole bar
// breathes slightly brighter overall. When the turn ends, the motion eases
// OUT over ~1.5s back to the static underline — never a hard snap.
//
// Design constraints (per the repo's "quiet status signal, not a light
// show" animation philosophy): no flicker, no color cycling beyond the
// existing gradient, one wave visible at a time, ~8s per full traversal
// (subtle but noticeable in peripheral vision), and phase-continuous —
// starting/stopping the turn never jumps the wave position.

/// The header gradient bar animator ("agent active" indicator).
pub struct HeaderFlow {
    /// Unbounded phase accumulator (seconds of animation time). Never reset
    /// so the wave position is continuous across busy↔idle transitions.
    phase: f32,
    /// 0 = fully idle (static bar), 1 = fully running. Eased toward the
    /// busy flag each tick — this is the fade-in/out envelope.
    amount: f32,
    /// The busy flag from the app, latched for tick().
    busy: bool,
}

impl HeaderFlow {
    /// Seconds for the brightness wave to traverse the bar once. Slow on
    /// purpose: it should read as "alive", not "flashing".
    const WAVE_PERIOD: f32 = 8.0;
    /// Ease-in/out time constant for the busy envelope (seconds). ~1.2s to
    /// full intensity, ~1.5s back to static.
    const EASE: f32 = 1.2;

    pub fn new() -> Self {
        Self { phase: 0.0, amount: 0.0, busy: false }
    }

    /// Latch the busy state (call before tick each frame).
    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }

    /// Advance the animator. `speed_mult` is the activity speed() so the
    /// wave pace scales gently with agent count like every other animator.
    pub fn tick(&mut self, dt: Duration, speed_mult: f32) {
        // Phase advances continuously; when idle the envelope is 0 so it has
        // no visible effect, but the position continuity is preserved.
        let advance = dt.as_secs_f32() * speed_mult;
        self.phase += advance;
        // Asymmetric exponential ease toward the busy target: ~1s to visibly
        // engage, ~0.8s to visibly settle (decay constant is ~3x steeper so
        // the indicator responds promptly when a turn ends).
        let target = if self.busy { 1.0 } else { 0.0 };
        let rate = if self.busy { 1.25 } else { 3.0 };
        let k = 1.0 - (-advance * rate / Self::EASE).exp();
        self.amount += (target - self.amount) * k;
        // Clamp away float drift so "fully idle" is exactly static.
        if self.amount < 0.004 {
            self.amount = 0.0;
        }
    }

    /// Per-cell brightness lift (0..1) for the column at `t` (0..=1 across
    /// the bar). Static zero when idle. One soft Gaussian-ish pulse rides
    /// the bar; the base lift adds a gentle whole-bar breath.
    pub fn brightness(&self, t: f32) -> f32 {
        if self.amount <= 0.0 {
            return 0.0;
        }
        // Wave center position, wrapping (period in phase-seconds).
        let center = ((self.phase / Self::WAVE_PERIOD) % 1.0 + 1.0) % 1.0;
        // Wrapped distance along the bar from the wave center, so the pulse
        // slides off the right edge and re-enters from the left seamlessly.
        let raw = (t - center).abs();
        let d = raw.min(1.0 - raw);
        // Smooth pulse: raised-cosine bump of width ~0.30 of the bar.
        let width = 0.30;
        let x = (d / width).min(1.0);
        let bump = 0.5 * (1.0 + (std::f32::consts::PI * x).cos()); // 1 at center → 0 at edge
        // Gentle whole-bar breathing (the existing pulse feel, subtler).
        let breath = 0.5 + 0.5 * (self.phase * 1.6).sin();
        // Base lift: a slightly brighter bar overall while running.
        let base = 0.05 + 0.05 * breath;
        (base + bump * 0.22) * self.amount
    }

    /// The eased busy envelope (0..1) — exposed for tests and potential
    /// future consumers (e.g. also tinting the logo while busy).
    pub fn amount(&self) -> f32 {
        self.amount
    }
}

impl Default for HeaderFlow {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod header_flow_tests {
    use super::*;

    fn secs(n: f32) -> Duration {
        Duration::from_secs_f32(n)
    }

    #[test]
    fn idle_flow_is_exactly_static() {
        let mut flow = HeaderFlow::new();
        flow.set_busy(false);
        flow.tick(secs(10.0), 1.0);
        assert_eq!(flow.amount(), 0.0);
        assert_eq!(flow.brightness(0.0), 0.0);
        assert_eq!(flow.brightness(0.5), 0.0);
        assert_eq!(flow.brightness(1.0), 0.0);
    }

    #[test]
    fn busy_envelope_eases_in_without_snap() {
        let mut flow = HeaderFlow::new();
        flow.set_busy(true);
        // First frame: tiny but nonzero — no instant jump.
        flow.tick(secs(1.0 / 30.0), 1.0);
        let a1 = flow.amount();
        assert!(a1 > 0.0 && a1 < 0.15, "first frame eases in, got {a1}");
        // Two seconds of ticking approaches full intensity monotonically.
        for _ in 0..60 {
            flow.tick(secs(1.0 / 30.0), 1.0);
        }
        let a2 = flow.amount();
        assert!(a2 > a1, "envelope rises: {a1} -> {a2}");
        assert!(a2 > 0.8, "approaches full intensity, got {a2}");
        assert!(a2 <= 1.0, "never exceeds 1");
    }

    #[test]
    fn idle_again_eases_out_to_static() {
        let mut flow = HeaderFlow::new();
        flow.set_busy(true);
        for _ in 0..120 {
            flow.tick(secs(1.0 / 30.0), 1.0);
        }
        assert!(flow.amount() > 0.9);
        flow.set_busy(false);
        // ~2.5s of frames decays the envelope to the static clamp (decay is
        // steeper than engage, so the indicator settles promptly).
        for _ in 0..75 {
            flow.tick(secs(1.0 / 30.0), 1.0);
        }
        assert_eq!(flow.amount(), 0.0, "clamped to exactly static");
        assert_eq!(flow.brightness(0.3), 0.0);
    }

    #[test]
    fn wave_brightness_is_bounded_and_peaks_near_center() {
        let mut flow = HeaderFlow::new();
        flow.set_busy(true);
        for _ in 0..120 {
            flow.tick(secs(1.0 / 30.0), 1.0);
        }
        // Sample the whole bar: bounded, and a clear peak exists (the wave).
        let mut peak = 0.0f32;
        let mut peak_t = 0.0f32;
        for i in 0..=100 {
            let t = i as f32 / 100.0;
            let b = flow.brightness(t);
            assert!((0.0..=0.35).contains(&b), "brightness {b} out of range at t={t}");
            if b > peak {
                peak = b;
                peak_t = t;
            }
        }
        assert!(peak > 0.15, "wave must be noticeable: peak {peak}");
        // The raised-cosine bump: brightness strictly decreases away from
        // the peak (sampled away from the wrap seam).
        let left = flow.brightness((peak_t - 0.15).max(0.0));
        let right = flow.brightness((peak_t + 0.15).min(1.0));
        assert!(left < peak, "left of peak is dimmer");
        assert!(right < peak, "right of peak is dimmer");
    }

    #[test]
    fn wave_travels_over_time() {
        // The peak position must move across the bar as time passes
        // (that's the "agent is active" signal).
        let mut flow = HeaderFlow::new();
        flow.set_busy(true);
        for _ in 0..120 {
            flow.tick(secs(1.0 / 30.0), 1.0);
        }
        let peak_at = |f: &HeaderFlow| {
            (0..=100)
                .map(|i| (f.brightness(i as f32 / 100.0), i as f32 / 100.0))
                .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap())
                .unwrap()
                .1
        };
        let p0 = peak_at(&flow);
        // Advance ~1.5s — 3/16 of a traversal; must register movement.
        for _ in 0..45 {
            flow.tick(secs(1.0 / 30.0), 1.0);
        }
        let p1 = peak_at(&flow);
        let moved = (p1 - p0).abs().max(((p1 - p0).abs() - 1.0).abs());
        assert!(moved > 0.02, "wave moved: {p0} -> {p1}");
    }

    #[test]
    fn phase_survives_busy_idle_transitions() {
        // Phase never resets: after busy→idle→busy the wave reappears where
        // it would have been, not back at the start (no position jump).
        let mut flow = HeaderFlow::new();
        flow.set_busy(true);
        for _ in 0..90 {
            flow.tick(secs(1.0 / 30.0), 1.0);
        }
        flow.set_busy(false);
        for _ in 0..30 {
            flow.tick(secs(1.0 / 30.0), 1.0);
        }
        flow.set_busy(true);
        flow.tick(secs(1.0 / 30.0), 1.0);
        // Just assert it re-engages smoothly (no panic, envelope rising).
        assert!(flow.amount() > 0.0);
    }
}

// ── Tiny deterministic RNG ──────────────────────────────────────────────────
//
// xorshift32 — deterministic so the particle field looks stable across frames
// at the same activity level, and avoids pulling in the `rand` crate here.

pub struct Rng {
    state: u32,
}

impl Rng {
    pub fn seeded(seed: u32) -> Self {
        Self { state: if seed == 0 { 0x9E3779B9 } else { seed } }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> f32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        (x >> 8) as f32 / ((1u32 << 24) as f32)
    }
}

// ── Clock for dt ────────────────────────────────────────────────────────────

pub struct Clock {
    last: Instant,
}

impl Clock {
    pub fn start() -> Self {
        Self { last: Instant::now() }
    }

    pub fn dt(&mut self) -> Duration {
        let now = Instant::now();
        let dt = now.duration_since(self.last);
        self.last = now;
        dt
    }
}
