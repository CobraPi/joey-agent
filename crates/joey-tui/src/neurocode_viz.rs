//! Interactive fullscreen explorer for the NeuroCode expansion window
//! (feature 015 follow-up).
//!
//! Opened by clicking the docked bottom-right context feed (`App::
//! neurocode_expanded`), this takes over the main screen with:
//!
//!   - a **graph canvas** — the primary target at the center, expanded nodes
//!     on concentric depth rings, typed edges drawn as colored lines;
//!     keyboard-spatial navigation, pan/zoom, click-to-select, and
//!     neighbor highlighting;
//!   - a **node browser** — the inclusion list (reason, depth, fan-in),
//!     synced with the canvas selection;
//!   - a **detail pane** — the selected node's full snapshot record;
//!   - a **raw-feed tab** — the exact text NeuroCode fed the model.
//!
//! Everything is deterministic and pure with respect to the snapshot:
//! `layout_nodes` maps (snapshot, camera) → cell positions, which the
//! renderer paints and the mouse hit-tests against.

use std::cell::RefCell;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::state::App;
use crate::theme::Theme;

use joey_neurocode::context::snapshot::{ContextGraphSnapshot, NodeSnapshot};

// ── Model ───────────────────────────────────────────────────────────────────

/// Which explorer pane is active (Tab cycles).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VizTab {
    /// Radial graph canvas (default).
    #[default]
    Graph,
    /// Inclusion list browser.
    Nodes,
    /// Raw assembled-context feed.
    Feed,
}

/// Explorer interaction state. Lives on `App::neurocode_viz`; reset whenever
/// a new snapshot lands or the explorer opens.
#[derive(Debug)]
pub struct VizState {
    pub tab: VizTab,
    /// Camera pan offset in terminal cells.
    pub pan: (i32, i32),
    /// Camera zoom (ring-radius multiplier).
    pub zoom: f32,
    /// Selected node (index into `snapshot.nodes`).
    pub selected: usize,
    /// Node-list cursor (kept in sync with `selected` when switching panes).
    pub list_cursor: usize,
    /// Raw-feed scroll (lines from tail).
    pub feed_scroll: usize,
    /// Detail-pane scroll (rows from top).
    pub detail_scroll: usize,
    /// Highlight the selected node's direct neighbors on the canvas.
    pub show_neighbors: bool,
    /// Canvas cell position of each node as drawn by the LAST frame
    /// (absolute screen cells, same order as `snapshot.nodes`). Interior
    /// mutability so the renderer can record from `&App`.
    pub node_cells: RefCell<Vec<(u16, u16)>>,
}

impl Default for VizState {
    /// zoom defaults to 1.0 (identity) — a derived-Default would leave it
    /// at 0.0 and collapse every depth ring onto the center.
    fn default() -> Self {
        Self {
            tab: VizTab::default(),
            pan: (0, 0),
            zoom: 1.0,
            selected: 0,
            list_cursor: 0,
            feed_scroll: 0,
            detail_scroll: 0,
            show_neighbors: true,
            node_cells: RefCell::new(Vec::new()),
        }
    }
}

impl VizState {
    pub fn reset(&mut self) {
        self.tab = VizTab::Graph;
        self.pan = (0, 0);
        self.zoom = 1.0;
        self.selected = 0;
        self.list_cursor = 0;
        self.feed_scroll = 0;
        self.detail_scroll = 0;
        self.show_neighbors = true;
        self.node_cells.borrow_mut().clear();
    }

    pub fn cycle_tab(&mut self) {
        self.tab = match self.tab {
            VizTab::Graph => VizTab::Nodes,
            VizTab::Nodes => VizTab::Feed,
            VizTab::Feed => VizTab::Graph,
        };
        // Keep list cursor synced with the canvas selection when arriving.
        if self.tab == VizTab::Nodes {
            self.list_cursor = self.selected;
        }
    }

    pub fn zoom_in(&mut self) {
        self.zoom = (self.zoom * 1.25).min(3.0);
    }

    pub fn zoom_out(&mut self) {
        self.zoom = (self.zoom / 1.25).max(0.4);
    }

    pub fn reset_view(&mut self) {
        self.pan = (0, 0);
        self.zoom = 1.0;
    }
}

// ── Layout ──────────────────────────────────────────────────────────────────

/// A node position in canvas-local cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodePos {
    pub x: i32,
    pub y: i32,
    /// Graph distance from the primary target (0 = primary).
    pub depth: usize,
}

/// Deterministic radial layout: primaries clustered at the center, expanded
/// nodes on concentric rings by depth, angle slot by index within the depth
/// group (with a per-ring twist so parents/children don't line up radially).
///
/// Pure function of the snapshot + camera — unit-testable and stable across
/// frames (no animation jitter).
pub fn layout_nodes(
    snapshot: &ContextGraphSnapshot,
    cx: i32,
    cy: i32,
    pan: (i32, i32),
    zoom: f32,
) -> Vec<NodePos> {
    let n = snapshot.nodes.len();
    if n == 0 {
        return Vec::new();
    }
    let max_depth = snapshot
        .nodes
        .iter()
        .map(|nd| nd.depth)
        .max()
        .unwrap_or(0)
        .max(1);

    // Count nodes per depth ring.
    let mut per_depth: Vec<usize> = vec![0; max_depth + 1];
    for nd in &snapshot.nodes {
        per_depth[nd.depth.min(max_depth)] += 1;
    }
    let mut filled: Vec<usize> = vec![0; max_depth + 1];

    // Multiple primaries: fan them slightly around the center.
    let primaries = per_depth[0].max(1);

    snapshot
        .nodes
        .iter()
        .map(|nd| {
            if nd.depth == 0 {
                // Primary cluster near the center.
                let k = filled[0];
                filled[0] += 1;
                let angle = (k as f32) * (std::f32::consts::TAU / primaries as f32);
                let r = if primaries <= 1 { 0.0 } else { 3.0 };
                NodePos {
                    x: cx + (angle.cos() * r).round() as i32,
                    y: cy + (angle.sin() * r * 0.5).round() as i32,
                    depth: 0,
                }
            } else {
                let d = nd.depth.min(max_depth);
                let k = filled[d];
                filled[d] += 1;
                let count = per_depth[d].max(1);
                // Twist each ring so children sit between their parents'
                // angular positions visually.
                let twist = (d as f32) * 0.5;
                let angle = (k as f32 + twist) * (std::f32::consts::TAU / count as f32)
                    + (d as f32 * 0.35);
                // Ring radius: 6 cells per depth step, scaled by zoom.
                let r = (6.0 * d as f32 * zoom).max(0.0);
                NodePos {
                    x: cx + (angle.cos() * r).round() as i32 + pan.0,
                    y: cy + (angle.sin() * r * 0.45).round() as i32 + pan.1,
                    depth: d,
                }
            }
        })
        .collect()
}

