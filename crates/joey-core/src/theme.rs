//! CharmTone-inspired color palette and theme system.
//!
//! Ports the CharmTone palette from charmbracelet/x/exp/charmtone so joey-agent
//! can use the same rich, named colors as Crush. Includes a foreground gradient
//! renderer that blends between two colors across grapheme clusters — the
//! signature visual effect of the Crush UI.

use nu_ansi_term::{AnsiString, Color as AnsiColor};

// ── RGB color ──────────────────────────────────────────────────────────────

/// An RGB color (0-255 per channel).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Rgb(r, g, b)
    }

    /// Parse a `#rrggbb` hex string.
    pub fn from_hex(hex: &str) -> Option<Self> {
        let hex = hex.strip_prefix('#').unwrap_or(hex);
        if hex.len() != 6 {
            return None;
        }
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        Some(Rgb(r, g, b))
    }

    /// Linear interpolation between two colors. `t` ranges 0.0 (self) to 1.0
    /// (other). Uses simple linear blend (close enough perceptually for
    /// terminal text; Crush uses the same approach via lipgloss.Blend1D).
    pub fn lerp(&self, other: &Rgb, t: f32) -> Rgb {
        Rgb(
            lerp_u8(self.0, other.0, t),
            lerp_u8(self.1, other.1, t),
            lerp_u8(self.2, other.2, t),
        )
    }

    /// Convert to an ANSI true-color.
    pub fn ansi(&self) -> AnsiColor {
        AnsiColor::Rgb(self.0, self.1, self.2)
    }
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let a = a as f32;
    let b = b as f32;
    (a + (b - a) * t).round().clamp(0.0, 255.0) as u8
}

// ── CharmTone Palette ──────────────────────────────────────────────────────
//
// Verbatim from charmbracelet/x/exp/charmtone/charmtone.go.
// Each color is the exact RGBA from the upstream source.

pub mod charmtone {
    use super::Rgb;

