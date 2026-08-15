//! Widgets that render the joey TUI panels.
//!
//! Visual style: a "busy yet elegant" synthwave-aurora dashboard. Deep
//! indigo-charcoal panels with gradient borders, a live particle backdrop,
//! animated spinners, an equalizer activity meter, and a scrolling
//! conversation transcript.

use std::time::Duration;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::anim::{Equalizer, ParticleField, Pulse, Spinner};
use crate::input::Input;
use crate::state::{
    AgentPhase, App, DisplayAgent, NoticeKind, RunMode, SlashCommandInfo, SubagentStatus,
    ToolStatus, TranscriptItem,
};
use crate::theme::{gradient_spans, Rgb, Theme};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Feature 005 (E2 resolution): max diff lines rendered in the TUI before
/// tail-truncation. Mirrors the CLI `MAX_DIFF_BLOCK_HEIGHT`.
const MAX_DIFF_LINES: usize = 50;
/// Feature 005 (T022): max lines in the collapsed reasoning view.
/// Must match `state::MAX_COLLAPSED_HEIGHT`.
const MAX_COLLAPSED_LINES: usize = 10;
/// Feature 005 (T022): max lines in the tail-window reasoning view.
/// Must match `state::MAX_TAIL_WINDOW_LINES`.
const MAX_TAIL_WINDOW_LINES_TUI: usize = 200;
/// Feature 007: max output lines shown collapsed for terminal/tool blocks.
/// Mirrors crush's `shellMaxCollapsedLines` / `responseContextHeight`.
const MAX_TOOL_OUTPUT_LINES: usize = 10;

/// Feature 013: maximum column width at which body text wraps, regardless of
/// panel width. Matches crush's `maxTextWidth` (messages.go:26). Body-text-
/// only (Clarification Q2): headers, borders, and tool/terminal output are
/// NOT capped (FR-005/007/008).
const MAX_CONTENT_WIDTH: usize = 120;

/// Feature 013: cap applied to BODY wrapping only (assistant/user/reasoning).
/// Degrades gracefully: when `content_w` is below the cap, returns
/// `content_w` unchanged (FR-007).
fn capped_content_width(content_w: usize) -> usize {
    content_w.min(MAX_CONTENT_WIDTH)
}

/// Feature 007: shared helper — take the first `max` lines of a string and
/// return the lines + an optional hidden-count affordance message.
/// Used by terminal-command blocks (T019) and tool-call bodies (T023).
/// Tail-anchored bounded view: show the LAST `max` lines with a
/// "… N earlier lines hidden" affordance (matches the reasoning expand
/// tail-window semantics — for long command output the END is what
/// matters).
fn bounded_tail_lines_with_affordance(text: &str, max: usize) -> (Vec<String>, Option<String>) {
    let all: Vec<&str> = text.lines().collect();
    if all.len() <= max {
        (all.iter().map(|s| s.to_string()).collect(), None)
    } else {
        let tail = &all[all.len() - max..];
        (
            tail.iter().map(|s| s.to_string()).collect(),
            Some(format!("… {} earlier lines hidden", all.len() - max)),
        )
    }
}

fn bounded_lines_with_affordance(text: &str, max: usize) -> (Vec<String>, Option<String>) {
    let all: Vec<&str> = text.lines().collect();
    if all.len() <= max {
        (all.iter().map(|s| s.to_string()).collect(), None)
    } else {
        let head = &all[..max];
        (
            head.iter().map(|s| s.to_string()).collect(),
            Some(format!("… {} more lines", all.len() - max)),
        )
    }
}

/// Helper: build a Block with a gradient title.
pub fn gradient_block(title: &str, theme: Theme) -> Block<'_> {
    let title_spans = gradient_spans(title, theme);
    Block::default()
        .borders(Borders::ALL)
        .title(Line::from(title_spans))
        .border_style(Style::default().fg(theme.separator.to_color()))
        .style(Style::default().bg(theme.bg_panel.to_color()))
}

/// Focused variant: the border tints toward the primary color. crush-style —
/// a steady focus indicator rather than a pulsing glow; `pulse` now only
/// contributes a subtle amount so the border reads as "focused", not "alive".
pub fn gradient_block_focused(title: &str, theme: Theme, pulse: f32) -> Block<'_> {
    let title_spans = gradient_spans(title, theme);
    let border = theme.separator.lerp(theme.primary, 0.75 + pulse * 0.1);
    Block::default()
        .borders(Borders::ALL)
        .title(Line::from(title_spans))
        .border_style(
            Style::default()
                .fg(border.to_color())
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(theme.bg_panel.to_color()))
}

fn panel_block(title: &str, theme: Theme, focused: bool, glow: f32) -> Block<'_> {
    if focused {
        gradient_block_focused(title, theme, glow)
    } else {
        gradient_block(title, theme)
    }
}

// ── Particle backdrop ───────────────────────────────────────────────────────
//
// Drawn first across the full terminal as a subtle animated starfield behind
// all panels. Panels have opaque backgrounds so they sit on top cleanly.

pub fn draw_particles(f: &mut Frame, field: &ParticleField, theme: Theme, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let buf = f.buffer_mut();
    for p in field.particles() {
        // Off-screen particles (spawn margins are negative) must be skipped
        // BEFORE the cast — `as u16` clamps negatives to 0 and would pile
        // them up along the top/left edges.
        if p.x < 0.0 || p.y < 0.0 {
            continue;
        }
        let x = p.x as u16;
        let y = p.y as u16;
        if x >= area.width || y >= area.height {
            continue;
        }
        // Fade in/out over the particle lifetime.
        let life_t = p.life / p.max_life.max(0.001);
        let alpha = (1.0 - (2.0 * life_t - 1.0).abs()).max(0.0) * 0.8;
        let col = ParticleField::particle_color(p, theme);
        let dimmed = col.lerp(theme.bg_base, 1.0 - alpha);
        let cell = &mut buf[(area.x + x, area.y + y)];
        // Pick a glyph by size for variety.
        let glyph = if p.size > 1.0 { '✦' } else { '·' };
        cell.set_char(glyph)
            .set_style(Style::default().fg(dimmed.to_color()));
    }
}

// ── Header banner ───────────────────────────────────────────────────────────

