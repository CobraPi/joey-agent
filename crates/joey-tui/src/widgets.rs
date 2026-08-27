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
use crate::app::SteerOverlay;
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

pub fn draw_header(f: &mut Frame, area: Rect, app: &App, theme: Theme, spinner: &Spinner, pulse: &Pulse, header_flow: Option<&crate::anim::HeaderFlow>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let buf_area = Block::default()
        .style(Style::default().bg(theme.bg_elevated.to_color()));
    f.render_widget(buf_area, area);

    // Left: the "joey" wordmark as an inverted brand chip — a solid
    // brand-cyan background with a near-black foreground reads with far
    // more contrast than color-on-dark text. A gold "✦" spark sits
    // outside the chip. The pulse drives a gentle breathing lift on the
    // chip background (20% max — visible but no strobing).
    let spark = "✦";
    let word = "joey";
    let glow = pulse.value();
    let chip_bg = theme
        .grad_0
        .lerp(Rgb(255, 255, 255), glow * 0.20)
        .to_color();
    let mut logo_spans: Vec<Span<'static>> = Vec::new();
    logo_spans.push(Span::styled(
        spark,
        Style::default()
            .fg(theme.gold.to_color())
            .add_modifier(Modifier::BOLD),
    ));
    logo_spans.push(Span::raw(" "));
    let chip_text = format!("  {word}  ");
    let chip_fg = theme.bg_void.to_color();
    for ch in chip_text.chars() {
        logo_spans.push(Span::styled(
            ch.to_string(),
            Style::default()
                .fg(chip_fg)
                .bg(chip_bg)
                .add_modifier(Modifier::BOLD),
        ));
    }
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
    
    // HyperCode indicator (enabled/disabled badge).
    if app.hypercode_enabled {
        // Live phase label during a /hypercode run; static ⚡ otherwise.
        let badge = match app.hypercode_phase.as_deref() {
            Some("planning") => "⚡ PLAN",
            Some("exploring") => "⚡ EXPL",
            Some("building") => "⚡ BUILD",
            Some("synthesizing") => "⚡ SYNTH",
            Some(other) if !other.is_empty() => "⚡ HYPER",
            _ => {
                if app.is_busy() {
                    "⚡ HYPER"
                } else {
                    "⚡"
                }
            }
        };
        let badge_style = Style::default()
            .fg(theme.accent.to_color())
            .add_modifier(Modifier::BOLD);
        let badge_start_x = x + 2;
        // FIX: advance one cell per char — the old loop wrote every char to
        // the same cell, so multi-char badges ("⚡ HYPER") rendered as just
        // their last character.
        for (i, ch) in badge.chars().enumerate() {
            let cx = badge_start_x + i as u16;
            if cx >= inner.x + inner.width {
                break;
            }
            let cell = &mut buf[(cx, inner.y)];
            cell.set_char(ch).set_style(badge_style);
        }
        // Record the badge rect for click hit-testing.
        app.last_hypercode_rect.set((
            badge_start_x,
            inner.y,
            badge.chars().count() as u16,
            1,
        ));
    }
    
    // Render right portion, right-aligned.
    let right_len: usize = right_spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    let mut rx = inner.x + inner.width.saturating_sub(right_len as u16);
    // Record the right section's rect for click hit-testing (opens the
    // agent-stats page). Zero when it doesn't fit.
    if right_len as u16 <= inner.width {
        app.last_header_right_rect
            .set((rx, inner.y, right_len as u16, 1));
    } else {
        app.last_header_right_rect.set((0, 0, 0, 0));
    }
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
    // While the agent is running this becomes the activity indicator: a
    // slow traveling brightness wave glides across the bar (see
    // `anim::HeaderFlow`). Idle = the static gradient underline.
    if area.height >= 2 {
        let underline_y = area.y + area.height - 1;
        for i in 0..area.width {
            let t = i as f32 / area.width.max(1) as f32;
            let mut c = crate::theme::sample_stops(
                &[theme.grad_0, theme.grad_1, theme.grad_2, theme.grad_3],
                t,
            );
            if let Some(flow) = header_flow {
                let lift = flow.brightness(t);
                if lift > 0.0 {
                    c = c.lerp(Rgb(255, 255, 255), lift.min(0.32));
                }
            }
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
        TranscriptItem::Tool { name, emoji, summary, status, duration_secs, result_preview, expand_state, full_args, full_result, is_terminal, exit_code, live_output, .. } => {
            use crate::state::ReasoningExpandState;
            let expanded = matches!(expand_state, ReasoningExpandState::TailWindow | ReasoningExpandState::Full);
            if *is_terminal {
                // Feature 007 (T019): terminal-command block layout.
                // Header: $ command  (exit N)  ⟳/✓/✗  ▸/▾
                let expand_hint = if expanded { "▾" } else { "▸" };
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
                // Body: while RUNNING, live-stream the tail of the output as
                // it arrives (AgentEvent::ToolOutput accumulation) — the
                // "watch the CLI output in realtime" inline view. Collapsed
                // to the last MAX_TOOL_OUTPUT_LINES lines with a streaming
                // affordance; clicking the block or Space expands it inline
                // (Ctrl+O still maximizes for live-follow).
                if matches!(status, ToolStatus::Running) {
                    if !live_output.is_empty() {
                        // While expanded inline, show the tail window of the
                        // live stream instead of the tight collapsed cap.
                        let cap = if expanded {
                            MAX_TAIL_WINDOW_LINES_TUI
                        } else {
                            MAX_TOOL_OUTPUT_LINES
                        };
                        let (shown, affordance) =
                            bounded_tail_lines_with_affordance(live_output, cap);
                        let total = live_output.lines().count();
                        // Absolute numbering of the tail window: the first
                        // shown line is (total - shown.len() + 1).
                        let first = total.saturating_sub(shown.len()) + 1;
                        let gutter_w = digits(total.max(1));
                        if let Some(msg) = affordance {
                            lines.push(Line::from(vec![Span::styled(
                                format!("    {} [click or space to expand]", msg),
                                Style::default().fg(theme.fg_most_subtle.to_color()),
                            )]));
                        }
                        for (gi, ol) in shown.iter().enumerate() {
                            let num = format!("{:>w$} │ ", first + gi, w = gutter_w);
                            for w in wrap(ol, content_w.saturating_sub(8 + gutter_w)) {
                                lines.push(Line::from(vec![
                                    Span::styled(num.clone(), Style::default().fg(theme.fg_most_subtle.to_color())),
                                    Span::styled(w, Style::default().fg(theme.fg_more_subtle.to_color())),
                                ]));
                            }
                        }
                        let n_lines = total.max(shown.len());
                        lines.push(Line::from(vec![Span::styled(
                            format!(
                                "    ⣿ streaming · {} lines · click to expand · Ctrl+O to maximize",
                                n_lines
                            ),
                            Style::default().fg(theme.busy.to_color()),
                        )]));
                    } else {
                        // Silent so far — show the heartbeat-style hint.
                        lines.push(Line::from(vec![Span::styled(
                            "    ⣿ running…".to_string(),
                            Style::default().fg(theme.busy.to_color()),
                        )]));
                    }
                } else if !result_preview.is_empty() {
                    // Finished: collapsed (last MAX_TOOL_OUTPUT_LINES), tail
                    // window (last 200), or full — the three-state cycle.
                    // The payload is envelope-unwrapped from the JSON
                    // envelope and shown in a line-numbered code view
                    // (crush-style gutter).
                    let preview_payload = crate::state::display_result_content(result_preview)
                        .unwrap_or_else(|| result_preview.clone());
                    if matches!(expand_state, ReasoningExpandState::Full) {
                        // FULL: the entire formatted result, no truncation.
                        let full = full_result
                            .as_deref()
                            .filter(|f| !f.is_empty())
                            .map(crate::state::format_tool_result_for_display)
                            .unwrap_or(preview_payload.clone());
                        let all: Vec<&str> = full.lines().collect();
                        let gutter_w = digits(all.len().max(1));
                        for (gi, ol) in all.iter().enumerate() {
                            let num = format!("{:>w$} │ ", gi + 1, w = gutter_w);
                            for w in wrap(ol, content_w.saturating_sub(8 + gutter_w)) {
                                lines.push(Line::from(vec![
                                    Span::styled(num.clone(), Style::default().fg(theme.fg_most_subtle.to_color())),
                                    Span::styled(w, Style::default().fg(theme.fg_more_subtle.to_color())),
                                ]));
                            }
                        }
                    } else if expanded {
                        // TAIL WINDOW: the last MAX_TAIL_WINDOW_LINES_TUI
                        // lines of the formatted full result.
                        let full = full_result
                            .as_deref()
                            .filter(|f| !f.is_empty())
                            .map(crate::state::format_tool_result_for_display)
                            .unwrap_or(preview_payload.clone());
                        let (shown, affordance) =
                            bounded_tail_lines_with_affordance(&full, MAX_TAIL_WINDOW_LINES_TUI);
                        let total = full.lines().count();
                        let first = total.saturating_sub(shown.len()) + 1;
                        let gutter_w = digits(total.max(first));
                        if let Some(msg) = affordance {
                            lines.push(Line::from(vec![Span::styled(
                                format!("    {} [click or space for full view]", msg),
                                Style::default().fg(theme.fg_most_subtle.to_color()),
                            )]));
                        }
                        for (gi, ol) in shown.iter().enumerate() {
                            let num = format!("{:>w$} │ ", first + gi, w = gutter_w);
                            for w in wrap(ol, content_w.saturating_sub(8 + gutter_w)) {
                                lines.push(Line::from(vec![
                                    Span::styled(num.clone(), Style::default().fg(theme.fg_most_subtle.to_color())),
                                    Span::styled(w, Style::default().fg(theme.fg_more_subtle.to_color())),
                                ]));
                            }
                        }
                    } else {
                        let (shown, affordance) = bounded_lines_with_affordance(
                            &preview_payload,
                            MAX_TOOL_OUTPUT_LINES,
                        );
                        let gutter_w = digits(shown.len().max(1));
                        for (gi, ol) in shown.iter().enumerate() {
                            let num = format!("{:>w$} │ ", gi + 1, w = gutter_w);
                            for w in wrap(ol, content_w.saturating_sub(8 + gutter_w)) {
                                lines.push(Line::from(vec![
                                    Span::styled(num.clone(), Style::default().fg(theme.fg_most_subtle.to_color())),
                                    Span::styled(w, Style::default().fg(theme.fg_more_subtle.to_color())),
                                ]));
                            }
                        }
                        if let Some(msg) = affordance {
                            lines.push(Line::from(vec![Span::styled(
                                format!("    {} [click or space to expand]", msg),
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
            let expand_hint = if expanded { "▾" } else { "▸" };
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
            if !result_preview.is_empty() && !matches!(status, ToolStatus::Running) && !expanded {
                // Feature 007 (T023): bounded output + envelope unwrapping +
                // a line-numbered gutter (crush-style code view).
                let payload = crate::state::display_result_content(result_preview)
                    .unwrap_or_else(|| result_preview.clone());
                let (shown, affordance) =
                    bounded_lines_with_affordance(&payload, MAX_TOOL_OUTPUT_LINES);
                let col = if matches!(status, ToolStatus::Failed) {
                    theme.error
                } else {
                    theme.fg_most_subtle
                };
                let gutter_w = digits(shown.len().max(1));
                for (gi, ol) in shown.iter().enumerate() {
                    let num = format!("{:>w$} │ ", gi + 1, w = gutter_w);
                    for w in wrap(ol, content_w.saturating_sub(8 + gutter_w)) {
                        lines.push(Line::from(vec![
                            Span::styled(num.clone(), Style::default().fg(theme.fg_most_subtle.to_color())),
                            Span::styled(w, Style::default().fg(col.to_color())),
                        ]));
                    }
                }
                if let Some(msg) = affordance {
                    lines.push(Line::from(vec![Span::styled(
                        format!("    {} [click or space to expand]", msg),
                        Style::default().fg(theme.fg_most_subtle.to_color()),
                    )]));
                }
            }
            // delegate_task expandability from ToolStart: while the call is
            // Running (result_preview is only populated at ToolEnd), the
            // block still gets the expand affordance so the delegated goal
            // is reachable immediately — the expanded view shows the args/
            // summary. Scoped narrowly to delegate_task; generic tools keep
            // the completion-gated affordance (pinned by existing tests).
            if matches!(status, ToolStatus::Running)
                && result_preview.is_empty()
                && !expanded
                && name == "delegate_task"
            {
                lines.push(Line::from(vec![Span::styled(
                    "    ⣿ delegate running… [click or space to expand]".to_string(),
                    Style::default().fg(theme.fg_most_subtle.to_color()),
                )]));
            }
            // Feature 005 (T029) + Feature 007 (T023): expanded view —
            // primary parameter + full result, bounded with affordance.
            if expanded {
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
                    let pretty_args = crate::state::pretty_json_if_parses(args_text);
                    let arg_lines: Vec<&str> = pretty_args.lines().collect();
                    let gutter_w = digits(arg_lines.len().max(1));
                    for (gi, arg_line) in arg_lines.iter().enumerate() {
                        let num = format!("      {:>w$} │ ", gi + 1, w = gutter_w);
                        for w in wrap(arg_line, content_w.saturating_sub(10 + gutter_w)) {
                            lines.push(Line::from(vec![
                                Span::styled(num.clone(), Style::default().fg(theme.fg_most_subtle.to_color())),
                                Span::styled(w, Style::default().fg(theme.fg_most_subtle.to_color())),
                            ]));
                        }
                    }
                } else if name == "delegate_task" && matches!(status, ToolStatus::Running) {
                    // Running delegate_task with no args/summary text yet
                    // (ToolStart's summary can be empty for delegation): keep
                    // the expanded block honest with a live placeholder.
                    lines.push(Line::from(vec![Span::styled(
                        "    ⣿ running…".to_string(),
                        Style::default().fg(theme.busy.to_color()),
                    )]));
                }
                if let Some(result) = full_result {
                    if !result.is_empty() {
                        lines.push(Line::from(vec![Span::styled(
                            "    result:".to_string(),
                            Style::default().fg(theme.fg_more_subtle.to_color()),
                        )]));
                        // Feature 007 (T023) + crush-style formatting:
                        // envelope-unwrapped + JSON pretty-printed, gutter-
                        // numbered. TAIL WINDOW bounds to the last 200
                        // lines; FULL shows everything.
                        let formatted = crate::state::format_tool_result_for_display(result);
                        if matches!(expand_state, ReasoningExpandState::Full) {
                            let all: Vec<&str> = formatted.lines().collect();
                            let gutter_w = digits(all.len().max(1));
                            for (gi, rl) in all.iter().enumerate() {
                                let num = format!("      {:>w$} │ ", gi + 1, w = gutter_w);
                                for w in wrap(rl, content_w.saturating_sub(10 + gutter_w)) {
                                    lines.push(Line::from(vec![
                                        Span::styled(num.clone(), Style::default().fg(theme.fg_most_subtle.to_color())),
                                        Span::styled(w, Style::default().fg(theme.fg_most_subtle.to_color()),
                                        ),
                                    ]));
                                }
                            }
                        } else {
                            let (shown, affordance) =
                                bounded_tail_lines_with_affordance(&formatted, MAX_TAIL_WINDOW_LINES_TUI);
                            let total = formatted.lines().count();
                            let first = total.saturating_sub(shown.len()) + 1;
                            let gutter_w = digits(total.max(first));
                            if let Some(msg) = affordance {
                                lines.push(Line::from(vec![Span::styled(
                                    format!("      {} [click or space for full view]", msg),
                                    Style::default().fg(theme.fg_most_subtle.to_color()),
                                )]));
                            }
                            for (gi, rl) in shown.iter().enumerate() {
                                let num = format!("      {:>w$} │ ", first + gi, w = gutter_w);
                                for w in wrap(rl, content_w.saturating_sub(10 + gutter_w)) {
                                    lines.push(Line::from(vec![
                                        Span::styled(num.clone(), Style::default().fg(theme.fg_most_subtle.to_color())),
                                        Span::styled(w, Style::default().fg(theme.fg_most_subtle.to_color()),
                                        ),
                                    ]));
                                }
                            }
                        }
                    }
                }
            }
            // Feature 013 (T004): uniform trailing blank separator (FR-001).
            // (The terminal-tool early-return at line ~426-427 already has one.)
            lines.push(Line::from(vec![Span::raw("")]));
        }
        TranscriptItem::FileDiff { path, stat, lines: diff_lines, is_binary, expand_state } => {
            use crate::state::ReasoningExpandState;
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
                // Crush-style diff view: dual old/new line-number gutters
                // (insertions blank the old number, deletions blank the new
                // — diffview.go), colored +/- markers, hunk headers as
                // subtle dividers. Three-state inline expand (reasoning
                // parity): collapsed = last MAX_DIFF_LINES; tail window =
                // last 200; full = the whole diff.
                let max_height = match expand_state {
                    ReasoningExpandState::Collapsed => MAX_DIFF_LINES,
                    ReasoningExpandState::TailWindow => MAX_TAIL_WINDOW_LINES_TUI,
                    ReasoningExpandState::Full => usize::MAX,
                };
                let parsed = parse_diff_lines(diff_lines);
                // Gutter width: the max line number across the whole diff
                // (stable width regardless of the window).
                let max_num = parsed
                    .iter()
                    .filter_map(|p| p.old_line.max(p.new_line))
                    .max()
                    .unwrap_or(1);
                let gw = digits(max_num);
                let start = if parsed.len() > max_height {
                    parsed.len() - max_height
                } else {
                    0
                };
                if start > 0 {
                    lines.push(Line::from(Span::styled(
                        format!("    … ({} earlier lines hidden) [click or space to expand]", start),
                        Style::default().fg(theme.fg_most_subtle.to_color()),
                    )));
                }
                for p in &parsed[start..] {
                    // Gutter spans: dual old/new numbers (blank where the
                    // side doesn't exist) + the +/- marker column.
                    let fmt_num =
                        |n: Option<usize>| n.map(|x| x.to_string()).unwrap_or_default();
                    let gutter: Vec<Span<'static>> = match p.prefix {
                        '-' => vec![
                            Span::styled(
                                format!("{:>w$} {:>w$} ", fmt_num(p.old_line), "", w = gw),
                                Style::default().fg(theme.fg_most_subtle.to_color()),
                            ),
                            Span::styled("- ", Style::default().fg(theme.error.to_color())),
                        ],
                        '+' => vec![
                            Span::styled(
                                format!("{:>w$} {:>w$} ", "", fmt_num(p.new_line), w = gw),
                                Style::default().fg(theme.fg_most_subtle.to_color()),
                            ),
                            Span::styled("+ ", Style::default().fg(theme.success.to_color())),
                        ],
                        '@' => vec![
                            Span::styled(
                                format!("{:>w$} {:>w$} ", "…", "…", w = gw),
                                Style::default().fg(theme.fg_most_subtle.to_color()),
                            ),
                            Span::raw("  "),
                        ],
                        _ => vec![
                            Span::styled(
                                format!("{:>w$} {:>w$} ", fmt_num(p.old_line), fmt_num(p.new_line), w = gw),
                                Style::default().fg(theme.fg_most_subtle.to_color()),
                            ),
                            Span::raw("  "),
                        ],
                    };
                    let content_col = match p.prefix {
                        '@' => theme.info,
                        '+' => theme.success,
                        '-' => theme.error,
                        _ => theme.fg_base,
                    };
                    for w in wrap(p.content, content_w.saturating_sub(4 + 2 * gw + 2)) {
                        let mut row = gutter.clone();
                        row.push(Span::styled(
                            w,
                            Style::default().fg(content_col.to_color()),
                        ));
                        lines.push(Line::from(row));
                    }
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

/// Public test wrapper over [`item_lines`] for integration tests.
pub fn item_lines_for_test(
    item: &TranscriptItem,
    content_w: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    item_lines(item, content_w, theme)
}

pub fn draw_transcript(f: &mut Frame, area: Rect, app: &App, theme: Theme, focused: bool, glow: f32) {
    // Build header showing message count and scroll position.
    let msg_count = app.transcript.len();
    let scroll_info = transcript_scroll_info(msg_count, app.scroll, app.last_max_scroll.get());
    // T023 (US5, FR-009, D2 Invariant 1): the focused-header scroll-key
    // hint routes through the shared `transcript_scroll_hint` so the
    // focused pane composes the IDENTICAL segment. Byte-identical title.
    let title = if focused {
        format!(" conversation {} ", focused_header_segments(&scroll_info))
    } else {
        format!(" conversation {} ", scroll_info)
    };
    let block = panel_block(&title, theme, focused, glow);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // T023 (US5, FR-009, D2 Invariant 1): the whole body (geometry,
    // lazy line build, scroll accounting, scrollbar, below-badge) is the
    // shared `render_transcript_body` — the SAME code path the focused
    // subagent pane renders through. Byte-identical output.
    render_transcript_body(
        f,
        inner,
        &app.transcript,
        &app.streaming_assistant,
        app.scroll,
        theme,
        &app.last_text_area,
        &app.last_max_scroll,
    );
}

/// T023 (US5, FR-009, D2 Invariant 1 — "single rendering source"): the
/// transcript body BOTH screens render — inner-area geometry (scrollbar
/// column reservation), the lazy newest-first line build (`item_lines` +
/// the shared `streaming_tail_lines` tail), bottom-anchored scroll
/// accounting with max-scroll recording, the `draw_scrollbar` track/thumb,
/// and the `draw_below_badge` scrolled-up indicator. Extracted verbatim
/// from `draw_transcript` so the orchestrator screen and the focused
/// subagent pane (`draw_pane_transcript`) share ONE implementation;
/// only the data source (main vs pane) and the geometry/scroll-recording
/// cells differ, passed in by the caller.
///
/// The caller renders the border block and passes its `inner` rect; an
/// empty transcript simply renders nothing inside the block (identical
/// empty-state behavior on both screens — header shows " 0 messages ·
/// live ", no scrollbar, no badge, because all three derive from the same
/// helpers over the same empty inputs).
fn render_transcript_body(
    f: &mut Frame,
    inner: Rect,
    transcript: &std::collections::VecDeque<TranscriptItem>,
    streaming_assistant: &str,
    scroll: Option<usize>,
    theme: Theme,
    text_area_cell: &std::cell::Cell<(u16, u16, u16, u16)>,
    max_scroll_cell: &std::cell::Cell<usize>,
) {
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
    text_area_cell.set((
        text_area.x,
        text_area.y,
        text_area.width,
        text_area.height,
    ));

    let content_w = content_width;
    let visible = text_area.height as usize;
    let offset = scroll.unwrap_or(0);
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
    // T023 (US5, FR-009, D2 Invariant 1): the tail chrome is the shared
    // `streaming_tail_lines` — verbatim extraction, byte-identical output.
    if !streaming_assistant.is_empty() {
        let tail = streaming_tail_lines(streaming_assistant, content_w, theme);
        built += tail.len();
        blocks_rev.push(tail);
    }

    let mut exhausted = true;
    for item in transcript.iter().rev() {
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
    max_scroll_cell.set(max_scroll);

    let clamped = offset.min(max_scroll);
    let scroll_rows = total.saturating_sub(visible + clamped).min(u16::MAX as usize);

    let para = Paragraph::new(Text::from(lines)).scroll((scroll_rows as u16, 0));
    f.render_widget(para, text_area);

    // ── Scrollbar ────────────────────────────────────────────────────
    draw_scrollbar(f, scrollbar_area, theme, total, visible, clamped, scroll.is_some());

    // Scrolled-up indicator: bottom-right badge showing the distance to live.
    draw_below_badge(f, text_area, theme, scroll, clamped);
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

/// Generic transcript hit-test core (expandable-stats / pane parity):
/// resolves a screen row to an item index in ANY bottom-anchored transcript
/// render (main or pane), given the raw render inputs. The accounting
/// mirrors [`draw_transcript`]: items newest-last with the streaming tail
/// (if any) bottommost, scroll anchored to the bottom.
pub fn transcript_hit_test_core(
    transcript: &std::collections::VecDeque<TranscriptItem>,
    streaming: &str,
    scroll: Option<usize>,
    last_max_scroll: usize,
    (_tx, ty, tw, th): (u16, u16, u16, u16),
    theme: Theme,
    row: u16,
) -> Option<usize> {
    if th == 0 || tw == 0 {
        return None;
    }
    if row < ty || row >= ty + th {
        return None;
    }
    let content_w = tw as usize;
    let visible = th as usize;
    let offset = scroll.unwrap_or(0);
    let click_row_from_top = (row - ty) as usize;

    let mut blocks_rev: Vec<(usize, usize)> = Vec::new();
    let mut built = 0usize;
    let has_streaming = !streaming.is_empty();
    if has_streaming {
        built += 1 + wrap(streaming, content_w.saturating_sub(2)).len();
    }
    let needed = visible + offset + 1;
    for (i, item) in transcript.iter().enumerate().rev() {
        if built >= needed {
            break;
        }
        let ls = item_lines(item, content_w, theme).len();
        built += ls;
        blocks_rev.push((i, ls));
    }
    let items_fwd: Vec<(usize, usize)> = blocks_rev.into_iter().rev().collect();
    let streaming_line_count = if has_streaming {
        1 + wrap(streaming, content_w.saturating_sub(2)).len()
    } else {
        0
    };
    let total = items_fwd.iter().map(|(_, c)| *c).sum::<usize>() + streaming_line_count;
    let clamped = offset.min(last_max_scroll.max(total.saturating_sub(visible)));
    let scroll_rows = total.saturating_sub(visible + clamped);
    let content_line = scroll_rows + click_row_from_top;
    if content_line >= total {
        return None;
    }
    let mut acc = 0usize;
    for &(item_idx, count) in &items_fwd {
        if content_line < acc + count {
            return Some(item_idx);
        }
        acc += count;
    }
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

/// T023 (US5, FR-009, D2 Invariant 1): the focused-header scroll-key hint
/// `draw_transcript` shows ("[scroll: j/k g/G PgUp/PgDn /search ]"),
/// extracted verbatim so the focused pane composes the IDENTICAL segment
/// from the same constant (no pane-local restatement).
fn transcript_scroll_hint() -> &'static str {
    "[scroll: j/k g/G PgUp/PgDn /search ]"
}

/// T023 (US5, FR-009, D2 Invariant 1): the combined focused-header segment —
/// scroll-key hint, single space, scroll-info — that BOTH the orchestrator's
/// `draw_transcript` title and the focused pane's header compose. One
/// composer so ordering/spacing can never drift between the two screens.
fn focused_header_segments(scroll_info: &str) -> String {
    format!("{} {}", transcript_scroll_hint(), scroll_info)
}

/// T036 (US5, FR-001/FR-009, SC-003): compose the focused subagent pane's
/// header the SAME way `draw_transcript` composes its single top title —
/// " ◆ subagent: {goal} [{model}] {status} {segment} " where `segment` is
/// the shared `focused_header_segments` composition when focused, the bare
/// `transcript_scroll_info` otherwise (exactly the orchestrator's focused/
/// unfocused title logic, riding the same helpers).
///
/// Ratatui renders LEFT titles clipped on the right when they exceed the
/// block's usable width (ratatui-widgets 0.3 `Block::render_left_titles`
/// caps `title.width` at the titles area — the trailing scroll segment
/// would be the part cut off). So the helper measures the composed title
/// (`unicode_width`, the same width engine ratatui buffers use) against
/// `pane_width - 2` (inside the block's left+right borders):
///
/// - FITS → returned as the top `title`, `bottom_fallback = None`.
/// - DOESN'T FIT → the bare pane title (goal/model/status, byte-identical
///   to the pre-T036 top title) is returned for the top row and the segment
///   rides the block's BOTTOM-right corner (`bottom_fallback = Some`), the
///   pre-T036 placement — keeping the scroll-info/hint fully visible at
///   every geometry instead of truncating it into ambiguity.
///
/// At the quickstart.md minimum geometry (96 cols) the pane's title row
/// holds 43 usable columns (96 − 19 rail − 34 sidebar − 2 borders) — not
/// even the bare 47-column title fits, so the fallback preserves today's
/// rendering there; the unified top placement engages on wider terminals
/// where the composed title genuinely fits.
struct PaneHeaderTitle {
    title: String,
    bottom_fallback: Option<String>,
}

fn pane_header_title(
    pane: &SubagentPane,
    scroll_info: &str,
    focused: bool,
    pane_width: u16,
) -> PaneHeaderTitle {
    let status = match pane.status {
        SubagentStatus::Running => "· live",
        SubagentStatus::Done => "· done",
        SubagentStatus::Failed => "· failed",
        SubagentStatus::Pending => "· queued",
        // Spec 020 (T030): halted before completing its goal.
        SubagentStatus::Stopped => "· stopped",
    };
    let base = format!(
        " ◆ subagent: {} [{}] {} ",
        pane.goal, pane.model, status
    );
    let segment = if focused {
        focused_header_segments(scroll_info)
    } else {
        scroll_info.to_string()
    };
    // Compose the unified top title exactly like `draw_transcript`'s
    // " conversation {segment} " — bare pane title, then the segment
    // (the base's trailing space provides the separator).
    let composed = format!("{}{}", base, segment);
    // Titles area = pane width minus the left+right border columns
    // (ratatui `Block::titles_area`).
    let usable = pane_width.saturating_sub(2) as usize;
    if composed.width() <= usable {
        PaneHeaderTitle {
            title: composed,
            bottom_fallback: None,
        }
    } else {
        // Fit fallback (T036): top row keeps the bare pane title; the
        // segment rides the bottom-right corner, fully visible.
        PaneHeaderTitle {
            title: base,
            bottom_fallback: Some(segment),
        }
    }
}

#[cfg(test)]
mod t036_pane_header_title_tests {
    use super::*;
    use crate::state::SubagentPane;

    /// Build a pane with the parity-suite fixture shape ("parity child"
    /// goal, "test-model" model, Running) through the real spawn path —
    /// `pane_header_title` is otherwise a pure function over the pane +
    /// width.
    fn pane(goal: &str, model: &str) -> SubagentPane {
        let mut app = App::new("s", "m");
        app.apply(joey_agent_core::events::AgentEvent::SubagentSpawn {
            id: 1,
            goal: goal.to_string(),
            model: model.to_string(),
            toolset_summary: "file, web".to_string(),
            depth: 0,
        });
        app.subagent_panes
            .pop()
            .expect("spawn created the pane")
    }

    fn scroll_info_40_live() -> String {
        transcript_scroll_info(40, None, 0) // " 40 messages · live "
    }

    /// The composed top title is exactly the orchestrator-style
    /// composition: bare pane title + shared segment, nothing else.
    #[test]
    fn t036_top_title_is_orchestrator_style_composition() {
        let p = pane("parity child", "test-model");
        let h = pane_header_title(&p, &scroll_info_40_live(), false, 140);
        assert!(h.bottom_fallback.is_none(), "fits at 140 cols");
        assert_eq!(
            h.title,
            " ◆ subagent: parity child [test-model] · live  40 messages · live "
        );
    }

    /// Focused: the segment is the shared `focused_header_segments`
    /// composition (scroll-key hint + space + scroll-info), byte-identical
    /// to what `draw_transcript`'s focused title would carry.
    #[test]
    fn t036_focused_top_title_carries_shared_hint_segment() {
        let p = pane("parity child", "test-model");
        let info = scroll_info_40_live();
        let h = pane_header_title(&p, &info, true, 200);
        assert!(h.bottom_fallback.is_none(), "focused title fits at 200");
        assert!(
            h.title.contains(&focused_header_segments(&info)),
            "focused composition embedded verbatim: {:?}",
            h.title
        );
        assert!(h.title.contains("[scroll: j/k g/G PgUp/PgDn /search ]"));
        // ...and ends with the scroll-info (hint comes first, like the
        // orchestrator's focused title).
        assert!(h.title.ends_with("40 messages · live "));
    }

    /// Boundary: the fit check is against `pane_width - 2` (inside the
    /// borders) using display width, so exactly-at-capacity fits and one
    /// column under falls back.
    #[test]
    fn t036_fit_boundary_is_width_minus_borders() {
        let p = pane("parity child", "test-model");
        let info = scroll_info_40_live();
        let composed_w =
            pane_header_title(&p, &info, false, 200).title.width();
        // Exactly fits → top placement.
        let exact = pane_header_title(&p, &info, false, (composed_w + 2) as u16);
        assert!(exact.bottom_fallback.is_none(), "exact fit composes top");
        // One column short → fallback keeps the segment fully visible on
        // the bottom row and the top title stays the bare pane title.
        let short = pane_header_title(&p, &info, false, (composed_w + 1) as u16);
        assert_eq!(
            short.title, " ◆ subagent: parity child [test-model] · live ",
            "fallback top title is the bare pane title (pre-T036 string)"
        );
        assert_eq!(
            short.bottom_fallback.as_deref(),
            Some(info.as_str()),
            "fallback carries the bare scroll-info segment"
        );
    }

    /// Min geometry (quickstart ≥96 cols): through the real layout the
    /// pane's title row holds 43 usable columns (96 − 19 rail − 34
    /// sidebar − 2 borders), and even the BARE 47-col title exceeds it —
    /// so the pane is on the bottom-corner fallback there, preserving the
    /// pre-T036 rendering (the documented behavior Wave 4 records in
    /// parity.md).
    #[test]
    fn t036_min_geometry_96_cols_uses_bottom_fallback() {
        let p = pane("parity child", "test-model");
        let info = scroll_info_40_live();
        // 96 − 19 rail − 34 sidebar = 43-col pane.
        let h = pane_header_title(&p, &info, false, 43);
        assert!(
            h.bottom_fallback.is_some(),
            "segment rides the bottom-right corner at min geometry"
        );
        assert!(h.title.contains("subagent: parity child"));
    }

    /// Spec 020 (T030, FR-016): a pane in the terminal `Stopped` state
    /// renders the "· stopped" status word in its header — the stopped
    /// state must be distinguishable from done/failed in listings.
    #[test]
    fn t030_stopped_pane_header_shows_stopped_status() {
        let mut p = pane("parity child", "test-model");
        p.status = SubagentStatus::Stopped;
        let h = pane_header_title(&p, &scroll_info_40_live(), false, 140);
        // The STATUS word sits right after the model bracket; the trailing
        // "· live" belongs to the scroll-info segment ("40 messages ·
        // live" = auto-follow), not the status.
        assert!(
            h.title.contains("[test-model] · stopped"),
            "header carries the stopped status word: {:?}",
            h.title
        );
        assert!(
            !h.title.contains("· done") && !h.title.contains("· failed"),
            "no other status word bleeds in: {:?}",
            h.title
        );
        // Boundary parity: the bare title (fallback path) also shows it.
        p.status = SubagentStatus::Stopped;
        let bare = pane_header_title(&p, &scroll_info_40_live(), false, 43);
        assert!(
            bare.title.contains("· stopped"),
            "bare/fallback title carries it too: {:?}",
            bare.title
        );
    }
}

/// T023 (US5, FR-009, D2 Invariant 1): the live streaming-assistant tail
/// block `draw_transcript` renders (bold info "◆ agent " header + indented
/// fg_base wrapped lines), extracted verbatim so `draw_pane_transcript`
/// composes the identical tail chrome from the same function.
fn streaming_tail_lines(streaming: &str, content_w: usize, theme: Theme) -> Vec<Line<'static>> {
    let mut tail = vec![Line::from(vec![Span::styled(
        "◆ agent ",
        Style::default().fg(theme.info.to_color()).add_modifier(Modifier::BOLD),
    )])];
    for wl in wrap(streaming, content_w.saturating_sub(2)) {
        tail.push(Line::from(vec![Span::styled(
            format!("  {}", wl),
            Style::default().fg(theme.fg_base.to_color()),
        )]));
    }
    tail
}

/// Shared scroll-info header segment (feature 017 T008 / US1, D2 "parity by
/// construction"): the exact " N messages · … " format `draw_transcript`
/// builds for its header. Extracted verbatim so the focused subagent pane
/// composes the IDENTICAL segment from its own transcript.
fn transcript_scroll_info(msg_count: usize, scroll: Option<usize>, last_max_scroll: usize) -> String {
    if let Some(offset) = scroll {
        let max = last_max_scroll;
        if max > 0 {
            let pct = ((1.0 - (offset as f64 / max as f64)) * 100.0).round() as usize;
            format!(" {} messages · {}% from top ", msg_count, pct)
        } else {
            format!(" {} messages ", msg_count)
        }
    } else {
        format!(" {} messages · live ", msg_count)
    }
}

/// Draw a scrollbar on the right edge of the transcript.
fn draw_scrollbar(
    f: &mut Frame,
    area: Rect,
    theme: Theme,
    total_lines: usize,
    visible_lines: usize,
    current_offset: usize,
    scrolled: bool,
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
    let thumb_color = if scrolled {
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

/// Shared scrolled-up indicator (feature 017 T008 / US1, D2): the exact
/// bottom-right " ↓ N line(s) below " badge `draw_transcript` paints,
/// extracted verbatim so the focused subagent pane composes the identical
/// affordance for its own text area.
fn draw_below_badge(
    f: &mut Frame,
    text_area: Rect,
    theme: Theme,
    scroll: Option<usize>,
    clamped: usize,
) {
    if scroll.is_some() && clamped > 0 {
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

/// Number of decimal digits in `n` (min 1).
fn digits(n: usize) -> usize {
    n.max(1).to_string().len()
}

/// One rendered line of a unified diff, split into its display parts for the
/// crush-style dual gutter: (old_line, new_line, prefix, content).
struct DiffLineParts<'a> {
    old_line: Option<usize>,
    new_line: Option<usize>,
    prefix: char,
    content: &'a str,
}

/// Walk a unified diff's lines, tracking old/new line numbers from the hunk
/// headers. Header/meta lines (`---`, `+++`, `diff --git`, `index`) yield
/// None pairs (rendered without gutters).
fn parse_diff_lines(diff_lines: &[String]) -> Vec<DiffLineParts<'_>> {
    let mut out = Vec::with_capacity(diff_lines.len());
    let mut old = 0usize;
    let mut new = 0usize;
    for l in diff_lines {
        if l.starts_with("@@") {
            // Parse `@@ -oldStart,oldLen +newStart,newLen @@` (start may be
            // bare when len == 1; negative-zero start means line 0).
            let extract = |tag: char| -> Option<usize> {
                let idx = l.find(tag)?;
                let rest = &l[idx + 1..];
                let num: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                num.parse::<usize>().ok().map(|n| n.max(1))
            };
            old = extract('-').unwrap_or(1);
            new = extract('+').unwrap_or(1);
            out.push(DiffLineParts { old_line: None, new_line: None, prefix: '@', content: l });
        } else if l.starts_with("---") || l.starts_with("+++") || l.starts_with("diff ") || l.starts_with("index ") {
            out.push(DiffLineParts { old_line: None, new_line: None, prefix: ' ', content: l });
        } else if let Some(content) = l.strip_prefix('-') {
            out.push(DiffLineParts { old_line: Some(old), new_line: None, prefix: '-', content });
            old += 1;
        } else if let Some(content) = l.strip_prefix('+') {
            out.push(DiffLineParts { old_line: None, new_line: Some(new), prefix: '+', content });
            new += 1;
        } else if let Some(content) = l.strip_prefix(' ') {
            out.push(DiffLineParts { old_line: Some(old), new_line: Some(new), prefix: ' ', content });
            old += 1;
            new += 1;
        } else {
            out.push(DiffLineParts { old_line: None, new_line: None, prefix: ' ', content: l });
        }
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
        // textwrap collapses blank lines to zero rows; preserve them as one
        // empty row each so numbered gutters and code views stay aligned
        // with the source (a missing line number mid-output is a lie).
        if line.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let wrapped = textwrap::wrap(line, width);
        if wrapped.is_empty() {
            out.push(String::new());
        } else {
            for w in wrapped {
                out.push(w.into_owned());
            }
        }
    }
    // An entirely-empty/whitespace input still renders as one blank row
    // (`"".lines()` yields nothing — callers expect at least one row).
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

// ── Reasoning box (live) ───────────────────────────────────────────────────

/// Live reasoning panel. In docked mode it renders as the fixed 8-row strip
/// at the bottom of the conversation area; in expanded mode
/// (`app.reasoning_expanded`, toggled by clicking the panel) the same
/// widget renders in the main screen area. Either way the panel records its
/// rect into `app.last_reasoning_rect` for mouse hit-testing, and zeroes it
/// when there is nothing live to show.
pub fn draw_reasoning(f: &mut Frame, area: Rect, app: &App, theme: Theme, spinner: &Spinner) {
    // T021 (US4, FR-008, D6) + T034: target-aware source. With a subagent
    // pane focused the panel renders the PANE's live `streaming_reasoning`
    // (content-based live condition: a non-empty accumulator IS a live
    // block — panes carry no `reasoning_open` flag) with the pane's own
    // expansion/view/timer state (full main-screen parity, FR-008) and
    // never leaks the main accumulators (focused-view isolation).
    // Unfocused, every field resolves exactly as before (byte-identical
    // main screen).
    let (live, stream, expanded, started, view) = if let Some(pane) = app.focused_pane() {
        (
            !pane.streaming_reasoning.is_empty(),
            &pane.streaming_reasoning,
            pane.reasoning_expanded,
            pane.reasoning_started,
            pane.reasoning_view,
        )
    } else {
        (
            app.reasoning_open && !app.streaming_reasoning.is_empty(),
            &app.streaming_reasoning,
            app.reasoning_expanded,
            app.reasoning_started,
            app.reasoning_view,
        )
    };
    // Record geometry for click hit-testing; zero it when not live so
    // stale rects can't catch clicks meant for the transcript.
    app.last_reasoning_rect.set(if live {
        (area.x, area.y, area.width, area.height)
    } else {
        (0, 0, 0, 0)
    });

    let title = if !live {
        "reasoning"
    } else if expanded {
        " reasoning · live · click or Esc to collapse "
    } else {
        // T034: the pane panel toggles the PANE's expansion, so it
        // carries the SAME affordance as main (FR-008 parity).
        " reasoning · live · click to expand "
    };
    let block = gradient_block_focused(title, theme, 0.5);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if !live {
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

    // Header: thinking duration (Feature 007) + overflow indicator.
    let mut header = vec![
        Span::styled(
            "◆ ".to_string(),
            Style::default().fg(theme.accent.to_color()),
        ),
        Span::styled(
            "thinking".to_string(),
            Style::default()
                .fg(theme.accent.to_color())
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(started) = started {
        let secs = started.elapsed().as_secs();
        header.push(Span::styled(
            format!("  {}s", secs),
            Style::default().fg(theme.info.to_color()),
        ));
    }
    lines.push(Line::from(header));

    // Body: the live stream, hard-wrapped, windowed by the view state —
    // tail-anchored while auto-following, absolute-window when the user
    // has scrolled up (frozen). The render also publishes the anchor's
    // upper bound so input handlers can detect "scrolled to the bottom".
    let wrapped = wrap(stream, content_w);
    let body_rows = inner.height.saturating_sub(lines.len() as u16 + 1).max(1) as usize; // +1 for the footer line
    let total = wrapped.len();
    let visible = body_rows.min(total);
    let max_anchor = total.saturating_sub(visible);
    app.last_reasoning_max_anchor.set(max_anchor);
    let start = match view {
        None => max_anchor, // following: window pinned to the live tail
        // Frozen: absolute window top, clamped into the valid range. (If
        // the clamp lands at the tail the view displays the tail; the
        // follow flag itself re-enables on the next scroll-down, which
        // resumes follow at `target >= max_anchor` — no lingering freeze.)
        Some(anchor) => anchor.min(max_anchor),
    };
    let end = (start + visible).min(total);
    for wl in &wrapped[start..end] {
        lines.push(Line::from(vec![Span::styled(
            wl.clone(),
            Style::default().fg(theme.fg_more_subtle.to_color()),
        )]));
    }

    // Footer: spinner (live) + overflow indicator when scrolled off the top.
    let mut footer = vec![Span::raw(" "), spinner.styled_glyph(theme)];
    if start > 0 {
        footer.push(Span::styled(
            format!("  ↑{} lines above", start),
            Style::default().fg(theme.fg_most_subtle.to_color()),
        ));
    }
    if total > visible && view.is_some() {
        footer.push(Span::styled(
            format!(
                "  ↓{} below · scroll to bottom to resume",
                total - end
            ),
            Style::default().fg(theme.fg_more_subtle.to_color()),
        ));
    }
    lines.push(Line::from(footer));

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

// ── Maximized terminal output viewer ─────────────────────────────────────

/// Fullscreen-ish code/output viewer for a tool call (terminal or generic).
/// Takes over the main screen area (below a transcript strip — see
/// `render_body`) while `app.output_viewer_open`. Shows the live
/// accumulation while the tool runs and the formatted full result after it
/// finishes — envelope-unwrapped and JSON pretty-printed — in a
/// text-editor-like view with a line-numbered gutter. Auto-follows the tail
/// unless the user scrolled up (frozen anchor); scrolling back to the bottom
/// resumes follow. Records its rect for mouse hit-testing.
pub fn draw_output_viewer(f: &mut Frame, area: Rect, app: &App, theme: Theme, spinner: &Spinner) {
    // T020 (US4, FR-006/FR-008, D6): target-aware source. With a subagent
    // pane focused the viewer renders the PANE's tool output (indices and
    // text resolve against the pane's transcript; `output_viewer_index`
    // is main-transcript-indexed so it is IGNORED while a pane is focused
    // and the pane's most recent tool is targeted instead). Unfocused,
    // every field resolves exactly as before (byte-identical main view).
    let pane = app.focused_pane();
    let pane_transcript = pane.map(|p| &p.transcript);
    let idx = if let Some(transcript) = pane_transcript {
        transcript
            .iter()
            .rposition(|i| matches!(i, TranscriptItem::Tool { .. }))
    } else {
        app.output_viewer_index.or_else(|| app.most_recent_tool_item())
    };
    let resolve = |i: usize| -> Option<&TranscriptItem> {
        pane_transcript.map(|t| t.get(i)).unwrap_or_else(|| app.transcript.get(i))
    };
    let live = idx
        .map(|i| {
            matches!(
                resolve(i),
                Some(TranscriptItem::Tool { status: ToolStatus::Running, .. })
            )
        })
        .unwrap_or(false);
    let is_term = idx
        .map(|i| {
            matches!(
                resolve(i),
                Some(TranscriptItem::Tool { is_terminal: true, .. })
            )
        })
        .unwrap_or(false);

    // Rect for mouse hit-testing (wheel scrolling inside the viewer).
    app.last_output_viewer_rect.set((area.x, area.y, area.width, area.height));

    let (cmd, tool_name, status_icon, exit_code, elapsed) = idx
        .and_then(|i| match resolve(i) {
            Some(TranscriptItem::Tool { name, summary, status, exit_code, duration_secs, .. }) => {
                Some((summary.clone(), name.clone(), *status, *exit_code, *duration_secs))
            }
            _ => None,
        })
        .unwrap_or_else(|| (String::new(), String::new(), ToolStatus::Done, None, None));

    let title = if live {
        " ⣿ output · LIVE · Ctrl+O or Esc to restore "
    } else {
        match status_icon {
            ToolStatus::Done => " ✓ output · finished · Ctrl+O or Esc to restore ",
            _ => " ✗ output · failed · Ctrl+O or Esc to restore ",
        }
    };
    let block = gradient_block_focused(title, theme, 0.6);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let content_w = inner.width.max(1) as usize;
    let mut lines: Vec<Line> = Vec::new();

    // Header line: terminal → `$ <cmd> (exit N) N.Ns`; generic tool →
    // `<name> <summary>`.
    let mut header = if is_term {
        vec![
            Span::styled(
                " $ ".to_string(),
                Style::default().fg(theme.accent.to_color()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                one_line(&cmd, content_w.saturating_sub(24)),
                Style::default().fg(theme.fg_base.to_color()),
            ),
        ]
    } else {
        vec![
            Span::styled(
                format!(" {} ", tool_name),
                Style::default().fg(theme.accent.to_color()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                one_line(&cmd, content_w.saturating_sub(tool_name.len() + 8)),
                Style::default().fg(theme.fg_base.to_color()),
            ),
        ]
    };
    if let Some(code) = exit_code {
        if code != 0 {
            header.push(Span::styled(
                format!("  (exit {})", code),
                Style::default().fg(theme.error.to_color()),
            ));
        }
    }
    if let Some(d) = elapsed {
        header.push(Span::styled(
            format!("  {:.1}s", d),
            Style::default().fg(theme.fg_more_subtle.to_color()),
        ));
    }
    lines.push(Line::from(header));
    lines.push(Line::from(vec![Span::raw("")]));

    // Body: windowed wrapped lines, tail-anchored while following, with a
    // line-number gutter (text-editor-like view).
    // T020: pane-focused → the pane tool's text (same precedence rules as
    // the main `output_viewer_text`: live accumulation while Running,
    // formatted full result once finished).
    let text = if let Some(transcript) = pane_transcript {
        match idx.and_then(|i| transcript.get(i)) {
            Some(TranscriptItem::Tool {
                status,
                live_output,
                full_result,
                result_preview,
                ..
            }) => {
                let full = full_result
                    .as_deref()
                    .filter(|f| !f.is_empty())
                    .map(crate::state::format_tool_result_for_display);
                match status {
                    ToolStatus::Running => live_output.clone(),
                    _ => full
                        .or_else(|| {
                            let p = result_preview.as_str();
                            (!p.is_empty()).then(|| p.to_string())
                        })
                        .unwrap_or_else(|| live_output.clone()),
                }
            }
            _ => String::new(),
        }
    } else {
        app.output_viewer_text()
    };
    let gutter_w = digits(text.lines().count().max(1));
    let wrapped: Vec<(usize, String)> = text
        .lines()
        .enumerate()
        .flat_map(|(i, l)| {
            wrap(l, content_w.saturating_sub(gutter_w + 3))
                .into_iter()
                .map(move |w| (i + 1, w))
        })
        .collect();
    let body_rows = inner.height.saturating_sub(lines.len() as u16 + 1).max(1) as usize; // +1 footer
    let total = wrapped.len();
    let visible = body_rows.min(total);
    let max_anchor = total.saturating_sub(visible);
    app.last_output_viewer_max_anchor.set(max_anchor);
    let start = match app.output_viewer_view {
        None => max_anchor,
        Some(anchor) => anchor.min(max_anchor),
    };
    let end = (start + visible).min(total);
    for (ln, wl) in &wrapped[start..end] {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:>w$} │ ", ln, w = gutter_w),
                Style::default().fg(theme.fg_most_subtle.to_color()),
            ),
            Span::styled(wl.clone(), Style::default().fg(theme.fg_more_subtle.to_color())),
        ]));
    }

    // Footer: live spinner or follow state + overflow indicators.
    let mut footer = vec![Span::raw(" ")];
    if live {
        footer.push(spinner.styled_glyph(theme));
        footer.push(Span::styled(
            " streaming".to_string(),
            Style::default().fg(theme.busy.to_color()),
        ));
    } else if app.output_viewer_view.is_none() {
        footer.push(Span::styled(
            "end of output".to_string(),
            Style::default().fg(theme.fg_most_subtle.to_color()),
        ));
    } else {
        footer.push(Span::styled(
            "frozen — scroll to bottom to resume".to_string(),
            Style::default().fg(theme.fg_most_subtle.to_color()),
        ));
    }
    if start > 0 {
        footer.push(Span::styled(
            format!("  ↑{} lines above", start),
            Style::default().fg(theme.fg_most_subtle.to_color()),
        ));
    }
    if total > visible && app.output_viewer_view.is_some() {
        footer.push(Span::styled(
            format!("  ↓{} below", total - end),
            Style::default().fg(theme.fg_more_subtle.to_color()),
        ));
    }
    lines.push(Line::from(footer));

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

// ── Expandable context stream (expandable-stats feature) ──────────────────
//
// Shared builder for the stats pages' context stream: one header row per
// entry (index/role/tokens/preview + expand affordance), and — when the
// entry is in the expansion set — the FULL content rendered inline with a
// line-number gutter, the same visual language as the maximized output
// viewer. Returns the built lines plus a per-entry (first_row, row_count)
// map used for click hit-testing against the pane's content grid.

/// Cap on the number of wrapped lines an expanded entry may occupy in the
/// stream. Entries larger than this get a "… N more lines (open output
/// viewer)" affordance instead of an unbounded inline dump; a giant tool
/// result would otherwise swallow the whole stream.
const MAX_EXPANDED_ENTRY_LINES: usize = 40;

/// The built context-stream pieces: display lines + the entry geometry.
pub(crate) struct ContextStream {
    pub lines: Vec<Line<'static>>,
    /// (entry_index, first_row, row_count) — rows are indices into `lines`.
    pub entry_rows: Vec<(usize, usize, usize)>,
    /// Total line count (== lines.len()).
    pub total: usize,
}

/// Build the context stream for a stats page.
///
/// * `entries` — the snapshot's entries (oldest first).
/// * `expanded` — which entry indices are expanded.
/// * `content_w` — usable width for previews/content.
/// * `empty_note` — placeholder line when there are no entries.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_context_stream(
    entries: &[joey_agent_core::events::ContextEntry],
    expanded: &std::collections::HashSet<usize>,
    content_w: usize,
    theme: Theme,
    empty_note: &str,
) -> ContextStream {
    let mut lines: Vec<Line> = Vec::new();
    let mut entry_rows: Vec<(usize, usize, usize)> = Vec::new();
    let idx_w = entries.len().max(1).to_string().len();

    for (i, e) in entries.iter().enumerate() {
        let first_row = lines.len();
        let (role_label, role_col) = match e.role.as_str() {
            "user" => ("user", theme.accent),
            "assistant" => ("asst", theme.info),
            "tool" => ("tool", theme.warning),
            _ => (e.role.as_str(), theme.fg_more_subtle),
        };
        let flag = if e.is_compressed_summary {
            " ⤳compressed"
        } else if e.has_tool_calls {
            " ⚒calls"
        } else {
            ""
        };
        let is_expanded = expanded.contains(&i);
        // Expand affordance: ▸ collapsed / ▾ expanded (always shown — every
        // entry is expandable, matching the main transcript's click-any-item).
        let arrow = if is_expanded { "▾" } else { "▸" };
        let header_preview_w = content_w.saturating_sub(24);
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {:>idx_w$} ", i + 1, idx_w = idx_w),
                Style::default().fg(theme.fg_most_subtle.to_color()),
            ),
            Span::styled(
                arrow.to_string(),
                Style::default()
                    .fg(theme.primary.to_color())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {:<5}", role_label),
                Style::default().fg(role_col.to_color()),
            ),
            Span::styled(
                format!("{:>7}t ", fmt_tokens(e.tokens)),
                Style::default().fg(theme.fg_more_subtle.to_color()),
            ),
            Span::styled(
                format!(
                    "{}{}",
                    if is_expanded {
                        String::new()
                    } else {
                        one_line(&e.preview, header_preview_w)
                    },
                    if is_expanded { "" } else { flag }
                ),
                Style::default().fg(theme.fg_subtle.to_color()),
            ),
        ]));

        if is_expanded {
            // Inline full content with a gutter (output-viewer style).
            let text = &e.full_content;
            let gutter_w = digits(text.lines().count().max(1));
            let body_w = content_w.saturating_sub(gutter_w + 5).max(4);
            let wrapped: Vec<(usize, String)> = text
                .lines()
                .enumerate()
                .flat_map(|(ln, l)| {
                    wrap(l, body_w)
                        .into_iter()
                        .map(move |w| (ln + 1, w))
                })
                .collect();
            let truncated = wrapped.len() > MAX_EXPANDED_ENTRY_LINES;
            let shown = if truncated {
                &wrapped[..MAX_EXPANDED_ENTRY_LINES]
            } else {
                &wrapped[..]
            };
            for (ln, wl) in shown {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{:>w$} │ ", ln, w = gutter_w),
                        Style::default().fg(theme.fg_most_subtle.to_color()),
                    ),
                    Span::styled(
                        wl.clone(),
                        Style::default().fg(theme.fg_more_subtle.to_color()),
                    ),
                ]));
            }
            if truncated {
                lines.push(Line::from(vec![Span::styled(
                    format!(
                        "   … {} more lines — too large to expand inline",
                        wrapped.len() - MAX_EXPANDED_ENTRY_LINES
                    ),
                    Style::default().fg(theme.fg_most_subtle.to_color()),
                )]));
            }
            lines.push(Line::from(vec![Span::raw("")]));
        }
        entry_rows.push((i, first_row, lines.len() - first_row));
    }

    if entries.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            format!(" {}", empty_note),
            Style::default().fg(theme.fg_most_subtle.to_color()),
        )]));
    }

    ContextStream {
        entry_rows,
        total: lines.len(),
        lines,
    }
}

/// Resolve a click (row within the stream's visible window) to the context
/// entry it belongs to. `start` is the window's first visible row index.
#[allow(dead_code)] // superseded by App-level geometry hit-testing; kept for
                    // direct callers building their own ContextStream.
pub(crate) fn context_entry_hit(
    stream: &ContextStream,
    start: usize,
    row: u16,
    area_y: u16,
) -> Option<usize> {
    let content_row = row.saturating_sub(area_y) as usize + start;
    for &(entry, first_row, count) in &stream.entry_rows {
        if content_row >= first_row && content_row < first_row + count {
            return Some(entry);
        }
    }
    None
}

// ── Agent stats page (maximized context window) ────────────────────────────

/// Render a bounded horizontal bar (`filled/total`), e.g. context usage.
fn usage_bar(width: usize, ratio: f32, theme: &Theme) -> String {
    let w = width.max(4);
    let filled = ((ratio.clamp(0.0, 1.0) * w as f32).round()) as usize;
    let mut s = String::with_capacity(w * 3);
    for i in 0..w {
        s.push(if i < filled { '█' } else { '░' });
    }
    let _ = theme;
    s
}

// ── Shared stats-page section builders (T035 / FR-009 / SC-003, D2) ────────
//
// Feature 017 T035 restored the plan-D2 "same widget functions" invariant
// for the two stats pages: `draw_stats_page` (orchestrator) and
// `draw_pane_stats_page` (focused subagent) are thin adapters composing
// the SAME shared builders below — dashboard context row, breakdown row,
// session row, usage sparkline, windowed context stream, and footer.
// Only the DATA and the pane-specific labels (" child ", "goal:") differ,
// passed in by each adapter; the builders parameterize labels and content
// and never homogenize them. Rendering is byte-identical to the
// pre-refactor hand-rolled pages for BOTH screens (pure refactor).

/// Threshold styling for the dashboard context row: ≥85% error + a
/// near-limit note, ≥65% warning, else success. Shared by both stats
/// pages (T035, FR-009/SC-003, D2).
fn context_pct_style(pct: f64, theme: Theme) -> (crate::theme::Rgb, &'static str) {
    if pct >= 85.0 {
        (theme.error, " ⚠ near limit")
    } else if pct >= 65.0 {
        (theme.warning, "")
    } else {
        (theme.success, "")
    }
}

/// Dashboard row 1: " context " label + "used / window (P.x%)warn" + the
/// `usage_bar` gauge, colored by usage threshold. Byte-identical to the
/// row both pages hand-rolled before T035 (FR-009/SC-003, D2).
fn stats_context_row(
    used: u64,
    window: u64,
    pct: f64,
    bar_w: usize,
    theme: Theme,
) -> Line<'static> {
    let (pct_col, warn) = context_pct_style(pct, theme);
    Line::from(vec![
        Span::styled(
            " context ".to_string(),
            Style::default().fg(theme.accent.to_color()).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "{} / {} ({:.1}%){}  ",
                fmt_tokens(used),
                if window > 0 { fmt_tokens(window) } else { "?".into() },
                pct,
                warn,
            ),
            Style::default().fg(pct_col.to_color()),
        ),
        Span::styled(
            usage_bar(bar_w, pct as f32 / 100.0, &theme),
            Style::default().fg(pct_col.to_color()),
        ),
    ])
}

/// A subtle-colored value span — the text style both pages use for the
/// breakdown/session row values (T035 shared extraction).
fn subtle_span(text: String, theme: Theme) -> Span<'static> {
    Span::styled(
        text,
        Style::default().fg(theme.fg_more_subtle.to_color()),
    )
}