/// Bresenham-ish integer line stepping between two cell points. Returns the
/// interior cells (endpoints excluded) for edge drawing.
pub fn line_cells(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    let (dx, dy) = (x1 - x0, y1 - y0);
    let steps = dx.abs().max(dy.abs()).max(1);
    for i in 1..steps {
        let t = i as f32 / steps as f32;
        out.push((x0 + (dx as f32 * t).round() as i32, y0 + (dy as f32 * t).round() as i32));
    }
    out
}

/// Pick the best glyph for a line segment cell given the overall slope.
fn edge_glyph(dx: i32, dy: i32) -> char {
    let ax = dx.abs();
    let ay = dy.abs();
    if ay == 0 {
        '─'
    } else if ax == 0 {
        '│'
    } else if ax > ay * 2 {
        '─'
    } else if ay > ax * 2 {
        '│'
    } else if (dx > 0) == (dy > 0) {
        '╲'
    } else {
        '╱'
    }
}

// ── Colors & glyphs ─────────────────────────────────────────────────────────

/// Node glyph by artifact kind.
fn node_glyph(kind: &str, selected: bool) -> char {
    if selected {
        return '◉';
    }
    match kind {
        "Class" => '◯',
        "Interface" => '◇',
        "Enum" => '◆',
        "PegaRule" => '✦',
        "Method" => '▫',
        "Field" => '∘',
        _ => '○',
    }
}

/// Reason-label → color role.
fn reason_color(theme: Theme, reason: &str) -> ratatui::style::Color {
    match reason {
        "implements" => theme.info.to_color(),
        "injects" => theme.secondary.to_color(),
        "exchanges type" => theme.keyword.to_color(),
        "references rule" => theme.gold.to_color(),
        "inherits rule" => theme.success.to_color(),
        "member of" => theme.fg_more_subtle.to_color(),
        _ => theme.fg_subtle.to_color(),
    }
}

/// Edge-kind tag → color role (kept aligned with reason colors).
fn edge_color(theme: Theme, kind: &str) -> ratatui::style::Color {
    match kind {
        "Implements" | "IsImplementedBy" => theme.info.to_color(),
        "Injects" => theme.secondary.to_color(),
        "ExchangesType" => theme.keyword.to_color(),
        "MemberOf" => theme.fg_most_subtle.to_color(),
        "ReferencesRule" => theme.gold.to_color(),
        "InheritsRule" => theme.success.to_color(),
        _ => theme.fg_more_subtle.to_color(),
    }
}

