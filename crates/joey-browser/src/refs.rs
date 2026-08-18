//! Property-based ephemeral element references + the cascading fallback
//! resolver (research.md D2; FR-004/005/006/007).
//!
//! No DOM mutation for identification: each scan computes descriptors
//! (role/text/locator/geometry); refids (`e<N>`) are registry-local and reset
//! every scan. Before every action the page is re-scanned and the action's
//! target descriptor is re-matched via the cascade:
//! refid → locator → text → geometry → refuse-with-candidates.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Viewport-coordinate rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// A discovered interactive element (snapshot-format.md ElementRef).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementRef {
    /// Assigned by `ElementRefRegistry::push` on insert (`e1`, `e2`, …).
    /// SCAN_JS rows arrive WITHOUT a refid (registry-local, reset per scan),
    /// so deserialization defaults it to empty and `push` overwrites.
    #[serde(default)]
    pub refid: String,
    pub role: String,
    /// Visible label, whitespace-normalized, ≤120 chars (char-boundary cut).
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Frame-context label: `main`, `iframe:name`, `oopif:name`.
    pub frame: String,
    /// Structural fallback locator (CSS-first).
    pub locator: String,
    pub geometry: Rect,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub attributes: std::collections::BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub interactable: bool,
}

const fn default_true() -> bool {
    true
}

/// Attribute allowlist (snapshot-format.md).
pub const ATTR_ALLOWLIST: &[&str] = &[
    "aria-label", "placeholder", "href", "name", "type",
];

/// Maximum visible-text length in characters.
pub const MAX_TEXT_CHARS: usize = 120;

/// Char-boundary-safe truncation (repo audit lesson).
pub fn truncate_char_safe(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut end = s
        .char_indices()
        .nth(max_chars)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

impl ElementRef {
    /// Validate invariants: refid pattern, non-empty locator, text cap.
    pub fn validate(&self) -> Result<(), String> {
        if !(self.refid.starts_with('e') && self.refid[1..].chars().all(|c| c.is_ascii_digit())) {
            return Err(format!("refid pattern violation: {}", self.refid));
        }
        if self.locator.is_empty() {
            return Err(format!("empty locator on {}", self.refid));
        }
        if self.text.chars().count() > MAX_TEXT_CHARS {
            return Err(format!("text cap violated on {}", self.refid));
        }
        Ok(())
    }
}

/// Which strategy resolved an action target (reported in results).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedBy {
    Refid,
    Locator,
    Text,
    Geometry,
    RefusedAmbiguous,
}

impl ResolvedBy {
    pub fn as_str(&self) -> &'static str {
        match self {
            ResolvedBy::Refid => "refid",
            ResolvedBy::Locator => "locator",
            ResolvedBy::Text => "text",
            ResolvedBy::Geometry => "geometry",
            ResolvedBy::RefusedAmbiguous => "refused_ambiguous",
        }
    }
}

/// Target descriptor as supplied by the model (contracts/browser-tools.md).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TargetDescriptor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<Rect>,
}

impl TargetDescriptor {
    pub fn is_empty(&self) -> bool {
        self.refid.is_none()
            && self.locator.is_none()
            && self.text.is_none()
            && self.geometry.is_none()
    }
}

/// One scan's element registry (reset per scan — FR-005).
#[derive(Debug, Default, Clone)]
pub struct ElementRefRegistry {
    pub elements: Vec<ElementRef>,
}

