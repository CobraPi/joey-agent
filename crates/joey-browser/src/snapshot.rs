//! Snapshot construction: viewport-priority presentation, out-of-view
//! summaries, feed deltas, budgets and truncation (contracts/snapshot-format.md;
//! FR-004a, FR-012).

use serde::{Deserialize, Serialize};

use crate::config::SnapshotBudgets;
use crate::refs::{ElementRef, Rect};

/// Observation mode (FR-014: explicit in every snapshot).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObsMode {
    Structural,
    Visual,
}

/// Compact out-of-view region summary (FR-004a).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionSummary {
    /// `above | below | left | right` or a named panel region.
    pub region: String,
    pub direction: String,
    /// role → count.
    pub counts: std::collections::BTreeMap<String, usize>,
    /// ≤80 chars, from nearest heading/landmark.
    pub note: String,
}

/// Detected blocking overlay record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blocker {
    pub kind: String,
    pub description: String,
    pub frame: String,
    /// `auto_dismissed | refused_unsafe | flagged`.
    pub dismissal: String,
}

/// Feed delta (FR-012).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delta {
    pub new_elements: Vec<ElementRef>,
    pub gone_refids: Vec<String>,
    pub out_of_view: Vec<RegionSummary>,
    pub cumulative_bytes: usize,
    pub cumulative_cap_bytes: usize,
}

/// Visual (Set-of-Mark) observation payload (mode = visual).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualObservation {
    /// data:image/png;base64,… (markers burned in).
    pub image: String,
    /// `dom_geometry | coarse_grid`.
    pub strategy: String,
    pub markers: Vec<Marker>,
    pub marker_table: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Marker {
    pub id: String,
    pub label: String,
    pub rect: Rect,
}

/// Truncation info (never silent — FR-004a/FR-012).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TruncationInfo {
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted: Option<usize>,
}

impl Default for TruncationInfo {
    fn default() -> Self {
        Self { applied: false, reason: None, omitted: None }
    }
}

/// Viewport state at scan time.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Viewport {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub scroll_y: f64,
}

/// The model-facing observation unit (snapshot-format.md envelope).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub v: u32,
    pub mode: ObsMode,
    pub url: String,
    pub title: String,
    pub frame_count: usize,
    pub viewport: Viewport,
    /// Viewport-priority ordered (in-view first, then near-view).
    pub elements: Vec<ElementRef>,
    pub out_of_view: Vec<RegionSummary>,
    pub blockers: Vec<Blocker>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<Delta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visual: Option<VisualObservation>,
    pub truncation: TruncationInfo,
}

impl Snapshot {
    /// Viewport-priority ordering: in-view first (DOM order), then near-view
    /// (within `margin` viewport heights), then everything else is summarized.
    pub fn partition_viewport(
        elements: Vec<ElementRef>,
        viewport: Viewport,
        margin: f64,
    ) -> (Vec<ElementRef>, Vec<ElementRef>) {
        let in_view = |e: &ElementRef| {
            let g = &e.geometry;
            g.y + g.h >= viewport.y && g.y <= viewport.y + viewport.h
        };
        let near = |e: &ElementRef| {
            let g = &e.geometry;
            let band = viewport.h * margin;
            g.y + g.h >= viewport.y - band && g.y <= viewport.y + viewport.h + band
        };
        let mut in_v: Vec<ElementRef> = Vec::new();
        let mut out_v: Vec<ElementRef> = Vec::new();
        for e in elements {
            if in_view(&e) {
                in_v.push(e);
            } else if near(&e) {
                in_v.push(e); // near-view also listed fully (data-model §3)
            } else {
                out_v.push(e);
            }
        }
        (in_v, out_v)
    }

    /// Build a RegionSummary set from out-of-view elements.
    pub fn summarize_out_of_view(out: &[ElementRef], viewport: Viewport) -> Vec<RegionSummary> {
        let mut below: std::collections::BTreeMap<String, usize> = Default::default();
        let mut above: std::collections::BTreeMap<String, usize> = Default::default();
        for e in out {
            let bucket = if e.geometry.y >= viewport.y + viewport.h { &mut below } else { &mut above };
            *bucket.entry(e.role.clone()).or_insert(0) += 1;
        }
        let mut regions = Vec::new();
        for (region, direction, counts) in [
            ("below", "down", below),
            ("above", "up", above),
        ] {
            if !counts.is_empty() {
                let total: usize = counts.values().sum();
                regions.push(RegionSummary {
                    region: region.into(),
                    direction: direction.into(),
                    counts,
                    note: format!("{total} more interactive element(s) out of view"),
                });
            }
        }
        regions
    }