pub fn draw_header(f: &mut Frame, area: Rect, app: &App, theme: Theme, spinner: &Spinner, pulse: &Pulse) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let buf_area = Block::default()
        .style(Style::default().bg(theme.bg_elevated.to_color()));
    f.render_widget(buf_area, area);

    // Left: gradient wordmark "joey" with a faint breathing highlight
    // (toned down considerably from the original glow — crush's header is
    // static; we keep a hint of life without the "light show" feel).
    let logo = "✦ joey";
    let glow = pulse.value();
    let bright_stops = [
        theme.grad_0.lerp(Rgb(255, 255, 255), glow * 0.08),
        theme.grad_1.lerp(Rgb(255, 255, 255), glow * 0.08),
        theme.grad_2.lerp(Rgb(255, 255, 255), glow * 0.08),
        theme.grad_3.lerp(Rgb(255, 255, 255), glow * 0.08),
    ];
    let logo_spans =
        crate::theme::gradient_spans_stops(logo, &bright_stops);
    let logo_line = Line::from(logo_spans);

    // Right: model + session id + spinner.
    let status_text = if app.is_busy() {
        format!(
            "{}  {}  ⚡{} active",
            app.model,
            short_id(&app.session_id),
            app.active_count()
        )
    } else {
        format!("{}  {}  ◌ idle", app.model, short_id(&app.session_id))
    };
    let mut right_spans: Vec<Span<'static>> = Vec::new();
    right_spans.push(Span::styled(
        status_text,
        Style::default().fg(theme.fg_subtle.to_color()),
    ));
    if app.is_busy() {
        right_spans.push(Span::raw(" "));
        right_spans.push(spinner.styled_glyph(theme));
    }

    // Render the line into a buffer at the header area.
    let inner = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), 1);
    let buf = f.buffer_mut();

    // Render left portion (logo) starting from inner.x.
    let mut x = inner.x;
    for span in &logo_line.spans {
        for ch in span.content.chars() {
            if x >= inner.x + inner.width {
                break;
            }
            let cell = &mut buf[(x, inner.y)];
            cell.set_char(ch).set_style(span.style);
            x += 1;
        }
    }
    // Render right portion, right-aligned.
    let right_len: usize = right_spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    let mut rx = inner.x + inner.width.saturating_sub(right_len as u16);
    for span in &right_spans {
        for ch in span.content.chars() {
            if rx >= inner.x + inner.width {
                break;
            }
            let cell = &mut buf[(rx, inner.y)];
            cell.set_char(ch).set_style(span.style);
            rx += 1;
        }
    }

    // Subtle gradient underline (only when the header has its second row).
    if area.height >= 2 {
        let underline_y = area.y + area.height - 1;
        for i in 0..area.width {
            let t = i as f32 / area.width.max(1) as f32;
            let c = crate::theme::sample_stops(&[theme.grad_0, theme.grad_1, theme.grad_2, theme.grad_3], t);
            let cell = &mut buf[(area.x + i, underline_y)];
            cell.set_char('─')
                .set_style(Style::default().fg(c.to_color()));
        }
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

// ── Conversation / transcript ───────────────────────────────────────────────

/// Render one transcript item as wrapped lines.
fn item_lines(item: &TranscriptItem, content_w: usize, theme: Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    match item {
        TranscriptItem::User { text } => {
            lines.push(Line::from(vec![Span::styled(
                "❯ ",
                Style::default().fg(theme.accent.to_color()).add_modifier(Modifier::BOLD),
            )]));
            // Feature 013: cap body width (FR-005); body-text-only (Q2).
            for wl in wrap(text, capped_content_width(content_w).saturating_sub(2)) {
                lines.push(Line::from(vec![Span::styled(
                    format!("  {}", wl),
                    Style::default().fg(theme.fg_base.to_color()),
                )]));
            }
            // Feature 013 (T003): uniform trailing blank separator (FR-001).
            lines.push(Line::from(vec![Span::raw("")]));
        }
        TranscriptItem::Assistant { text } => {
            lines.push(Line::from(vec![Span::styled(
                "◆ agent ",
                Style::default()
                    .fg(theme.info.to_color())
                    .add_modifier(Modifier::BOLD),
            )]));
            // Feature 013: cap body width (FR-005); body-text-only (Q2).
            for wl in wrap(text, capped_content_width(content_w).saturating_sub(2)) {
                lines.push(Line::from(vec![Span::styled(
                    format!("  {}", wl),
                    Style::default().fg(theme.fg_base.to_color()),
                )]));
            }
            lines.push(Line::from(vec![Span::raw("")]));
        }
        TranscriptItem::Reasoning { text, expand_state, thought_duration } => {
            // Feature 007 (T012): crush-parity boxed reasoning render.
            use crate::state::ReasoningExpandState;
            let all_lines: Vec<&str> = text.lines().collect();
            let total = all_lines.len();
            let (shown_lines, affordance): (Vec<String>, Option<String>) = match expand_state {
                ReasoningExpandState::Collapsed => {
                    if total <= MAX_COLLAPSED_LINES {
                        (all_lines.iter().map(|s| s.to_string()).collect(), None)
                    } else {
                        // Feature 007 (T033): show the LAST (newest) N lines,
                        // matching crush's tail-biased collapsed view (contracts/
                        // block-layout.md §1, feature-005 contracts/expandable.md).
                        let recent = &all_lines[total - MAX_COLLAPSED_LINES..];
                        (
                            recent.iter().map(|s| s.to_string()).collect(),
                            Some(format!(
                                "… ({} lines hidden) [click or space to expand]",
                                total - MAX_COLLAPSED_LINES
                            )),
                        )
                    }
                }
                ReasoningExpandState::TailWindow => {
                    if total <= MAX_TAIL_WINDOW_LINES_TUI {
                        (all_lines.iter().map(|s| s.to_string()).collect(), None)
                    } else {
                        let tail = &all_lines[total - MAX_TAIL_WINDOW_LINES_TUI..];
                        (
                            tail.iter().map(|s| s.to_string()).collect(),
                            Some(format!(
                                "… {} earlier lines hidden [click or space for full view]",
                                total - MAX_TAIL_WINDOW_LINES_TUI
                            )),
                        )
                    }
                }
                ReasoningExpandState::Full => {
                    (all_lines.iter().map(|s| s.to_string()).collect(), None)
                }
            };
            // State-aware bordered-box title.
            let title = match expand_state {
                ReasoningExpandState::Collapsed => "reasoning",
                ReasoningExpandState::TailWindow => "reasoning (tail)",
                ReasoningExpandState::Full => "reasoning (full)",
            };
            let border_style = Style::default().fg(theme.fg_more_subtle.to_color());
            lines.push(Line::from(vec![Span::styled(
                format!(" ┌─ {} ", title),
                border_style,
            )]));
            lines.push(Line::from(vec![Span::styled(
                " │",
                border_style,
            )]));
            for wl in &shown_lines {
                // Feature 013: cap body width (FR-005); body-text-only (Q2).
                let wrapped = wrap(wl, capped_content_width(content_w).saturating_sub(4));
                for w in wrapped {
                    lines.push(Line::from(vec![Span::styled(
                        format!(" │ {}", w),
                        Style::default()
                            .fg(theme.fg_more_subtle.to_color())
                            .add_modifier(Modifier::DIM),
                    )]));
                }
            }
            if let Some(msg) = affordance {
                lines.push(Line::from(vec![Span::styled(
                    format!(" │ {}", msg),
                    Style::default().fg(theme.fg_most_subtle.to_color()),
                )]));
            }
            // Thought-for-Ns footer (only when duration > 0).
            if let Some(dur) = thought_duration {
                let secs = dur.as_secs_f64();
                if secs > 0.0 {
                    lines.push(Line::from(vec![Span::styled(
                        format!(" │"),
                        border_style,
                    )]));
                    lines.push(Line::from(vec![Span::styled(
                        format!(" └─ Thought for {:.1}s", secs),
                        Style::default().fg(theme.fg_more_subtle.to_color()),
                    )]));
                }
            } else {
                lines.push(Line::from(vec![Span::styled(
                    " └─",
                    border_style,
                )]));
            }
            lines.push(Line::from(vec![Span::raw("")]));
        }
        TranscriptItem::Tool { name, emoji, summary, status, duration_secs, result_preview, expanded, full_args, full_result, is_terminal, exit_code } => {
            if *is_terminal {
                // Feature 007 (T019): terminal-command block layout.
                // Header: $ command  (exit N)  ⟳/✓/✗  ▸/▾
                let expand_hint = if *expanded { "▾" } else { "▸" };
                let mut spans = vec![
                    Span::styled(
                        "  $ ".to_string(),
                        Style::default().fg(theme.accent.to_color()).add_modifier(Modifier::BOLD),
                    ),
                ];
                // Command text (use summary which equals the command for terminal).
                spans.push(Span::styled(
                    one_line(summary, content_w.saturating_sub(20)),
                    Style::default().fg(theme.fg_base.to_color()),
                ));
                // Running spinner or done icon.
                let (status_icon, status_col) = match status {
                    ToolStatus::Running => ("⟳", theme.busy),
                    ToolStatus::Done => ("✓", theme.success),
                    ToolStatus::Failed => ("✗", theme.error),
                };
                spans.push(Span::styled(
                    format!("  {}", status_icon),
                    Style::default().fg(status_col.to_color()).add_modifier(Modifier::BOLD),
                ));
                // Exit code badge (only when Some and non-zero).
                if let Some(code) = exit_code {
                    if *code != 0 {
                        spans.push(Span::styled(
                            format!(" (exit {})", code),
                            Style::default().fg(theme.error.to_color()),
                        ));
                    }
                }
                // Duration.
                if let Some(d) = duration_secs {
                    spans.push(Span::styled(
                        format!("  {:.1}s", d),
                        Style::default().fg(theme.fg_more_subtle.to_color()),
                    ));
                }
                spans.push(Span::styled(
                    format!("  {}", expand_hint),
                    Style::default().fg(theme.fg_most_subtle.to_color()),
                ));
                lines.push(Line::from(spans));
                // Body: collapsed (first MAX_TOOL_OUTPUT_LINES) or expanded
                // (the FULL result, bounded by the tail-window cap with an
                // affordance — feature parity with the generic tool view).
                let output = result_preview.as_str();
                if !output.is_empty() && !matches!(status, ToolStatus::Running) {
                    if *expanded {
                        // Full result when available; the preview otherwise.
                        let full = full_result
                            .as_deref()
                            .filter(|f| !f.is_empty())
                            .unwrap_or(output);
                        let (shown, affordance) =
                            bounded_tail_lines_with_affordance(full, MAX_TAIL_WINDOW_LINES_TUI);
                        if let Some(msg) = affordance {
                            lines.push(Line::from(vec![Span::styled(
                                format!("    {}", msg),
                                Style::default().fg(theme.fg_most_subtle.to_color()),
                            )]));
                        }
                        for ol in &shown {
                            for w in wrap(ol, content_w.saturating_sub(4)) {
                                lines.push(Line::from(vec![Span::styled(
                                    format!("    {}", w),
                                    Style::default().fg(theme.fg_more_subtle.to_color()),
                                )]));
                            }
                        }
                    } else {
                        let (shown, affordance) = bounded_lines_with_affordance(
                            output,
                            MAX_TOOL_OUTPUT_LINES,
                        );
                        for ol in &shown {
                            for w in wrap(ol, content_w.saturating_sub(4)) {
                                lines.push(Line::from(vec![Span::styled(
                                    format!("    {}", w),
                                    Style::default().fg(theme.fg_more_subtle.to_color()),
                                )]));
                            }
                        }
                        if let Some(msg) = affordance {
                            lines.push(Line::from(vec![Span::styled(
                                format!("    {}", msg),
                                Style::default().fg(theme.fg_most_subtle.to_color()),
                            )]));
                        }
                    }
                }
                lines.push(Line::from(vec![Span::raw("")]));
                return lines;
            }
            // ── Generic (non-terminal) tool layout ──
            let (icon, col) = match status {
                ToolStatus::Running => ("⟳", theme.busy),
                ToolStatus::Done => ("✓", theme.success),
                ToolStatus::Failed => ("✗", theme.error),
            };
            let dur_str = duration_secs
                .map(|d| format!("  {:.1}s", d))
                .unwrap_or_default();
            let expand_hint = if *expanded { "▾" } else { "▸" };
            let mut spans = vec![
                Span::styled(
                    format!("  {} ", icon),
                    Style::default().fg(col.to_color()).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{} ", emoji),
                    Style::default().fg(theme.accent.to_color()),
                ),
                Span::styled(
                    name.clone(),
                    Style::default().fg(theme.fg_base.to_color()).add_modifier(Modifier::BOLD),
                ),
            ];
            if !summary.is_empty() {
                let s = one_line(summary, content_w.saturating_sub(name.len() + 12));
                spans.push(Span::styled(
                    format!(" {}", s),
                    Style::default().fg(theme.fg_most_subtle.to_color()),
                ));
            }
            spans.push(Span::styled(
                dur_str,
                Style::default().fg(theme.fg_more_subtle.to_color()),
            ));
            // Feature 005 (T029): expand affordance indicator.
            spans.push(Span::styled(
                format!("  {}", expand_hint),
                Style::default().fg(theme.fg_most_subtle.to_color()),
            ));
            lines.push(Line::from(spans));
            if !result_preview.is_empty() && !matches!(status, ToolStatus::Running) && !*expanded {
                // Feature 007 (T023): use the shared bounded-output helper.
                let (shown, affordance) =
                    bounded_lines_with_affordance(result_preview, MAX_TOOL_OUTPUT_LINES);
                let col = if matches!(status, ToolStatus::Failed) {
                    theme.error
                } else {
                    theme.fg_most_subtle
                };
                for ol in &shown {
                    for w in wrap(ol, content_w.saturating_sub(4)) {
                        lines.push(Line::from(vec![Span::styled(
                            format!("    {}", w),
                            Style::default().fg(col.to_color()),
                        )]));
                    }
                }
                if let Some(msg) = affordance {
                    lines.push(Line::from(vec![Span::styled(
                        format!("    {} [click or space to expand]", msg),
                        Style::default().fg(theme.fg_most_subtle.to_color()),
                    )]));
                }
            }
            // Feature 005 (T029) + Feature 007 (T023): expanded view —
            // primary parameter + full result, bounded with affordance.
            if *expanded {
                // T034 (convergence): `full_args` is never populated (contracts/
                // agent-event.md Approach A carries no args), so fall back to
                // `summary` — the primary parameter — so the expand affordance
                // is honest (block-layout.md §3: "expanded view uses `summary`
                // for its param display").
                let args_text = full_args
                    .as_deref()
                    .filter(|a| !a.is_empty())
                    .unwrap_or(summary);
                if !args_text.is_empty() {
                    lines.push(Line::from(vec![Span::styled(
                        "    args:".to_string(),
                        Style::default().fg(theme.fg_more_subtle.to_color()),
                    )]));
                    for arg_line in args_text.lines() {
                        for w in wrap(arg_line, content_w.saturating_sub(8)) {
                            lines.push(Line::from(vec![Span::styled(
                                format!("      {}", w),
                                Style::default().fg(theme.fg_most_subtle.to_color()),
                            )]));
                        }
                    }
                }
                if let Some(result) = full_result {
                    if !result.is_empty() {
                        lines.push(Line::from(vec![Span::styled(
                            "    result:".to_string(),
                            Style::default().fg(theme.fg_more_subtle.to_color()),
                        )]));
                        // Feature 007 (T023): use shared helper for consistency.
                        let (shown, affordance) =
                            bounded_lines_with_affordance(result, MAX_TAIL_WINDOW_LINES_TUI);
                        if let Some(msg) = affordance {
                            lines.push(Line::from(vec![Span::styled(
                                format!("      {}", msg),
                                Style::default().fg(theme.fg_most_subtle.to_color()),
                            )]));
                        }
                        for rl in &shown {
                            for w in wrap(rl, content_w.saturating_sub(6)) {
                                lines.push(Line::from(vec![Span::styled(
                                    format!("      {}", w),
                                    Style::default().fg(theme.fg_most_subtle.to_color()),
                                )]));
                            }
                        }
                    }
                }
            }
            // Feature 013 (T004): uniform trailing blank separator (FR-001).
            // (The terminal-tool early-return at line ~426-427 already has one.)
            lines.push(Line::from(vec![Span::raw("")]));
        }
        TranscriptItem::FileDiff { path, stat, lines: diff_lines, is_binary, expanded } => {
            // Feature 005 (T019): render the inline diff block.
            // Header: "  ◆ path  +N -M"
            lines.push(Line::from(vec![
                Span::styled("  ◆ ", Style::default().fg(theme.fg_subtle.to_color())),
                Span::styled(
                    format!("{}  {}", path, stat),
                    Style::default().fg(theme.fg_subtle.to_color()),
                ),
            ]));
            if *is_binary {
                // T017 parity: binary placeholder (FR-016).
                lines.push(Line::from(Span::styled(
                    "    binary file changed",
                    Style::default().fg(theme.fg_most_subtle.to_color()),
                )));
            } else {
                // E2 resolution: height-bound the diff block; the bound
                // lifts when expanded (click or Space/x).
                let max_height = if *expanded { usize::MAX } else { MAX_DIFF_LINES };
                let start = if diff_lines.len() > max_height {
                    diff_lines.len() - max_height
                } else {
                    0
                };
                if start > 0 {
                    lines.push(Line::from(Span::styled(
                        format!("    … ({} earlier lines hidden) [click or space to expand]", start),
                        Style::default().fg(theme.fg_most_subtle.to_color()),
                    )));
                }
                for dl in &diff_lines[start..] {
                    let col = if dl.starts_with("+++") || dl.starts_with("---") {
                        theme.fg_most_subtle
                    } else if dl.starts_with("@@") {
                        theme.info
                    } else if dl.starts_with('+') {
                        theme.success
                    } else if dl.starts_with('-') {
                        theme.error
                    } else {
                        theme.fg_base
                    };
                    lines.push(Line::from(Span::styled(
                        format!("    {}", dl),
                        Style::default().fg(col.to_color()),
                    )));
                }
            }
            // Feature 013 (T005): uniform trailing blank separator (FR-001).
            lines.push(Line::from(vec![Span::raw("")]));
        }
        TranscriptItem::Notice { text, kind } => {
            let col = match kind {
                NoticeKind::Info => theme.info,
                NoticeKind::Warning => theme.warning,
                NoticeKind::Success => theme.success,
                NoticeKind::Busy => theme.busy,
            };
            lines.push(Line::from(vec![
                Span::styled("  · ", Style::default().fg(col.to_color())),
                Span::styled(
                    one_line(text, content_w.saturating_sub(4)),
                    Style::default().fg(theme.fg_more_subtle.to_color()),
                ),
            ]));
            // Feature 013 (T006): uniform trailing blank separator (FR-001).
            lines.push(Line::from(vec![Span::raw("")]));
        }
        TranscriptItem::Error { text } => {
            for wl in wrap(text, content_w.saturating_sub(4)) {
                lines.push(Line::from(vec![Span::styled(
                    format!("  ✗ {}", wl),
                    Style::default().fg(theme.error.to_color()).add_modifier(Modifier::BOLD),
                )]));
            }
            // Feature 013 (T007): uniform trailing blank separator (FR-001).
            lines.push(Line::from(vec![Span::raw("")]));
        }
    }
    lines
}

pub fn draw_transcript(f: &mut Frame, area: Rect, app: &App, theme: Theme, focused: bool, glow: f32) {
    // Build header showing message count and scroll position.
    let msg_count = app.transcript.len();
    let scroll_info = if let Some(offset) = app.scroll {
        let max = app.last_max_scroll.get();
        if max > 0 {
            let pct = ((1.0 - (offset as f64 / max as f64)) * 100.0).round() as usize;
            format!(" {} messages · {}% from top ", msg_count, pct)
        } else {
            format!(" {} messages ", msg_count)
        }
    } else {
        format!(" {} messages · live ", msg_count)
    };
    let title = if focused {
        format!(" conversation [scroll: j/k g/G PgUp/PgDn /search ] {} ", scroll_info)
    } else {
        format!(" conversation {} ", scroll_info)
    };
    let block = panel_block(&title, theme, focused, glow);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Reserve 1 column on the right for the scrollbar.
    let content_width = inner.width.saturating_sub(1) as usize;
    let scrollbar_area = Rect::new(
        inner.x + inner.width.saturating_sub(1),
        inner.y,
        1,
        inner.height,
    );
    let text_area = Rect::new(inner.x, inner.y, inner.width.saturating_sub(1), inner.height);

    // Feature 007 (T026): record text-area geometry for click hit-testing.
    app.last_text_area.set((
        text_area.x,
        text_area.y,
        text_area.width,
        text_area.height,
    ));

    let content_w = content_width;
    let visible = text_area.height as usize;
    let offset = app.scroll.unwrap_or(0);
    // One extra line beyond the viewport tells us whether more content
    // exists above (so scroll_up may keep going).
    let needed = visible + offset + 1;

    // Build lines lazily from the NEWEST item backwards — older items that
    // can't be on screen are never wrapped. This keeps per-frame cost
    // proportional to the viewport, not the session length.
    let mut blocks_rev: Vec<Vec<Line>> = Vec::new();
    let mut built = 0usize;

    // Live streaming tail is the newest block. (Live reasoning is rendered by
    // the dedicated reasoning panel, not here.)
    if !app.streaming_assistant.is_empty() {
        let mut tail = vec![Line::from(vec![Span::styled(
            "◆ agent ",
            Style::default().fg(theme.info.to_color()).add_modifier(Modifier::BOLD),
        )])];
        for wl in wrap(&app.streaming_assistant, content_w.saturating_sub(2)) {
            tail.push(Line::from(vec![Span::styled(
                format!("  {}", wl),
                Style::default().fg(theme.fg_base.to_color()),
            )]));
        }
        built += tail.len();
        blocks_rev.push(tail);
    }

    let mut exhausted = true;
    for item in app.transcript.iter().rev() {
        if built >= needed {
            exhausted = false;
            break;
        }
        let ls = item_lines(item, content_w, theme);
        built += ls.len();
        blocks_rev.push(ls);
    }

    let lines: Vec<Line> = blocks_rev.into_iter().rev().flatten().collect();
    let total = lines.len();

    // Record how far up the user may scroll. When we stopped building early
    // there is definitely more above — allow at least another page.
    let max_scroll = if exhausted {
        total.saturating_sub(visible)
    } else {
        offset + visible
    };
    app.last_max_scroll.set(max_scroll);

    let clamped = offset.min(max_scroll);
    let scroll_rows = total.saturating_sub(visible + clamped).min(u16::MAX as usize);

    let para = Paragraph::new(Text::from(lines)).scroll((scroll_rows as u16, 0));
    f.render_widget(para, text_area);

    // ── Scrollbar ────────────────────────────────────────────────────
    draw_scrollbar(f, scrollbar_area, app, theme, total, visible, clamped);

    // Scrolled-up indicator: bottom-right badge showing the distance to live.
    if app.scroll.is_some() && clamped > 0 {
        let badge = format!(" ↓ {} line{} below ", clamped, if clamped == 1 { "" } else { "s" });
        let bw = UnicodeWidthStr::width(badge.as_str()) as u16;
        if bw < text_area.width {
            let bx = text_area.x + text_area.width - bw;
            let by = text_area.y + text_area.height - 1;
            let buf = f.buffer_mut();
            for (xx, ch) in (bx..).zip(badge.chars()) {
                let cell = &mut buf[(xx, by)];
                cell.set_char(ch).set_style(
                    Style::default()
                        .fg(theme.bg_void.to_color())
                        .bg(theme.gold.to_color())
                        .add_modifier(Modifier::BOLD),
                );
            }
        }
    }
}

/// Feature 007 (T026): resolve which transcript item (if any) is at the given
/// absolute screen `(row, col)`. Returns the item index in `app.transcript`
/// (0-based from the oldest), or `None` if the click is outside the text area
/// or on empty space (below all items).
///
/// This mirrors the line accounting in [`draw_transcript`]: items are built
/// newest-last, the live streaming tail (if any) is the bottommost block, and
/// the viewport is anchored to the bottom with an optional scroll offset.
pub fn transcript_hit_test(app: &App, theme: Theme, row: u16, col: u16) -> Option<usize> {
    let (tx, ty, tw, th) = app.last_text_area.get();
    if th == 0 || tw == 0 {
        return None;
    }
    // Click must be inside the text area (excluding the scrollbar column).
    if row < ty || row >= ty + th || col < tx || col >= tx + tw {
        return None;
    }
    let content_w = tw as usize;
    let visible = th as usize;
    let offset = app.scroll.unwrap_or(0);

    // The click's row within the text area, measured from the TOP.
    let click_row_from_top = (row - ty) as usize;

    // Replicate draw_transcript's line accounting: build items newest-last,
    // tracking per-item line counts.
    let mut blocks_rev: Vec<(usize, usize)> = Vec::new(); // (item_index, line_count)
    let mut built = 0usize;

    // Live streaming assistant tail (renders as a block at the bottom).
    let has_streaming = !app.streaming_assistant.is_empty();
    if has_streaming {
        let tail_lines = 1 + wrap(&app.streaming_assistant, content_w.saturating_sub(2)).len();
        built += tail_lines;
    }

    let needed = visible + offset + 1;
    for (i, item) in app.transcript.iter().enumerate().rev() {
        if built >= needed {
            break;
        }
        let ls = item_lines(item, content_w, theme);
        let count = ls.len();
        built += count;
        blocks_rev.push((i, count));
    }

    // The items are now in reverse order (newest first). We need them oldest
    // first to map screen rows top→down.
    let items_fwd: Vec<(usize, usize)> = blocks_rev.into_iter().rev().collect();

    // If there's a streaming tail, it sits AFTER all items (bottommost).
    let streaming_line_count = if has_streaming {
        1 + wrap(&app.streaming_assistant, content_w.saturating_sub(2)).len()
    } else {
        0
    };

    // Total lines we know about.
    let total = items_fwd.iter().map(|(_, c)| *c).sum::<usize>() + streaming_line_count;

    // The viewport scroll: draw_transcript anchors to the bottom (scroll=None
    // means "live at bottom"). `scroll_rows` = how many lines the Paragraph is
    // scrolled DOWN from the top of the content.
    let clamped = offset.min(app.last_max_scroll.get().max(total.saturating_sub(visible)));
    let scroll_rows = total.saturating_sub(visible + clamped).min(u16::MAX as usize);

    // The visible content starts at content line `scroll_rows` and fills
    // `visible` rows downward. Map the click's top-relative row to a content
    // line index.
    let content_line = scroll_rows + click_row_from_top;
    if content_line >= total {
        return None; // Click is below all content.
    }

    // Walk items oldest→newest, accumulating line counts, to find which item
    // owns `content_line`.
    let mut acc = 0usize;
    for &(item_idx, count) in &items_fwd {
        if content_line < acc + count {
            return Some(item_idx);
        }
        acc += count;
    }
    // If we reach here, the click was on the streaming tail — no expandable
    // item to toggle.
    None
}

/// Resolve the transcript item that owns the FIRST fully-visible content
/// line of the viewport (the item at the top of the screen). Used by the
/// keyboard expand toggle (Space/Enter/x in transcript focus) so expansion
/// works without a mouse: scroll so the item you want is at the top, then
/// toggle. Returns None when the viewport is empty or unrenderable.
pub fn transcript_item_at_top(app: &App, theme: Theme) -> Option<usize> {
    let (_tx, _ty, tw, th) = app.last_text_area.get();
    if th == 0 || tw == 0 {
        return None;
    }
    let content_w = tw as usize;
    let visible = th as usize;
    let offset = app.scroll.unwrap_or(0);

    // Same line accounting as transcript_hit_test: items newest-last.
    let mut blocks_rev: Vec<(usize, usize)> = Vec::new();
    let mut built = 0usize;
    let has_streaming = !app.streaming_assistant.is_empty();
    if has_streaming {
        let tail_lines = 1 + wrap(&app.streaming_assistant, content_w.saturating_sub(2)).len();
        built += tail_lines;
    }
    let needed = visible + offset + 1;
    for (i, item) in app.transcript.iter().enumerate().rev() {
        if built >= needed {
            break;
        }
        let ls = item_lines(item, content_w, theme);
        let count = ls.len();
        built += count;
        blocks_rev.push((i, count));
    }
    let items_fwd: Vec<(usize, usize)> = blocks_rev.into_iter().rev().collect();
    let streaming_line_count = if has_streaming {
        1 + wrap(&app.streaming_assistant, content_w.saturating_sub(2)).len()
    } else {
        0
    };
    let total = items_fwd.iter().map(|(_, c)| *c).sum::<usize>() + streaming_line_count;
    if total == 0 {
        return None;
    }
    let clamped = offset.min(app.last_max_scroll.get().max(total.saturating_sub(visible)));
    let scroll_rows = total.saturating_sub(visible + clamped).min(u16::MAX as usize);
    if scroll_rows >= total {
        return None;
    }
    // The item owning content line `scroll_rows` (the top visible line).
    let mut acc = 0usize;
    for (idx, count) in &items_fwd {
        if scroll_rows < acc + count {
            return Some(*idx);
        }
        acc += count;
    }
    items_fwd.last().map(|(idx, _)| *idx)
}

/// Draw a scrollbar on the right edge of the transcript.
fn draw_scrollbar(
    f: &mut Frame,
    area: Rect,
    app: &App,
    theme: Theme,
    total_lines: usize,
    visible_lines: usize,
    current_offset: usize,
) {
    if area.width == 0 || area.height == 0 || total_lines <= visible_lines {
        // No scrollbar needed — everything fits.
        return;
    }

    let buf = f.buffer_mut();
    let h = area.height as usize;

    // Calculate the thumb position and size.
    let content_ratio = visible_lines as f64 / total_lines as f64;
    let thumb_size = ((h as f64 * content_ratio).ceil() as usize).max(1).min(h);
    let scroll_progress = if total_lines > visible_lines {
        current_offset as f64 / (total_lines - visible_lines) as f64
    } else {
        0.0
    };
    // When auto-following (scroll=None, offset=0), the thumb is at the bottom.
    let thumb_top = (h - thumb_size)
        .saturating_mul((1.0 - scroll_progress) as usize);

    let track_color = theme.bg_panel.to_color();
    let thumb_color = if app.scroll.is_some() {
        theme.gold.to_color()
    } else {
        theme.info.to_color()
    };

    for y in 0..h {
        let cell = &mut buf[(area.x, area.y + y as u16)];
        let in_thumb = y >= thumb_top && y < thumb_top + thumb_size;
        let ch = if in_thumb { '█' } else { '│' };
        cell.set_char(ch).set_style(
            Style::default()
                .fg(if in_thumb { thumb_color } else { track_color }),
        );
    }
}

/// Collapse a possibly multi-line string to one line, truncated to `max` chars.
fn one_line(s: &str, max: usize) -> String {
    let mut out: String = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max.max(1))
        .collect();
    if out.is_empty() {
        out.push('…');
    }
    out
}