fn fmt_tokens(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

// ── Interaction ─────────────────────────────────────────────────────────────

impl VizState {
    /// Move the canvas selection to the nearest node in `dir` (dx, dy)
    /// relative to the currently selected node's screen cell — radial-menu
    /// style spatial navigation. No-op when it would leave the last node.
    pub fn select_directional(
        &mut self,
        _snapshot: &ContextGraphSnapshot,
        dx: i32,
        dy: i32,
    ) {
        let cells = self.node_cells.borrow().clone();
        if cells.is_empty() || self.selected >= cells.len() {
            return;
        }
        let (sx, sy) = cells[self.selected];
        let (sx, sy) = (sx as i32, sy as i32);
        let mut best: Option<(f32, usize)> = None;
        for (idx, (nx, ny)) in cells.iter().enumerate() {
            if idx == self.selected {
                continue;
            }
            let (nx, ny) = (*nx as i32, *ny as i32);
            let (vx, vy) = (nx - sx, ny - sy);
            // Must make progress in the requested direction.
            let dot = vx as f32 * dx as f32 + vy as f32 * dy as f32;
            if dot <= 0.0 {
                continue;
            }
            // Score: prefer small angular deviation from the direction.
            let vlen = ((vx * vx + vy * vy) as f32).sqrt().max(1.0);
            let dlen = ((dx * dx + dy * dy) as f32).sqrt().max(1.0);
            let cos = dot / (vlen * dlen);
            // Prefer closer nodes on ties: + small distance penalty.
            let score = -cos + vlen * 0.002;
            if best.map(|(b, _)| score < b).unwrap_or(true) {
                best = Some((score, idx));
            }
        }
        if let Some((_, idx)) = best {
            self.selected = idx;
            self.list_cursor = idx;
            self.detail_scroll = 0;
        }
    }

    /// Select an absolute node index (click or list jump), clamped.
    pub fn select(&mut self, idx: usize) {
        self.selected = idx;
        self.list_cursor = idx;
        self.detail_scroll = 0;
    }

    /// Move the node-list cursor by delta, clamped, syncing the selection.
    pub fn list_move(&mut self, snapshot: &ContextGraphSnapshot, up: bool) {
        let max = snapshot.nodes.len().saturating_sub(1);
        self.list_cursor = if up {
            self.list_cursor.saturating_sub(1)
        } else {
            (self.list_cursor + 1).min(max)
        };
        self.selected = self.list_cursor;
        self.detail_scroll = 0;
    }
}

/// Handle a mouse click inside the explorer area. Returns true when the
/// click docked the explorer (caller stops). `title_row` is the explorer's
/// first screen row — the dock affordance.
pub fn explorer_click(app: &mut App, row: u16, col: u16, area: Rect) -> bool {
    // Title bar click docks.
    if row <= area.y {
        app.toggle_neurocode_expanded();
        return true;
    }
    let Some(snapshot) = app.neurocode_snapshot.as_ref() else {
        // No graph — the whole area is the raw feed; dock on any click to
        // preserve the pre-visualization behavior.
        app.toggle_neurocode_expanded();
        return true;
    };
    let viz = &mut app.neurocode_viz;
    // Node-list pane hit → select that row.
    let (lx, ly, lw, lh) = app.last_viz_nodes_rect.get();
    if lw > 0 && row >= ly && row < ly + lh && col >= lx && col < lx + lw {
        let inner_y = row.saturating_sub(ly).saturating_sub(1); // -1 border
        if let Some(idx) = list_row_to_index(snapshot, inner_y as usize) {
            viz.select(idx);
        }
        return false;
    }
    // Canvas: nearest node within a small radius wins; else ignore (pan is
    // keyboard-driven; accidental docks are worse than no-ops).
    let cells = viz.node_cells.borrow().clone();
    let mut best: Option<(u32, usize)> = None;
    for (idx, (nx, ny)) in cells.iter().enumerate() {
        let d = nx.abs_diff(col).max(ny.abs_diff(row)) as u32;
        if d <= 2 && best.map(|(bd, _)| d < bd).unwrap_or(true) {
            best = Some((d, idx));
        }
    }
    if let Some((_, idx)) = best {
        viz.select(idx);
    }
    false
}

/// Map a node-list inner row to a node index, accounting for the header
/// row offset. Pure; mirrors the renderer's row order.
fn list_row_to_index(snapshot: &ContextGraphSnapshot, inner_row: usize) -> Option<usize> {
    // Row 0 is the column header; rows 1.. are nodes in snapshot order.
    inner_row.checked_sub(1).filter(|&i| i < snapshot.nodes.len())
}

/// Handle a mouse-wheel event inside the explorer area.
pub fn explorer_scroll(app: &mut App, row: u16, col: u16, up: bool) {
    let Some(snapshot) = app.neurocode_snapshot.as_ref() else {
        // Raw-feed fallback: scroll the feed.
        if up {
            app.neurocode_scroll = app.neurocode_scroll.saturating_add(3);
        } else {
            app.neurocode_scroll = app.neurocode_scroll.saturating_sub(3);
        }
        return;
    };
    // Wheel over the node-list pane scrolls the list (even on the graph
    // tab, where the list shares the screen with the canvas).
    let (lx, ly, lw, lh) = app.last_viz_nodes_rect.get();
    if lw > 0 && row >= ly && row < ly + lh && col >= lx && col < lx + lw {
        app.neurocode_viz.list_move(snapshot, up);
        return;
    }
    match app.neurocode_viz.tab {
        VizTab::Graph => {
            // Wheel over the canvas zooms.
            if up {
                app.neurocode_viz.zoom_in();
            } else {
                app.neurocode_viz.zoom_out();
            }
        }
        VizTab::Nodes => {
            app.neurocode_viz.list_move(snapshot, up);
        }
        VizTab::Feed => {
            if up {
                app.neurocode_scroll = app.neurocode_scroll.saturating_add(3);
            } else {
                app.neurocode_scroll = app.neurocode_scroll.saturating_sub(3);
            }
        }
    }
}

/// Handle a key while the explorer is open. Returns true when the key was
/// consumed (caller must not forward it). Esc is NOT handled here — the
/// global Esc handler docks the explorer. Shift+arrows pan the canvas.
pub fn explorer_key(app: &mut App, key: &crossterm::event::KeyEvent) -> bool {
    use crossterm::event::KeyCode;
    let has_snapshot = app.neurocode_snapshot.is_some();
    let shift = key
        .modifiers
        .contains(crossterm::event::KeyModifiers::SHIFT);
    // Shift+arrows pan the canvas (all tabs; harmless elsewhere).
    if shift {
        let consumed = match key.code {
            KeyCode::Up => {
                app.neurocode_viz.pan.1 = app.neurocode_viz.pan.1.saturating_sub(2);
                true
            }
            KeyCode::Down => {
                app.neurocode_viz.pan.1 = app.neurocode_viz.pan.1.saturating_add(2);
                true
            }
            KeyCode::Left => {
                app.neurocode_viz.pan.0 = app.neurocode_viz.pan.0.saturating_sub(4);
                true
            }
            KeyCode::Right => {
                app.neurocode_viz.pan.0 = app.neurocode_viz.pan.0.saturating_add(4);
                true
            }
            _ => false,
        };
        if consumed {
            return true;
        }
    }
    let code = key.code;
    let viz = &mut app.neurocode_viz;
    match code {
        KeyCode::Tab => {
            viz.cycle_tab();
            true
        }
        KeyCode::BackTab => {
            viz.cycle_tab();
            viz.cycle_tab();
            true
        }
        _ if !has_snapshot => {
            // Raw-feed fallback: only scrolling keys are explorer-owned.
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    app.neurocode_scroll = app.neurocode_scroll.saturating_add(1);
                    true
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.neurocode_scroll = app.neurocode_scroll.saturating_sub(1);
                    true
                }
                _ => false,
            }
        }
        KeyCode::Char('+') | KeyCode::Char('=') => {
            viz.zoom_in();
            true
        }
        KeyCode::Char('-') => {
            viz.zoom_out();
            true
        }
        KeyCode::Char('0') => {
            viz.reset_view();
            true
        }
        KeyCode::Char(' ') => {
            viz.show_neighbors = !viz.show_neighbors;
            true
        }
        KeyCode::Enter => {
            // Graph → jump to the Nodes pane with this node under the
            // cursor; Nodes → jump back to the canvas re-centered on the
            // selection; Feed → back to the graph.
            viz.tab = match viz.tab {
                VizTab::Graph => VizTab::Nodes,
                _ => VizTab::Graph,
            };
            if viz.tab == VizTab::Nodes {
                viz.list_cursor = viz.selected;
            } else {
                // Re-center: zero the pan so the selection's ring is framed.
                viz.pan = (0, 0);
            }
            true
        }
        KeyCode::Char('g') if viz.tab == VizTab::Nodes => {
            viz.select(0);
            true
        }
        KeyCode::Char('G') if viz.tab == VizTab::Nodes => {
            if let Some(snap) = app.neurocode_snapshot.as_ref() {
                let last = snap.nodes.len().saturating_sub(1);
                viz.select(last);
            }
            true
        }
        KeyCode::Up | KeyCode::Char('k') => {
            match viz.tab {
                VizTab::Graph => {
                    if let Some(snap) = app.neurocode_snapshot.as_ref() {
                        viz.select_directional(snap, 0, -1);
                    }
                }
                VizTab::Nodes => {
                    if let Some(snap) = app.neurocode_snapshot.as_ref() {
                        viz.list_move(snap, true);
                    }
                }
                VizTab::Feed => {
                    app.neurocode_scroll = app.neurocode_scroll.saturating_add(1);
                }
            }
            true
        }
        KeyCode::Down | KeyCode::Char('j') => {
            match viz.tab {
                VizTab::Graph => {
                    if let Some(snap) = app.neurocode_snapshot.as_ref() {
                        viz.select_directional(snap, 0, 1);
                    }
                }
                VizTab::Nodes => {
                    if let Some(snap) = app.neurocode_snapshot.as_ref() {
                        viz.list_move(snap, false);
                    }
                }
                VizTab::Feed => {
                    app.neurocode_scroll = app.neurocode_scroll.saturating_sub(1);
                }
            }
            true
        }
        KeyCode::Left | KeyCode::Char('h') => {
            match viz.tab {
                VizTab::Graph => {
                    if let Some(snap) = app.neurocode_snapshot.as_ref() {
                        viz.select_directional(snap, -1, 0);
                    }
                }
                VizTab::Feed => {
                    app.neurocode_scroll = app.neurocode_scroll.saturating_add(2);
                }
                _ => {}
            }
            true
        }
        KeyCode::Right | KeyCode::Char('l') => {
            match viz.tab {
                VizTab::Graph => {
                    if let Some(snap) = app.neurocode_snapshot.as_ref() {
                        viz.select_directional(snap, 1, 0);
                    }
                }
                VizTab::Feed => {
                    app.neurocode_scroll = app.neurocode_scroll.saturating_sub(2);
                }
                _ => {}
            }
            true
        }
        _ => false,
    }
}