impl ElementRefRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert with sequential refid assignment (`e1`, `e2`, …).
    pub fn push(&mut self, mut el: ElementRef) -> ElementRef {
        let n = self.elements.len() + 1;
        el.refid = format!("e{n}");
        let out = el.clone();
        self.elements.push(el);
        out
    }

    pub fn get(&self, refid: &str) -> Option<&ElementRef> {
        self.elements.iter().find(|e| e.refid == refid)
    }

    /// Resolve a model-supplied descriptor through the cascade.
    ///
    /// Order: refid → locator → text → geometry. Ambiguous text matches
    /// disambiguate by proximity to the descriptor's geometry when present,
    /// else refuse with candidates (FR-007).
    pub fn resolve(
        &self,
        target: &TargetDescriptor,
    ) -> Result<(&ElementRef, ResolvedBy), ResolutionFailure> {
        if target.is_empty() {
            return Err(ResolutionFailure::Empty);
        }
        // 1. refid — only against THIS registry (stale refids simply miss).
        if let Some(refid) = &target.refid {
            if let Some(el) = self.get(refid) {
                return Ok((el, ResolvedBy::Refid));
            }
        }
        // 2. locator — exact match first, then suffix match (a re-render
        //    often re-roots the subtree).
        if let Some(loc) = &target.locator {
            if let Some(el) = self.elements.iter().find(|e| &e.locator == loc) {
                return Ok((el, ResolvedBy::Locator));
            }
            let hits: Vec<&ElementRef> = self
                .elements
                .iter()
                .filter(|e| e.locator.ends_with(loc.as_str()))
                .collect();
            match hits.len() {
                1 => return Ok((hits[0], ResolvedBy::Locator)),
                0 => {}
                _ => return Err(ResolutionFailure::ambiguous(&hits)),
            }
        }
        // 3. text — exact (normalized) match; ambiguity resolved by geometry
        //    proximity when the descriptor carries geometry, else refused.
        if let Some(text) = &target.text {
            let needle = normalize_text(text);
            let hits: Vec<&ElementRef> = self
                .elements
                .iter()
                .filter(|e| normalize_text(&e.text) == needle)
                .collect();
            match hits.len() {
                0 => {}
                1 => return Ok((hits[0], ResolvedBy::Text)),
                _ => {
                    if let Some(g) = target.geometry {
                        if let Some(nearest) =
                            hits.iter().min_by(|a, b| {
                                dist2(a.geometry.center(), g.center())
                                    .partial_cmp(&dist2(b.geometry.center(), g.center()))
                                    .expect("finite f64")
                            })
                        {
                            return Ok((nearest, ResolvedBy::Text));
                        }
                    }
                    return Err(ResolutionFailure::ambiguous(&hits));
                }
            }
        }
        // 4. geometry — element whose rect contains/nearest the point.
        if let Some(g) = target.geometry {
            let (cx, cy) = g.center();
            if let Some(nearest) = self.elements.iter().min_by(|a, b| {
                dist2(a.geometry.center(), (cx, cy))
                    .partial_cmp(&dist2(b.geometry.center(), (cx, cy)))
                    .expect("finite f64")
            }) {
                // Guard: only accept when reasonably close (inside the rect
                // or within half a rect's dimension).
                let (nx, ny) = nearest.geometry.center();
                let near = (nx - cx).abs() <= nearest.geometry.w.max(8.0)
                    && (ny - cy).abs() <= nearest.geometry.h.max(8.0);
                if near {
                    return Ok((nearest, ResolvedBy::Geometry));
                }
            }
            return Err(ResolutionFailure::TargetGone);
        }
        Err(ResolutionFailure::TargetGone)
    }
}

/// Resolution failure detail.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ResolutionFailure {
    #[error("target descriptor is empty")]
    Empty,
    #[error("target no longer resolvable (element gone after re-render)")]
    TargetGone,
    #[error("ambiguous match ({} candidates): {}", .0.len(), candidates_summary(.0))]
    Ambiguous(Vec<String>),
}

fn candidates_summary(c: &[String]) -> String {
    c.iter()
        .take(5)
        .cloned()
        .collect::<Vec<_>>()
        .join(" | ")
}

impl ResolutionFailure {
    fn ambiguous(hits: &[&ElementRef]) -> Self {
        ResolutionFailure::Ambiguous(
            hits.iter()
                .map(|e| format!("{} [{}] {:?}", e.refid, e.role, truncate_char_safe(&e.text, 40)))
                .collect(),
        )
    }
}

fn dist2(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (a.0 - b.0, a.1 - b.1);
    dx * dx + dy * dy
}