/// Word-wrap a string to a given display width.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return text.lines().map(String::from).collect();
    }
    let mut out = Vec::new();
    for line in text.lines() {
        let wrapped = textwrap::wrap(line, width);
        if wrapped.is_empty() {
            out.push(String::new());
        } else {
            for w in wrapped {
                out.push(w.into_owned());
            }
        }
    }
    out
}

// ── Reasoning box (live) ───────────────────────────────────────────────────

pub fn draw_reasoning(f: &mut Frame, area: Rect, app: &App, theme: Theme, spinner: &Spinner) {
    let block = gradient_block_focused("reasoning", theme, 0.5);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if !app.reasoning_open || app.streaming_reasoning.is_empty() {
        let placeholder = Line::from(vec![
            Span::styled(
                "  (idle) ",
                Style::default().fg(theme.fg_most_subtle.to_color()),
            ),
        ]);
        f.render_widget(Paragraph::new(placeholder), inner);
        return;
    }

    let content_w = inner.width.max(1) as usize;
    let mut lines: Vec<Line> = Vec::new();
    for wl in wrap(&app.streaming_reasoning, content_w) {
        lines.push(Line::from(vec![Span::styled(
            wl,
            Style::default().fg(theme.fg_more_subtle.to_color()),
        )]));
    }
    // trailing spinner
    lines.push(Line::from(vec![Span::raw(" "), spinner.styled_glyph(theme)]));
    // Keep the newest reasoning visible.
    let total = lines.len();
    let visible = inner.height as usize;
    let scroll = total.saturating_sub(visible).min(u16::MAX as usize) as u16;
    f.render_widget(Paragraph::new(Text::from(lines)).scroll((scroll, 0)), inner);
}