// ── Rendering ───────────────────────────────────────────────────────────────

/// Draw the fullscreen explorer. Records its rect into `last_neurocode_rect`
/// (mouse routing) and the node-list pane into `last_viz_nodes_rect`.
pub fn draw_explorer(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    app.last_neurocode_rect
        .set((area.x, area.y, area.width, area.height));
    app.last_viz_nodes_rect.set((0, 0, 0, 0));

    let snapshot = app.neurocode_snapshot.as_ref();

    // Title bar (also the dock affordance for mouse clicks).
    let title = format!(
        " ⚡ neurocode explorer · {} · click title or Esc to dock ",
        match app.neurocode_viz.tab {
            VizTab::Graph => "graph",
            VizTab::Nodes => "nodes",
            VizTab::Feed => "feed",
        }
    );
    let block = crate::widgets::gradient_block(&title, theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 4 || inner.height < 2 {
        return;
    }

    // No snapshot → raw-feed fallback (identical content to the old
    // expanded feed, so nothing regresses while cold/un-indexed).
    let Some(snapshot) = snapshot else {
        draw_fallback_feed(f, inner, app, theme);
        return;
    };

    // Chrome: stats/tab strip (top) + key-hint footer (bottom), content
    // between them.
    let rows = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Length(1),
            ratatui::layout::Constraint::Min(3),
            ratatui::layout::Constraint::Length(1),
        ])
        .split(inner);

    // Stats line + tab strip.
    let expanded = snapshot.nodes.iter().filter(|n| !n.primary).count();
    let tab_span = |label: &str, active: bool| {
        Span::styled(
            format!("[{}] ", label),
            Style::default()
                .fg(if active {
                    theme.accent.to_color()
                } else {
                    theme.fg_more_subtle.to_color()
                })
                .add_modifier(if active {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        )
    };
    let dropped = if snapshot.budget.dropped_for_budget > 0 {
        format!(" · {} dropped", snapshot.budget.dropped_for_budget)
    } else {
        String::new()
    };
    let edge_word = if snapshot.edges.len() == 1 { "edge" } else { "edges" };
    let stats = format!(
        "  {} tier {} · Σ{} tok · {} nodes ({} target + {} expanded) · {} {} · budget {}/{}{}",
        tab_dot(app.neurocode_viz.tab == VizTab::Graph),
        snapshot.tier,
        fmt_tokens(snapshot.token_estimate),
        snapshot.nodes.len(),
        snapshot.nodes.iter().filter(|n| n.primary).count(),
        expanded,
        snapshot.edges.len(),
        edge_word,
        expanded,
        snapshot.budget.max_expanded_nodes,
        dropped,
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            tab_span("1 graph", app.neurocode_viz.tab == VizTab::Graph),
            tab_span("2 nodes", app.neurocode_viz.tab == VizTab::Nodes),
            tab_span("3 feed", app.neurocode_viz.tab == VizTab::Feed),
            Span::styled(
                stats,
                Style::default().fg(theme.fg_more_subtle.to_color()),
            ),
        ])),
        rows[0],
    );

    // Content.
    match app.neurocode_viz.tab {
        VizTab::Graph => {
            let content = rows[1];
            if content.width >= 48 {
                // Canvas (left) | right column (list + detail).
                let right_w = 36u16.min(content.width / 2);
                let cols = ratatui::layout::Layout::default()
                    .direction(ratatui::layout::Direction::Horizontal)
                    .constraints([
                        ratatui::layout::Constraint::Min(20),
                        ratatui::layout::Constraint::Length(right_w),
                    ])
                    .split(content);
                draw_canvas(f, cols[0], app, theme, snapshot);

                let detail_h = (cols[1].height / 2).clamp(4, 14).min(cols[1].height);
                let right_rows = ratatui::layout::Layout::default()
                    .direction(ratatui::layout::Direction::Vertical)
                    .constraints([
                        ratatui::layout::Constraint::Min(3),
                        ratatui::layout::Constraint::Length(detail_h),
                    ])
                    .split(cols[1]);
                draw_node_list(f, right_rows[0], app, theme, snapshot);
                draw_detail(f, right_rows[1], app, theme, snapshot);
            } else {
                // Narrow: canvas only.
                draw_canvas(f, content, app, theme, snapshot);
            }
        }
        VizTab::Nodes => {
            let content = rows[1];
            let detail_h = (content.height / 3).clamp(4, 12).min(content.height);
            let vrows = ratatui::layout::Layout::default()
                .direction(ratatui::layout::Direction::Vertical)
                .constraints([
                    ratatui::layout::Constraint::Min(3),
                    ratatui::layout::Constraint::Length(detail_h),
                ])
                .split(content);
            draw_node_list(f, vrows[0], app, theme, snapshot);
            draw_detail(f, vrows[1], app, theme, snapshot);
        }
        VizTab::Feed => {
            draw_feed_pane(f, rows[1], app, theme);
        }
    }

    // Footer: per-tab key hints + reason legend on the graph tab.
    let footer = match app.neurocode_viz.tab {
        VizTab::Graph => {
            " ←→↑↓ select · Shift+←→↑↓ pan · wheel/+- zoom · 0 reset · Tab pane · ⏎ nodes · Space neighbors "
        }
        VizTab::Nodes => " ↑↓/jk move · ⏎ back to graph · g/G ends · Tab pane ",
        VizTab::Feed => " ↑↓ scroll · Tab pane ",
    };
    f.render_widget(
        Paragraph::new(Line::styled(
            footer.to_string(),
            Style::default().fg(theme.fg_most_subtle.to_color()),
        )),
        rows[2],
    );
}

