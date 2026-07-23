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

    /// Effective animation speed multiplier. 1.0 baseline, up to ~3x when busy.
    pub fn speed(self) -> f32 {
        0.6 + self.intensity * 2.4
    }

    /// FPS target for the render loop, scaled by activity.
    pub fn target_fps(self) -> u32 {
        (16.0 + self.intensity * 28.0).round() as u32
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
        let spawn_rate = 0.5 + intensity * 8.0; // particles / sec baseline scaled
        self.spawn_accum += dt * spawn_rate * (area / 2000.0).max(1.0).min(12.0);
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
            let target = (0.08 + intensity * 0.92) * (0.4 * s + 0.6 * s2);
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