/// The plain label column both breakdown rows indent with — NOTE the two
/// screens deliberately differ (main indents 10 spaces, pane 9, keeping
/// each row's value text column-aligned with its " context "/" child "
/// label above); the width is therefore a per-screen parameter, not a
/// shared constant (byte-identical to the pre-T035 inline strings).
fn stats_breakdown_label(spaces: usize) -> Span<'static> {
    Span::styled(" ".repeat(spaces), Style::default())
}

/// Usage sparkline for dashboard row 4 (orchestrator page only — the pane
/// page has no per-call series): downsampled to `spark_w` samples, each
/// mapped to a bar-height glyph, " · streaming" suffix while live.
/// Byte-identical to the row `draw_stats_page` hand-rolled before T035.
fn usage_sparkline_row(
    series: &[(u64, u64)],
    spark_w: usize,
    live: bool,
    theme: Theme,
) -> Line<'static> {
    let spark: String = if series.is_empty() {
        "·".repeat(spark_w)
    } else {
        // Downsample to spark_w samples; each maps to a bar height glyph.
        let samples: Vec<u64> = {
            // Take the LAST spark_w samples; pad-left with zeros when fewer.
            let start = series.len().saturating_sub(spark_w);
            let mut v: Vec<u64> = series[start..].iter().map(|x| x.0).collect();
            if v.len() < spark_w {
                let pad = spark_w - v.len();
                let mut padded = vec![0u64; pad];
                padded.extend(v);
                v = padded;
            }
            v
        };
        let max = samples.iter().copied().max().unwrap_or(1).max(1);
        const BARS: [char; 5] = ['▁', '▂', '▃', '▅', '▇'];
        samples
            .iter()
            .map(|v| {
                let idx = ((v.saturating_mul(4)) / max).min(4) as usize;
                BARS[idx]
            })
            .collect()
    };
    Line::from(vec![
        Span::styled(" calls    ".to_string(), Style::default().fg(theme.accent.to_color())),
        Span::styled(
            format!("{} {}", spark, if live { " · streaming" } else { "" }),
            Style::default().fg(theme.info.to_color()),
        ),
    ])
}