// ── Activity / tools sidebar ────────────────────────────────────────────────

pub fn draw_omo_panel(
    f: &mut Frame,
    area: Rect,
    app: &App,
    theme: Theme,
    spinner: &Spinner,
    equalizer: &Equalizer,
) {
    let block = gradient_block("omo", theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // T072: graceful degradation for very short terminals (<9 rows).
    if inner.height < 3 {
        // Single-line: just show active agent name or "idle".
        let label = if app.is_busy() { "busy" } else { "idle" };
        f.render_widget(
            Paragraph::new(Text::from(vec![Line::from(vec![Span::styled(
                format!(" ◌ {}", label),
                Style::default().fg(theme.fg_more_subtle.to_color()),
            )])])),
            inner,
        );
        return;
    }

    let cw = inner.width.max(1) as usize;
    let mut lines: Vec<Line> = Vec::new();

    // ── Section 0: pinned active agent + concurrency indicator (T066) ──
    if let Some(active) = app.agent_roster.get(app.active_agent_index) {
        let model_str = active
            .resolved_model
            .clone()
            .unwrap_or_else(|| "unavailable".to_string());
        lines.push(Line::from(vec![
            Span::styled(
                "★ ".to_string(),
                Style::default().fg(theme.gold.to_color()),
            ),
            Span::styled(
                active.display_name.clone(),
                Style::default()
                    .fg(theme.fg_base.to_color())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  [{}]", truncate_str(&model_str, cw.saturating_sub(10))),
                Style::default().fg(theme.fg_most_subtle.to_color()),
            ),
        ]));
        // Concurrency indicator (T070): X/Y slots.
        let active_count = app
            .subagent_entries
            .iter()
            .filter(|e| e.status == SubagentStatus::Running)
            .count();
        let slots = app.active_agents.len() + active_count;
        lines.push(Line::from(vec![Span::styled(
            format!("  {}/{} slots", slots, slots.max(1)),
            Style::default().fg(theme.fg_more_subtle.to_color()),
        )]));
        lines.push(Line::from(vec![Span::raw("")]));
    }

    // ── Section 1: active agents list (or idle roster, T067) ──
    // T067: When idle (no active agents, no subagent entries), show the full
    // agent roster with resolved models.
    let is_idle = app.active_agents.is_empty() && app.subagent_entries.is_empty();
    if is_idle && !app.agent_roster.is_empty() {
        // T067/T072: Full roster on idle, capped by terminal height.
        let max_agents = if inner.height < 15 { 5 } else { 12 };
        for (i, agent) in app.agent_roster.iter().take(max_agents).enumerate() {
            let marker = if Some(i) == Some(app.active_agent_index) {
                "▶"
            } else {
                " "
            };
            let model_str = agent
                .resolved_model
                .clone()
                .unwrap_or_else(|| "unavailable".to_string());
            let (model_col, name_col) = if agent.resolved_model.is_some() {
                (theme.fg_more_subtle, theme.fg_base)
            } else {
                (theme.fg_most_subtle, theme.fg_most_subtle)
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} ", marker),
                    Style::default().fg(theme.gold.to_color()),
                ),
                Span::styled(
                    truncate_str(&agent.display_name, 12),
                    Style::default().fg(name_col.to_color()),
                ),
                Span::styled(
                    format!("  {}", truncate_str(&model_str, cw.saturating_sub(18))),
                    Style::default().fg(model_col.to_color()),
                ),
            ]));
        }
        lines.push(Line::from(vec![Span::raw("")]));
    } else if app.active_agents.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  ◌ idle — awaiting input".to_string(),
            Style::default().fg(theme.fg_most_subtle.to_color()),
        )]));
    } else {
        for a in &app.active_agents {
            let (phase_text, phase_col): (String, Rgb) = match &a.phase {
                AgentPhase::Idle => ("queued".to_string(), theme.fg_more_subtle),
                AgentPhase::QueryingModel => ("querying model".to_string(), theme.info),
                AgentPhase::RunningTool(t) => (t.clone(), theme.accent),
                AgentPhase::Reasoning => ("reasoning".to_string(), theme.keyword),
                AgentPhase::Done => ("done".to_string(), theme.success),
            };
            let mut spans = vec![
                Span::raw("  "),
                spinner.styled_glyph(theme),
                Span::raw(" "),
                Span::styled(
                    format!("#{} ", a.id),
                    Style::default().fg(theme.fg_most_subtle.to_color()),
                ),
                Span::styled(
                    phase_text.clone(),
                    Style::default().fg(phase_col.to_color()).add_modifier(Modifier::BOLD),
                ),
            ];
            if a.max_iterations > 0 {
                spans.push(Span::styled(
                    format!("  [{}/{}]", a.iterations, a.max_iterations),
                    Style::default().fg(theme.fg_more_subtle.to_color()),
                ));
            }
            lines.push(Line::from(spans));
        }
    }
    lines.push(Line::from(vec![Span::raw("")]));

    // Section 1b: active subagents (delegation roster, T064).
    // T072: Cap subagent entries when terminal is short.
    if !app.subagent_entries.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  subagents".to_string(),
            Style::default().fg(theme.fg_more_subtle.to_color()),
        )]));
        let max_entries = if inner.height < 15 { 3 } else { 20 };
        for entry in app.subagent_entries.iter().take(max_entries) {
            let (icon, col) = match entry.status {
                SubagentStatus::Pending => ("○", theme.fg_more_subtle),
                SubagentStatus::Running => ("⟳", theme.busy),
                SubagentStatus::Done => ("✓", theme.success),
                SubagentStatus::Failed => ("✗", theme.error),
            };
            let elapsed = entry.started.elapsed().as_secs();
            let label = truncate_str(&entry.agent_type, cw.saturating_sub(14));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{} ", icon),
                    Style::default().fg(col.to_color()).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    label,
                    Style::default().fg(theme.fg_subtle.to_color()),
                ),
                Span::styled(
                    format!("  {}s", elapsed),
                    Style::default().fg(theme.fg_most_subtle.to_color()),
                ),
            ]));
        }
        lines.push(Line::from(vec![Span::raw("")]));
    }

    // Section 1c: Atlas job board (T155, FR-036, US6/AC4).
    // Shown during Atlas plan execution (BoulderWorkStarted sets the flag).
    // Lists each delegated task's title, status (pending/running/done/failed),
    // tool-call count, and last tool used.
    if app.job_board_visible && !app.subagent_entries.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  ┌─ jobs ─────────────────".to_string(),
            Style::default()
                .fg(theme.accent.to_color())
                .add_modifier(Modifier::BOLD),
        )]));
        let max_jobs = if inner.height < 15 { 3 } else { 12 };
        for entry in app.subagent_entries.iter().take(max_jobs) {
            let (marker, status_word, col) = match entry.status {
                SubagentStatus::Pending => ("○", "pending", theme.fg_more_subtle),
                SubagentStatus::Running => ("►", "running", theme.busy),
                SubagentStatus::Done => ("✓", "done", theme.success),
                SubagentStatus::Failed => ("✗", "failed", theme.error),
            };
            // Title line: marker + task title (or agent_type fallback).
            let title = entry
                .task_title
                .clone()
                .unwrap_or_else(|| entry.agent_type.clone());
            let title_disp = truncate_str(&title, cw.saturating_sub(6));
            lines.push(Line::from(vec![
                Span::raw("  │ "),
                Span::styled(
                    format!("{} ", marker),
                    Style::default().fg(col.to_color()).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    title_disp,
                    Style::default().fg(theme.fg_subtle.to_color()),
                ),
            ]));
            // Detail line: status + tool-call count + last tool.
            let mut detail = format!("    {}, {} calls", status_word, entry.tool_call_count);
            if let Some(ref lt) = entry.last_tool {
                let lt_short = truncate_str(lt, 16);
                detail.push_str(&format!(" · {}", lt_short));
            }
            lines.push(Line::from(vec![Span::styled(
                detail,
                Style::default().fg(theme.fg_most_subtle.to_color()),
            )]));
        }
        lines.push(Line::from(vec![Span::styled(
            "  └─────────────────────────".to_string(),
            Style::default().fg(theme.fg_more_subtle.to_color()),
        )]));
        lines.push(Line::from(vec![Span::raw("")]));
    }

    // Section 2: equalizer bars.
    lines.push(Line::from(vec![Span::styled(
        "  activity".to_string(),
        Style::default().fg(theme.fg_more_subtle.to_color()),
    )]));
    let bars_row = render_equalizer(equalizer, theme, cw.saturating_sub(2));
    lines.push(bars_row);
    lines.push(Line::from(vec![Span::raw("")]));

    // Section 3: token stats.
    lines.push(Line::from(vec![Span::styled(
        "  tokens".to_string(),
        Style::default().fg(theme.fg_more_subtle.to_color()),
    )]));
    let t = app.tokens;
    lines.push(Line::from(vec![Span::styled(
        format!("   in  {}", fmt_tokens(t.prompt)),
        Style::default().fg(theme.fg_subtle.to_color()),
    )]));
    lines.push(Line::from(vec![Span::styled(
        format!("   out {}", fmt_tokens(t.completion)),
        Style::default().fg(theme.fg_subtle.to_color()),
    )]));
    lines.push(Line::from(vec![Span::styled(
        format!("   api {}", t.iterations),
        Style::default().fg(theme.fg_subtle.to_color()),
    )]));

    // Section 4: learnings / wisdom counter (persistent, T065).
    if app.learnings_count > 0 {
        lines.push(Line::from(vec![Span::raw("")]));
        lines.push(Line::from(vec![Span::styled(
            format!("  ♾ {} learnings", app.learnings_count),
            Style::default().fg(theme.keyword.to_color()),
        )]));
    }

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// Render the equalizer as a single Line of block characters.
fn render_equalizer(eq: &Equalizer, theme: Theme, width: usize) -> Line<'static> {
    let blocks = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
    let n = eq.len().min(width);
    for (i, h) in eq.heights() {
        if i >= n {
            break;
        }
        let idx = ((h.clamp(0.0, 1.0) * (blocks.len() - 1) as f32).round() as usize).min(blocks.len() - 1);
        let t = i as f32 / n.max(1) as f32;
        let col = crate::theme::sample_stops(&[theme.grad_0, theme.grad_1, theme.grad_2, theme.grad_3], t);
        spans.push(Span::styled(
            blocks[idx].to_string(),
            Style::default().fg(col.to_color()),
        ));
    }
    Line::from(spans)
}

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Truncate a string to `max` chars, appending an ellipsis if cut.
fn truncate_str(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".to_string();
    }
    format!("{}…", chars[..max - 1].iter().collect::<String>())
}

// ── Input box ───────────────────────────────────────────────────────────────