fn tab_dot(active: bool) -> char {
    if active {
        '●'
    } else {
        '○'
    }
}

/// Raw-feed fallback (no snapshot): the old expanded-feed rendering —
/// tail-anchored scrolling window over `app.neurocode_context`.
fn draw_fallback_feed(f: &mut Frame, inner: Rect, app: &App, theme: Theme) {
    let cw = inner.width as usize;
    let lines: Vec<Line> = if app.neurocode_context.is_empty() {
        vec![Line::styled(
            " (no context assembled yet — send a prompt)".to_string(),
            Style::default().fg(theme.fg_most_subtle.to_color()),
        )]
    } else {
        let wrapped = textwrap::wrap(&app.neurocode_context, cw.saturating_sub(2).max(10));
        let body: Vec<Line> = wrapped
            .iter()
            .map(|w| {
                Line::styled(
                    format!(" {}", w),
                    Style::default().fg(theme.fg_subtle.to_color()),
                )
            })
            .collect();
        let visible = inner.height as usize;
        let total = body.len();
        if total <= visible {
            body
        } else {
            let scroll = app.neurocode_scroll.min(total - visible);
            let start = total - visible - scroll;
            body[start..start + visible].to_vec()
        }
    };
    f.render_widget(Paragraph::new(lines), inner);
}

/// The feed tab: same content as the fallback but under the explorer chrome.
fn draw_feed_pane(f: &mut Frame, inner: Rect, app: &App, theme: Theme) {
    draw_fallback_feed(f, inner, app, theme);
}

