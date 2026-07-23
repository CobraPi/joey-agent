//! Neon-aurora theme & palette for the joey TUI.
//!
//! A unique, vibrant palette built around a cool indigo-charcoal background
//! and a four-stop signature gradient (cyan → violet → magenta → lime).
//! Inspired by crush's structured semantic-token approach but with a distinct
//! synthwave-aurora identity: jewel-toned, high-saturation accents on a deep,
//! near-black canvas for maximum vibrance without losing elegance.

use ratatui::style::Color;

/// An sRGB color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self(r, g, b)
    }

    /// Linear-interpolate between two colors (t in 0..=1).
    pub fn lerp(self, other: Rgb, t: f32) -> Rgb {
        Rgb(
            lerp_u8(self.0, other.0, t),
            lerp_u8(self.1, other.1, t),
            lerp_u8(self.2, other.2, t),
        )
    }

    pub fn to_color(self) -> Color {
        Color::Rgb(self.0, self.1, self.2)
    }

    /// Perceived luminance (0..1) for contrast-aware dimming.
    pub fn luma(self) -> f32 {
        let r = self.0 as f32 / 255.0;
        let g = self.1 as f32 / 255.0;
        let b = self.2 as f32 / 255.0;
        0.2126 * r + 0.7152 * g + 0.0722 * b
    }
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let t = t.clamp(0.0, 1.0);
    (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8
}

// ── Raw palette ───────────────────────────────────────────────────────────
//
//  Aurora-synthwave palette. Cool deep backgrounds; a vibrant triad of
//  electric cyan, hot orchid-pink and acid lime, bridged by electric violet.

#[allow(dead_code)]
pub mod palette {
    use super::Rgb;
    // backgrounds (cool indigo-charcoal, deep → raised)
    pub const BG_VOID: Rgb = Rgb(0x0B, 0x0B, 0x12);
    pub const BG_BASE: Rgb = Rgb(0x10, 0x10, 0x1B);
    pub const BG_PANEL: Rgb = Rgb(0x16, 0x16, 0x26);
    pub const BG_ELEVATED: Rgb = Rgb(0x1D, 0x1D, 0x31);
    pub const BG_HIGHEST: Rgb = Rgb(0x28, 0x28, 0x40);

    // foregrounds (lavender-tinted neutrals)
    pub const FG_BASE: Rgb = Rgb(0xEA, 0xEA, 0xF6);
    pub const FG_SUBTLE: Rgb = Rgb(0xAE, 0xAE, 0xCE);
    pub const FG_MORE_SUBTLE: Rgb = Rgb(0x78, 0x78, 0xA2);
    pub const FG_MOST_SUBTLE: Rgb = Rgb(0x4C, 0x4C, 0x6A);

    // brand triad + bridges
    pub const CYAN: Rgb = Rgb(0x22, 0xE4, 0xE8); // primary
    pub const ORCHID: Rgb = Rgb(0xFF, 0x3D, 0x9A); // secondary
    pub const LIME: Rgb = Rgb(0xB6, 0xFF, 0x3D); // accent
    pub const VIOLET: Rgb = Rgb(0xB9, 0x4F, 0xFF); // keyword / bridge
    pub const GOLD: Rgb = Rgb(0xFF, 0xC9, 0x3D); // highlight

    // status spectrum
    pub const MINT: Rgb = Rgb(0x2E, 0xFF, 0xA8); // success
    pub const SKY: Rgb = Rgb(0x3D, 0xBF, 0xFF); // info
    pub const AMBER: Rgb = Rgb(0xFF, 0xB1, 0x3D); // warning
    pub const CORAL: Rgb = Rgb(0xFF, 0x4D, 0x6D); // error
    pub const YELLOW: Rgb = Rgb(0xFF, 0xDD, 0x3D); // busy
    pub const TEAL: Rgb = Rgb(0x18, 0xD6, 0xC9); // success subtle
}

