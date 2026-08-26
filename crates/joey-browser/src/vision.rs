//! Visual (Set-of-Mark) observation: screenshot + numbered markers, drawn
//! as an injected page overlay so no Rust image dependency is needed
//! (research.md D6; FR-013/014; contracts/snapshot-format.md).

use serde_json::json;

use crate::cdp::domains::ScreenshotResult;
use crate::cdp::BrowserError;
use crate::session::BrowserManager;
use crate::snapshot::{Marker, VisualObservation};

/// How many grid cells the coarse-grid strategy uses.
const GRID_COLS: usize = 6;
const GRID_ROWS: usize = 4;

impl BrowserManager {
    /// Capture a viewport screenshot with numbered markers burned in.
    ///
    /// `geometry_hints`: element rects (x, y, w, h) from the last scan; when
    /// empty/None the coarse grid strategy covers the viewport instead.
    pub async fn visual_observe(
        &self,
        geometry_hints: Option<&[(f64, f64, f64, f64)]>,
    ) -> Result<VisualObservation, BrowserError> {
        let page = self.ensure_page().await?;
        let has_hints = geometry_hints.map(|h| !h.is_empty()).unwrap_or(false);
        let markers: Vec<Marker> = if has_hints {
            let hints = geometry_hints.unwrap_or(&[]);
            hints
                .iter()
                .enumerate()
                .take(24)
                .map(|(i, &(x, y, w, h))| Marker {
                    id: format!("m{}", i + 1),
                    label: String::new(),
                    rect: crate::refs::Rect { x, y, w, h },
                })
                .collect()
        } else {
            coarse_grid()
        };
        let strategy = if has_hints { "dom_geometry" } else { "coarse_grid" };

        let spec: Vec<serde_json::Value> = markers
            .iter()
            .map(|m| {
                json!({
                    "id": m.id,
                    "x": m.rect.x,
                    "y": m.rect.y,
                    "w": m.rect.w,
                    "h": m.rect.h,
                    "label": m.label,
                })
            })
            .collect();
        let inject = crate::extract::MARKERS_JS.replace(
            "%MARKER_SPEC%",
            &serde_json::to_string(&spec).unwrap_or_else(|_| "[]".into()),
        );
        self.evaluate(&inject).await?;

        let shot: ScreenshotResult = serde_json::from_value(
            self.conn()?
                .send(
                    "Page.captureScreenshot",
                    json!({ "format": "png" }),
                    Some(&page.session_id),
                )
                .await?,
        )
        .map_err(|e| BrowserError::Protocol(format!("screenshot decode: {e}")))?;

        let _ = self.evaluate(crate::extract::CLEANUP_MARKERS_JS).await;

        let marker_table = markers
            .iter()
            .map(|m| format!("{} ({:.0},{:.0})", m.id, m.rect.x, m.rect.y + m.rect.h / 2.0))
            .collect::<Vec<_>>()
            .join(" · ");

        Ok(VisualObservation {
            image: format!("data:image/png;base64,{}", shot.data),
            strategy: strategy.to_string(),
            markers,
            marker_table,
        })
    }
}

/// Evenly spaced grid cells covering the viewport (nothing-discoverable case).
fn coarse_grid() -> Vec<Marker> {
    let mut out = Vec::new();
    for r in 0..GRID_ROWS {
        for c in 0..GRID_COLS {
            let w = 1280.0 / GRID_COLS as f64;
            let h = 800.0 / GRID_ROWS as f64;
            out.push(Marker {
                id: format!("m{}", r * GRID_COLS + c + 1),
                label: String::new(),
                rect: crate::refs::Rect {
                    x: c as f64 * w,
                    y: r as f64 * h,
                    w,
                    h,
                },
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_covers_viewport_with_sequential_ids() {
        let g = coarse_grid();
        assert_eq!(g.len(), GRID_COLS * GRID_ROWS);
        assert_eq!(g[0].id, "m1");
        assert_eq!(g.last().unwrap().id, format!("m{}", GRID_COLS * GRID_ROWS));
        assert!((g[1].rect.x - g[0].rect.x - g[0].rect.w).abs() < 1e-9);
    }

    #[test]
    fn marker_ids_unique() {
        let g = coarse_grid();
        let mut ids: Vec<&str> = g.iter().map(|m| m.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), g.len());
    }
}