    // Spectrum: warm to cool
    pub const CUMIN: Rgb = Rgb(0xBF, 0x97, 0x6F);
    pub const TANG: Rgb = Rgb(0xFF, 0x98, 0x5A);
    pub const YAM: Rgb = Rgb(0xFF, 0xB5, 0x87);
    pub const PAPRIKA: Rgb = Rgb(0xD3, 0x6C, 0x64);
    pub const BENGAL: Rgb = Rgb(0xFF, 0x6E, 0x63);
    pub const UNI: Rgb = Rgb(0xFF, 0x93, 0x7D);
    pub const SRIRACHA: Rgb = Rgb(0xEB, 0x42, 0x68);
    pub const CORAL: Rgb = Rgb(0xFF, 0x57, 0x7D);
    pub const SALMON: Rgb = Rgb(0xFF, 0x7F, 0x90);
    pub const CHILI: Rgb = Rgb(0xE2, 0x30, 0x80);
    pub const CHERRY: Rgb = Rgb(0xFF, 0x38, 0x8B);
    pub const TUNA: Rgb = Rgb(0xFF, 0x6D, 0xAA);
    pub const MACARON: Rgb = Rgb(0xE9, 0x40, 0xB0);
    pub const PONY: Rgb = Rgb(0xFF, 0x4F, 0xBF);
    pub const CHEEKY: Rgb = Rgb(0xFF, 0x79, 0xD0);
    pub const FLAMINGO: Rgb = Rgb(0xF9, 0x47, 0xE3);
    pub const DOLLY: Rgb = Rgb(0xFF, 0x60, 0xFF);
    pub const BLUSH: Rgb = Rgb(0xFF, 0x84, 0xFF);
    pub const URCHIN: Rgb = Rgb(0xC3, 0x37, 0xE0);
    pub const MOCHI: Rgb = Rgb(0xEB, 0x5D, 0xFF);
    pub const LILAC: Rgb = Rgb(0xF3, 0x79, 0xFF);
    pub const PRINCE: Rgb = Rgb(0x9C, 0x35, 0xE1);
    pub const VIOLET: Rgb = Rgb(0xC2, 0x59, 0xFF);
    pub const MAUVE: Rgb = Rgb(0xD4, 0x6E, 0xFF);
    pub const GRAPE: Rgb = Rgb(0x71, 0x34, 0xDD);
    pub const PLUM: Rgb = Rgb(0x99, 0x53, 0xFF);
    pub const ORCHID: Rgb = Rgb(0xAD, 0x6E, 0xFF);
    pub const JELLY: Rgb = Rgb(0x4A, 0x30, 0xD9);
    pub const CHARPLE: Rgb = Rgb(0x6B, 0x50, 0xFF);
    pub const HAZY: Rgb = Rgb(0x8B, 0x75, 0xFF);
    pub const OX: Rgb = Rgb(0x33, 0x31, 0xB2);
    pub const SAPPHIRE: Rgb = Rgb(0x49, 0x49, 0xFF);
    pub const GUPPY: Rgb = Rgb(0x72, 0x72, 0xFF);
    pub const OCEANIA: Rgb = Rgb(0x2B, 0x55, 0xB3);
    pub const THUNDER: Rgb = Rgb(0x47, 0x76, 0xFF);
    pub const ANCHOVY: Rgb = Rgb(0x71, 0x9A, 0xFC);
    pub const DAMSON: Rgb = Rgb(0x00, 0x7A, 0xB8);
    pub const MALIBU: Rgb = Rgb(0x00, 0xA4, 0xFF);
    pub const SARDINE: Rgb = Rgb(0x4F, 0xBE, 0xFE);
    pub const ZINC: Rgb = Rgb(0x10, 0xB1, 0xAE);
    pub const TURTLE: Rgb = Rgb(0x0A, 0xDC, 0xD9);
    pub const LICHEN: Rgb = Rgb(0x5C, 0xDF, 0xEA);
    pub const GUAC: Rgb = Rgb(0x12, 0xC7, 0x8F);
    pub const JULEP: Rgb = Rgb(0x00, 0xFF, 0xB2);
    pub const BOK: Rgb = Rgb(0x68, 0xFF, 0xD6);
    pub const MUSTARD: Rgb = Rgb(0xF5, 0xEF, 0x34);
    pub const CITRON: Rgb = Rgb(0xE8, 0xFF, 0x27);
    pub const ZEST: Rgb = Rgb(0xE8, 0xFE, 0x96);

    pub const BUTTER: Rgb = Rgb(0xFF, 0xFA, 0xF1);

    // Neutrals (dark to light)
    pub const PEPPER: Rgb = Rgb(0x20, 0x1F, 0x26);
    pub const BBQ: Rgb = Rgb(0x2D, 0x2C, 0x36);
    pub const CHAR: Rgb = Rgb(0x3A, 0x39, 0x43);
    pub const IRON: Rgb = Rgb(0x4D, 0x4C, 0x57);
    pub const OYSTER: Rgb = Rgb(0x60, 0x5F, 0x6B);
    pub const SQUID: Rgb = Rgb(0x85, 0x83, 0x92);
    pub const STEAM: Rgb = Rgb(0xA2, 0xA0, 0xAD);
    pub const SMOKE: Rgb = Rgb(0xBF, 0xBC, 0xC8);
    pub const STEEP: Rgb = Rgb(0xD6, 0xD3, 0xDC);
    pub const SASH: Rgb = Rgb(0xEC, 0xEB, 0xF0);
    pub const SALT: Rgb = Rgb(0xF7, 0xF6, 0xFB);
    pub const SODA: Rgb = Rgb(0xFB, 0xFB, 0xFB);
}

// ── Theme (CharmTone Pantera — Crush's default dark theme) ─────────────────