/// The per-screen inputs `render_stats_page_composed` needs beyond the
/// recording cells: the dashboard values and the context-stream source.
/// T035 (FR-009/SC-003, D2): one struct so BOTH stats pages drive the
/// SAME section builders; labels stay per-screen (" session " vs
/// " child   ", pane "goal:" breakdown) — parameterized, not homogenized.
struct StatsPageData<'a> {
    /// Tokens counted against the context window (system + history).
    used: u64,
    /// Context-window size in tokens (0 → rendered as "?").
    window: u64,
    /// Context usage percentage (drives bar color + near-limit note).
    pct: f64,
    /// Breakdown-row value span (label indent is per-screen: 10 spaces
    /// main / 9 pane — column-aligns with each screen's row labels).
    breakdown_value: Span<'static>,
    /// Breakdown-row leading indent (see `stats_breakdown_label`).
    breakdown_indent: usize,
    /// Session-row label (" session  " main / " child   " pane).
    session_label: &'static str,
    /// Session-row value span.
    session_value: Span<'static>,
    /// Per-API-call usage series → row 4 sparkline. `None` on the pane
    /// page (no per-call series; the row is orchestrator-only, exactly as
    /// before T035).
    usage_series: Option<&'a [(u64, u64)]>,
    /// Context-window entries for the stream (main vs pane snapshot).
    entries: &'a [joey_agent_core::events::ContextEntry],
    /// Which entry indices are expanded (click/Space affordance).
    expanded: &'a std::collections::HashSet<usize>,
    /// Placeholder line when there are no entries (per-screen wording).
    empty_note: &'static str,
}