pub fn draw_input(
    f: &mut Frame,
    area: Rect,
    input: &Input,
    app: &App,
    theme: Theme,
    focused: bool,
    glow: f32,
) {
    let title = if app.is_busy() { "input · ⏎ queues" } else { "input" };
    let block = panel_block(title, theme, focused, glow);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // 2 columns of prefix ("❯ " / "… ") then content.
    let cw = inner.width.saturating_sub(2) as usize;
    let (first_line, x_off) = input.view_offset(inner.height as usize, cw.max(1));

    let mut lines: Vec<Line> = Vec::new();
    for (idx, l) in input.lines().iter().enumerate().skip(first_line) {
        let prefix = if idx == 0 { "❯ " } else { "… " };
        let prefix_span = Span::styled(
            prefix,
            Style::default().fg(theme.accent.to_color()).add_modifier(Modifier::BOLD),
        );
        // Horizontal crop around the cursor, respecting display width.
        let mut cropped = String::new();
        let mut used = 0usize;
        for ch in l.chars().skip(x_off) {
            let w = ch.width().unwrap_or(0);
            if used + w > cw {
                break;
            }
            used += w;
            cropped.push(ch);
        }
        let content_span = Span::styled(
            cropped,
            Style::default().fg(theme.fg_base.to_color()),
        );
        lines.push(Line::from(vec![prefix_span, content_span]));
    }

    // Placeholder when the buffer is empty.
    if input.is_empty() {
        let hint = if app.is_busy() {
            "agent working — type to queue · Esc interrupts · Ctrl+C quits"
        } else {
            "type a prompt · ? for help · Ctrl+C quits"
        };
        lines.clear();
        lines.push(Line::from(vec![
            Span::styled(
                "❯ ",
                Style::default().fg(theme.accent.to_color()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(hint, Style::default().fg(theme.fg_most_subtle.to_color())),
        ]));
    }

    f.render_widget(Paragraph::new(Text::from(lines)), inner);

    // Place the block cursor (only when the input owns focus).
    if focused {
        let (cur_line, cur_col) = input.cursor();
        let view_line = cur_line.saturating_sub(first_line);
        let col_w: usize = input
            .lines()
            .get(cur_line)
            .map(|l| {
                l.chars()
                    .skip(x_off)
                    .take(cur_col.saturating_sub(x_off))
                    .map(|c| c.width().unwrap_or(0))
                    .sum()
            })
            .unwrap_or(0);
        let cursor_x = inner.x + 2 + col_w.min(u16::MAX as usize) as u16;
        let cursor_y = inner.y + view_line.min(u16::MAX as usize) as u16;
        if cursor_x < inner.x + inner.width && cursor_y < inner.y + inner.height {
            let cell = &mut f.buffer_mut()[(cursor_x, cursor_y)];
            cell.set_style(
                Style::default()
                    .bg(theme.secondary.to_color())
                    .add_modifier(Modifier::REVERSED),
            );
        }
    }
}

// ── Status bar ──────────────────────────────────────────────────────────────

pub fn draw_status(f: &mut Frame, area: Rect, app: &App, theme: Theme, elapsed: Duration) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let bg = theme.bg_elevated.to_color();
    let bg_block = Block::default().style(Style::default().bg(bg));
    f.render_widget(bg_block, area);

    let mut spans: Vec<Span<'static>> = Vec::new();
    // mode badge
    let (mode_text, mode_col) = match app.mode {
        RunMode::Input => (" INPUT ", theme.success),
        RunMode::Busy => (" BUSY ", theme.busy),
        RunMode::Quitting => (" QUIT ", theme.warning),
    };
    spans.push(Span::styled(
        mode_text.to_string(),
        Style::default().bg(mode_col.to_color()).fg(theme.bg_void.to_color()).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw("  "));
    // active agent (OMO agent picker, T145). When the roster is populated,
    // show the active agent's colored display name so the status bar reflects
    // a Tab switch even when the picker overlay is closed.
    if let Some(agent) = app.agent_roster.get(app.active_agent_index) {
        if !agent.display_name.is_empty() {
            spans.push(Span::styled(
                format!("◆ {}", agent.display_name),
                Style::default().fg(theme.accent.to_color()).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw("  "));
        }
    }
    // NeuroCode active badge (feature 015): shown whenever the engine is
    // wired + active, so the user always knows context-graph injection is on.
    if app.neurocode_active {
        spans.push(Span::styled(
            " ⚡NEUROCODE ",
            Style::default()
                .bg(theme.accent.to_color())
                .fg(theme.bg_void.to_color())
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("  "));
    }
    // cwd
    let cwd_short = shorten_path(&app.cwd, 28);
    spans.push(Span::styled(
        format!(" {}", cwd_short),
        Style::default().fg(theme.fg_more_subtle.to_color()),
    ));
    spans.push(Span::raw("  "));
    // provider
    if !app.provider.is_empty() {
        spans.push(Span::styled(
            app.provider.clone(),
            Style::default().fg(theme.keyword.to_color()),
        ));
        spans.push(Span::raw("  "));
    }
    // token total
    spans.push(Span::styled(
        format!(" Σ {}", fmt_tokens(app.tokens.total())),
        Style::default().fg(theme.info.to_color()),
    ));
    spans.push(Span::raw("  "));
    // elapsed on current turn
    if app.is_busy() {
        spans.push(Span::styled(
            format!("⏱ {}", fmt_elapsed(elapsed)),
            Style::default().fg(theme.warning.to_color()),
        ));
    } else {
        spans.push(Span::styled(
            "ready".to_string(),
            Style::default().fg(theme.fg_more_subtle.to_color()),
        ));
    }

    let line = Line::from(spans);
    let para = Paragraph::new(line).style(Style::default().bg(bg));
    f.render_widget(para, area);

    // Right-aligned keymap hint (matches the actual bindings).
    let hint = if app.is_busy() {
        "⏎ queue  Esc interrupt  ^T scroll  ^R reasoning  ? help"
    } else {
        "⏎ send  ⌥⏎ newline  ^T scroll  ^R reasoning  ? help  ^C quit"
    };
    let hint_style = Style::default().fg(theme.fg_most_subtle.to_color());
    let hint_w = UnicodeWidthStr::width(hint) as u16;
    let hx = area.x + area.width.saturating_sub(hint_w + 1);
    let hy = area.y;
    if hx > area.x {
        let buf = f.buffer_mut();
        for (xx, ch) in (hx..).zip(hint.chars()) {
            if xx >= area.x + area.width {
                break;
            }
            let cell = &mut buf[(xx, hy)];
            cell.set_char(ch).set_style(hint_style);
        }
    }
}

fn fmt_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}.{}s", s, d.subsec_millis() / 100)
    }
}

fn shorten_path(p: &str, max: usize) -> String {
    if p.chars().count() <= max {
        return p.to_string();
    }
    let last = p.rsplit('/').next().unwrap_or(p);
    if last.chars().count() >= max {
        let cut: String = last.chars().take(max.saturating_sub(2)).collect();
        return format!("…/{}", cut);
    }
    format!("…/{}", last)
}

// ── Help overlay ────────────────────────────────────────────────────────────

pub fn draw_help_overlay(f: &mut Frame, area: Rect, theme: Theme) {
    // Centered modal.
    let w = 56.min(area.width);
    let h = 26.min(area.height);
    if w < 20 || h < 5 {
        return;
    }
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height - h) / 2;
    let modal = Rect::new(x, y, w, h);
    f.render_widget(Clear, modal);
    let block = gradient_block_focused(" help — ? closes ", theme, 0.8);
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    let keymap = [
        ("Enter", "send · queues next prompt while busy"),
        ("Alt+Enter / Ctrl+J", "insert newline"),
        ("Tab", "agent picker · slash-menu next when popup open"),
        ("Shift+Tab", "reverse cycle picker / slash menu"),
        ("/", "open slash-command popup (type to filter)"),
        ("/cmd arg", "subcommand suggestions (↑/↓ · ⏎ select)"),
        ("@ / path word", "context refs · file & folder completions"),
        ("↑ / ↓ (input)", "input history recall (shared with CLI)"),
        ("↑ / ↓ (popup)", "navigate slash commands · ⏎ select · Esc close"),
        ("Ctrl+C ×1 / ×2 (busy)", "interrupt turn / KILL & restart engine"),
                ("Ctrl+D", "quit (on empty input)"),
        ("Shift+Up / Ctrl+T / PgUp·PgDn", "scroll transcript (enters scroll mode)"),
        ("Ctrl+B / Ctrl+F", "half-page scroll up/down"),
        ("j / k  ↑ / ↓", "scroll one line (in transcript focus)"),
        ("Space / x (transcript)", "expand the tool/terminal item at the top of the view"),
        ("g / G", "top / bottom (transcript focus)"),
        ("y / Y", "copy last agent / user message to clipboard"),
        ("/copy [n]", "copy nth assistant message (−n counts from last)"),
        ("Ctrl+S", "search transcript · n/N cycle matches"),
        ("Ctrl+R", "toggle reasoning panel"),
        ("Alt+↑ / Alt+↓", "scroll NeuroCode context feed (when active)"),
        ("Ctrl+L", "clear transcript view"),
        ("Ctrl+A/E  Ctrl+U/K/W", "line start/end · kill line/word"),
        ("? / F1", "toggle this help"),
    ];
    let items: Vec<ListItem> = keymap
        .iter()
        .map(|(k, desc)| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("  {:<22}", k),
                    Style::default().fg(theme.accent.to_color()).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    *desc,
                    Style::default().fg(theme.fg_subtle.to_color()),
                ),
            ]))
        })
        .collect();
    let list = List::new(items);
    f.render_widget(list, inner);
}

// ── Search bar overlay ──────────────────────────────────────────────

/// Render the search bar as a bottom overlay. Only draws when
/// `app.search_open` is true.
pub fn draw_search_bar(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if !app.search_open {
        return;
    }
    let theme = *theme;

    // Bottom 3-row bar.
    let h = 3u16;
    let y = area.y + area.height.saturating_sub(h);
    let search_area = Rect::new(area.x, y, area.width, h);
    f.render_widget(Clear, search_area);

    let title = if app.search_query.is_empty() {
        " search (Esc to close) "
    } else if app.search_has_match {
        " search · match found (n=next N=prev) "
    } else {
        " search · no matches "
    };
    let block = gradient_block_focused(title, theme, 0.7);
    let inner = block.inner(search_area);
    f.render_widget(block, search_area);

    let prompt_line = Line::from(vec![
        Span::styled(
            "/",
            Style::default()
                .fg(theme.gold.to_color())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            &app.search_query,
            Style::default().fg(theme.fg_base.to_color()),
        ),
        Span::styled(
            "▏",
            Style::default().fg(theme.accent.to_color()),
        ),
    ]);
    f.render_widget(Paragraph::new(prompt_line), inner);
}

// ── Agent picker overlay (T028 / BC-013) ────────────────────────────────────

/// Render the agent picker as a centered popup. Only draws when
/// `app.agent_picker_open` is true.
pub fn draw_agent_picker(f: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if !app.agent_picker_open {
        return;
    }

    let theme = *theme;
    let roster_len = app.agent_roster.len();
    if roster_len == 0 {
        return;
    }

    // Width ~44; height = one row per agent + footer + border (2).
    let content_rows = roster_len + 1; // +1 for the hint/footer line
    let w = 44.min(area.width);
    let h = ((content_rows + 2) as u16).min(area.height); // +2 for borders
    if w < 24 || h < 5 {
        return;
    }
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height - h) / 2;
    let modal = Rect::new(x, y, w, h);

    f.render_widget(Clear, modal);
    let block = gradient_block_focused(" Agent Mode ", theme, 0.8);
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    let mut lines: Vec<Line> = Vec::with_capacity(roster_len + 1);
    for (i, agent) in app.agent_roster.iter().enumerate() {
        let is_cursor = i == app.agent_picker_cursor;
        let is_active = i == app.active_agent_index;

        let marker = if is_cursor { "► " } else { "  " };
        let marker_col = if is_cursor { theme.accent } else { theme.fg_most_subtle };

        let name_col = if is_active {
            theme.gold
        } else if is_cursor {
            theme.fg_base
        } else {
            theme.fg_subtle
        };

        let mut spans: Vec<Span<'static>> = vec![Span::styled(
            marker.to_string(),
            Style::default().fg(marker_col.to_color()).add_modifier(Modifier::BOLD),
        )];

        // Active badge (star) before the display name.
        if is_active {
            spans.push(Span::styled(
                "★ ".to_string(),
                Style::default().fg(theme.gold.to_color()),
            ));
        }

        let name_mod = if is_active || is_cursor {
            Modifier::BOLD
        } else {
            Modifier::empty()
        };
        spans.push(Span::styled(
            agent.display_name.clone(),
            Style::default().fg(name_col.to_color()).add_modifier(name_mod),
        ));

        // Mode tag (Primary/Sub).
        spans.push(Span::styled(
            format!("  {}", agent.mode),
            Style::default().fg(theme.fg_more_subtle.to_color()),
        ));

        // Resolved model in brackets (dimmed).
        let model_str = agent
            .resolved_model
            .clone()
            .unwrap_or_else(|| "unavailable".to_string());
        spans.push(Span::styled(
            format!("  [{}]", model_str),
            Style::default().fg(theme.fg_most_subtle.to_color()),
        ));

        lines.push(Line::from(spans));
    }

    // Footer hint.
    lines.push(Line::from(vec![Span::styled(
        " ↑↓ navigate · ⏎ select · Esc cancel ".to_string(),
        Style::default().fg(theme.fg_most_subtle.to_color()),
    )]));

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// Build the TUI agent roster from joey-omo's `AgentRegistry`.
///
/// The "Default" agent (the existing joey-agent) is always first, followed by
/// each available primary agent in canonical Tab order (`tab_order()`).
pub fn build_agent_roster_from_registry(registry: &joey_omo::AgentRegistry) -> Vec<DisplayAgent> {
    let mut roster = Vec::new();

    // 1. The "Default" agent — always present, always first.
    roster.push(DisplayAgent {
        name: "default".to_string(),
        display_name: "Default".to_string(),
        color: String::new(),
        mode: "Primary".to_string(),
        resolved_model: None,
        description: "The standard joey-agent (no OMO orchestration)".to_string(),
    });

    // 2. Available primary agents in canonical Tab order.
    for agent in registry.tab_order() {
        roster.push(DisplayAgent {
            name: agent.name.clone(),
            display_name: agent.display_name.clone(),
            color: agent.color.clone(),
            mode: agent.mode.label().to_string(),
            resolved_model: agent.resolved_model.clone(),
            description: agent.description.clone(),
        });
    }

    roster
}

