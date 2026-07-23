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
    AgentPhase, App, NoticeKind, RunMode, ToolStatus, TranscriptItem,
};
use crate::theme::{gradient_spans, Rgb, Theme};
use unicode_width::UnicodeWidthStr;

/// Helper: build a Block with a gradient title.
pub fn gradient_block(title: &str, theme: Theme) -> Block<'_> {
    let title_spans = gradient_spans(title, theme);
    Block::default()
        .borders(Borders::ALL)
        .title(Line::from(title_spans))
        .border_style(Style::default().fg(theme.separator.to_color()))
        .style(Style::default().bg(theme.bg_panel.to_color()))
}

/// Focused variant: the border glows in the primary color.
pub fn gradient_block_focused(title: &str, theme: Theme, pulse: f32) -> Block<'_> {
    let title_spans = gradient_spans(title, theme);
    // Blend border between separator and primary using the pulse value.
    let border = theme.separator.lerp(theme.primary, 0.4 + pulse * 0.6);
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

// ── Particle backdrop ───────────────────────────────────────────────────────
//
// Drawn first across the full terminal as a subtle animated starfield behind
// all panels. Panels have opaque backgrounds so they sit on top cleanly.

pub fn draw_particles(f: &mut Frame, field: &ParticleField, theme: Theme, area: Rect) {
    let buf = f.buffer_mut();
    for p in field.particles() {
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
    let buf_area = Block::default()
        .style(Style::default().bg(theme.bg_elevated.to_color()));
    f.render_widget(buf_area, area);

    // Left: gradient wordmark "joey" with glowing effect.
    let logo = "✦ joey";
    let glow = pulse.value();
    // Brighten the gradient with glow.
    let bright_stops = [
        theme.grad_0.lerp(Rgb(255, 255, 255), glow * 0.4),
        theme.grad_1.lerp(Rgb(255, 255, 255), glow * 0.4),
        theme.grad_2.lerp(Rgb(255, 255, 255), glow * 0.4),
        theme.grad_3.lerp(Rgb(255, 255, 255), glow * 0.4),
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

    // Subtle gradient underline.
    let underline_y = area.y + area.height.saturating_sub(1);
    for i in 0..area.width {
        let t = i as f32 / area.width.max(1) as f32;
        let c = crate::theme::sample_stops(&[theme.grad_0, theme.grad_1, theme.grad_2, theme.grad_3], t);
        let cell = &mut buf[(area.x + i, underline_y)];
        cell.set_char('─')
            .set_style(Style::default().fg(c.to_color()));
    }
}

fn short_id(id: &str) -> String {
    let len = id.len().min(8);
    id[..len].to_string()
}

// ── Conversation / transcript ───────────────────────────────────────────────

pub fn draw_transcript(f: &mut Frame, area: Rect, app: &App, theme: Theme) {
    let block = gradient_block("conversation", theme);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Build the text lines from the transcript, wrapping each item.
    let content_w = inner.width.max(1) as usize;
    let mut lines: Vec<Line> = Vec::new();

    for item in app.transcript.iter() {
        match item {
            TranscriptItem::User { text } => {
                let prompt_span = Span::styled(
                    "❯ ",
                    Style::default().fg(theme.accent.to_color()).add_modifier(Modifier::BOLD),
                );
                lines.push(Line::from(vec![prompt_span]));
                for wl in wrap(text, content_w.saturating_sub(2)) {
                    lines.push(Line::from(vec![Span::styled(
                        format!("  {}", wl),
                        Style::default().fg(theme.fg_base.to_color()),
                    )]));
                }
            }
            TranscriptItem::Assistant { text } => {
                let badge = Span::styled(
                    "◆ assistant ",
                    Style::default()
                        .fg(theme.info.to_color())
                        .add_modifier(Modifier::BOLD),
                );
                lines.push(Line::from(vec![badge]));
                for wl in wrap(text, content_w.saturating_sub(2)) {
                    lines.push(Line::from(vec![Span::styled(
                        format!("  {}", wl),
                        Style::default().fg(theme.fg_base.to_color()),
                    )]));
                }
                lines.push(Line::from(vec![Span::raw("")]));
            }
            TranscriptItem::AssistantStreaming { text } => {
                let badge = Span::styled(
                    "◆ assistant ",
                    Style::default()
                        .fg(theme.info.to_color())
                        .add_modifier(Modifier::BOLD),
                );
                lines.push(Line::from(vec![badge]));
                for wl in wrap(text, content_w.saturating_sub(2)) {
                    lines.push(Line::from(vec![Span::styled(
                        format!("  {}", wl),
                        Style::default().fg(theme.fg_base.to_color()),
                    )]));
                }
            }
            TranscriptItem::Reasoning { text } => {
                let head = Span::styled(
                    "┄ reasoning ",
                    Style::default().fg(theme.fg_more_subtle.to_color()),
                );
                lines.push(Line::from(vec![head]));
                for wl in wrap(text, content_w.saturating_sub(2)) {
                    lines.push(Line::from(vec![Span::styled(
                        format!("  {}", wl),
                        Style::default()
                            .fg(theme.fg_more_subtle.to_color())
                            .add_modifier(Modifier::DIM),
                    )]));
                }
            }
            TranscriptItem::Tool { name, emoji, summary, status, duration_secs, result_preview } => {
                let (icon, col) = match status {
                    ToolStatus::Running => ("⟳", theme.busy),
                    ToolStatus::Done => ("✓", theme.success),
                    ToolStatus::Failed => ("✗", theme.error),
                };
                let dur_str = duration_secs
                    .map(|d| format!("  {:.1}s", d))
                    .unwrap_or_default();
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
                    let s: String = summary.chars().take(content_w.saturating_sub(name.len() + 10)).collect();
                    spans.push(Span::styled(
                        format!(" {}", s),
                        Style::default().fg(theme.fg_most_subtle.to_color()),
                    ));
                }
                spans.push(Span::styled(
                    dur_str,
                    Style::default().fg(theme.fg_more_subtle.to_color()),
                ));
                lines.push(Line::from(spans));
                if *status == ToolStatus::Done && !result_preview.is_empty() {
                    let preview: String = result_preview.chars().take(100).collect();
                    lines.push(Line::from(vec![Span::styled(
                        format!("    └ {}", preview),
                        Style::default().fg(theme.fg_most_subtle.to_color()),
                    )]));
                }
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
                    Span::styled(text.clone(), Style::default().fg(theme.fg_more_subtle.to_color())),
                ]));
            }
            TranscriptItem::Error { text } => {
                lines.push(Line::from(vec![Span::styled(
                    format!("  ✗ {}", text),
                    Style::default().fg(theme.error.to_color()).add_modifier(Modifier::BOLD),
                )]));
            }
        }
    }

    // Append live streaming content (not yet committed).
    if !app.streaming_assistant.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "◆ assistant ",
            Style::default().fg(theme.info.to_color()).add_modifier(Modifier::BOLD),
        )]));
        for wl in wrap(&app.streaming_assistant, content_w.saturating_sub(2)) {
            lines.push(Line::from(vec![Span::styled(
                format!("  {}", wl),
                Style::default().fg(theme.fg_base.to_color()),
            )]));
        }
    }
    if app.reasoning_open && !app.streaming_reasoning.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "┄ reasoning ",
            Style::default().fg(theme.fg_more_subtle.to_color()),
        )]));
        for wl in wrap(&app.streaming_reasoning, content_w.saturating_sub(2)) {
            lines.push(Line::from(vec![Span::styled(
                format!("  {}", wl),
                Style::default().fg(theme.fg_more_subtle.to_color()).add_modifier(Modifier::DIM),
            )]));
        }
    }

    // Compute scroll: auto-follow bottom unless user scrolled up.
    let total = lines.len();
    let visible = inner.height as usize;
    let scroll = match app.scroll {
        Some(offset) => total.saturating_sub(visible + offset),
        None => total.saturating_sub(visible),
    };

    let para = Paragraph::new(Text::from(lines))
        .scroll((scroll as u16, 0));
    f.render_widget(para, inner);
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
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