/// The node browser list.
fn draw_node_list(f: &mut Frame, area: Rect, app: &App, theme: Theme, snapshot: &ContextGraphSnapshot) {
    if area.width < 6 || area.height < 2 {
        return;
    }
    app.last_viz_nodes_rect
        .set((area.x, area.y, area.width, area.height));
    let block = crate::widgets::gradient_block(" nodes in context ", theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 4 || inner.height < 1 {
        return;
    }

    let cw = inner.width as usize;
    let mut lines: Vec<Line> = Vec::with_capacity(snapshot.nodes.len() + 1);
    lines.push(Line::from(vec![
        Span::styled(
            format!("{:<2}", "#"),
            Style::default().fg(theme.fg_most_subtle.to_color()),
        ),
        Span::styled(
            format!("{:<w$}", "node", w = (cw / 3).max(12).min(24)),
            Style::default().fg(theme.fg_most_subtle.to_color()),
        ),
        Span::styled(
            "why included".to_string(),
            Style::default().fg(theme.fg_most_subtle.to_color()),
        ),
    ]));

    let name_w = (cw / 3).max(12).min(24);
    for (idx, nd) in snapshot.nodes.iter().enumerate() {
        let selected = idx == app.neurocode_viz.list_cursor;
        let name: String = nd.name.chars().take(name_w).collect();
        let reason = match (&nd.reason, &nd.via) {
            (Some(r), Some(v)) => format!("{} ← {}", r, v),
            (Some(r), None) => r.clone(),
            (None, _) => "TARGET".to_string(),
        };
        let glyph = if nd.primary { '◆' } else { node_glyph(&nd.kind, false) };
        let reason_col = if nd.primary {
            theme.accent.to_color()
        } else {
            reason_color(theme, nd.reason.as_deref().unwrap_or(""))
        };
        let mut spans = vec![
            Span::styled(
                format!("{:<2}", glyph),
                Style::default().fg(reason_col),
            ),
            Span::styled(
                format!("{:<w$}", name, w = name_w),
                Style::default()
                    .fg(if selected {
                        theme.fg_base.to_color()
                    } else {
                        theme.fg_subtle.to_color()
                    })
                    .add_modifier(if selected {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(
                format!("d{} ·Δ{} ", nd.depth, nd.fan_in),
                Style::default().fg(theme.fg_more_subtle.to_color()),
            ),
            Span::styled(reason, Style::default().fg(reason_col)),
        ];
        if selected {
            spans.insert(
                0,
                Span::styled("›".to_string(), Style::default().fg(theme.accent.to_color())),
            );
        } else {
            spans.insert(0, Span::raw(" ".to_string()));
        }
        lines.push(Line::from(spans));
    }

    // Window the list around the cursor.
    let visible = inner.height as usize;
    let total = lines.len();
    let cursor_row = app.neurocode_viz.list_cursor + 1; // +1 header
    let start = if total <= visible {
        0
    } else if cursor_row >= visible {
        (cursor_row + 1 - visible).min(total - visible)
    } else {
        0
    };
    let end = (start + visible).min(total);
    f.render_widget(Paragraph::new(lines[start..end].to_vec()), inner);
}

/// The selected-node detail pane.
fn draw_detail(f: &mut Frame, area: Rect, app: &App, theme: Theme, snapshot: &ContextGraphSnapshot) {
    if area.width < 8 || area.height < 2 || snapshot.nodes.is_empty() {
        return;
    }
    let block = crate::widgets::gradient_block(" detail ", theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 4 || inner.height < 1 {
        return;
    }
    let sel = app
        .neurocode_viz
        .selected
        .min(snapshot.nodes.len() - 1);
    let nd: &NodeSnapshot = &snapshot.nodes[sel];
    let cw = inner.width as usize;

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            format!("{} ", node_glyph(&nd.kind, true)),
            Style::default().fg(theme.accent.to_color()),
        ),
        Span::styled(
            nd.name.clone(),
            Style::default()
                .fg(theme.fg_base.to_color())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {}", nd.kind),
            Style::default().fg(theme.fg_more_subtle.to_color()),
        ),
        if nd.primary {
            Span::styled(
                "  TARGET".to_string(),
                Style::default().fg(theme.accent.to_color()).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw(String::new())
        },
    ]));
    let kv = |lines: &mut Vec<Line>, k: &str, v: String| {
        for (i, chunk) in textwrap::wrap(&v, cw.saturating_sub(12).max(8))
            .into_iter()
            .enumerate()
        {
            let label = if i == 0 { format!("{:<10}", k) } else { format!("{:<10}", "") };
            lines.push(Line::from(vec![
                Span::styled(label, Style::default().fg(theme.fg_more_subtle.to_color())),
                Span::styled(chunk.to_string(), Style::default().fg(theme.fg_subtle.to_color())),
            ]));
        }
    };
    kv(&mut lines, "fqcn", nd.fqcn.clone());
    if !nd.source_path.is_empty() {
        kv(&mut lines, "file", nd.source_path.clone());
    }
    if !nd.package.is_empty() {
        kv(&mut lines, "package", nd.package.clone());
    }
    if let Some(reason) = &nd.reason {
        let via = nd
            .via
            .as_ref()
            .map(|v| format!(" (via {})", v))
            .unwrap_or_default();
        kv(&mut lines, "included", format!("{}{}", reason, via));
    }
    kv(&mut lines, "depth", format!("{}", nd.depth));
    kv(&mut lines, "fan-in", format!("{} dependents", nd.fan_in));
    if !nd.annotations.is_empty() {
        kv(&mut lines, "annot", format!("@{}", nd.annotations.join(" @")));
    }
    if !nd.interfaces.is_empty() {
        kv(&mut lines, "impl", nd.interfaces.join(", "));
    }
    if !nd.dependencies.is_empty() {
        kv(&mut lines, "deps", nd.dependencies.join(", "));
    }
    // Member roster (methods/fields), truncated to fit.
    if !nd.members.is_empty() {
        lines.push(Line::styled(
            format!(" members ({})", nd.members.len()),
            Style::default().fg(theme.fg_more_subtle.to_color()),
        ));
        for m in nd.members.iter().take((inner.height as usize).saturating_sub(lines.len() + 1)) {
            let sig = if m.signature.is_empty() {
                m.name.clone()
            } else {
                m.signature.clone()
            };
            let trunc: String = sig.chars().take(cw.saturating_sub(4)).collect();
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {} ", if m.kind == "method" { "ƒ" } else { "·" }),
                    Style::default().fg(
                        if m.kind == "method" {
                            theme.info.to_color()
                        } else {
                            theme.fg_more_subtle.to_color()
                        },
                    ),
                ),
                Span::styled(trunc, Style::default().fg(theme.fg_subtle.to_color())),
            ]));
        }
    }

    // Scroll window over the detail lines.
    let visible = inner.height as usize;
    let total = lines.len();
    let scroll = app.neurocode_viz.detail_scroll.min(total.saturating_sub(visible));
    let start = if total > visible { total - visible - scroll } else { 0 };
    let end = (start + visible).min(total);
    f.render_widget(Paragraph::new(lines[start..end].to_vec()), inner);
}