// ── Feature 007 (T013): unit tests for boxed reasoning ────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{NoticeKind, ReasoningExpandState};
    use std::time::Duration;

    /// Helper: convert a `Line` into its plain-text content.
    fn line_text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect::<String>()
    }

    /// Helper: collect all text from `item_lines` output.
    fn render_text(item: &TranscriptItem, width: usize) -> Vec<String> {
        let theme = Theme::aurora();
        item_lines(item, width, theme)
            .iter()
            .map(line_text)
            .collect()
    }

    #[test]
    fn test_collapsed_reasoning_affordance() {
        // 20 lines of text, collapsed state → should show affordance with
        // (20 - MAX_COLLAPSED_LINES) = 10 hidden.
        let long_text: String = (0..20).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        let item = TranscriptItem::Reasoning {
            text: long_text,
            expand_state: ReasoningExpandState::Collapsed,
            thought_duration: None,
        };
        let rendered = render_text(&item, 80);
        let affordance_line = rendered.iter().find(|l| l.contains("lines hidden"));
        assert!(
            affordance_line.is_some(),
            "collapsed should emit a hidden-lines affordance; got: {:?}",
            rendered
        );
        assert!(
            affordance_line.unwrap().contains("10"),
            "should show 10 hidden lines; got: {}",
            affordance_line.unwrap()
        );
        // T033: collapsed shows the LAST (newest) N lines, tail-biased like crush.
        assert!(
            rendered.iter().any(|l| l.contains("line 19")),
            "collapsed should show the newest line (line 19); got: {:?}",
            rendered
        );
        assert!(
            !rendered.iter().any(|l| l.contains("line 0")),
            "collapsed should NOT show the oldest line (line 0); got: {:?}",
            rendered
        );
    }

    #[test]
    fn test_tail_window_reasoning_affordance() {
        // 250 lines, tail-window state → should show affordance with
        // (250 - MAX_TAIL_WINDOW_LINES) = 50 hidden.
        let long_text: String = (0..250).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        let item = TranscriptItem::Reasoning {
            text: long_text,
            expand_state: ReasoningExpandState::TailWindow,
            thought_duration: None,
        };
        let rendered = render_text(&item, 80);
        let affordance = rendered.iter().find(|l| l.contains("earlier lines hidden"));
        assert!(
            affordance.is_some(),
            "tail-window should emit an earlier-lines affordance; got: {:?}",
            rendered
        );
    }

    #[test]
    fn test_footer_shows_duration_when_some() {
        let item = TranscriptItem::Reasoning {
            text: "some thought".into(),
            expand_state: ReasoningExpandState::Full,
            thought_duration: Some(Duration::from_secs(3)),
        };
        let rendered = render_text(&item, 80);
        let footer = rendered.iter().find(|l| l.contains("Thought for"));
        assert!(
            footer.is_some(),
            "footer should show 'Thought for' when duration is Some; got: {:?}",
            rendered
        );
        assert!(footer.unwrap().contains("3.0s"));
    }

    #[test]
    fn test_footer_omitted_when_duration_none() {
        let item = TranscriptItem::Reasoning {
            text: "some thought".into(),
            expand_state: ReasoningExpandState::Full,
            thought_duration: None,
        };
        let rendered = render_text(&item, 80);
        assert!(
            !rendered.iter().any(|l| l.contains("Thought for")),
            "footer should be absent when duration is None; got: {:?}",
            rendered
        );
    }

    #[test]
    fn test_short_reasoning_no_affordance() {
        // 5 lines (≤ MAX_COLLAPSED_LINES), collapsed → no affordance.
        let short_text: String = (0..5).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        let item = TranscriptItem::Reasoning {
            text: short_text,
            expand_state: ReasoningExpandState::Collapsed,
            thought_duration: None,
        };
        let rendered = render_text(&item, 80);
        assert!(
            !rendered.iter().any(|l| l.contains("lines hidden")),
            "short reasoning should have no affordance; got: {:?}",
            rendered
        );
    }

    #[test]
    fn test_box_borders_present() {
        let item = TranscriptItem::Reasoning {
            text: "hello".into(),
            expand_state: ReasoningExpandState::Collapsed,
            thought_duration: None,
        };
        let rendered = render_text(&item, 80);
        assert!(
            rendered.iter().any(|l| l.contains("┌─")),
            "should have top border ┌─; got: {:?}",
            rendered
        );
        assert!(
            rendered.iter().any(|l| l.contains("└─")),
            "should have bottom border └─; got: {:?}",
            rendered
        );
    }

    // ── Feature 007 (T020): terminal block tests ──────────────────────

    use crate::state::{is_terminal_block, ToolStatus};

    #[test]
    fn test_is_terminal_block_classification() {
        assert!(is_terminal_block("terminal"));
        assert!(!is_terminal_block("read_file"));
        assert!(!is_terminal_block("write_file"));
        assert!(!is_terminal_block("search_files"));
    }

    #[test]
    fn test_terminal_header_shows_prompt() {
        let item = TranscriptItem::Tool {
            name: "terminal".into(),
            emoji: "⚡".into(),
            summary: "ls -la crates".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.1),
            result_preview: "drwxr-xr-x".into(),
            expanded: false,
            full_args: None,
            full_result: None,
            is_terminal: true,
            exit_code: Some(0),
        };
        let rendered = render_text(&item, 80);
        let header = rendered.iter().find(|l| l.contains("$")).unwrap();
        assert!(header.contains("ls -la crates"), "header should show command; got: {}", header);
    }

    #[test]
    fn test_exit_badge_only_for_nonzero() {
        let item_fail = TranscriptItem::Tool {
            name: "terminal".into(),
            emoji: "⚡".into(),
            summary: "false".into(),
            status: ToolStatus::Failed,
            duration_secs: Some(0.01),
            result_preview: String::new(),
            expanded: false,
            full_args: None,
            full_result: None,
            is_terminal: true,
            exit_code: Some(1),
        };
        let rendered = render_text(&item_fail, 80);
        assert!(
            rendered.iter().any(|l| l.contains("(exit 1)")),
            "non-zero exit should show badge; got: {:?}",
            rendered
        );

        let item_ok = TranscriptItem::Tool {
            name: "terminal".into(),
            emoji: "⚡".into(),
            summary: "true".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.01),
            result_preview: String::new(),
            expanded: false,
            full_args: None,
            full_result: None,
            is_terminal: true,
            exit_code: Some(0),
        };
        let rendered_ok = render_text(&item_ok, 80);
        assert!(
            !rendered_ok.iter().any(|l| l.contains("(exit")),
            "zero exit should NOT show badge; got: {:?}",
            rendered_ok
        );
    }

    #[test]
    fn test_terminal_collapsed_output_affordance() {
        let long_output: String = (0..25).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        let item = TranscriptItem::Tool {
            name: "terminal".into(),
            emoji: "⚡".into(),
            summary: "echo".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.1),
            result_preview: long_output,
            expanded: false,
            full_args: None,
            full_result: None,
            is_terminal: true,
            exit_code: Some(0),
        };
        let rendered = render_text(&item, 80);
        assert!(
            rendered.iter().any(|l| l.contains("more lines")),
            "collapsed output should show affordance; got: {:?}",
            rendered
        );
    }

    #[test]
    fn test_terminal_running_shows_spinner() {
        let item = TranscriptItem::Tool {
            name: "terminal".into(),
            emoji: "⚡".into(),
            summary: "sleep 5".into(),
            status: ToolStatus::Running,
            duration_secs: None,
            result_preview: String::new(),
            expanded: false,
            full_args: None,
            full_result: None,
            is_terminal: true,
            exit_code: None,
        };
        let rendered = render_text(&item, 80);
        assert!(
            rendered.iter().any(|l| l.contains("⟳")),
            "running terminal should show spinner; got: {:?}",
            rendered
        );
    }

    // ── Feature 007 (T024): generic tool header tests ─────────────────

    #[test]
    fn test_tool_header_composition() {
        let item = TranscriptItem::Tool {
            name: "read_file".into(),
            emoji: "📖".into(),
            summary: "src/main.rs".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.05),
            result_preview: "fn main() {}".into(),
            expanded: false,
            full_args: None,
            full_result: None,
            is_terminal: false,
            exit_code: None,
        };
        let rendered = render_text(&item, 80);
        let header = rendered.first().unwrap();
        assert!(header.contains("✓"), "should show success icon; got: {}", header);
        assert!(header.contains("read_file"), "should show bold name; got: {}", header);
        assert!(header.contains("src/main.rs"), "should show primary param; got: {}", header);
    }

    #[test]
    fn test_tool_collapsed_result_affordance() {
        let long_result: String = (0..25).map(|i| format!("result line {}", i)).collect::<Vec<_>>().join("\n");
        let item = TranscriptItem::Tool {
            name: "search_files".into(),
            emoji: "🔍".into(),
            summary: "*.rs".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.3),
            result_preview: long_result,
            expanded: false,
            full_args: None,
            full_result: None,
            is_terminal: false,
            exit_code: None,
        };
        let rendered = render_text(&item, 80);
        assert!(
            rendered.iter().any(|l| l.contains("more lines")),
            "collapsed result should show affordance; got: {:?}",
            rendered
        );
    }

    #[test]
    fn test_tool_expanded_shows_full_result() {
        let item = TranscriptItem::Tool {
            name: "read_file".into(),
            emoji: "📖".into(),
            summary: "foo.rs".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.1),
            result_preview: "line1\nline2".into(),
            expanded: true,
            full_args: Some("path: foo.rs".into()),
            full_result: Some("line1\nline2\nline3".into()),
            is_terminal: false,
            exit_code: None,
        };
        let rendered = render_text(&item, 80);
        assert!(
            rendered.iter().any(|l| l.contains("args:")),
            "expanded should show args label; got: {:?}",
            rendered
        );
        assert!(
            rendered.iter().any(|l| l.contains("result:")),
            "expanded should show result label; got: {:?}",
            rendered
        );
        assert!(
            rendered.iter().any(|l| l.contains("line3")),
            "expanded should show full result; got: {:?}",
            rendered
        );
    }

    /// T034 (convergence): when `full_args` is `None` (the real production
    /// case — Approach A carries no args), the expanded view falls back to
    /// showing the primary parameter from `summary`, so the expand affordance
    /// is not misleading (block-layout.md §3).
    #[test]
    fn test_tool_expanded_falls_back_to_summary_for_param() {
        let item = TranscriptItem::Tool {
            name: "read_file".into(),
            emoji: "📖".into(),
            summary: "src/lib.rs".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.1),
            result_preview: "preview".into(),
            expanded: true,
            full_args: None,
            full_result: Some("full body".into()),
            is_terminal: false,
            exit_code: None,
        };
        let rendered = render_text(&item, 80);
        assert!(
            rendered.iter().any(|l| l.contains("args:")),
            "expanded should show args label even without full_args; got: {:?}",
            rendered
        );
        assert!(
            rendered.iter().any(|l| l.contains("src/lib.rs")),
            "expanded should fall back to summary (src/lib.rs) for the param; got: {:?}",
            rendered
        );
    }

    // ── Feature 007 (T026): click hit-testing tests ───────────────────

    /// Helper: build an App with two transcript items so we can verify
    /// hit-testing resolves to the correct one.
    fn app_with_two_items() -> App {
        use crate::state::ToolStatus;
        let mut app = App::new("s", "m");
        app.push_item(TranscriptItem::User {
            text: "hello".into(),
        });
        app.push_item(TranscriptItem::Tool {
            name: "read_file".into(),
            emoji: "📖".into(),
            summary: "foo.rs".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.1),
            result_preview: "line1".into(),
            expanded: false,
            full_args: None,
            full_result: None,
            is_terminal: false,
            exit_code: None,
        });
        app
    }

    #[test]
    fn test_hit_test_outside_text_area() {
        let app = app_with_two_items();
        let theme = Theme::aurora();
        // No geometry set (last_text_area defaults to 0,0,0,0).
        assert_eq!(transcript_hit_test(&app, theme, 5, 5), None);
    }

    #[test]
    fn test_hit_test_resolves_correct_item() {
        let app = app_with_two_items();
        let theme = Theme::aurora();
        // Simulate a text area at (0, 0) with width 80, height 20.
        app.last_text_area.set((0, 0, 80, 20));

        // Feature 013: Item 0 (User "hello") now renders as 3 lines: header
        // "❯ " + body + the uniform trailing blank separator (FR-001).
        // Clicking rows 0-2 should hit item 0.
        assert_eq!(
            transcript_hit_test(&app, theme, 0, 10),
            Some(0),
            "top row should resolve to item 0"
        );
        assert_eq!(
            transcript_hit_test(&app, theme, 1, 10),
            Some(0),
            "row 1 (body of item 0) should still resolve to item 0"
        );
        assert_eq!(
            transcript_hit_test(&app, theme, 2, 10),
            Some(0),
            "row 2 (trailing separator of item 0) should still resolve to item 0"
        );

        // Clicking row 3+ should hit item 1 (the tool).
        let hit = transcript_hit_test(&app, theme, 3, 10);
        assert_eq!(hit, Some(1), "row 3 should resolve to item 1");
    }

    #[test]
    fn test_hit_test_below_content_returns_none() {
        let app = app_with_two_items();
        let theme = Theme::aurora();
        // Large text area, click far below content → None.
        app.last_text_area.set((0, 0, 80, 40));
        // Row 39 is near the bottom of the text area but well below the
        // ~3 lines of actual content.
        assert_eq!(
            transcript_hit_test(&app, theme, 39, 10),
            None,
            "click below all content should return None"
        );
    }

    #[test]
    fn test_hit_test_click_outside_bounds() {
        let app = app_with_two_items();
        let theme = Theme::aurora();
        app.last_text_area.set((5, 5, 80, 20));
        // Click is at row 0, which is above the text area (starts at row 5).
        assert_eq!(
            transcript_hit_test(&app, theme, 0, 10),
            None,
            "click above text area should return None"
        );
    }

    #[test]
    fn test_toggle_item_expand_by_index_tool() {
        let mut app = app_with_two_items();
        // Item 1 is the tool, starts unexpanded.
        assert_eq!(
            matches!(&app.transcript[1], TranscriptItem::Tool { expanded: false, .. }),
            true
        );
        app.toggle_item_expand_by_index(1);
        assert_eq!(
            matches!(&app.transcript[1], TranscriptItem::Tool { expanded: true, .. }),
            true,
            "toggle should expand the tool"
        );
        app.toggle_item_expand_by_index(1);
        assert_eq!(
            matches!(&app.transcript[1], TranscriptItem::Tool { expanded: false, .. }),
            true,
            "second toggle should collapse the tool"
        );
    }

    #[test]
    fn test_toggle_item_expand_by_index_reasoning() {
        let mut app = App::new("s", "m");
        app.push_item(TranscriptItem::Reasoning {
            text: (0..20).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n"),
            expand_state: ReasoningExpandState::Collapsed,
            thought_duration: None,
        });
        // Collapsed → cycle should advance (long text → TailWindow).
        app.toggle_item_expand_by_index(0);
        assert_eq!(
            matches!(
                &app.transcript[0],
                TranscriptItem::Reasoning { expand_state: ReasoningExpandState::TailWindow, .. }
            ),
            true,
            "reasoning should cycle from Collapsed to TailWindow"
        );
    }

    #[test]
    fn test_toggle_item_expand_non_expandable_is_noop() {
        let mut app = app_with_two_items();
        // Item 0 is User — not expandable. Should be a no-op (no panic).
        app.toggle_item_expand_by_index(0);
        assert!(matches!(&app.transcript[0], TranscriptItem::User { .. }));
    }

    // ── Feature 013 (US1): TUI vertical rhythm / trailing blank ──────

    /// T009: every `item_lines` variant returns a Vec<Line> whose LAST
    /// element is an empty line (the uniform trailing separator).
    #[test]
    fn test_all_variants_have_trailing_blank() {
        let theme = Theme::aurora();
        let w = 80usize;

        let cases: Vec<(&str, TranscriptItem)> = vec![
            ("User", TranscriptItem::User { text: "hi".into() }),
            ("Assistant", TranscriptItem::Assistant { text: "hello".into() }),
            (
                "Reasoning",
                TranscriptItem::Reasoning {
                    text: "thinking".into(),
                    expand_state: ReasoningExpandState::Full,
                    thought_duration: Some(Duration::from_secs(2)),
                },
            ),
            (
                "Tool(terminal)",
                TranscriptItem::Tool {
                    name: "terminal".into(),
                    emoji: "⚡".into(),
                    summary: "echo hi".into(),
                    status: ToolStatus::Done,
                    duration_secs: Some(0.1),
                    result_preview: "hi".into(),
                    expanded: false,
                    full_args: None,
                    full_result: None,
                    is_terminal: true,
                    exit_code: Some(0),
                },
            ),
            (
                "Tool(generic)",
                TranscriptItem::Tool {
                    name: "read_file".into(),
                    emoji: "📖".into(),
                    summary: "foo.rs".into(),
                    status: ToolStatus::Done,
                    duration_secs: Some(0.1),
                    result_preview: "fn main() {}".into(),
                    expanded: false,
                    full_args: None,
                    full_result: None,
                    is_terminal: false,
                    exit_code: None,
                },
            ),
            (
                "FileDiff",
                TranscriptItem::FileDiff {
                    path: "a.txt".into(),
                    stat: "+1 -0".into(),
                    lines: vec!["+hello".into()],
                    is_binary: false, expanded: false,
                },
            ),
            (
                "Notice",
                TranscriptItem::Notice {
                    text: "notice".into(),
                    kind: NoticeKind::Info,
                },
            ),
            (
                "Error",
                TranscriptItem::Error { text: "boom".into() },
            ),
        ];

        for (label, item) in &cases {
            let ls = item_lines(item, w, theme);
            let last = ls.last().expect("variant must produce at least one line");
            let text: String = last.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                text.is_empty(),
                "T009 [{}]: last line must be an empty Span (trailing separator); got {:?}",
                label,
                text
            );
        }
    }

    /// T010: concatenating `item_lines` outputs of two adjacent items of
    /// different type yields exactly ONE empty line between them — never
    /// zero, never two (INV-1, FR-001).
    #[test]
    fn test_adjacent_items_have_exactly_one_blank() {
        let theme = Theme::aurora();
        let w = 80usize;

        fn mk_user() -> TranscriptItem {
            TranscriptItem::User { text: "u".into() }
        }
        fn mk_asst() -> TranscriptItem {
            TranscriptItem::Assistant { text: "a".into() }
        }
        fn mk_reasoning() -> TranscriptItem {
            TranscriptItem::Reasoning {
                text: "r".into(),
                expand_state: ReasoningExpandState::Full,
                thought_duration: Some(Duration::from_secs(1)),
            }
        }
        fn mk_tool(expanded: bool) -> TranscriptItem {
            TranscriptItem::Tool {
                name: "read_file".into(),
                emoji: "📖".into(),
                summary: "f".into(),
                status: ToolStatus::Done,
                duration_secs: Some(0.1),
                result_preview: "x".into(),
                expanded,
                full_args: None,
                full_result: None,
                is_terminal: false,
                exit_code: None,
            }
        }
        fn mk_filediff() -> TranscriptItem {
            TranscriptItem::FileDiff {
                path: "a".into(),
                stat: "+1".into(),
                lines: vec!["+x".into()],
                is_binary: false, expanded: false,
            }
        }
        fn mk_notice() -> TranscriptItem {
            TranscriptItem::Notice {
                text: "n".into(),
                kind: NoticeKind::Info,
            }
        }
        fn mk_error() -> TranscriptItem {
            TranscriptItem::Error { text: "e".into() }
        }

        // (label, a, b) — pairs to check.
        let pairs: Vec<(&str, TranscriptItem, TranscriptItem)> = vec![
            ("user→assistant", mk_user(), mk_asst()),
            ("assistant→reasoning", mk_asst(), mk_reasoning()),
            ("reasoning→assistant", mk_reasoning(), mk_asst()),
            ("tool→tool", mk_tool(false), mk_tool(false)),
            ("tool→filediff", mk_tool(false), mk_filediff()),
            ("notice→notice", mk_notice(), mk_notice()),
            ("error→notice", mk_error(), mk_notice()),
            ("filediff→tool", mk_filediff(), mk_tool(false)),
        ];

        for (label, a, b) in &pairs {
            let la = item_lines(a, w, theme);
            let lb = item_lines(b, w, theme);
            // Concatenate the rendered text.
            let mut combined: Vec<String> =
                la.iter().map(line_text).collect::<Vec<_>>();
            combined.extend(lb.iter().map(line_text));

            // Count trailing-blank region between a and b: the last line of
            // a is a blank; the first line of b is NOT a blank (a header).
            // So the gap is exactly one blank iff la's last line is empty
            // and lb's first line is non-empty.
            let a_last = combined[la.len() - 1].is_empty();
            let b_first_nonempty = !combined[la.len()].is_empty();
            assert!(
                a_last && b_first_nonempty,
                "T010 [{}]: expected exactly one blank between items; a_last_empty={}, b_first_nonempty={}",
                label,
                a_last,
                b_first_nonempty
            );
            // Also assert the full sequence has no two consecutive blanks
            // anywhere (INV-1).
            for w2 in combined.windows(2) {
                let both_blank = w2[0].is_empty() && w2[1].is_empty();
                assert!(
                    !both_blank,
                    "T010 [{}]: found double-blank in sequence {:?}",
                    label, combined
                );
            }
        }
    }

    // ── Feature 013 (US2): TUI body width cap & indent ───────────────

    /// T018: `capped_content_width` degrades gracefully (FR-007).
    #[test]
    fn test_capped_content_width_degrades() {
        assert_eq!(capped_content_width(200), MAX_CONTENT_WIDTH);
        assert_eq!(capped_content_width(120), MAX_CONTENT_WIDTH);
        assert_eq!(capped_content_width(80), 80);
        assert_eq!(capped_content_width(0), 0);
    }

    /// T019: with content_w = 200, an Assistant body wraps at ≤ MAX_CONTENT_WIDTH - 2.
    #[test]
    fn test_assistant_body_capped_on_wide_panel() {
        let long: String = (0..40)
            .map(|i| format!("word{} ", i))
            .collect::<String>();
        let item = TranscriptItem::Assistant { text: long };
        let theme = Theme::aurora();
        let ls = item_lines(&item, 200, theme);
        // Body lines are all lines except the header (first) and the
        // trailing separator (last). Check their display width.
        let body = &ls[1..ls.len() - 1];
        assert!(!body.is_empty(), "should have body lines");
        for (i, l) in body.iter().enumerate() {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            let w = UnicodeWidthStr::width(text.as_str());
            assert!(
                w <= MAX_CONTENT_WIDTH,
                "T019: body line {} width {} exceeds cap {} (text: {:?})",
                i,
                w,
                MAX_CONTENT_WIDTH,
                text
            );
        }
    }

    /// T020: with content_w = 80 (below cap), Assistant wraps at full
    /// width (~78) — no premature wrap (FR-007).
    #[test]
    fn test_assistant_body_uses_full_width_below_cap() {
        // 80-col panel, body wraps at ~78 (full minus 2-space indent).
        // Construct a line that is exactly 76 chars wide; it should fit on
        // ONE body line (proving no premature wrap below the cap).
        let line76: String = "w".repeat(76);
        let item = TranscriptItem::Assistant { text: line76 };
        let theme = Theme::aurora();
        let ls = item_lines(&item, 80, theme);
        let body = &ls[1..ls.len() - 1];
        assert_eq!(
            body.len(),
            1,
            "T020: 76-char line at content_w=80 should fit on one body line (no premature wrap); got {} lines",
            body.len()
        );
    }

    /// T021: the Reasoning box border renders intact and is NOT affected by
    /// the width cap (FR-008 border/header alignment). The border string is
    /// a short fixed header (` ┌─ reasoning …`); the cap only shrinks the
    /// BODY wrap width inside the box, never the border text itself.
    #[test]
    fn test_reasoning_border_not_capped_on_wide_panel() {
        let item = TranscriptItem::Reasoning {
            text: "thinking".into(),
            expand_state: ReasoningExpandState::Full,
            thought_duration: None,
        };
        let theme = Theme::aurora();
        // The border line must be present and identical at both a wide panel
        // (where the cap kicks in for body text) and a narrow one. This proves
        // the cap does not corrupt/truncate the border (FR-008).
        let ls_wide = item_lines(&item, 200, theme);
        let ls_narrow = item_lines(&item, 80, theme);
        let border_wide = ls_wide
            .iter()
            .map(line_text)
            .find(|t| t.contains("┌─"))
            .expect("wide panel should have a top border");
        let border_narrow = ls_narrow
            .iter()
            .map(line_text)
            .find(|t| t.contains("┌─"))
            .expect("narrow panel should have a top border");
        assert_eq!(
            border_wide, border_narrow,
            "T021: border text must be identical regardless of panel width (FR-008)"
        );
        assert!(
            border_wide.contains("reasoning"),
            "T021: border should contain the title; got {:?}",
            border_wide
        );
    }

    /// T022 (FR-006 codification): tool/terminal body lines indent by exactly
    /// 4 spaces (`format!("    {}", ...)`).
    #[test]
    fn test_tool_body_indent_is_four_spaces() {
        let theme = Theme::aurora();

        // Generic tool with body output.
        let generic = TranscriptItem::Tool {
            name: "read_file".into(),
            emoji: "📖".into(),
            summary: "foo.rs".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.1),
            result_preview: "body line one".into(),
            expanded: false,
            full_args: None,
            full_result: None,
            is_terminal: false,
            exit_code: None,
        };
        let ls_g = item_lines(&generic, 80, theme);
        // Find the body line(s) (after the header, before the trailing blank).
        for l in &ls_g[1..ls_g.len() - 1] {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                text.starts_with("    "),
                "T022 generic: body line should indent 4 spaces; got {:?}",
                text
            );
        }

        // Terminal tool with body output.
        let term = TranscriptItem::Tool {
            name: "terminal".into(),
            emoji: "⚡".into(),
            summary: "echo hi".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.1),
            result_preview: "hi".into(),
            expanded: false,
            full_args: None,
            full_result: None,
            is_terminal: true,
            exit_code: Some(0),
        };
        let ls_t = item_lines(&term, 80, theme);
        // The terminal arm early-returns; the last line is the trailing blank.
        // Body lines sit between the header (first) and the trailing blank (last).
        for l in &ls_t[1..ls_t.len() - 1] {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                text.starts_with("    "),
                "T022 terminal: body line should indent 4 spaces; got {:?}",
                text
            );
        }
    }
}