// ── Activity / tools sidebar ────────────────────────────────────────────────

pub fn draw_activity(
    f: &mut Frame,
    area: Rect,
    app: &App,
    theme: Theme,
    spinner: &Spinner,
    equalizer: &Equalizer,
) {
    let block = gradient_block_focused("activity", theme, 0.5);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cw = inner.width.max(1) as usize;

    // Section 1: active agents list.
    let mut lines: Vec<Line> = Vec::new();
    if app.active_agents.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "  ◌ idle — awaiting input".to_string(),
            Style::default().fg(theme.fg_most_subtle.to_color()),
        )]));
    } else {
        for a in &app.active_agents {
            let (phase_text, phase_col): (String, Rgb) = match &a.phase {
                AgentPhase::Idle => ("queued".to_string(), theme.fg_more_subtle),
                AgentPhase::QueryingModel => ("querying model".to_string(), theme.info),
                AgentPhase::RunningTool(t) => {
                    (t.clone(), theme.accent)
                }
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
        format!("   it  {}", t.iterations),
        Style::default().fg(theme.fg_subtle.to_color()),
    )]));

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

// ── Input box ───────────────────────────────────────────────────────────────

pub fn draw_input(f: &mut Frame, area: Rect, input: &Input, app: &App, theme: Theme) {
    let focused = matches!(app.mode, RunMode::Input);
    let title = if app.is_busy() { "input (busy)" } else { "input" };
    let block = if focused {
        gradient_block_focused(title, theme, 0.8)
    } else {
        gradient_block(title, theme)
    };
    let inner = block.inner(area);
    f.render_widget(block, area);

    let (first_line, x_off) = input.view_offset(inner);
    let cw = inner.width.saturating_sub(1) as usize;

    let mut lines: Vec<Line> = Vec::new();
    let visible_lines = &input.lines()[first_line..];
    for (idx, l) in visible_lines.iter().enumerate() {
        let actual_line = first_line + idx;
        let prefix = if actual_line == 0 { "❯ " } else { "… " };
        let prefix_span = Span::styled(
            prefix,
            Style::default().fg(theme.accent.to_color()).add_modifier(Modifier::BOLD),
        );
        // Horizontal crop around cursor.
        let chars: Vec<char> = l.chars().collect();
        let cropped: String = chars.iter().skip(x_off).take(cw).collect();
        let content_span = Span::styled(
            cropped,
            Style::default().fg(theme.fg_base.to_color()),
        );
        lines.push(Line::from(vec![prefix_span, content_span]));
    }

    // Ensure at least one line.
    if lines.is_empty() {
        let prefix_span = Span::styled(
            "❯ ",
            Style::default().fg(theme.accent.to_color()).add_modifier(Modifier::BOLD),
        );
        let placeholder = Span::styled(
            if app.is_busy() { "agent working… (Esc/Ctrl-C to interrupt)" } else { "" },
            Style::default().fg(theme.fg_most_subtle.to_color()),
        );
        lines.push(Line::from(vec![prefix_span, placeholder]));
    }

    f.render_widget(Paragraph::new(Text::from(lines)), inner);

    // Place the block cursor.
    let (cur_line, cur_col) = input.cursor();
    let view_line = cur_line.saturating_sub(first_line);
    let cursor_x = inner.x + 2 + (cur_col.saturating_sub(x_off)) as u16;
    let cursor_y = inner.y + view_line as u16;
    if cursor_x < inner.x + inner.width && cursor_y < inner.y + inner.height {
        let cell = &mut f.buffer_mut()[(cursor_x, cursor_y)];
        cell.set_style(
            Style::default()
                .bg(theme.secondary.to_color())
                .add_modifier(Modifier::REVERSED),
        );
    }
}