/// The complete theme struct mirroring Crush's CharmtonePantera.
/// All semantic color tokens used throughout the UI.
pub struct Theme {
    pub primary: Rgb,
    pub secondary: Rgb,
    pub accent: Rgb,
    pub keyword: Rgb,

    pub fg_base: Rgb,
    pub fg_subtle: Rgb,
    pub fg_more_subtle: Rgb,
    pub fg_most_subtle: Rgb,

    pub on_primary: Rgb,

    pub bg_base: Rgb,
    pub bg_least_visible: Rgb,
    pub bg_less_visible: Rgb,
    pub bg_most_visible: Rgb,

    pub separator: Rgb,

    pub destructive: Rgb,
    pub error: Rgb,
    pub warning_subtle: Rgb,
    pub warning: Rgb,
    pub denied: Rgb,
    pub busy: Rgb,
    pub info: Rgb,
    pub info_more_subtle: Rgb,
    pub info_most_subtle: Rgb,
    pub success: Rgb,
    pub success_more_subtle: Rgb,
    pub success_most_subtle: Rgb,
}

impl Theme {
    /// CharmTone Pantera — the default Crush dark theme.
    pub fn pantera() -> Self {
        use charmtone::*;
        Theme {
            primary: CHARPLE,
            secondary: DOLLY,
            accent: BOK,
            keyword: BLUSH,

            fg_base: SASH,
            fg_subtle: SMOKE,
            fg_more_subtle: SQUID,
            fg_most_subtle: OYSTER,

            on_primary: BUTTER,

            bg_base: PEPPER,
            bg_least_visible: BBQ,
            bg_less_visible: CHAR,
            bg_most_visible: IRON,

            separator: CHAR,

            destructive: CORAL,
            error: SRIRACHA,
            warning_subtle: ZEST,
            warning: MUSTARD,
            denied: TANG,
            busy: CITRON,
            info: MALIBU,
            info_more_subtle: SARDINE,
            info_most_subtle: DAMSON,
            success: JULEP,
            success_more_subtle: BOK,
            success_most_subtle: GUAC,
        }
    }

    /// Convenience: get the ANSI color for a semantic token.
    pub fn primary_ansi(&self) -> AnsiColor {
        self.primary.ansi()
    }
}

// ── Gradient text ──────────────────────────────────────────────────────────

/// Split a string into user-perceived characters (grapheme clusters).
/// For our purposes, a simple char-based split is sufficient — we split on
/// Unicode scalar values, which covers the vast majority of use cases
/// (combining marks are rare in terminal UI text).
fn grapheme_clusters(s: &str) -> Vec<&str> {
    // Simple approach: iterate chars and slice. For ASCII/Latin/CJK this
    // produces the same result as a full grapheme cluster segmentation.
    let mut result = Vec::new();
    let mut idx = 0;
    for (byte_idx, _) in s.char_indices().skip(1) {
        result.push(&s[idx..byte_idx]);
        idx = byte_idx;
    }
    if idx < s.len() {
        result.push(&s[idx..]);
    }
    result
}

/// Render a string with a horizontal foreground gradient from `c1` to `c2`.
/// Each character gets its own interpolated color.
pub fn gradient_fg(text: &str, c1: Rgb, c2: Rgb) -> String {
    gradient_fg_bold(text, c1, c2, false)
}

/// Render a string with a bold horizontal foreground gradient.
pub fn gradient_fg_bold(text: &str, c1: Rgb, c2: Rgb, bold: bool) -> String {
    let clusters = grapheme_clusters(text);
    if clusters.is_empty() {
        return String::new();
    }
    if clusters.len() == 1 {
        let mut style = nu_ansi_term::Style::new().fg(c1.ansi());
        if bold {
            style = style.bold();
        }
        return style.paint(clusters[0]).to_string();
    }

    let n = clusters.len() as f32;
    let mut out = String::new();
    for (i, cluster) in clusters.iter().enumerate() {
        let t = i as f32 / (n - 1.0);
        let c = c1.lerp(&c2, t);
        let mut style = nu_ansi_term::Style::new().fg(c.ansi());
        if bold {
            style = style.bold();
        }
        out.push_str(&style.paint(*cluster).to_string());
    }
    out
}