/// Whitespace normalization for text matching.
pub fn normalize_text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(role: &str, text: &str, x: f64, y: f64) -> ElementRef {
        ElementRef {
            refid: String::new(),
            role: role.into(),
            text: text.into(),
            value: None,
            frame: "main".into(),
            locator: format!("div > button:nth-of-type({})", (x as i64) + 1),
            geometry: Rect { x, y, w: 100.0, h: 40.0 },
            attributes: Default::default(),
            interactable: true,
        }
    }

    fn registry(els: Vec<ElementRef>) -> ElementRefRegistry {
        let mut r = ElementRefRegistry::new();
        for e in els {
            r.push(e);
        }
        r
    }

    #[test]
    fn refids_sequential_and_unique() {
        let r = registry(vec![el("button", "A", 0.0, 0.0), el("button", "B", 0.0, 50.0)]);
        assert_eq!(r.elements[0].refid, "e1");
        assert_eq!(r.elements[1].refid, "e2");
        assert!(r.elements.iter().all(|e| e.validate().is_ok()));
    }

    #[test]
    fn cascade_refid_then_locator_then_text_then_geometry() {
        let r = registry(vec![
            el("button", "Save", 0.0, 0.0),
            el("button", "Cancel", 0.0, 50.0),
        ]);
        // refid hit
        let t = TargetDescriptor { refid: Some("e1".into()), ..Default::default() };
        assert_eq!(r.resolve(&t).unwrap().1, ResolvedBy::Refid);
        // locator hit (exact)
        let t = TargetDescriptor { locator: Some("div > button:nth-of-type(1)".into()), ..Default::default() };
        assert_eq!(r.resolve(&t).unwrap().1, ResolvedBy::Locator);
        // text hit
        let t = TargetDescriptor { text: Some("Cancel".into()), ..Default::default() };
        assert_eq!(r.resolve(&t).unwrap().0.refid, "e2");
        // geometry hit (nearest center within bounds)
        let t = TargetDescriptor {
            geometry: Some(Rect { x: 10.0, y: 60.0, w: 20.0, h: 20.0 }),
            ..Default::default()
        };
        assert_eq!(r.resolve(&t).unwrap().1, ResolvedBy::Geometry);
    }

    #[test]
    fn stale_refid_falls_through_to_locator() {
        let r = registry(vec![el("button", "Save", 0.0, 0.0)]);
        let t = TargetDescriptor {
            refid: Some("e99".into()), // stale — destroyed by re-render
            locator: Some("div > button:nth-of-type(1)".into()),
            ..Default::default()
        };
        let (el, by) = r.resolve(&t).unwrap();
        assert_eq!(by, ResolvedBy::Locator);
        assert_eq!(el.text, "Save");
    }

    #[test]
    fn ambiguous_text_without_geometry_refuses_with_candidates() {
        let r = registry(vec![
            el("button", "Submit", 0.0, 0.0),
            el("button", "Submit", 0.0, 100.0),
            el("button", "Submit", 0.0, 200.0),
        ]);
        let t = TargetDescriptor { text: Some("Submit".into()), ..Default::default() };
        match r.resolve(&t) {
            Err(ResolutionFailure::Ambiguous(c)) => {
                assert_eq!(c.len(), 3);
                assert!(c[0].contains("e1"));
            }
            other => panic!("expected ambiguity refusal, got {other:?}"),
        }
    }

    #[test]
    fn ambiguous_text_with_geometry_disambiguates() {
        let r = registry(vec![
            el("button", "Submit", 0.0, 0.0),
            el("button", "Submit", 0.0, 200.0),
        ]);
        let t = TargetDescriptor {
            text: Some("Submit".into()),
            geometry: Some(Rect { x: 10.0, y: 210.0, w: 20.0, h: 20.0 }),
            ..Default::default()
        };
        let (el, by) = r.resolve(&t).unwrap();
        assert_eq!(by, ResolvedBy::Text);
        assert_eq!(el.refid, "e2");
    }

    #[test]
    fn empty_and_gone() {
        let r = registry(vec![el("button", "A", 0.0, 0.0)]);
        assert!(matches!(
            r.resolve(&TargetDescriptor::default()),
            Err(ResolutionFailure::Empty)
        ));
        let t = TargetDescriptor { refid: Some("e404".into()), ..Default::default() };
        assert!(matches!(r.resolve(&t), Err(ResolutionFailure::TargetGone)));
    }

    #[test]
    fn text_truncation_is_char_boundary_safe() {
        // 4-byte emoji at the cut boundary.
        let s = "😀".repeat(50);
        let cut = truncate_char_safe(&s, 10);
        assert_eq!(cut.chars().count(), 10);
        assert!(cut.chars().all(|c| c == '😀'));
    }

    #[test]
    fn normalization_collapses_whitespace() {
        assert_eq!(normalize_text("  Hello   \n world  "), "Hello world");
    }
}
