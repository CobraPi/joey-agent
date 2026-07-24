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
    AgentPhase, App, DisplayAgent, NoticeKind, RunMode, ToolStatus, TranscriptItem,
};
use crate::theme::{gradient_spans, Rgb, Theme};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
            for wl in wrap(text, content_w.saturating_sub(2)) {
                lines.push(Line::from(vec![Span::styled(
                    format!("  {}", wl),
                    Style::default().fg(theme.fg_base.to_color()),
                )]));
            }
        }
        TranscriptItem::Assistant { text } => {
            lines.push(Line::from(vec![Span::styled(
                "◆ assistant ",
                Style::default()
                    .fg(theme.info.to_color())
                    .add_modifier(Modifier::BOLD),
            )]));
            for wl in wrap(text, content_w.saturating_sub(2)) {
                lines.push(Line::from(vec![Span::styled(
                    format!("  {}", wl),
                    Style::default().fg(theme.fg_base.to_color()),
                )]));
            }
            lines.push(Line::from(vec![Span::raw("")]));
        }
        TranscriptItem::Reasoning { text } => {
            lines.push(Line::from(vec![Span::styled(
                "┄ reasoning ",
                Style::default().fg(theme.fg_more_subtle.to_color()),
            )]));
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
                let s = one_line(summary, content_w.saturating_sub(name.len() + 10));
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
            if !result_preview.is_empty() && !matches!(status, ToolStatus::Running) {
                let preview = one_line(result_preview, 100);
                let col = if matches!(status, ToolStatus::Failed) {
                    theme.error
                } else {
                    theme.fg_most_subtle
                };
                lines.push(Line::from(vec![Span::styled(
                    format!("    └ {}", preview),
                    Style::default().fg(col.to_color()),
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
                Span::styled(
                    one_line(text, content_w.saturating_sub(4)),
                    Style::default().fg(theme.fg_more_subtle.to_color()),
                ),
            ]));
        }
        TranscriptItem::Error { text } => {
            for wl in wrap(text, content_w.saturating_sub(4)) {
                lines.push(Line::from(vec![Span::styled(
                    format!("  ✗ {}", wl),
                    Style::default().fg(theme.error.to_color()).add_modifier(Modifier::BOLD),
                )]));
            }
        }
    }
    lines
}

pub fn draw_transcript(f: &mut Frame, area: Rect, app: &App, theme: Theme, focused: bool, glow: f32) {
    let block = panel_block("conversation", theme, focused, glow);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let content_w = inner.width as usize;
    let visible = inner.height as usize;
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
            "◆ assistant ",
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
    f.render_widget(para, inner);

    // Scrolled-up indicator: bottom-right badge showing the distance to live.
    if app.scroll.is_some() && clamped > 0 {
        let badge = format!(" ↓ {} line{} below ", clamped, if clamped == 1 { "" } else { "s" });
        let bw = UnicodeWidthStr::width(badge.as_str()) as u16;
        if bw < inner.width {
            let bx = inner.x + inner.width - bw;
            let by = inner.y + inner.height - 1;
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

pub fn draw_activity(
    f: &mut Frame,
    area: Rect,
    app: &App,
    theme: Theme,
    spinner: &Spinner,
    equalizer: &Equalizer,
) {
    let block = gradient_block("activity", theme);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

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
            "agent working — type to queue the next prompt · Esc interrupts"
        } else {
            "type a prompt · ? for help"
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
        "⏎ queue  Esc interrupt  ^R reasoning  ? help"
    } else {
        "⏎ send  ⌥⏎ newline  Tab focus  ^R reasoning  ? help  Esc quit"
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
    let h = 18.min(area.height);
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
        ("Esc / Ctrl+C", "interrupt turn (idle: quit)"),
        ("Ctrl+C ×2", "force exit during a turn"),
        ("Ctrl+D", "quit (on empty input)"),
        ("Tab", "focus input ↔ transcript"),
        ("↑ / ↓", "scroll transcript (single-line input)"),
        ("PgUp / PgDn", "scroll transcript"),
        ("g / G", "top / bottom (transcript focus)"),
        ("Ctrl+R", "toggle reasoning panel"),
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