// ── Slash-command popup ─────────────────────────────────────────────────────

/// Floating slash-command menu anchored above the input box (IDE-style).
///
/// Shows the commands matching the current `/fragment` (prefix match over
/// names + aliases — same semantics as `App::slash_matches`), with the
/// description of the highlighted command on a footer line. ↑/↓/Tab navigate,
/// Enter accepts, Esc closes (handled in `Tui::handle_input_key`).
pub fn draw_slash_popup(f: &mut Frame, area: Rect, app: &App, input_text: &str, theme: Theme) {
    // Subcommand stage: offer the command's pipe-hint subcommands.
    if app.slash_subcommand_stage {
        let subs = app.slash_subcommand_matches(input_text);
        if subs.is_empty() || !app.slash_menu_open {
            return;
        }
        const VISIBLE_ROWS: usize = 8;
        let scroll = app.slash_menu_cursor.saturating_sub(VISIBLE_ROWS.saturating_sub(1));
        let visible: Vec<&String> = subs.iter().skip(scroll).take(VISIBLE_ROWS).collect();

        let w = 56.min(area.width);
        let h = ((visible.len() + 3) as u16).min(area.height);
        if w < 30 || h < 4 {
            return;
        }
        let x = area.x + 1;
        let bottom_margin = 6u16.min(area.height.saturating_sub(h));
        let y = area.y + area.height - h - bottom_margin;
        let modal = Rect::new(x, y, w, h);

        f.render_widget(Clear, modal);
        let first_line = input_text.lines().next().unwrap_or("");
        let base = first_line.split(' ').next().unwrap_or("");
        let title = format!(" Arguments for {base} ");
        let block = gradient_block_focused(&title, theme, 0.6);
        let inner = block.inner(modal);
        f.render_widget(block, modal);
        if inner.width < 10 || inner.height < 1 {
            return;
        }

        let mut lines: Vec<Line> = Vec::with_capacity(visible.len() + 1);
        for (row, sub) in visible.iter().enumerate() {
            let abs_idx = scroll + row;
            let is_cursor = abs_idx == app.slash_menu_cursor;
            let marker = if is_cursor { "► " } else { "  " };
            let col = if is_cursor { theme.accent } else { theme.fg_subtle };
            let mod_ = if is_cursor { Modifier::BOLD } else { Modifier::empty() };
            lines.push(Line::from(vec![
                Span::styled(
                    marker.to_string(),
                    Style::default().fg(col.to_color()).add_modifier(Modifier::BOLD),
                ),
                Span::styled(sub.to_string(), Style::default().fg(theme.fg_base.to_color()).add_modifier(mod_)),
            ]));
        }
        lines.push(Line::from(Span::styled(
            " ↑↓ navigate · ⏎ select · Esc close ".to_string(),
            Style::default().fg(theme.fg_most_subtle.to_color()),
        )));
        f.render_widget(Paragraph::new(Text::from(lines)), inner);
        return;
    }

    let fragment = app.slash_fragment(input_text);
    let matches = app.slash_matches(fragment);
    if matches.is_empty() || !app.slash_menu_open {
        return;
    }

    // Keep the cursor visible when the list is taller than the popup.
    const VISIBLE_ROWS: usize = 8;
    let scroll = app
        .slash_menu_cursor
        .saturating_sub(VISIBLE_ROWS.saturating_sub(1));
    let visible: Vec<&&SlashCommandInfo> = matches
        .iter()
        .skip(scroll)
        .take(VISIBLE_ROWS)
        .collect();

    // Size: command column (fixed 22) + description; +2 borders, +1 footer.
    let w = 66.min(area.width);
    let content_rows = visible.len();
    let h = ((content_rows + 3) as u16).min(area.height);
    if w < 30 || h < 4 {
        return;
    }

    // Anchor: bottom-left of the screen, directly above the input box area
    // (input occupies roughly the last ~5 rows; the popup's bottom sits at
    // area.height - 6 from the top).
    let x = area.x + 1;
    let bottom_margin = 6u16.min(area.height.saturating_sub(h));
    let y = area.y + area.height - h - bottom_margin;
    let modal = Rect::new(x, y, w, h);

    f.render_widget(Clear, modal);
    let title = if fragment.is_empty() {
        " Commands ".to_string()
    } else {
        format!(" Commands matching /{} ", fragment)
    };
    let block = gradient_block_focused(&title, theme, 0.6);
    let inner = block.inner(modal);
    f.render_widget(block, modal);
    if inner.width < 10 || inner.height < 1 {
        return;
    }

    let mut lines: Vec<Line> = Vec::with_capacity(content_rows + 1);
    for (row, cmd) in visible.iter().enumerate() {
        let abs_idx = scroll + row;
        let is_cursor = abs_idx == app.slash_menu_cursor;
        let marker = if is_cursor { "► " } else { "  " };
        let marker_col = if is_cursor { theme.accent } else { theme.fg_most_subtle };

        // Command column: /name (+ dim " (alias)" when the fragment matches
        // an alias rather than the canonical name).
        let via_alias = !fragment.is_empty()
            && !cmd.name.starts_with(fragment)
            && cmd.aliases.iter().any(|a| a.starts_with(fragment));
        let label = if via_alias {
            format!("/{} ({})", cmd.name, cmd.aliases.iter().find(|a| a.starts_with(fragment)).unwrap_or(&String::new()))
        } else {
            format!("/{}", cmd.name)
        };
        let name_col = if is_cursor {
            theme.fg_base
        } else if cmd.implemented {
            theme.fg_subtle
        } else {
            theme.fg_most_subtle
        };
        let name_mod = if is_cursor { Modifier::BOLD } else { Modifier::empty() };

        let mut spans: Vec<Span<'static>> = vec![
            Span::styled(
                marker.to_string(),
                Style::default().fg(marker_col.to_color()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:<20}", label),
                Style::default().fg(name_col.to_color()).add_modifier(name_mod),
            ),
        ];

        // Description column (truncated to fit).
        let desc_width = (inner.width as usize).saturating_sub(24);
        let desc: String = cmd
            .description
            .chars()
            .take(desc_width)
            .collect();
        let dim = if cmd.implemented { theme.fg_subtle } else { theme.fg_most_subtle };
        spans.push(Span::styled(
            desc,
            Style::default().fg(dim.to_color()),
        ));
        if !cmd.implemented {
            spans.push(Span::styled(
                " ·n/a".to_string(),
                Style::default().fg(theme.fg_most_subtle.to_color()),
            ));
        }
        lines.push(Line::from(spans));
    }

    // Footer: args hint of the highlighted command + navigation hint.
    if let Some(cmd) = matches.get(app.slash_menu_cursor) {
        let hint = if cmd.args_hint.is_empty() {
            cmd.description.clone()
        } else {
            format!("args: {}", cmd.args_hint)
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", hint),
                Style::default().fg(theme.gold.to_color()),
            ),
        ]));
    }
    lines.push(Line::from(Span::styled(
        " ↑↓ navigate · Tab cycle · ⏎ select · Esc close ".to_string(),
        Style::default().fg(theme.fg_most_subtle.to_color()),
    )));

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// Generic completion popup (@-context refs / file paths). Host-fed items
/// (`App::completion_items`); ↑/↓/Tab navigate, Enter accepts, Esc closes.
pub fn draw_completion_popup(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    if !app.completion_menu_open || app.completion_items.is_empty() {
        return;
    }
    const VISIBLE_ROWS: usize = 8;
    let scroll = app.completion_menu_cursor.saturating_sub(VISIBLE_ROWS.saturating_sub(1));
    let visible: Vec<&joey_tools::completion::CompletionItem> =
        app.completion_items.iter().skip(scroll).take(VISIBLE_ROWS).collect();

    let w = 72.min(area.width);
    let h = ((visible.len() + 2) as u16).min(area.height);
    if w < 30 || h < 3 {
        return;
    }
    let x = area.x + 1;
    let bottom_margin = 6u16.min(area.height.saturating_sub(h));
    let y = area.y + area.height - h - bottom_margin;
    let modal = Rect::new(x, y, w, h);

    f.render_widget(Clear, modal);
    let block = gradient_block_focused(" Completions ", theme, 0.6);
    let inner = block.inner(modal);
    f.render_widget(block, modal);
    if inner.width < 10 || inner.height < 1 {
        return;
    }

    let mut lines: Vec<Line> = Vec::with_capacity(visible.len() + 1);
    for (row, item) in visible.iter().enumerate() {
        let abs_idx = scroll + row;
        let is_cursor = abs_idx == app.completion_menu_cursor;
        let marker = if is_cursor { "► " } else { "  " };
        let col = if is_cursor { theme.accent } else { theme.fg_subtle };
        let mod_ = if is_cursor { Modifier::BOLD } else { Modifier::empty() };

        // Display column (fixed 28) + meta column (truncated).
        let display: String = item.display.chars().take(26).collect();
        let meta_width = (inner.width as usize).saturating_sub(32);
        let meta: String = item.meta.chars().take(meta_width).collect();
        lines.push(Line::from(vec![
            Span::styled(
                marker.to_string(),
                Style::default().fg(col.to_color()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{display:<26}"),
                Style::default().fg(theme.fg_base.to_color()).add_modifier(mod_),
            ),
            Span::styled(meta, Style::default().fg(theme.fg_most_subtle.to_color())),
        ]));
    }
    lines.push(Line::from(Span::styled(
        " ↑↓ navigate · ⏎ insert · Esc close ".to_string(),
        Style::default().fg(theme.fg_most_subtle.to_color()),
    )));
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// NeuroCode live context feed (bottom-right panel). Shown only when the
/// engine is active (`app.neurocode_active`). Renders a header line
/// (tier · tokens · nodes · cold badge) and a scrolling window over the
/// exact context text NeuroCode assembled for the latest request — the same
/// string prepended to the system prompt (`AgentEvent::NeuroCodeContext`).
pub fn draw_neurocode_panel(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    if !app.neurocode_active || area.width == 0 || area.height == 0 {
        return;
    }
    let block = gradient_block(" neurocode · context feed ", theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 8 || inner.height < 2 {
        return;
    }

    let cw = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();

    // Header: tier + tokens + nodes (+ cold badge when degraded).
    let mut header = vec![
        Span::styled(
            "⚡ ".to_string(),
            Style::default().fg(theme.accent.to_color()),
        ),
        Span::styled(
            app.neurocode_tier.clone(),
            Style::default()
                .fg(theme.accent.to_color())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  Σ{} tok", fmt_tokens(app.neurocode_tokens as u64)),
            Style::default().fg(theme.info.to_color()),
        ),
        Span::styled(
            format!("  ◇{} nodes", app.neurocode_nodes),
            Style::default().fg(theme.fg_more_subtle.to_color()),
        ),
    ];
    if app.neurocode_cold {
        header.push(Span::styled(
            "  COLD".to_string(),
            Style::default().fg(theme.warning.to_color()).add_modifier(Modifier::BOLD),
        ));
    }
    lines.push(Line::from(header));
    lines.push(Line::styled(
        "─".repeat(cw.saturating_sub(2)),
        Style::default().fg(theme.fg_most_subtle.to_color()),
    ));

    // Body: the live context text, hard-wrapped, windowed by scroll.
    if app.neurocode_context.is_empty() {
        lines.push(Line::styled(
            " (no context assembled yet — send a prompt)",
            Style::default().fg(theme.fg_most_subtle.to_color()),
        ));
    } else {
        let wrapped = textwrap::wrap(&app.neurocode_context, cw.saturating_sub(2).max(10));
        let body_lines: Vec<Line> = wrapped
            .iter()
            .map(|w| {
                Line::styled(
                    format!(" {}", w),
                    Style::default().fg(theme.fg_subtle.to_color()),
                )
            })
            .collect();
        // Window: tail-anchored by default (the latest context is what's
        // being fed NOW); scroll moves the window up.
        let visible_rows = (inner.height as usize).saturating_sub(lines.len() + 1);
        let total = body_lines.len();
        let max_scroll = total.saturating_sub(visible_rows);
        let scroll = app.neurocode_scroll.min(max_scroll);
        let window: Vec<Line> = if total <= visible_rows {
            body_lines
        } else {
            // Tail-anchor: show the LAST visible_rows, offset by scroll.
            let start = total.saturating_sub(visible_rows).saturating_sub(scroll);
            let end = (start + visible_rows).min(total);
            body_lines[start..end].to_vec()
        };
        if total > visible_rows {
            let pct = if max_scroll == 0 {
                100
            } else {
                ((scroll as f32 / max_scroll as f32) * 100.0) as u16
            };
            lines.extend(window);
            lines.push(Line::styled(
                format!(" ↑{} lines · {:>3}% ", total.saturating_sub(visible_rows), 100 - pct.min(100)),
                Style::default().fg(theme.fg_most_subtle.to_color()),
            ));
        } else {
            lines.extend(window);
        }
    }

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}