// ── Status bar ──────────────────────────────────────────────────────────────

pub fn draw_status(f: &mut Frame, area: Rect, app: &App, theme: Theme, elapsed: Duration) {
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

    // Right-aligned keymap hint.
    let hint = "⏎ send  ⎇↵ newline  ↑↓ scroll  R reasoning  ? help  ⎋ quit";
    let hint_span = Span::styled(
        hint.to_string(),
        Style::default().fg(theme.fg_most_subtle.to_color()),
    );

    let line = Line::from(spans);
    // Compose with right-aligned hint by rendering hint into trailing cells.
    let para = Paragraph::new(line).style(Style::default().bg(bg));
    f.render_widget(para, area);

    // Right-aligned hint.
    let hint_w = UnicodeWidthStr::width(hint) as u16;
    let hx = area.x + area.width.saturating_sub(hint_w + 1);
    let hy = area.y;
    if hx > area.x {
        let buf = f.buffer_mut();
        let mut xx = hx;
        for ch in hint.chars() {
            if xx >= area.x + area.width {
                break;
            }
            let cell = &mut buf[(xx, hy)];
            cell.set_char(ch)
                .set_style(hint_span.style);
            xx += 1;
        }
    }
}

fn fmt_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}.{:.0}s", s, d.subsec_millis() / 100)
    }
}

fn shorten_path(p: &str, max: usize) -> String {
    if p.len() <= max {
        return p.to_string();
    }
    let last = p.rsplit('/').next().unwrap_or(p);
    if last.len() >= max {
        return format!("…/{}", &last[..max.saturating_sub(2).min(last.len())]);
    }
    format!("…/{}", last)
}

// ── Help overlay ────────────────────────────────────────────────────────────

pub fn draw_help_overlay(f: &mut Frame, area: Rect, theme: Theme) {
    // Centered modal.
    let w = 52.min(area.width);
    let h = 16.min(area.height);
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height - h) / 2;
    let modal = Rect::new(x, y, w, h);
    f.render_widget(Clear, modal);
    let block = gradient_block_focused(" help — press ? to close ", theme, 0.8);
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    let keymap = [
        ("Enter", "send message"),
        ("Alt+Enter", "insert newline"),
        ("Ctrl+C / Esc", "interrupt running turn"),
        ("Ctrl+C twice", "quit"),
        ("↑ / ↓", "scroll transcript"),
        ("PageUp/PageDn", "scroll faster"),
        ("R", "toggle reasoning panel"),
        ("Tab", "cycle focus"),
        ("?", "toggle this help"),
    ];
    let items: Vec<ListItem> = keymap
        .iter()
        .map(|(k, desc)| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("  {:<16}", k),
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

// ── Streaming-only renderer: full-frame used by the headless runner ────────
//
// This lets us render a TUI during a turn even when there's no line-editor,
// mirroring the behavior of `render_turn` but animated.

// Re-exports so callers can access the helpers.
pub use draw_transcript as render_transcript;