/// Render a multi-line string with a vertical gradient. Each line uses a
/// gradient from `c1` to `c2`, and the ramp itself shifts per line to create
/// a flowing diagonal effect.
pub fn gradient_vertical(text: &str, c1: Rgb, c2: Rgb) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let n = lines.len() as f32;
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let t = i as f32 / n.max(1.0);
        // Shift the gradient endpoints per line for a flowing effect.
        let start = c1.lerp(&c2, t * 0.3);
        let end = c1.lerp(&c2, (t + 0.3).min(1.0));
        out.push_str(&gradient_fg(line, start, end));
        out.push('\n');
    }
    out.trim_end_matches('\n').to_string()
}

/// A diagonal field: a row of `╱` characters in a given color.
/// Crush's signature decoration.
pub fn diagonal_field(width: usize, color: Rgb) -> String {
    let chars = "╱".repeat(width);
    color.ansi().paint(chars).to_string()
}

/// A gradient diagonal field: `╱` characters with a left-to-right gradient.
pub fn gradient_diagonal_field(width: usize, c1: Rgb, c2: Rgb) -> String {
    let field: String = std::iter::repeat('╱').take(width).collect();
    gradient_fg(&field, c1, c2)
}

/// Paint a string in a given Rgb color.
pub fn paint(text: &str, color: Rgb) -> AnsiString<'_> {
    color.ansi().paint(text)
}

/// Paint a string bold in a given Rgb color.
pub fn paint_bold(text: &str, color: Rgb) -> AnsiString<'_> {
    nu_ansi_term::Style::new()
        .fg(color.ansi())
        .bold()
        .paint(text)
}

/// Paint a string dim in a given Rgb color.
pub fn paint_dim(text: &str, color: Rgb) -> AnsiString<'_> {
    nu_ansi_term::Style::new()
        .fg(color.ansi())
        .dimmed()
        .paint(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let c = Rgb::from_hex("#6B50FF").unwrap();
        assert_eq!(c, charmtone::CHARPLE);
        let c2 = Rgb::from_hex("FF60FF").unwrap();
        assert_eq!(c2, charmtone::DOLLY);
    }

    #[test]
    fn lerp_endpoints() {
        let a = Rgb(0, 0, 0);
        let b = Rgb(255, 255, 255);
        assert_eq!(a.lerp(&b, 0.0), a);
        assert_eq!(a.lerp(&b, 1.0), b);
        let mid = a.lerp(&b, 0.5);
        assert_eq!(mid, Rgb(128, 128, 128));
    }

    #[test]
    fn gradient_single_char() {
        let s = gradient_fg("X", charmtone::CHARPLE, charmtone::DOLLY);
        assert!(s.contains("\x1b[38;2;107;80;255m"));
        assert!(s.contains("X"));
    }

    #[test]
    fn gradient_multi_char() {
        let s = gradient_fg("HI", charmtone::CHARPLE, charmtone::DOLLY);
        // First char should be c1, second should be c2
        assert!(s.contains("\x1b[38;2;107;80;255m")); // Charple
        assert!(s.contains("\x1b[38;2;255;96;255m")); // Dolly
    }

    #[test]
    fn diagonal_field_correct_width() {
        let f = diagonal_field(5, charmtone::CHARPLE);
        // Strip ANSI to check visible content
        let stripped: String = f.chars().filter(|&c| c != '\x1b').collect();
        let visible: String = stripped
            .chars()
            .filter(|c| !c.is_ascii_digit() && *c != '[' && *c != 'm' && *c != ';')
            .collect();
        assert_eq!(visible, "╱╱╱╱╱");
    }
}