    /// Enforce the per-step byte budget on serialized elements: drop the
    /// least-recently-relevant (farthest below) elements to summary form.
    /// Never silent: truncation info always records the reason + omitted.
    pub fn enforce_step_budget(
        elements: Vec<ElementRef>,
        out_of_view: &mut Vec<RegionSummary>,
        budgets: &SnapshotBudgets,
    ) -> (Vec<ElementRef>, TruncationInfo) {
        let serialized_len = |els: &[ElementRef]| -> usize {
            serde_json::to_string(els).map(|s| s.len()).unwrap_or(0)
        };
        let mut kept = elements;
        let mut omitted = 0usize;
        while serialized_len(&kept) > budgets.max_step_bytes && kept.len() > 1 {
            // Drop the bottom-most element (farthest from viewport top).
            if let Some(idx) = kept
                .iter()
                .enumerate()
                .max_by(|a, b| {
                    a.1.geometry
                        .y
                        .partial_cmp(&b.1.geometry.y)
                        .expect("finite f64")
                })
                .map(|(i, _)| i)
            {
                let dropped = kept.remove(idx);
                omitted += 1;
                // Fold into the "below" summary.
                let needs_below = !out_of_view.iter().any(|r| r.region == "below");
                if needs_below {
                    out_of_view.push(RegionSummary {
                        region: "below".into(),
                        direction: "down".into(),
                        counts: Default::default(),
                        note: String::new(),
                    });
                }
                let entry = out_of_view
                    .iter_mut()
                    .find(|r| r.region == "below")
                    .expect("just pushed");
                *entry.counts.entry(dropped.role.clone()).or_insert(0) += 1;
            }
        }
        let applied = omitted > 0;
        (
            kept,
            TruncationInfo {
                applied,
                reason: applied.then(|| "step_budget".to_string()),
                omitted: applied.then_some(omitted),
            },
        )
    }

    /// Compute a feed delta against the previous snapshot's elements
    /// (identity = normalized text + role + frame).
    pub fn compute_delta(
        prev: &Snapshot,
        current_elements: &[ElementRef],
        out_of_view: Vec<RegionSummary>,
        budgets: &SnapshotBudgets,
    ) -> Delta {
        let key = |e: &ElementRef| (e.role.clone(), crate::refs::normalize_text(&e.text), e.frame.clone());
        let prev_keys: std::collections::HashSet<(String, String, String)> =
            prev.elements.iter().map(key).collect();
        let mut new_elements = Vec::new();
        for e in current_elements {
            if !prev_keys.contains(&key(e)) {
                new_elements.push(e.clone());
            }
        }
        // gone: previous keys not present now
        let cur_keys: std::collections::HashSet<(String, String, String)> =
            current_elements.iter().map(key).collect();
        let gone_refids: Vec<String> = prev
            .elements
            .iter()
            .filter(|e| !cur_keys.contains(&key(e)))
            .map(|e| e.refid.clone())
            .collect();
        let cumulative_bytes = prev
            .delta
            .as_ref()
            .map(|d| d.cumulative_bytes)
            .unwrap_or(0)
            + serde_json::to_string(&new_elements).map(|s| s.len()).unwrap_or(0);
        Delta {
            new_elements,
            gone_refids,
            out_of_view,
            cumulative_bytes,
            cumulative_cap_bytes: budgets.cumulative_cap_bytes,
        }
    }

    /// Compact line grammar (snapshot-format.md): token-efficient rendering.
    pub fn render_line(e: &ElementRef) -> String {
        let mut s = format!(
            "{} [{}] \"{}\" @{} ({:.0},{:.0} {:.0}x{:.0})",
            e.refid,
            e.role,
            crate::refs::truncate_char_safe(&e.text, 60),
            e.frame,
            e.geometry.x,
            e.geometry.y,
            e.geometry.w,
            e.geometry.h
        );
        s.push_str(&format!(" locator={}", e.locator));
        if !e.interactable {
            s.push_str(" (not-interactable)");
        }
        s
    }

    /// Full JSON rendering: pretty ≤4KB else compact.
    pub fn render(&self) -> String {
        let compact = serde_json::to_string(self).unwrap_or_default();
        if compact.len() <= 4096 {
            serde_json::to_string_pretty(self).unwrap_or(compact)
        } else {
            compact
        }
    }
}

/// Convenience constructor used by the scan pipeline.
pub fn structural_snapshot(
    url: String,
    title: String,
    frame_count: usize,
    viewport: Viewport,
    elements: Vec<ElementRef>,
    blockers: Vec<Blocker>,
    budgets: &SnapshotBudgets,
    margin: f64,
) -> Snapshot {
    let (listed, out) = Self0::partition(elements, viewport, margin);
    let mut out_of_view = Snapshot::summarize_out_of_view(&out, viewport);
    let (elements, truncation) =
        Snapshot::enforce_step_budget(listed, &mut out_of_view, budgets);
    Snapshot {
        v: 1,
        mode: ObsMode::Structural,
        url,
        title,
        frame_count,
        viewport,
        elements,
        out_of_view,
        blockers,
        delta: None,
        visual: None,
        truncation,
    }
}