// ── Semantic theme ──────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub struct Theme {
    // brand
    pub primary: Rgb,
    pub secondary: Rgb,
    pub accent: Rgb,
    pub keyword: Rgb,
    pub gold: Rgb,
    // foreground scale
    pub fg_base: Rgb,
    pub fg_subtle: Rgb,
    pub fg_more_subtle: Rgb,
    pub fg_most_subtle: Rgb,
    // background scale
    pub bg_void: Rgb,
    pub bg_base: Rgb,
    pub bg_panel: Rgb,
    pub bg_elevated: Rgb,
    pub bg_highest: Rgb,
    // status
    pub success: Rgb,
    pub info: Rgb,
    pub warning: Rgb,
    pub error: Rgb,
    pub busy: Rgb,
    pub success_subtle: Rgb,
    pub separator: Rgb,
    // signature gradient stops
    pub grad_0: Rgb,
    pub grad_1: Rgb,
    pub grad_2: Rgb,
    pub grad_3: Rgb,
}

impl Theme {
    pub const fn aurora() -> Self {
        use palette::*;
        Theme {
            primary: CYAN,
            secondary: ORCHID,
            accent: LIME,
            keyword: VIOLET,
            gold: GOLD,
            fg_base: FG_BASE,
            fg_subtle: FG_SUBTLE,
            fg_more_subtle: FG_MORE_SUBTLE,
            fg_most_subtle: FG_MOST_SUBTLE,
            bg_void: BG_VOID,
            bg_base: BG_BASE,
            bg_panel: BG_PANEL,
            bg_elevated: BG_ELEVATED,
            bg_highest: BG_HIGHEST,
            success: MINT,
            info: SKY,
            warning: AMBER,
            error: CORAL,
            busy: YELLOW,
            success_subtle: TEAL,
            separator: BG_HIGHEST,
            grad_0: CYAN,
            grad_1: VIOLET,
            grad_2: ORCHID,
            grad_3: LIME,
        }
    }

    /// Sample the signature 4-stop gradient at position t (0..=1).
    pub fn gradient(self, t: f32) -> Rgb {
        let stops = [self.grad_0, self.grad_1, self.grad_2, self.grad_3];
        sample_stops(&stops, t)
    }

    /// A two-color gradient ramp as ratatui colors across N samples.
    pub fn ramp2(a: Rgb, b: Rgb, n: usize) -> Vec<Color> {
        match n {
            0 => Vec::new(),
            1 => vec![a.to_color()],
            _ => (0..n)
                .map(|i| a.lerp(b, i as f32 / (n - 1) as f32).to_color())
                .collect(),
        }
    }
}

/// Multi-stop gradient sampler with clamped edges.
pub fn sample_stops(stops: &[Rgb], t: f32) -> Rgb {
    if stops.is_empty() {
        return Rgb(0, 0, 0);
    }
    if stops.len() == 1 {
        return stops[0];
    }
    let t = t.clamp(0.0, 1.0);
    let pos = t * (stops.len() - 1) as f32;
    let i = pos.floor() as usize;
    if i >= stops.len() - 1 {
        return stops[stops.len() - 1];
    }
    stops[i].lerp(stops[i + 1], pos - i as f32)
}

/// Gradient-style a run of text into colored [`Span`]s using the theme signature.
pub fn gradient_spans(text: &str, theme: Theme) -> Vec<ratatui::text::Span<'static>> {
    gradient_spans_stops(text, &[theme.grad_0, theme.grad_1, theme.grad_2, theme.grad_3])
}

/// Gradient-style text across arbitrary stops.
pub fn gradient_spans_stops(text: &str, stops: &[Rgb]) -> Vec<ratatui::text::Span<'static>> {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Span;
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let n = chars.len();
    if n == 1 {
        let c = sample_stops(stops, 0.0).to_color();
        return vec![Span::styled(chars[0].to_string(), Style::default().fg(c))];
    }
    let mut out = Vec::with_capacity(n);
    for (i, ch) in chars.iter().enumerate() {
        let t = i as f32 / (n - 1) as f32;
        let col = sample_stops(stops, t).to_color();
        out.push(Span::styled(
            ch.to_string(),
            Style::default().fg(col).add_modifier(Modifier::BOLD),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gradient_endpoints() {
        let t = Theme::aurora();
        assert_eq!(t.gradient(0.0), t.grad_0);
        assert_eq!(t.gradient(1.0), t.grad_3);
    }
    #[test]
    fn ramp_length() {
        let t = Theme::aurora();
        let r = Theme::ramp2(t.primary, t.secondary, 5);
        assert_eq!(r.len(), 5);
    }
}