/// T035 (FR-009/SC-003, D2 "same widget functions"): the composed body
/// BOTH stats pages render — gradient title bar (caller-composed title),
/// dashboard rows (context bar + breakdown + session [+ calls sparkline]),
/// blank separator, the `build_context_stream` stream windowed with
/// freeze-on-scroll anchors recorded into the caller's cells, and the
/// footer (live spinner, updated-ago note, ↑above/↓below counters,
/// expand hint). Byte-identical to the pre-T035 pages; only the data
/// (`StatsPageData`), the scroll-anchor state, and the recording cells
/// differ per screen.
#[allow(clippy::too_many_arguments)]
fn render_stats_page_composed(
    f: &mut Frame,
    area: Rect,
    title: &str,
    live: bool,
    data: StatsPageData<'_>,
    // Scroll anchor state (`app.stats_view` / `pane.stats_view`);
    // `None` = auto-follow the live tail.
    stats_view: Option<usize>,
    // Render-time anchor bound (`app.last_stats_max_anchor` /
    // `pane.last_stats_max_anchor`).
    max_anchor_cell: &std::cell::Cell<usize>,
    // Visible-window recorder for click hit-testing
    // (`app.last_stats_window` / `app.last_pane_stats_window`).
    window_cell: &std::cell::Cell<(u16, usize)>,
    // Entry-geometry recorder for click hit-testing
    // (`app.last_stats_stream_rows` / `app.last_pane_stats_stream_rows`).
    stream_rows_cell: &std::cell::RefCell<Vec<(usize, usize, usize)>>,
    // " updated Ns ago" footer segment source (main page only).
    context_updated_at: Option<std::time::Instant>,
    spinner: &Spinner,
    theme: Theme,
) {
    let block = gradient_block_focused(title, theme, 0.6);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let content_w = inner.width.max(1) as usize;

    // ── Dashboard section (fixed rows at the top) ──────────────────────
    let mut lines: Vec<Line> = Vec::new();
    let bar_w = (content_w.saturating_sub(34)).clamp(10, 40);

    // Row 1: context window usage bar.
    lines.push(stats_context_row(data.used, data.window, data.pct, bar_w, theme));

    // Row 2: breakdown + compression info (per-screen value span).
    lines.push(Line::from(vec![
        stats_breakdown_label(data.breakdown_indent),
        data.breakdown_value,
    ]));

    // Row 3: session token totals (per-screen label + value spans).
    lines.push(Line::from(vec![
        Span::styled(
            data.session_label.to_string(),
            Style::default().fg(theme.accent.to_color()),
        ),
        data.session_value,
    ]));

    // Row 4: usage sparkline (per-API-call prompt tokens, recent window)
    // — orchestrator page only (pane passes no usage_series).
    if let Some(series) = data.usage_series {
        let spark_w = content_w.saturating_sub(24).clamp(10, 60);
        lines.push(usage_sparkline_row(series, spark_w, live, theme));
    }

    lines.push(Line::from(vec![Span::raw("")]));

    // ── Context stream (windowed, auto-follow/freeze, EXPANDABLE) ─────
    // Expandable-stats feature: one header row per entry
    // ("#idx ▸ role tokens preview") plus the full content inline when the
    // entry is expanded — same affordance as the main transcript (click a
    // row or press Space on the selected row to toggle).
    let stream = build_context_stream(
        data.entries,
        data.expanded,
        content_w,
        theme,
        data.empty_note,
    );

    let body_rows = inner.height.saturating_sub(lines.len() as u16 + 1).max(1) as usize; // +1 footer
    let total = stream.total;
    let visible = body_rows.min(total);
    let max_anchor = total.saturating_sub(visible);
    max_anchor_cell.set(max_anchor);
    let start = match stats_view {
        None => max_anchor, // following the live tail
        Some(anchor) => anchor.min(max_anchor),
    };
    let end = (start + visible).min(total);
    // Record the visible window + entry geometry for click hit-testing.
    // The stream renders AFTER `lines.len()` dashboard header rows, so the
    // first content row on screen is inner.y + lines.len() — recording just
    // inner.y made every click resolve ~5 rows above the intended entry.
    let header_rows = lines.len() as u16;
    window_cell.set((inner.y + header_rows, start));
    stream_rows_cell.borrow_mut().clear();
    stream_rows_cell
        .borrow_mut()
        .extend(stream.entry_rows.iter().copied());
    lines.extend(stream.lines.into_iter().skip(start).take(visible));

    // Footer: live indicator + overflow counters + follow state.
    let mut footer = vec![Span::raw(" ")];
    if live {
        footer.push(spinner.styled_glyph(theme));
        footer.push(Span::styled(
            " live".to_string(),
            Style::default().fg(theme.busy.to_color()),
        ));
    }
    if let Some(at) = context_updated_at {
        footer.push(Span::styled(
            format!("  · updated {:.0}s ago", at.elapsed().as_secs_f32()),
            Style::default().fg(theme.fg_most_subtle.to_color()),
        ));
    }
    if start > 0 {
        footer.push(Span::styled(
            format!("  ↑{} above", start),
            Style::default().fg(theme.fg_most_subtle.to_color()),
        ));
    }
    if total > visible && stats_view.is_some() {
        footer.push(Span::styled(
            format!("  ↓{} below · scroll to bottom to resume", total - end),
            Style::default().fg(theme.fg_more_subtle.to_color()),
        ));
    }
    footer.push(Span::styled(
        "  · click a row (or Space) to expand".to_string(),
        Style::default().fg(theme.fg_most_subtle.to_color()),
    ));
    lines.push(Line::from(footer));

    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// The maximized agent-stats page: a live dashboard (context window usage,
/// token accounting, model/session, compression, per-call usage sparkline)
/// on top and the full context-window stream below — one line per history
/// message, auto-following the tail with freeze-on-scroll (same semantics
/// as the live reasoning panel). Opened by clicking the header's right
/// section or Ctrl+A; Esc restores.
pub fn draw_stats_page(f: &mut Frame, area: Rect, app: &App, theme: Theme, spinner: &Spinner) {
    // Rect for mouse hit-testing (wheel scrolling inside the page).
    app.last_stats_rect.set((area.x, area.y, area.width, area.height));

    let live = app.is_busy();
    let title = if live {
        " ◆ agent stats · LIVE · Esc to restore "
    } else {
        " ◆ agent stats · Esc to restore "
    };

    // T035 (FR-009/SC-003, D2 "same widget functions"): thin adapter —
    // the composed body is the shared `render_stats_page_composed` the
    // pane stats page also drives; this adapter only supplies the MAIN
    // app's data, labels (" session  ", compression/turns breakdown),
    // per-call usage sparkline, and recording cells.
    let used = app.context_system_tokens + app.context_history_tokens;
    let threshold_note = if app.compression_threshold > 0 {
        format!("compress@{}", fmt_tokens(app.compression_threshold))
    } else {
        "compress@?".to_string()
    };
    render_stats_page_composed(
        f,
        area,
        title,
        live,
        StatsPageData {
            used,
            window: app.context_window,
            pct: app.context_usage_pct(),
            breakdown_value: subtle_span(
                format!(
                    "system {} · history {} · msgs {} · {} · compacted {}x",
                    fmt_tokens(app.context_system_tokens),
                    fmt_tokens(app.context_history_tokens),
                    app.context_entries.len(),
                    threshold_note,
                    app.compactions
                ),
                theme,
            ),
            breakdown_indent: 10,
            session_label: " session  ",
            session_value: subtle_span(
                format!(
                    "prompt {} · completion {} · total {} · turns {} · iters {}",
                    fmt_tokens(app.tokens.prompt),
                    fmt_tokens(app.tokens.completion),
                    fmt_tokens(app.tokens.total()),
                    app.turns,
                    app.tokens.iterations
                ),
                theme,
            ),
            usage_series: Some(&app.usage_series),
            entries: &app.context_entries,
            expanded: &app.expanded_context,
            empty_note: "(no context yet — send a prompt)",
        },
        app.stats_view,
        &app.last_stats_max_anchor,
        &app.last_stats_window,
        &app.last_stats_stream_rows,
        app.context_updated_at,
        spinner,
        theme,
    );
}

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
                // Spec 020 (T030): halted — warning, not error.
                SubagentStatus::Stopped => ("■", theme.warning),
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
                // Spec 020 (T030, FR-016): distinguishable stopped state;
                // the stop reason rides the entry phase line below.
                SubagentStatus::Stopped => ("■", "stopped", theme.warning),
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
    // Terminal governor contention (spec 018, T019 / FR-011): shown ONLY
    // while commands are queued — no persistent chrome when the governor
    // is uncontended.
    if app.terminal_queued > 0 {
        spans.push(Span::styled(
            format!("⚙ {} active, {} queued", app.terminal_active, app.terminal_queued),
            Style::default().fg(theme.warning.to_color()),
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
        ("Space / x (transcript)", "inline-expand the item at the top of the view"),
        ("g / G", "top / bottom (transcript focus)"),
        ("y / Y", "copy last agent / user message to clipboard"),
        ("/copy [n]", "copy nth assistant message (−n counts from last)"),
        ("Ctrl+S", "search transcript · n/N cycle matches"),
        ("Ctrl+R", "toggle reasoning panel"),
        ("Ctrl+E", "cycle the newest reasoning block (tail ↔ full)"),
        ("Ctrl+G", "cycle the newest tool block (inline expand)"),
        ("Ctrl+O", "maximize live terminal output · Esc restores"),
        ("  viewer: ↑↓/PgUp·PgDn", "scroll output (g/G top/bottom) · auto-follows tail"),
        ("Ctrl+A / click header ▸", "agent stats page · live context window stream"),
        ("  stats: ↑↓/PgUp·PgDn", "scroll context (g/G top/bottom) · auto-follows tail"),
        ("Alt+↑ / Alt+↓", "scroll NeuroCode context feed (when active)"),
        ("click feed panel", "open the fullscreen NeuroCode graph explorer"),
        ("  explorer: ←→↑↓/hjkl", "select nodes on the graph canvas"),
        ("  explorer: Shift+←→↑↓", "pan the graph canvas"),
        ("  explorer: +/−/wheel/0", "zoom in/out · reset view"),
        ("  explorer: Tab / ⏎", "cycle graph · nodes · feed panes"),
        ("  explorer: Esc / click title", "dock the explorer back"),
        ("Ctrl+L", "clear transcript view"),
        ("Ctrl+P", "back to the orchestrator tab (from a subagent pane)"),
        ("Ctrl+N", "expand / collapse the subagent rail (or click its title)"),
        ("click rail tabs", "focus a subagent · bottom tab = orchestrator"),
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
///
/// T016 (US3, FR-007, design D5): the bar renders over the PANE view when
/// a subagent pane is focused, and the match indicator then mirrors the
/// PANE's per-view SearchState (`SubagentPane::search_has_match`, set by
/// the T015 focus-follow `run_search`/`search_next`) instead of the
/// orchestrator's App-level indicator — the same three titles, byte-for
/// byte (FR-009 chrome parity by construction). The prompt line always
/// shows the live `App::search_query` (the bar being typed into; T015's
/// run_search carries it into the pane's preserved query).
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

    // T016: indicator routes to the TARGET view — the focused pane's own
    // match state when one is focused, the orchestrator's otherwise.
    let has_match = match app.focused_pane() {
        Some(pane) => pane.search_has_match,
        None => app.search_has_match,
    };
    let title = if app.search_query.is_empty() {
        " search (Esc to close) "
    } else if has_match {
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

// ── Steer overlay bar (T023 / US6, FR-017, spec 020) ───────────────────────

/// T023 (US6, FR-017, spec 020): render the steer overlay as a bottom
/// text-input bar, mirroring [`draw_search_bar`]'s chrome (3-row Clear +
/// focused gradient block). The title carries the bound child's goal
/// (truncated) plus the Esc/Enter hints — `goal` is used for the title
/// only, per `SteerOverlay`'s contract; the prompt line always shows the
/// live draft with the accent cursor caret.
pub(crate) fn draw_steer_bar(f: &mut Frame, area: Rect, steer: &SteerOverlay, theme: &Theme) {
    let theme = *theme;

    // Bottom 3-row bar.
    let h = 3u16;
    let y = area.y + area.height.saturating_sub(h);
    let steer_area = Rect::new(area.x, y, area.width, h);
    f.render_widget(Clear, steer_area);

    // Title: the bound child's goal (tail-truncated to keep the key hints
    // visible) + the same Esc-hint convention as the search bar.
    let goal = steer.goal.trim();
    let goal_prefix = if goal.chars().count() > 32 {
        let cut: String = goal.chars().take(31).collect();
        format!("{cut}… ")
    } else if goal.is_empty() {
        String::new()
    } else {
        format!("{goal} ")
    };
    let title = format!(" steer {goal_prefix}(Esc cancel · Enter send) ");
    let block = gradient_block_focused(&title, theme, 0.7);
    let inner = block.inner(steer_area);
    f.render_widget(block, steer_area);

    let prompt_line = Line::from(vec![
        Span::styled(
            "»",
            Style::default()
                .fg(theme.gold.to_color())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            steer.text.as_str(),
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

    // ── Header gradient bar animation (agent-active indicator) ──────────

    /// Render the header underline row through the real widget and return
    /// the per-cell foreground colors of that row.
    fn header_underline_colors(app: &App, flow: Option<&crate::anim::HeaderFlow>) -> Vec<ratatui::style::Color> {
        use ratatui::backend::TestBackend;
        let theme = Theme::aurora();
        let mut terminal = ratatui::Terminal::new(TestBackend::new(80, 4)).unwrap();
        let spinner = Spinner::dots();
        let pulse = Pulse::new();
        terminal
            .draw(|f| {
                draw_header(f, f.area(), app, theme, &spinner, &pulse, flow);
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .skip(80 * 3) // the underline is the last row of the 4-row area
            .take(80)
            .map(|c| c.fg)
            .collect()
    }

    fn busy_app() -> App {
        let mut app = App::new("sess", "model");
        app.mode = crate::state::RunMode::Busy;
        app
    }

    fn stepped_flow(frames: usize) -> crate::anim::HeaderFlow {
        let mut flow = crate::anim::HeaderFlow::new();
        flow.set_busy(true);
        for _ in 0..frames {
            flow.tick(Duration::from_secs_f32(1.0 / 30.0), 1.0);
        }
        flow
    }

    #[test]
    fn header_underline_is_static_when_flow_idle() {
        // No animator (None) and a fully-idle animator must render the SAME
        // static gradient: backward compatibility for non-Tui callers and
        // a guarantee that "idle" is byte-identical to the old look.
        let app = busy_app(); // even busy: an idle animator overrides
        let base = header_underline_colors(&app, None);
        let mut idle_flow = crate::anim::HeaderFlow::new();
        idle_flow.set_busy(false);
        idle_flow.tick(Duration::from_secs_f32(5.0), 1.0);
        let idle = header_underline_colors(&app, Some(&idle_flow));
        assert_eq!(base, idle, "idle flow == static gradient");
        // Sanity: the static row is actually a gradient (ends differ).
        assert_ne!(base.first(), base.last());
    }

    /// Render the header's first row through TestBackend and return the
    /// buffer cells (symbol + fg + bg + modifier) so style contracts can
    /// be asserted.
    fn header_cells(app: &App) -> Vec<(char, ratatui::style::Color, ratatui::style::Color, ratatui::style::Modifier)> {
        use ratatui::backend::TestBackend;
        let theme = Theme::aurora();
        let mut terminal = ratatui::Terminal::new(TestBackend::new(100, 4)).unwrap();
        let spinner = Spinner::dots();
        let pulse = Pulse::new();
        terminal
            .draw(|f| {
                draw_header(f, f.area(), app, theme, &spinner, &pulse, None);
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .take(100) // first header row only
            .map(|c| {
                (
                    c.symbol().chars().next().unwrap_or(' '),
                    c.fg,
                    c.bg,
                    c.modifier,
                )
            })
            .collect()
    }

    #[test]
    fn header_wordmark_renders_as_bold_solid_brand_chip() {
        let cells = header_cells(&App::new("sess", "model"));
        let text: String = cells.iter().map(|(s, _, _, _)| *s).collect();

        // Layout contract (inner starts at area.x+1): pad, gold spark, gap,
        // then the inverted "joey" chip ("  joey  " = 8 cells).
        assert!(text.starts_with(" ✦   joey  "), "wordmark layout: {text:?}");
        let chip: String = text.chars().skip(3).take(8).collect();
        assert_eq!(chip, "  joey  ", "chip renders the padded wordmark: {text:?}");

        // Spark is gold on the header background.
        let (_, sfg, _, _) = cells[1];
        assert_eq!(sfg, ratatui::style::Color::Rgb(0xFF, 0xC9, 0x3D));

        // Prominence contract: every chip cell is INVERTED — its background
        // is one solid brand-cyan fill (not the header's elevated bg), and
        // the foreground is the near-black void color, bold. This is what
        // makes the wordmark pop: contrast comes from the filled background,
        // not just glyph color.
        let header_bg = ratatui::style::Color::Rgb(0x1D, 0x1D, 0x31); // bg_elevated
        // Pulse::new() has phase 0 → value() = 0.5 → glow lift = 0.10, so
        // the expected fill is grad_0 cyan (0x22,0xE4,0xE8) lerped 10%
        // toward white: (0x38,0xE7,0xEA).
        let expected_chip_bg = ratatui::style::Color::Rgb(0x38, 0xE7, 0xEA);
        let chip_cells: Vec<char> = "  joey  ".chars().collect();
        for (i, (sym, fg, bg, modifier)) in cells.iter().enumerate().skip(3).take(8) {
            assert_ne!(*bg, header_bg, "chip cell {i} must be color-filled");
            assert_eq!(*bg, expected_chip_bg, "chip bg is solid brand cyan at cell {i}");
            assert_eq!(*fg, ratatui::style::Color::Rgb(0x0B, 0x0B, 0x12), "chip fg is void: cell {i}");
            assert_eq!(*sym, chip_cells[i - 3]);
            assert!(modifier.contains(ratatui::style::Modifier::BOLD), "chip glyphs are bold: cell {i}");
            // Brightness + hue: high-luma cyan — green/blue dominate red.
            if let ratatui::style::Color::Rgb(r, g, b) = bg {
                let luma = (*r as u32 + *g as u32 + *b as u32) / 3;
                assert!(luma > 0x60, "chip bg is bright at cell {i}: luma {luma:#x}");
                assert!(g > r && b > r, "chip bg is cyan-family at cell {i}");
            } else {
                panic!("chip bg must be Rgb at cell {i}");
            }
        }

        // The chip is SOLID: every chip cell shares one identical background
        // (the old per-cell gradient is gone).
        let first_bg = cells[3].2;
        for (i, (_, _, bg, _)) in cells.iter().enumerate().skip(4).take(7) {
            assert_eq!(*bg, first_bg, "chip background is uniform at cell {i}");
        }
    }

    /// HyperCode badge: draw the header through TestBackend and read the
    /// rendered text, verifying the live-phase labels replace the static ⚡.
    fn header_text(app: &App) -> String {
        use ratatui::backend::TestBackend;
        let theme = Theme::aurora();
        let mut terminal = ratatui::Terminal::new(TestBackend::new(100, 4)).unwrap();
        let spinner = Spinner::dots();
        let pulse = Pulse::new();
        terminal
            .draw(|f| {
                draw_header(f, f.area(), app, theme, &spinner, &pulse, None);
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .take(100) // first header row only
            .map(|c| c.symbol().to_string())
            .collect()
    }

    /// Status bar (spec 018, T019): render draw_status through TestBackend
    /// and collect the first row's text so span gating can be asserted.
    fn status_text(app: &App) -> String {
        use ratatui::backend::TestBackend;
        let theme = Theme::aurora();
        let mut terminal = ratatui::Terminal::new(TestBackend::new(120, 1)).unwrap();
        terminal
            .draw(|f| {
                draw_status(f, f.area(), app, theme, Duration::from_secs(2));
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .take(120)
            .map(|c| c.symbol().to_string())
            .collect()
    }

    #[test]
    fn status_terminal_span_shows_only_while_queued() {
        // FR-011: uncontended governor → no span at all.
        let mut app = App::new("sess", "model");
        let idle = status_text(&app);
        assert!(!idle.contains("queued"), "no span when nothing queued: {idle}");
        assert!(!idle.contains("active,"), "no span when nothing queued: {idle}");

        // Contended: A active, Q queued → span present with both counts.
        app.apply(joey_agent_core::AgentEvent::TerminalQueueState {
            active: 2,
            queued: 3,
        });
        let busy = status_text(&app);
        assert!(busy.contains("2 active, 3 queued"), "span renders counts: {busy}");

        // Contention clears → span disappears again.
        app.apply(joey_agent_core::AgentEvent::TerminalQueueState {
            active: 1,
            queued: 0,
        });
        let cleared = status_text(&app);
        assert!(!cleared.contains("queued"), "span gone once queue drains: {cleared}");
    }

    #[test]
    fn hypercode_badge_shows_live_phase() {
        // Disabled: no badge at all.
        let plain = header_text(&App::new("sess", "model"));
        assert!(!plain.contains("HYPER") && !plain.contains("PLAN"), "no badge when disabled");

        // Enabled, idle: plain ⚡ (no phase label).
        let mut app = App::new("sess", "model");
        app.hypercode_enabled = true;
        let idle = header_text(&app);
        assert!(idle.contains('⚡'), "badge present when enabled: {idle}");
        assert!(!idle.contains("PLAN") && !idle.contains("EXPL"), "idle shows no phase: {idle}");

        // Live phases render their labels.
        for (phase, label) in [
            ("planning", "PLAN"),
            ("exploring", "EXPL"),
            ("building", "BUILD"),
            ("synthesizing", "SYNTH"),
        ] {
            app.hypercode_phase = Some(phase.to_string());
            let text = header_text(&app);
            assert!(text.contains(label), "{phase} shows {label}: {text}");
        }
    }

    #[test]
    fn header_underline_animates_when_busy() {
        let app = busy_app();
        // Two engaged animator snapshots ~0.5s apart must differ somewhere
        // in the row (the wave has moved).
        let f1 = stepped_flow(60); // ~2s engaged
        let f2 = stepped_flow(75); // +0.5s
        let row1 = header_underline_colors(&app, Some(&f1));
        let row2 = header_underline_colors(&app, Some(&f2));
        assert_ne!(
            row1, row2,
            "the underline must change across frames while the agent runs"
        );
        // And the busy row differs from the static one (brighter somewhere).
        let stat = header_underline_colors(&app, None);
        assert_ne!(row1, stat, "busy underline != static underline");
    }

    #[test]
    fn header_underline_wave_is_graded_not_bicolor() {
        // Subtlety contract: while busy, adjacent cells change gradually —
        // no hard cliff between the wave and the rest of the bar. Sample
        // the per-channel deltas between adjacent cells.
        let app = busy_app();
        let flow = stepped_flow(90);
        let row = header_underline_colors(&app, Some(&flow));
        let mut max_jump = 0u8;
        for w in row.windows(2) {
            if let (ratatui::style::Color::Rgb(ar, ag, ab), ratatui::style::Color::Rgb(br, bg, bb)) =
                (w[0], w[1])
            {
                let jump = ar
                    .abs_diff(br)
                    .max(ag.abs_diff(bg))
                    .max(ab.abs_diff(bb));
                max_jump = max_jump.max(jump);
            }
        }
        assert!(
            max_jump <= 48,
            "wave must be graded (max adjacent channel jump {max_jump} > 48)"
        );
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
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: true,
            exit_code: Some(0),
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
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
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: true,
            exit_code: Some(1),
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
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
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: true,
            exit_code: Some(0),
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
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
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: true,
            exit_code: Some(0),
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
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
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: true,
            exit_code: None,
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
        };
        let rendered = render_text(&item, 80);
        assert!(
            rendered.iter().any(|l| l.contains("⟳")),
            "running terminal should show spinner; got: {:?}",
            rendered
        );
    }

    // ── Live output streaming (inline tail + maximized viewer) ──────────

    #[test]
    fn test_terminal_running_streams_live_output_tail() {
        // A running terminal with accumulated live output shows the tail
        // lines + the streaming affordance (Ctrl+O hint) — output is now
        // visible WHILE the command runs, not only after ToolEnd.
        let item = TranscriptItem::Tool {
            name: "terminal".into(),
            emoji: "⚡".into(),
            summary: "build".into(),
            status: ToolStatus::Running,
            duration_secs: None,
            result_preview: String::new(),
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: true,
            exit_code: None,
            live_output: (1..=30).map(|i| format!("build line {i}")).collect::<Vec<_>>().join("\n"),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
        };
        let rendered = render_text(&item, 80);
        assert!(
            rendered.iter().any(|l| l.contains("build line 30")),
            "live tail shows the newest line; got: {:?}",
            rendered
        );
        assert!(
            !rendered.iter().any(|l| l.contains("build line 1\n") || l.trim() == "    build line 1"),
            "older head lines beyond MAX_TOOL_OUTPUT_LINES hidden; got: {:?}",
            rendered
        );
        assert!(
            rendered.iter().any(|l| l.contains("streaming") && l.contains("Ctrl+O")),
            "streaming affordance present; got: {:?}",
            rendered
        );
    }

    #[test]
    fn test_terminal_running_no_output_shows_hint() {
        let item = TranscriptItem::Tool {
            name: "terminal".into(),
            emoji: "⚡".into(),
            summary: "sleep 5".into(),
            status: ToolStatus::Running,
            duration_secs: None,
            result_preview: String::new(),
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: true,
            exit_code: None,
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
        };
        let rendered = render_text(&item, 80);
        assert!(
            rendered.iter().any(|l| l.contains("running")),
            "silent running command shows the running hint; got: {:?}",
            rendered
        );
    }

    #[test]
    fn test_output_viewer_renders_live_content() {
        use joey_agent_core::AgentEvent;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut app = crate::state::App::new("s", "m");
        app.apply(AgentEvent::ToolStart {
            name: "terminal".into(),
            emoji: "💻".into(),
            summary: "cargo test".into(),
        });
        app.apply(AgentEvent::ToolOutput {
            name: "terminal".into(),
            chunk: "running 140 tests\n".into(),
        });
        app.toggle_output_viewer(None);
        let theme = crate::theme::Theme::aurora();
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let spinner = crate::anim::Spinner::dots();
        terminal
            .draw(|f| {
                draw_output_viewer(f, f.area(), &app, theme, &spinner);
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        assert!(text.contains("output · LIVE"), "viewer title rendered");
        assert!(text.contains("cargo test"), "command header rendered");
        assert!(text.contains("running 140 tests"), "live output rendered");
        assert!(text.contains("streaming"), "live badge rendered");
        assert!(text.contains("1 │ "), "line-number gutter rendered");
    }

    // ── Agent stats page (maximized context window) ────────────────────

    #[test]
    fn test_stats_page_renders_dashboard_and_stream() {
        use joey_agent_core::AgentEvent;
        use joey_agent_core::events::ContextEntry;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut app = crate::state::App::new("sess-12345678", "glm-5.2");
        let entries: Vec<ContextEntry> = vec![
            ContextEntry {
                role: "user".into(),
                tokens: 140,
                preview: "please fix the login bug".into(),
                has_tool_calls: false,
                is_compressed_summary: false,
                full_content: String::new(),
            },
            ContextEntry {
                role: "assistant".into(),
                tokens: 90,
                preview: "I'll search the codebase".into(),
                has_tool_calls: true,
                is_compressed_summary: false,
                full_content: String::new(),
            },
            ContextEntry {
                role: "tool".into(),
                tokens: 2_400,
                preview: "crates/auth/src/lib.rs:42".into(),
                has_tool_calls: false,
                is_compressed_summary: false,
                full_content: String::new(),
            },
        ];
        app.apply(AgentEvent::ContextSnapshot {
            entries,
            system_tokens: 3_200,
            history_tokens: 2_630,
            context_window: 200_000,
            compression_threshold: 160_000,
            compactions: 2,
            model: "glm-5.2".into(),
        });
        app.apply(AgentEvent::ApiCallEnd {
            usage: joey_providers::Usage {
                prompt_tokens: 6_100,
                completion_tokens: 480,
                ..Default::default()
            },
        });
        app.toggle_stats();
        let theme = crate::theme::Theme::aurora();
        let backend = TestBackend::new(110, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let spinner = crate::anim::Spinner::dots();
        terminal
            .draw(|f| {
                draw_stats_page(f, f.area(), &app, theme, &spinner);
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        assert!(text.contains("agent stats"), "title rendered");
        assert!(text.contains("context"), "context row rendered");
        assert!(text.contains("200.0K"), "window size rendered");
        assert!(text.contains("session"), "session row rendered");
        assert!(text.contains("calls"), "sparkline row rendered");
        assert!(text.contains("user"), "role labels rendered");
        assert!(text.contains("please fix the login bug"), "previews rendered");
        assert!(text.contains("⚒calls"), "tool-call flag rendered");
        assert!(text.contains("compacted 2x"), "compression count rendered");
    }

    #[test]
    fn test_stats_page_empty_state_renders_placeholder() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut app = crate::state::App::new("s", "m");
        app.toggle_stats();
        let theme = crate::theme::Theme::aurora();
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let spinner = crate::anim::Spinner::dots();
        terminal
            .draw(|f| {
                draw_stats_page(f, f.area(), &app, theme, &spinner);
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        assert!(text.contains("no context yet"), "placeholder rendered");
    }

    #[test]
    fn test_stats_page_auto_follow_shows_tail() {
        // With more entries than fit, auto-follow shows the TAIL (newest
        // messages), and the footer shows the above-counter.
        use joey_agent_core::AgentEvent;
        use joey_agent_core::events::ContextEntry;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut app = crate::state::App::new("s", "m");
        let entries: Vec<ContextEntry> = (0..80)
            .map(|i| ContextEntry {
                role: "user".into(),
                tokens: 10,
                preview: format!("message number {i}"),
                has_tool_calls: false,
                is_compressed_summary: false,
                full_content: String::new(),
            })
            .collect();
        app.apply(AgentEvent::ContextSnapshot {
            entries,
            system_tokens: 100,
            history_tokens: 800,
            context_window: 50_000,
            compression_threshold: 40_000,
            compactions: 0,
            model: "m".into(),
        });
        app.toggle_stats();
        let theme = crate::theme::Theme::aurora();
        let backend = TestBackend::new(90, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        let spinner = crate::anim::Spinner::dots();
        terminal
            .draw(|f| {
                draw_stats_page(f, f.area(), &app, theme, &spinner);
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        assert!(text.contains("message number 79"), "tail (newest) visible");
        assert!(!text.contains("message number 0\n"), "head hidden while following");
        assert!(text.contains("above"), "overflow counter rendered");
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
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: false,
            exit_code: None,
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
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
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: false,
            exit_code: None,
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
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
            expand_state: ReasoningExpandState::TailWindow,
            full_args: Some("path: foo.rs".into()),
            full_result: Some("line1\nline2\nline3".into()),
            is_terminal: false,
            exit_code: None,
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
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
            expand_state: ReasoningExpandState::TailWindow,
            full_args: None,
            full_result: Some("full body".into()),
            is_terminal: false,
            exit_code: None,
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
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
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: false,
            exit_code: None,
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
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
        // Item 1 is the tool, starts collapsed.
        assert_eq!(
            matches!(&app.transcript[1], TranscriptItem::Tool { expand_state: ReasoningExpandState::Collapsed, .. }),
            true
        );
        app.toggle_item_expand_by_index(1);
        // Short result: the cycle skips the redundant tail window and goes
        // straight to Full (same rule as reasoning blocks).
        assert_eq!(
            matches!(&app.transcript[1], TranscriptItem::Tool { expand_state: ReasoningExpandState::Full, .. }),
            true,
            "toggle should expand the tool (short result skips to Full)"
        );
        app.toggle_item_expand_by_index(1);
        assert_eq!(
            matches!(&app.transcript[1], TranscriptItem::Tool { expand_state: ReasoningExpandState::Collapsed, .. }),
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
                    expand_state: ReasoningExpandState::Collapsed,
                    full_args: None,
                    full_result: None,
                    is_terminal: true,
                    exit_code: Some(0),
                    live_output: String::new(),
                    live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
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
                    expand_state: ReasoningExpandState::Collapsed,
                    full_args: None,
                    full_result: None,
                    is_terminal: false,
                    exit_code: None,
                    live_output: String::new(),
                    live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
                },
            ),
            (
                "FileDiff",
                TranscriptItem::FileDiff {
                    path: "a.txt".into(),
                    stat: "+1 -0".into(),
                    lines: vec!["+hello".into()],
                    is_binary: false, expand_state: ReasoningExpandState::Collapsed,
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
                expand_state: if expanded {
                    ReasoningExpandState::TailWindow
                } else {
                    ReasoningExpandState::Collapsed
                },
                full_args: None,
                full_result: None,
                is_terminal: false,
                exit_code: None,
                live_output: String::new(),
                live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
            }
        }
        fn mk_filediff() -> TranscriptItem {
            TranscriptItem::FileDiff {
                path: "a".into(),
                stat: "+1".into(),
                lines: vec!["+x".into()],
                is_binary: false, expand_state: ReasoningExpandState::Collapsed,
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

    /// T022 (FR-006 codification, updated for the crush-style code view):
    /// tool/terminal body lines carry the line-numbered gutter — the body
    /// content column starts at a stable indent and the first body span is
    /// the gutter (`"N │ "`). The essential contract: body lines are
    /// visually indented past the header and structurally carry a gutter.
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
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: false,
            exit_code: None,
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
        };
        let ls_g = item_lines(&generic, 80, theme);
        for l in &ls_g[1..ls_g.len() - 1] {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                text.trim_start().starts_with("1 │ ") || text.starts_with(' '),
                "T022 generic: body line carries the line-number gutter; got {:?}",
                text
            );
            assert!(text.contains("│"), "T022 generic: gutter separator present; got {:?}", text);
        }

        // Terminal tool with body output.
        let term = TranscriptItem::Tool {
            name: "terminal".into(),
            emoji: "⚡".into(),
            summary: "echo hi".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.1),
            result_preview: "hi".into(),
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: true,
            exit_code: Some(0),
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
        };
        let ls_t = item_lines(&term, 80, theme);
        for l in &ls_t[1..ls_t.len() - 1] {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                text.trim_start().starts_with("1 │ ") || text.starts_with(' '),
                "T022 terminal: body line carries the line-number gutter; got {:?}",
                text
            );
            assert!(text.contains("│"), "T022 terminal: gutter separator present; got {:?}", text);
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
///
/// In expanded mode (`app.neurocode_expanded`, toggled by clicking the
/// docked panel) the same widget renders in the main screen area — same
/// content, more rows. Either way the panel records its rect into
/// `app.last_neurocode_rect` for mouse hit-testing.
pub fn draw_neurocode_panel(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    if !app.neurocode_active || area.width == 0 || area.height == 0 {
        return;
    }
    // Record geometry for mouse hit-testing (click toggles docked ↔
    // expanded). Zeroed only on deactivate; stale-but-inactive rects are
    // harmless because the guard above skips drawing.
    app.last_neurocode_rect
        .set((area.x, area.y, area.width, area.height));

    let title = if app.neurocode_expanded {
        " neurocode · context feed · click or Esc to dock "
    } else {
        " neurocode · context feed · click to expand "
    };
    let block = gradient_block(title, theme);
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
    // Realtime refresh stamp: how long ago the final context blob landed.
    // Makes every re-assembly visibly "live" even when content is unchanged.
    if let Some(at) = app.neurocode_updated_at {
        let secs = at.elapsed().as_secs();
        let ago = if secs < 1 {
            "now".to_string()
        } else if secs < 60 {
            format!("{}s ago", secs)
        } else {
            format!("{}m ago", secs / 60)
        };
        header.push(Span::styled(
            format!("  ↻ {}", ago),
            Style::default().fg(theme.fg_more_subtle.to_color()),
        ));
    }
    lines.push(Line::from(header));
    lines.push(Line::styled(
        "─".repeat(cw.saturating_sub(2)),
        Style::default().fg(theme.fg_most_subtle.to_color()),
    ));

    // Live assembly stage (feature 015 follow-up): while NeuroCode is
    // assembling, stream the current stage with a time-animated spinner so
    // the feed updates in realtime — BEFORE the final context lands.
    if !app.neurocode_stage.is_empty() {
        let elapsed = app
            .neurocode_stage_at
            .map(|t| t.elapsed().as_millis())
            .unwrap_or(0) as usize;
        const FRAMES: [&str; 4] = ["⠋", "⠙", "⠹", "⠸"];
        let frame = FRAMES[(elapsed / 120) % FRAMES.len()];
        let stage_line = format!(" {} {}", frame, app.neurocode_stage);
        let trunc: String = stage_line.chars().take(cw.saturating_sub(2)).collect();
        lines.push(Line::styled(
            trunc,
            Style::default().fg(theme.accent.to_color()),
        ));
    }

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


// ── Crush-style formatting tests (gutters, envelopes, pretty JSON) ─────────

#[cfg(test)]
mod crush_format_tests {
    use super::*;
    use crate::state::ReasoningExpandState;

    fn render_item(item: &TranscriptItem, width: usize) -> Vec<String> {
        let theme = Theme::aurora();
        item_lines(item, width, theme)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect()
    }

    #[test]
    fn test_tool_result_envelope_unwrapped_in_body() {
        // The terminal tool's result is a JSON envelope; the body must show
        // the OUTPUT payload with line numbers, not `{"output":"…"}`.
        let item = TranscriptItem::Tool {
            name: "terminal".into(),
            emoji: "⚡".into(),
            summary: "cargo build".into(),
            status: ToolStatus::Done,
            duration_secs: Some(2.0),
            result_preview: r#"{"output":"Compiling foo v0.1.0\nFinished dev","exit_code":0,"error":null}"#.into(),
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: true,
            exit_code: Some(0),
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
        };
        let rendered = render_item(&item, 90);
        let joined = rendered.join("\n");
        assert!(!joined.contains(r#"\"#) || !joined.contains(r#""output""#), "no raw JSON envelope in body");
        assert!(joined.contains("1 │ Compiling foo"), "payload line 1 with gutter: {:?}", joined);
        assert!(joined.contains("2 │ Finished dev"), "payload line 2 with gutter");
    }

    #[test]
    fn test_generic_tool_json_payload_pretty_printed() {
        // A generic tool whose result IS JSON gets pretty-printed in the
        // maximized viewer (no literal \n runs).
        let raw = r#"{"files":[{"name":"a.rs","lines":10},{"name":"b.rs","lines":20}],"total":30}"#;
        let formatted = crate::state::format_tool_result_for_display(raw);
        assert!(formatted.contains("\n  "), "pretty-printed with indentation");
        assert!(!formatted.contains(r#"\n"#), "no literal escape runs: {:?}", formatted);
        assert!(formatted.contains("\"files\""));
    }

    #[test]
    fn test_tool_error_envelope_unwrapped() {
        let out = crate::state::display_result_content(r#"{"error":"boom"}"#);
        assert_eq!(out.as_deref(), Some("boom"));
    }

    #[test]
    fn test_non_json_results_pass_through() {
        assert_eq!(crate::state::display_result_content("plain text"), None);
        assert_eq!(crate::state::pretty_json_if_parses("plain text"), "plain text");
        // Read-file content is plain text: unchanged.
        let content = "fn main() {\n    println!(\"hi\");\n}\n";
        assert_eq!(crate::state::format_tool_result_for_display(content), content);
    }

    #[test]
    fn test_terminal_live_tail_has_absolute_line_numbers() {
        // 30 lines of live output, tail window of 10 → first shown line is 21.
        let item = TranscriptItem::Tool {
            name: "terminal".into(),
            emoji: "⚡".into(),
            summary: "build".into(),
            status: ToolStatus::Running,
            duration_secs: None,
            result_preview: String::new(),
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: true,
            exit_code: None,
            live_output: (1..=30).map(|i| format!("build line {i}")).collect::<Vec<_>>().join("\n"),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
        };
        let rendered = render_item(&item, 90);
        let joined = rendered.join("\n");
        assert!(joined.contains("21 │ build line 21"), "tail window starts at absolute line 21: {:?}", joined);
        assert!(joined.contains("30 │ build line 30"), "newest line numbered 30");
        assert!(!joined.contains("build line 1\n"), "head hidden");
    }

    #[test]
    fn test_diff_dual_gutter_line_numbers() {
        // A diff with context, a deletion, and an insertion: each rendered
        // row carries old/new numbers; insertion blanks the old number,
        // deletion blanks the new one (crush diffview semantics).
        let item = TranscriptItem::FileDiff {
            path: "src/main.rs".into(),
            stat: "+1 -1".into(),
            lines: vec![
                "--- a/src/main.rs".into(),
                "+++ b/src/main.rs".into(),
                "@@ -10,4 +10,4 @@ fn main() {".into(),
                " let keep = 1;".into(),
                "-let old = 2;".into(),
                "+let new = 2;".into(),
                " }".into(),
            ],
            is_binary: false,
            expand_state: ReasoningExpandState::TailWindow,
        };
        let rendered = render_item(&item, 100);
        let joined = rendered.join("\n");
        // Context lines carry both numbers.
        assert!(joined.contains("10 10"), "context carries old+new: {:?}", joined);
        // Deletion: old number present, new blank.
        assert!(joined.contains("11   - let old = 2;") || joined.matches("11").count() >= 1, "deletion carries old number");
        // Insertion: new number present, old blank.
        assert!(joined.contains("  11 + let new = 2;") || joined.contains("+ let new = 2;"), "insertion rendered with marker");
        assert!(joined.contains("@@"), "hunk header preserved");
    }

    #[test]
    fn test_output_viewer_shows_generic_tool_with_gutter() {
        use joey_agent_core::AgentEvent;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut app = crate::state::App::new("s", "m");
        app.apply(AgentEvent::ToolStart {
            name: "search_files".into(),
            emoji: "🔍".into(),
            summary: "pattern=foo".into(),
        });
        app.apply(AgentEvent::ToolEnd {
            name: "search_files".into(),
            is_error: false,
            result_preview: r#"{"matches":3}"#.into(),
            duration_secs: 0.4,
            exit_code: None,
            full_result: r#"{"matches":3,"files":["a.rs","b.rs","c.rs"]}"#.into(),
        });
        app.toggle_output_viewer(None);
        let theme = crate::theme::Theme::aurora();
        let backend = TestBackend::new(90, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let spinner = crate::anim::Spinner::dots();
        terminal
            .draw(|f| {
                draw_output_viewer(f, f.area(), &app, theme, &spinner);
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        assert!(text.contains("search_files"), "generic tool header rendered");
        assert!(text.contains("1 │ {"), "pretty JSON starts with gutter");
        assert!(text.contains("  \"matches\""), "JSON pretty-printed (indented key)");
    }

    #[test]
    fn test_output_viewer_terminal_envelope_unwrapped() {
        use joey_agent_core::AgentEvent;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut app = crate::state::App::new("s", "m");
        app.apply(AgentEvent::ToolStart {
            name: "terminal".into(),
            emoji: "💻".into(),
            summary: "make build".into(),
        });
        app.apply(AgentEvent::ToolEnd {
            name: "terminal".into(),
            is_error: false,
            result_preview: r#"{"output":"step 1 done\nstep 2 done","exit_code":0}"#.into(),
            duration_secs: 3.2,
            exit_code: Some(0),
            full_result: r#"{"output":"step 1 done\nstep 2 done","exit_code":0,"error":null}"#.into(),
        });
        app.toggle_output_viewer(None);
        let theme = crate::theme::Theme::aurora();
        let backend = TestBackend::new(90, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let spinner = crate::anim::Spinner::dots();
        terminal
            .draw(|f| {
                draw_output_viewer(f, f.area(), &app, theme, &spinner);
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect();
        assert!(text.contains("make build"), "command header rendered");
        assert!(text.contains("1 │ step 1 done"), "payload line 1 with gutter");
        assert!(text.contains("2 │ step 2 done"), "payload line 2 with gutter");
        assert!(!text.contains("exit_code"), "envelope fields not shown");
        assert!(!text.contains("\\n"), "no literal \\n runs");
    }
}

#[cfg(test)]
mod visual_check_tests {
    //! Print-oriented sanity checks: drive the REAL item_lines with realistic
    //! tool results and assert the exact user-visible rows.
    use super::*;
    use crate::state::ReasoningExpandState;

    #[test]
    fn terminal_block_visual() {
        let item = TranscriptItem::Tool {
            name: "terminal".into(),
            emoji: "💻".into(),
            summary: "cargo test -p joey-tui".into(),
            status: ToolStatus::Done,
            duration_secs: Some(4.2),
            result_preview: r#"{"output":"running 184 tests\ntest a ... ok\ntest b ... ok\n\ntest result: ok. 184 passed","exit_code":0,"error":null}"#.into(),
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: None,
            is_terminal: true,
            exit_code: Some(0),
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
        };
        let rendered: Vec<String> = item_lines(&item, 100, Theme::aurora())
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        for l in &rendered {
            println!("TERM|{}|", l);
        }
        // Envelope unwrapped: no JSON keys, real newlines as separate rows.
        assert!(!rendered.iter().any(|l| l.contains("exit_code") || l.contains("\"output\"")));
        assert!(rendered.iter().any(|l| l == "1 │ running 184 tests"), "row 1");
        assert!(rendered.iter().any(|l| l == "4 │ "), "blank line 4 preserved with gutter");
        assert!(rendered.iter().any(|l| l == "5 │ test result: ok. 184 passed"), "row 5");
    }

    #[test]
    fn diff_block_visual() {
        let item = TranscriptItem::FileDiff {
            path: "crates/joey-tui/src/lib.rs".into(),
            stat: "+2 -1".into(),
            lines: vec![
                "--- a/crates/joey-tui/src/lib.rs".into(),
                "+++ b/crates/joey-tui/src/lib.rs".into(),
                "@@ -41,6 +41,7 @@ pub mod state;".into(),
                " pub mod theme;".into(),
                "-pub mod old;".into(),
                "+pub mod new;".into(),
                "+pub mod extra;".into(),
                " pub mod input;".into(),
            ],
            is_binary: false,
            expand_state: ReasoningExpandState::TailWindow,
        };
        let rendered: Vec<String> = item_lines(&item, 100, Theme::aurora())
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        for l in &rendered {
            println!("DIFF|{}|", l);
        }
        let joined = rendered.join("\n");
        // Dual gutters: context has both numbers, deletion old-only,
        // insertion new-only (crush diffview semantics).
        assert!(joined.contains("41 41"), "context carries old+new");
        assert!(joined.contains("42    - pub mod old;"), "deletion: old number, blank new: {:?}", joined);
        assert!(joined.contains("   42 + pub mod new;"), "insertion: blank old, new number");
        assert!(joined.contains("   43 + pub mod extra;"), "second insertion numbered");
        assert!(joined.contains("43 44"), "context after edits renumbers new side");
    }
}

// ── Per-subagent panes (parallel-subagent feature) ─────────────────────────
//
// The right-side vertical tab rail stacks one tab per spawned subagent.
// Clicking a tab focuses that child: the main transcript area + the
// maximized stats/context window retarget to the child's live stream. The
// orchestrator's own view is always the leftmost implicit tab (focus None).

use crate::state::SubagentPane;

/// Draw the vertical subagent tab rail on the RIGHT edge of the body area.
/// Each pane gets one stacked tab (goal preview + status glyph). Records
/// per-tab hit rects on the App for click routing.
///
/// Two modes (expandable-rail feature):
/// - collapsed (default): fixed 19-col tab strip, 2 rows per pane —
///   byte-for-byte parity with the original layout.
/// - expanded (`subagent_rail_expanded`, Ctrl+N / title-click): wider
///   detail cards (4 rows: goal, model/depth/iterations, phase, last
///   tool). The width itself is decided by `render_body` (which clamps
///   back to 19 when the transcript would drop below ~60 cols); the
///   widget detects the clamped case by its `area.width` and renders
///   collapsed regardless of the flag.
pub fn draw_subagent_rail(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    // Reset hit rects for this frame; re-recorded per drawn tab below.
    app.last_subagent_tab_rects.borrow_mut().clear();

    if app.subagent_panes.is_empty() || area.width < 3 || area.height < 3 {
        app.last_subagent_rail_title_rect.set((0, 0, 0, 0));
        app.last_subagent_rail_rect.set((0, 0, 0, 0));
        app.last_subagent_rail_max_scroll.set(0);
        app.last_subagent_rail_drawn_offset.set(0);
        return;
    }

    // The rail only receives the EXPANDED width when render_body honored
    // it; a 19-col area means the terminal was too narrow (clamped).
    let expanded = app.subagent_rail_expanded && area.width >= 24;

    // Collapsed keeps the original 18-of-19-cols geometry (19 total minus
    // the 1-col LEFT border = 18 inner); expanded fills the wider
    // allocation render_body granted, capped at 48.
    let rail_w = if expanded { area.width.min(48) } else { 19u16.min(area.width) };
    let rail = Rect::new(area.x + area.width - rail_w, area.y, rail_w, area.height);
    // Whole-strip rect for mouse-wheel routing (handle_mouse_scroll).
    app.last_subagent_rail_rect
        .set((rail.x, rail.y, rail.width, rail.height));
    // Panel background.
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(theme.separator.to_color()))
        .style(Style::default().bg(theme.bg_panel.to_color()));
    let inner = block.inner(rail);
    f.render_widget(block, rail);

    // Title row — clickable: toggles rail expansion (Ctrl+N parity).
    // The trailing ▸ (collapsed → can widen) / ▾ (expanded → can shrink)
    // hints the toggle and still fits the 18-col collapsed strip.
    let hint = if expanded { "▾" } else { "▸" };
    let title = Line::from(vec![
        Span::styled(
            " subagents ".to_string(),
            Style::default()
                .fg(theme.primary.to_color())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("({})", app.subagent_panes.len()),
            Style::default().fg(theme.fg_more_subtle.to_color()),
        ),
        Span::styled(
            format!(" {}", hint),
            Style::default().fg(theme.fg_more_subtle.to_color()),
        ),
    ]);
    if inner.width > 0 {
        f.render_widget(
            Paragraph::new(title).style(Style::default().bg(theme.bg_panel.to_color())),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        app.last_subagent_rail_title_rect
            .set((inner.x, inner.y, inner.width, 1));
    } else {
        app.last_subagent_rail_title_rect.set((0, 0, 0, 0));
    }

    // Truncate a string to at most `w` chars (char-boundary safe, matching
    // the collapsed tab's `chars().take()` policy).
    fn trunc(s: &str, w: usize) -> String {
        s.chars().take(w.max(0)).collect()
    }

    // ── Scrollable tab window (overflow panes) ────────────────────────
    // Compute the visible pane capacity from the actual inner area using
    // the SAME break condition the draw loops below apply (rows per tab:
    // 2 collapsed / 4 expanded; 2 rows reserved for the pinned
    // orchestrator tab; 1 row for the title).
    let row_h: u16 = if expanded { 4 } else { 2 };
    let limit_y = inner.y + inner.height.saturating_sub(2);
    let mut capacity = 0usize;
    while inner.y + 1 + (capacity as u16) * row_h + row_h - 1 < limit_y {
        capacity += 1;
    }
    let max_scroll = app.subagent_panes.len().saturating_sub(capacity);
    let offset = app.subagent_rail_scroll.min(max_scroll);
    app.last_subagent_rail_max_scroll.set(max_scroll);
    app.last_subagent_rail_drawn_offset.set(offset);
    // When the tabs overflow, reserve the rail's inner RIGHT column for a
    // scroll indicator so it never overlaps tab content.
    let content_w = if max_scroll > 0 {
        inner.width.saturating_sub(1)
    } else {
        inner.width
    };

    if expanded {
        // ── Expanded: 4-row detail cards ────────────────────────────
        // Line 1: status glyph + task title/goal (click target).
        // Line 2: model · depth · API iterations.
        // Line 3: live phase (from the matched activity entry).
        // Line 4: last invoked tool, when known.
        let label_w = content_w.saturating_sub(3) as usize;
        let detail_w = content_w.saturating_sub(3) as usize;
        for (vi, (i, pane)) in app
            .subagent_panes
            .iter()
            .enumerate()
            .skip(offset)
            .enumerate()
        {
            let card_y = inner.y + 1 + (vi as u16) * 4;
            if card_y + 3 >= limit_y {
                break; // rail full (reserve 2 rows for the orchestrator tab)
            }
            let focused = app.focused_subagent == Some(i);
            let (glyph, color) = match pane.status {
                SubagentStatus::Running => ("◐", theme.busy),
                SubagentStatus::Done => ("✓", theme.success),
                SubagentStatus::Failed => ("✗", theme.error),
                SubagentStatus::Pending => ("·", theme.fg_more_subtle),
                // Spec 020 (T030): halted before completing its goal.
                SubagentStatus::Stopped => ("■", theme.warning),
            };
            // Task title (falls back to the goal) truncated to the card.
            let title_src = app
                .subagent_entries
                .iter()
                .rev()
                .find(|e| e.child_id == pane.child_id)
                .and_then(|e| e.task_title.clone())
                .unwrap_or_else(|| pane.goal.clone());
            let title_line = trunc(&title_src, label_w);
            let focus_style = if focused {
                Style::default()
                    .bg(theme.primary.to_color())
                    .fg(theme.bg_void.to_color())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .bg(theme.bg_panel.to_color())
                    .fg(theme.fg_base.to_color())
            };
            let mut spans = vec![
                Span::styled(format!("{} ", glyph), Style::default().fg(color.to_color())),
                Span::styled(title_line, focus_style),
            ];
            if focused {
                spans.insert(0, Span::styled("▸".to_string(), Style::default().fg(theme.primary.to_color())));
            }
            let card_area = Rect::new(inner.x, card_y, content_w, 4);
            f.render_widget(
                Paragraph::new(Line::from(spans)).style(Style::default().bg(if focused {
                    theme.primary.to_color()
                } else {
                    theme.bg_panel.to_color()
                })),
                Rect::new(inner.x, card_y, content_w, 1),
            );
            // Detail lines (dim; never the click target).
            let entry = app
                .subagent_entries
                .iter()
                .rev()
                .find(|e| e.child_id == pane.child_id);
            let dim = Style::default()
                .fg(theme.fg_more_subtle.to_color())
                .bg(theme.bg_panel.to_color());
            let line2 = format!(
                "{} · d{} · {} it",
                trunc(&pane.model, detail_w.saturating_sub(12)),
                pane.depth,
                pane.tokens.iterations
            );
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(format!("  {}", line2), dim)))
                    .style(Style::default().bg(theme.bg_panel.to_color())),
                Rect::new(inner.x, card_y + 1, content_w, 1),
            );
            let phase = entry
                .map(|e| e.phase.clone())
                .unwrap_or_else(|| match pane.status {
                    SubagentStatus::Running => "running".to_string(),
                    SubagentStatus::Done => "done".to_string(),
                    SubagentStatus::Failed => "failed".to_string(),
                    SubagentStatus::Pending => "queued".to_string(),
                    // Spec 020 (T030): partial-result terminal state.
                    SubagentStatus::Stopped => "stopped".to_string(),
                });
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!("  {}", trunc(&phase, detail_w)),
                    dim,
                )))
                .style(Style::default().bg(theme.bg_panel.to_color())),
                Rect::new(inner.x, card_y + 2, content_w, 1),
            );
            if let Some(tool) = entry.and_then(|e| e.last_tool.clone()) {
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!("  ⚒ {}", trunc(&tool, detail_w)),
                        dim,
                    )))
                    .style(Style::default().bg(theme.bg_panel.to_color())),
                    Rect::new(inner.x, card_y + 3, content_w, 1),
                );
            }
            // Record the clickable row (the card's title line).
            app.last_subagent_tab_rects
                .borrow_mut()
                .push((card_area.x, card_y, card_area.width, 1));
        }
    } else {
        // Tabs: 2 rows each, stacked vertically below the title.
        let label_w = content_w.saturating_sub(2) as usize;
        for (vi, (i, pane)) in app
            .subagent_panes
            .iter()
            .enumerate()
            .skip(offset)
            .enumerate()
        {
            let tab_y = inner.y + 1 + (vi as u16) * 2;
            if tab_y + 1 >= limit_y {
                break; // rail full (reserve 2 rows for the orchestrator tab)
            }
            let focused = app.focused_subagent == Some(i);
            let (glyph, color) = match pane.status {
                SubagentStatus::Running => ("◐", theme.busy),
                SubagentStatus::Done => ("✓", theme.success),
                SubagentStatus::Failed => ("✗", theme.error),
                SubagentStatus::Pending => ("·", theme.fg_more_subtle),
                // Spec 020 (T030): halted before completing its goal.
                SubagentStatus::Stopped => ("■", theme.warning),
            };
            let goal_line: String = pane.goal.chars().take(label_w.max(4)).collect();
            let tab_area = Rect::new(inner.x, tab_y, content_w, 2);
            let style = if focused {
                Style::default()
                    .bg(theme.primary.to_color())
                    .fg(theme.bg_void.to_color())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .bg(theme.bg_panel.to_color())
                    .fg(theme.fg_base.to_color())
            };
            let mut spans = vec![
                Span::styled(format!("{} ", glyph), Style::default().fg(color.to_color())),
                Span::styled(goal_line, style),
            ];
            if focused {
                spans.insert(0, Span::styled("▸".to_string(), Style::default().fg(theme.primary.to_color())));
            }
            f.render_widget(
                Paragraph::new(Line::from(spans)).style(Style::default().bg(if focused {
                    theme.primary.to_color()
                } else {
                    theme.bg_panel.to_color()
                })),
                tab_area,
            );
            // Record the clickable row (first line of the tab).
            app.last_subagent_tab_rects
                .borrow_mut()
                .push((tab_area.x, tab_y, tab_area.width, 1));
        }
    }

    // ── Scroll indicator (only when tabs overflow) ────────────────────
    // Minimal custom scrollbar on the rail's inner RIGHT column, drawn
    // with the same direct '█'/'│' cell writes as `draw_scrollbar` (the
    // ratatui Scrollbar widget is deliberately not used in this crate).
    // Content width was already shrunk by 1 col so it never overlaps.
    if max_scroll > 0 && inner.width >= 2 {
        let track_y = inner.y + 1;
        let track_h = (inner.height.saturating_sub(3)) as usize; // below title, above orch tab
        if track_h > 0 {
            let x = inner.x + inner.width - 1;
            let track_color = theme.bg_panel.to_color();
            let thumb_color = theme.info.to_color();
            // Thumb sized by the visible fraction of the pane list.
            let total = app.subagent_panes.len();
            let visible = capacity.min(total);
            let ratio = visible as f64 / total as f64;
            let thumb_size = ((track_h as f64 * ratio).ceil() as usize)
                .max(1)
                .min(track_h);
            // offset counts skipped panes from the top; progress toward
            // the bottom of the list pushes the thumb DOWN (rail scrolls
            // the opposite direction of the transcript scrollbar, whose
            // None-scroll pins the thumb at the bottom).
            let progress = offset as f64 / max_scroll as f64;
            let thumb_top = ((track_h - thumb_size) as f64 * progress).round() as usize;
            let buf = f.buffer_mut();
            for dy in 0..track_h {
                let cell = &mut buf[(x, track_y + dy as u16)];
                let in_thumb = dy >= thumb_top && dy < thumb_top + thumb_size;
                let ch = if in_thumb { '█' } else { '│' };
                cell.set_char(ch).set_style(
                    Style::default()
                        .fg(if in_thumb { thumb_color } else { track_color })
                        .bg(track_color),
                );
            }
            // Above/below availability glyphs at the track's ends: '▲'
            // when earlier panes are hidden, '▼' when later ones are.
            if offset > 0 {
                let cell = &mut buf[(x, track_y)];
                cell.set_char('▲').set_style(
                    Style::default().fg(theme.fg_more_subtle.to_color()).bg(track_color),
                );
            }
            if offset < max_scroll {
                let cell = &mut buf[(x, track_y + (track_h - 1) as u16)];
                cell.set_char('▼').set_style(
                    Style::default().fg(theme.fg_more_subtle.to_color()).bg(track_color),
                );
            }
        }
    }

    // ── Orchestrator tab (pinned at the rail's bottom) ─────────────────
    // Always present when the rail is drawn: a dedicated way back to the
    // main view. Clicking any pane tab focuses that child; clicking THIS
    // one (or pressing Ctrl+P) returns to the orchestrator. Highlighted
    // exactly like a focused pane tab when the main view IS the active one
    // (focused_subagent == None), so the current view is always visible in
    // the rail.
    {
        let orch_y = inner.y + inner.height.saturating_sub(1);
        let orch_area = Rect::new(inner.x, orch_y, inner.width, 1);
        if orch_y > inner.y {
            let focused = app.focused_subagent.is_none();
            let style = if focused {
                Style::default()
                    .bg(theme.primary.to_color())
                    .fg(theme.bg_void.to_color())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .bg(theme.bg_panel.to_color())
                    .fg(theme.fg_base.to_color())
            };
            let glyph = if app.is_busy() { "◐" } else { "◆" };
            let glyph_col = if app.is_busy() { theme.busy } else { theme.accent };
            let mut spans = vec![
                Span::styled(format!("{} ", glyph), Style::default().fg(glyph_col.to_color())),
                Span::styled("orchestrator".to_string(), style),
            ];
            if focused {
                spans.insert(0, Span::styled("▸".to_string(), Style::default().fg(theme.primary.to_color())));
            }
            f.render_widget(
                Paragraph::new(Line::from(spans)).style(Style::default().bg(if focused {
                    theme.primary.to_color()
                } else {
                    theme.bg_panel.to_color()
                })),
                orch_area,
            );
            // Record the orchestrator tab's own click rect (checked by
            // App::orchestrator_tab_hit BEFORE the per-pane rects).
            app.last_orchestrator_tab_rect
                .set((orch_area.x, orch_y, orch_area.width, 1));
        }
    }
}

/// Draw the focused subagent's transcript in the main area (parallel-
/// subagent feature). Mirrors `draw_transcript` but reads from the pane and
/// records geometry into the App-level pane cells.
pub fn draw_pane_transcript(
    f: &mut Frame,
    area: Rect,
    app: &App,
    pane: &SubagentPane,
    theme: Theme,
    focused: bool,
    glow: f32,
) {
    // T023 (US5, FR-009, D2 Invariant 1): the pane header carries the same scroll-info segment
    // the orchestrator's `draw_transcript` builds — same helper, same N
    // (transcript item count), same bound (the pane's own recorded max).
    let scroll_info = transcript_scroll_info(
        pane.transcript.len(),
        pane.scroll,
        app.last_pane_max_scroll.get(),
    );
    // T036 (US5, FR-001/FR-009, SC-003): UNIFIED PLACEMENT — the scroll
    // segment now composes into the pane's TOP title exactly the way
    // `draw_transcript` composes its single top title, via the shared
    // `pane_header_title` helper below (which composes " ◆ subagent:
    // {goal} [{model}] {status} {segment} " with the SAME
    // `focused_header_segments` / bare-`transcript_scroll_info` segment
    // logic the orchestrator title uses). Ratatui clips LEFT titles on
    // the right (ratatui-widgets 0.3 `Block::render_left_titles`
    // truncates a too-long title at the pane's usable width), so the
    // helper first checks the composed title FITS the pane's title row
    // (`area.width - 2` inside the borders); when it would truncate the
    // segment, the block falls back to the pre-T036 bottom-right corner
    // placement so the scroll-info (and the focused scroll-key hint)
    // always stay fully visible — same strings, same
    // `transcript_scroll_info` / `transcript_scroll_hint` either way.
    let header = pane_header_title(pane, &scroll_info, focused, area.width);
    let mut block = panel_block(&header.title, theme, focused, glow);
    if let Some(bottom) = header.bottom_fallback {
        // Fit fallback (T036): the composed top title would be clipped —
        // ride the segment on the block's bottom-right corner instead.
        block = block.title_bottom(Line::from(bottom).right_aligned());
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        app.last_pane_text_area.set((0, 0, 0, 0));
        return;
    }

    // T023 (US5, FR-009, D2 Invariant 1 — "single rendering source"): the
    // pane's ENTIRE body (scrollbar-column geometry, lazy line build via
    // the shared `item_lines` + `streaming_tail_lines`, bottom-anchored
    // scroll accounting, `draw_scrollbar`, `draw_below_badge`) is the SAME
    // `render_transcript_body` the orchestrator's `draw_transcript` calls.
    // Only the data source (the pane's transcript/stream/scroll) and the
    // recording cells (pane-side geometry/max-scroll) differ — borders,
    // colors, glyphs, and empty-state behavior are shared by construction.
    render_transcript_body(
        f,
        inner,
        &pane.transcript,
        &pane.streaming_assistant,
        pane.scroll,
        theme,
        &app.last_pane_text_area,
        &app.last_pane_max_scroll,
    );
}

/// Draw the focused subagent's maximized stats/context page (parallel-
/// subagent feature). Mirrors `draw_stats_page` but reads the pane's
/// context-window snapshot + usage; driven when the user opens the stats
/// view while a pane is focused.
pub fn draw_pane_stats_page(
    f: &mut Frame,
    area: Rect,
    app: &App,
    pane: &SubagentPane,
    theme: Theme,
    spinner: &Spinner,
) {
    app.last_pane_stats_rect.set((area.x, area.y, area.width, area.height));
    let live = pane.status == SubagentStatus::Running;
    let title = format!(
        " ◆ subagent stats · {} · {} ",
        pane.model,
        if live { "LIVE · Esc to restore" } else { "Esc to restore" }
    );

    // T035 (FR-009/SC-003, D2 "same widget functions"): thin adapter —
    // the SAME shared `render_stats_page_composed` the orchestrator's
    // `draw_stats_page` drives; this adapter supplies the PANE's data,
    // its intentionally different labels (" child   ", "goal:" breakdown,
    // no per-call sparkline — the child has no usage series) and its own
    // recording cells (per-pane scroll anchor + App-level hit-test cells).
    render_stats_page_composed(
        f,
        area,
        &title,
        live,
        StatsPageData {
            used: pane.context_system_tokens + pane.context_history_tokens,
            window: pane.context_window,
            pct: pane.context_usage_pct(),
            breakdown_value: subtle_span(
                format!(
                    "goal: {} · system {} · history {} · msgs {}",
                    pane.goal,
                    fmt_tokens(pane.context_system_tokens),
                    fmt_tokens(pane.context_history_tokens),
                    pane.context_entries.len(),
                ),
                theme,
            ),
            breakdown_indent: 9,
            session_label: " child   ",
            session_value: subtle_span(
                format!(
                    "prompt {} · completion {} · total {} · iters {} · {:.0}s elapsed",
                    fmt_tokens(pane.tokens.prompt),
                    fmt_tokens(pane.tokens.completion),
                    fmt_tokens(pane.tokens.total()),
                    pane.tokens.iterations,
                    pane.started.elapsed().as_secs_f32(),
                ),
                theme,
            ),
            usage_series: None,
            entries: &pane.context_entries,
            expanded: &pane.expanded_context,
            empty_note: "(no context yet — the child is waiting for its first response)",
        },
        // T004: the anchor bound + view anchor are read PER-PANE so each
        // pane preserves its own scroll across focus switches (FR-010).
        pane.stats_view,
        &pane.last_stats_max_anchor,
        &app.last_pane_stats_window,
        &app.last_pane_stats_stream_rows,
        None,
        spinner,
        theme,
    );
}