/// Internal helper so structural_snapshot can call partition without a Self.
struct Self0;
impl Self0 {
    fn partition(
        elements: Vec<ElementRef>,
        viewport: Viewport,
        margin: f64,
    ) -> (Vec<ElementRef>, Vec<ElementRef>) {
        Snapshot::partition_viewport(elements, viewport, margin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp() -> Viewport {
        Viewport { x: 0.0, y: 0.0, w: 1280.0, h: 800.0, scroll_y: 0.0 }
    }

    fn el(y: f64, role: &str, text: &str) -> ElementRef {
        ElementRef {
            refid: format!("e{}", (y as i64) + 1),
            role: role.into(),
            text: text.into(),
            value: None,
            frame: "main".into(),
            locator: format!("button:nth-of-type({})", (y as i64) + 1),
            geometry: Rect { x: 0.0, y, w: 100.0, h: 40.0 },
            attributes: Default::default(),
            interactable: true,
        }
    }

    fn budgets() -> SnapshotBudgets {
        SnapshotBudgets { max_step_bytes: 8192, cumulative_cap_bytes: 65_536, viewport_margin: 1.0 }
    }

    #[test]
    fn viewport_priority_partition() {
        let els = vec![
            el(400.0, "button", "middle"),   // in view
            el(5000.0, "link", "far below"), // out
            el(850.0, "button", "near"),     // near-view band
        ];
        let (in_v, out_v) = Snapshot::partition_viewport(els, vp(), 1.0);
        assert_eq!(in_v.len(), 2, "in + near listed fully");
        assert_eq!(out_v.len(), 1);
        assert_eq!(out_v[0].text, "far below");
    }

    #[test]
    fn out_of_view_summary_shape() {
        let out = vec![el(5000.0, "button", "a"), el(6000.0, "link", "b"), el(-500.0, "button", "c")];
        let regions = Snapshot::summarize_out_of_view(&out, vp());
        assert_eq!(regions.len(), 2, "below + above");
        let below = regions.iter().find(|r| r.region == "below").unwrap();
        assert_eq!(below.counts.get("button"), Some(&1));
        assert_eq!(below.counts.get("link"), Some(&1));
        let above = regions.iter().find(|r| r.region == "above").unwrap();
        assert_eq!(above.counts.get("button"), Some(&1));
    }

    #[test]
    fn step_budget_drops_farthest_first_with_truncation_info() {
        let many: Vec<ElementRef> = (0..200)
            .map(|i| el(1000.0 + (i as f64) * 10.0, "button", &format!("btn-{i}")))
            .collect();
        let mut oov = Vec::new();
        let tiny = SnapshotBudgets { max_step_bytes: 2048, ..budgets() };
        let (kept, trunc) = Snapshot::enforce_step_budget(many, &mut oov, &tiny);
        assert!(kept.len() < 200);
        assert!(trunc.applied);
        assert_eq!(trunc.reason.as_deref(), Some("step_budget"));
        assert!(trunc.omitted.unwrap() > 0);
        assert!(oov.iter().any(|r| r.region == "below"), "dropped folded into summary");
    }

    #[test]
    fn delta_new_and_gone() {
        let mut prev = structural_snapshot(
            "u".into(), "t".into(), 1, vp(),
            vec![el(100.0, "button", "A"), el(200.0, "button", "B")],
            vec![], &budgets(), 1.0,
        );
        let cur_els = vec![el(100.0, "button", "A"), el(300.0, "button", "C")];
        let d = Snapshot::compute_delta(&prev, &cur_els, vec![], &budgets());
        assert_eq!(d.new_elements.len(), 1);
        assert_eq!(d.new_elements[0].text, "C");
        // el(200)=e201 is the gone element ("B" not present in current).
        assert_eq!(d.gone_refids, vec!["e201".to_string()]);
        prev.delta = Some(d);
    }

    #[test]
    fn render_line_grammar() {
        let e = el(100.0, "button", "Save");
        let line = Snapshot::render_line(&e);
        assert!(line.starts_with("e"));
        assert!(line.contains("[button] \"Save\" @main (0,100 100x40)"));
        assert!(line.contains("locator=button:nth-of-type"));
    }

    #[test]
    fn render_pretty_under_4kb_compact_over() {
        let s = structural_snapshot("u".into(), "t".into(), 1, vp(), vec![el(10.0, "button", "x")], vec![], &budgets(), 1.0);
        assert!(s.render().contains('\n'), "small → pretty");
        let mut big_els = Vec::new();
        for i in 0..500 {
            big_els.push(el(10.0, "button", &format!("item number {i} with padding text")));
        }
        // Bypass step budget for this test (huge budget) to force >4KB.
        let big = SnapshotBudgets { max_step_bytes: 1_000_000, ..budgets() };
        let s2 = structural_snapshot("u".into(), "t".into(), 1, vp(), big_els, vec![], &big, 1.0);
        assert!(!s2.render().contains("\n  "), "large → compact");
    }
}