/// The radial graph canvas: edges first, then nodes + labels.
fn draw_canvas(f: &mut Frame, area: Rect, app: &App, theme: Theme, snapshot: &ContextGraphSnapshot) {
    let buf = f.buffer_mut();
    // Panel background so the particles don't bleed through.
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            buf[(x, y)].set_style(Style::default().bg(theme.bg_panel.to_color()));
        }
    }

    let viz = &app.neurocode_viz;
    let cx = area.x as i32 + (area.width as i32 / 2);
    let cy = area.y as i32 + (area.height as i32 / 2);
    let positions = layout_nodes(snapshot, cx, cy, viz.pan, viz.zoom);

    // Neighbor set of the selection (for highlighting).
    let selected = viz.selected.min(snapshot.nodes.len().saturating_sub(1));
    let neighbors: Vec<usize> = if viz.show_neighbors {
        snapshot
            .edges
            .iter()
            .filter_map(|e| {
                if e.from == selected {
                    Some(e.to)
                } else if e.to == selected {
                    Some(e.from)
                } else {
                    None
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    // Record absolute cells for hit-testing.
    let cells: Vec<(u16, u16)> = positions
        .iter()
        .map(|p| {
            (
                p.x.clamp(0, u16::MAX as i32) as u16,
                p.y.clamp(0, u16::MAX as i32) as u16,
            )
        })
        .collect();
    *viz.node_cells.borrow_mut() = cells;

    // Edges.
    for e in &snapshot.edges {
        let (a, b) = (positions[e.from], positions[e.to]);
        let active = e.from == selected || e.to == selected;
        let color = edge_color(theme, &e.kind);
        let style = Style::default()
            .fg(if active {
                color
            } else {
                theme.fg_most_subtle.to_color()
            })
            .bg(theme.bg_panel.to_color());
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let glyph = edge_glyph(dx, dy);
        for (lx, ly) in line_cells(a.x, a.y, b.x, b.y) {
            if lx >= area.x as i32
                && lx < (area.x + area.width) as i32
                && ly >= area.y as i32
                && ly < (area.y + area.height) as i32
            {
                let cell = &mut buf[(lx as u16, ly as u16)];
                if cell.symbol() == " " || !active {
                    cell.set_char(if active { glyph } else { '·' })
                        .set_style(style);
                }
            }
        }
    }

    // Nodes + labels.
    for (idx, nd) in snapshot.nodes.iter().enumerate() {
        let p = positions[idx];
        if p.x < area.x as i32
            || p.x >= (area.x + area.width) as i32
            || p.y < area.y as i32
            || p.y >= (area.y + area.height) as i32
        {
            continue;
        }
        let is_sel = idx == selected;
        let is_neighbor = neighbors.contains(&idx);
        let glyph = node_glyph(&nd.kind, is_sel || nd.primary && is_sel);
        let color = if is_sel {
            theme.accent.to_color()
        } else if nd.primary {
            theme.primary.to_color()
        } else if is_neighbor {
            reason_color(theme, nd.reason.as_deref().unwrap_or(""))
        } else {
            theme.fg_subtle.to_color()
        };
        let cell = &mut buf[(p.x as u16, p.y as u16)];
        cell.set_char(glyph).set_style(
            Style::default()
                .fg(color)
                .bg(theme.bg_panel.to_color())
                .add_modifier(if is_sel || nd.primary {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        );
        // Labels: primaries + selection + neighbors always; others only
        // when zoomed in enough for the space.
        let label_w = 18u16;
        let show_label = nd.primary
            || is_sel
            || is_neighbor
            || (viz.zoom >= 1.2 && viz.zoom < 9.9);
        if show_label
            && p.y + 1 < (area.y + area.height) as i32
            && p.x + 1 < (area.x + area.width) as i32
        {
            let room = ((area.x + area.width) as i32 - (p.x + 1)).max(0) as u16;
            let take = label_w.min(room).max(2) as usize;
            let label: String = nd.name.chars().take(take).collect();
            let lx = (p.x + 1) as u16;
            let ly = (p.y + 1) as u16;
            let label_color = if is_sel {
                theme.accent.to_color()
            } else if is_neighbor || nd.primary {
                theme.fg_base.to_color()
            } else {
                theme.fg_more_subtle.to_color()
            };
            let cell = &mut buf[(lx, ly)];
            cell.set_symbol(&label).set_style(
                Style::default()
                    .fg(label_color)
                    .bg(theme.bg_panel.to_color()),
            );
        }
    }

    // Zoom indicator (bottom-right corner of the canvas).
    let zlabel = format!("{}×", viz.zoom);
    if area.width > 8 && area.height > 1 {
        let x = area.x + area.width - zlabel.chars().count() as u16 - 1;
        let y = area.y + area.height - 1;
        let cell = &mut buf[(x, y)];
        cell.set_symbol(&zlabel)
            .set_style(Style::default().fg(theme.fg_most_subtle.to_color()).bg(theme.bg_panel.to_color()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(n: usize) -> ContextGraphSnapshot {
        let mut s = ContextGraphSnapshot::default();
        s.tier = "Frontier".into();
        for i in 0..n {
            s.nodes.push(NodeSnapshot {
                id: i as u64,
                name: format!("N{}", i),
                fqcn: format!("x.N{}", i),
                kind: if i == 0 { "Class".into() } else { "Interface".into() },
                depth: if i == 0 { 0 } else { 1 + (i % 3) },
                primary: i == 0,
                reason: if i == 0 { None } else { Some("injects".into()) },
                ..Default::default()
            });
        }
        s
    }

    #[test]
    fn layout_centers_primary_and_rings_by_depth() {
        let s = snap(7);
        let pos = layout_nodes(&s, 40, 12, (0, 0), 1.0);
        // Primary at the exact center.
        assert_eq!((pos[0].x, pos[0].y), (40, 12));
        // Deeper nodes sit strictly farther from the center (per ring step).
        let dist = |p: &NodePos| ((p.x - 40).pow(2) + (p.y - 12).pow(2)) as f32;
        let d1: Vec<f32> = pos.iter().filter(|p| p.depth == 1).map(dist).collect();
        let d3: Vec<f32> = pos.iter().filter(|p| p.depth == 3).map(dist).collect();
        assert!(!d1.is_empty() && !d3.is_empty());
        assert!(d3[0] > d1[0], "depth-3 ring outside depth-1 ring");
        // Zoom scales rings outward.
        let zoomed = layout_nodes(&s, 40, 12, (0, 0), 2.0);
        assert!(dist(&zoomed[1]) > dist(&pos[1]));
        // Pan shifts expanded nodes, not the primary.
        let panned = layout_nodes(&s, 40, 12, (5, 0), 1.0);
        assert_eq!((panned[0].x, panned[0].y), (40, 12));
        assert_eq!(panned[1].x, pos[1].x + 5);
    }

    #[test]
    fn line_cells_walks_between_endpoints() {
        let cells = line_cells(0, 0, 4, 0);
        assert_eq!(cells, vec![(1, 0), (2, 0), (3, 0)]);
        let cells = line_cells(0, 0, 0, 3);
        assert_eq!(cells, vec![(0, 1), (0, 2)]);
        let diag = line_cells(0, 0, 2, 2);
        assert!(!diag.is_empty());
        assert!(diag.iter().all(|(x, y)| x == y));
    }

    #[test]
    fn directional_selection_moves_right() {
        let mut s = snap(5);
        // Left node, right node, up node, down node around the primary.
        s.nodes[1].depth = 1;
        s.nodes[2].depth = 1;
        let mut v = VizState::default();
        v.reset();
        // Fake recorded cells: primary center, one node to the right, one up.
        *v.node_cells.borrow_mut() = vec![(40, 12), (50, 12), (40, 4)];
        v.select_directional(&s, 1, 0);
        assert_eq!(v.selected, 1, "right selects the right-hand node");
        v.select_directional(&s, 0, -1);
        // From node 1 at (50,12), the up node (40,4) qualifies.
        assert_eq!(v.selected, 2, "up selects the upper node");
    }

    #[test]
    fn list_move_clamps_and_syncs_selection() {
        let s = snap(4);
        let mut v = VizState::default();
        v.list_move(&s, false);
        v.list_move(&s, false);
        assert_eq!(v.list_cursor, 2);
        assert_eq!(v.selected, 2, "list cursor drives selection");
        v.list_move(&s, true);
        assert_eq!(v.list_cursor, 1);
        // Clamp at both ends.
        let mut v0 = VizState::default();
        v0.list_move(&s, true);
        assert_eq!(v0.list_cursor, 0);
        for _ in 0..10 {
            v0.list_move(&s, false);
        }
        assert_eq!(v0.list_cursor, 3);
    }

    #[test]
    fn zoom_bounds_and_reset() {
        let mut v = VizState::default();
        for _ in 0..30 {
            v.zoom_in();
        }
        assert!(v.zoom <= 3.0);
        for _ in 0..40 {
            v.zoom_out();
        }
        assert!(v.zoom >= 0.4);
        v.pan = (7, -3);
        v.reset_view();
        assert_eq!((v.pan.0, v.pan.1, v.zoom), (0, 0, 1.0));
    }

    #[test]
    fn tab_cycles_graph_nodes_feed() {
        let mut v = VizState::default();
        assert_eq!(v.tab, VizTab::Graph);
        v.cycle_tab();
        assert_eq!(v.tab, VizTab::Nodes);
        v.cycle_tab();
        assert_eq!(v.tab, VizTab::Feed);
        v.cycle_tab();
        assert_eq!(v.tab, VizTab::Graph);
    }

    #[test]
    fn list_row_mapping_skips_header() {
        let s = snap(3);
        assert_eq!(list_row_to_index(&s, 0), None, "header row");
        assert_eq!(list_row_to_index(&s, 1), Some(0));
        assert_eq!(list_row_to_index(&s, 3), Some(2));
        assert_eq!(list_row_to_index(&s, 4), None, "past the end");
    }

    /// Visual smoke (run with JOEY_TUI_VISUAL=1 --nocapture to eyeball):
    /// renders the explorer graph view through a TestBackend and dumps the
    /// buffer when the env var is set. Always asserts the node glyphs and
    /// labels landed in the buffer.
    #[test]
    fn visual_graph_frame_printable() {
        use ratatui::backend::TestBackend;
        let mut s = snap(9);
        // Give the nodes kinds + edges so the canvas has variety.
        s.edges.push(joey_neurocode::EdgeSnapshot {
            from: 0,
            to: 3,
            kind: "Implements".into(),
        });
        s.edges.push(joey_neurocode::EdgeSnapshot {
            from: 0,
            to: 5,
            kind: "Injects".into(),
        });
        let mut app = App::new("s", "m");
        app.neurocode_snapshot = Some(s);
        app.neurocode_active = true;
        app.neurocode_expanded = true;

        let terminal = ratatui::Terminal::new(TestBackend::new(100, 30)).unwrap();
        let tui = crate::app::Tui::new_for_test(app, Theme::aurora(), terminal);
        // Draw the explorer directly through a TestBackend frame (Tui's
        // own terminal field is module-private).
        let area = Rect::new(0, 4, 100, 22);
        let backend = ratatui::Terminal::new(TestBackend::new(100, 30)).unwrap();
        let buffer = {
            let mut term = backend;
            term.draw(|f| draw_explorer(f, area, &tui.app, Theme::aurora()))
                .unwrap();
            term.backend().buffer().clone()
        };

        let text: String = buffer.content.iter().map(|c| c.symbol().to_string()).collect();
        assert!(text.contains("N0"), "primary labeled");
        assert!(text.contains("neurocode explorer"), "chrome present");
        if std::env::var("JOEY_TUI_VISUAL").is_ok() {
            println!("════ EXPLORER GRAPH ════");
            for row in 0..30usize {
                let line: String = buffer.content[row * 100..row * 100 + 100]
                    .iter()
                    .map(|c| c.symbol().to_string())
                    .collect();
                let trimmed = line.trim_end();
                if !trimmed.is_empty() {
                    println!("{:2}|{}", row, trimmed);
                }
            }
        }
        let _ = tui; // silence unused when JOEY_TUI_VISUAL unset
    }
}
