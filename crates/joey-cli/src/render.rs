//! Terminal rendering: streaming output with the dim `┌─ Reasoning` box
//! (cli.py:5651-5697), tool-completion lines honoring `display.tool_progress`
//! (cli.py:10652-10761), the welcome banner (banner.py:580+), and the exit
//! outro (cli.py:12690-12727).
//!
//! Crush-inspired visual style: CharmTone Pantera theme, gradient text,
//! spec 004 claude-code-style animations.
//!
//! ## Animation architecture (spec 004)
//!
//! `render_turn` uses a single `tokio::select!` tick loop (FR-010) that
//! multiplexes agent events with animation repaints at a configurable frame
//! rate (default 12 fps; see `display.animation_fps` in config). When
//! animations are disabled (`--quiet` or non-interactive stdout), the tick arm
//! is inert and behavior is identical to plain-text streaming.
//!
//! Capability detection (`capability::RenderCapability`) runs once at startup
//! and classifies the terminal as Full / Reduced / NonInteractive. Each
//! animation kind (banner, spinner, caret, tool line, prompt) has a profile
//! per capability level, registered in `profile.rs`.
//!
//! Key behaviors:
//! - **Banner** (`banner_animated`): gradient wipe-in entrance on Full; static
//!   on Reduced/NonInteractive.
//! - **Spinner**: braille-dots spinner on `ApiCallStart`, finalized on first
//!   `ContentDelta` or `ToolStart`.
//! - **Streaming**: raw token-by-token reveal, then a single markdown reflow
//!   on `Done` (interactive only).
//! - **Usage summary**: printed below the finalized text via `println!` —
//!   never overwrites the streamed region (FR-005).
//! diagonal field decorations, and semantic color tokens.

use std::io::Write;
use std::time::Instant;

use joey_agent_core::AgentEvent;
use joey_core::branding;
use joey_core::theme::{self, Theme};
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;

// ── Theme accessor ─────────────────────────────────────────────────────────

/// Lazy singleton for the active theme.
fn theme() -> &'static Theme {
    use std::sync::OnceLock;
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(Theme::pantera)
}

// ---------------------------------------------------------------------------
// Basic styled prints (now with CharmTone colors)
// ---------------------------------------------------------------------------

pub fn info(msg: &str) {
    println!("{}", theme().fg_more_subtle.ansi().paint(msg));
}

pub fn error(msg: &str) {
    eprintln!("{}", theme().error.ansi().paint(format!("error: {}", msg)));
}

pub fn warning(msg: &str) {
    eprintln!("{}", theme().warning.ansi().paint(format!("⚠ {}", msg)));
}

pub fn success(msg: &str) {
    println!("{}", theme().success.ansi().paint(msg));
}

pub fn check_ok(text: &str, detail: &str) {
    let t = theme();
    let d = if detail.is_empty() {
        String::new()
    } else {
        format!(" {}", t.fg_more_subtle.ansi().paint(detail))
    };
    println!("  {} {}{}", t.success.ansi().paint("✓"), text, d);
}

pub fn check_warn(text: &str, detail: &str) {
    let t = theme();
    let d = if detail.is_empty() {
        String::new()
    } else {
        format!(" {}", t.fg_more_subtle.ansi().paint(detail))
    };
    println!("  {} {}{}", t.warning.ansi().paint("⚠"), text, d);
}

pub fn check_fail(text: &str, detail: &str) {
    let t = theme();
    let d = if detail.is_empty() {
        String::new()
    } else {
        format!(" {}", t.fg_more_subtle.ansi().paint(detail))
    };
    println!("  {} {}{}", t.error.ansi().paint("✗"), text, d);
}

pub fn check_info(text: &str) {
    println!("    {} {}", theme().info.ansi().paint("→"), text);
}

/// A `◆ Section` banner with gradient (doctor.py:192-196 `_section`).
pub fn section(title: &str) {
    println!();
    let t = theme();
    let header = format!("◆ {}", title);
    let gradient = theme::gradient_fg_bold(&header, t.info, t.secondary, true);
    println!("{}", gradient);
}

/// A boxed header with gradient border.
pub fn boxed_header(title: &str) {
    let t = theme();
    let inner_width = 57usize;
    let border = theme::gradient_diagonal_field(inner_width + 2, t.info, t.primary);
    println!("{}", border);
    let pad_total = inner_width.saturating_sub(UnicodeWidthStr::width(title));
    let left = pad_total / 2;
    let right = pad_total - left;
    let inner = format!("│{}{}{}│", " ".repeat(left), title, " ".repeat(right));
    println!("{}", t.info.ansi().paint(inner));
    println!("{}", border);
}

// ---------------------------------------------------------------------------
// Streaming turn renderer
// ---------------------------------------------------------------------------

/// Render options resolved from config + CLI flags for one session.
#[derive(Clone)]
pub struct RenderOptions {
    /// Gate the live reasoning box (`display.show_reasoning`).
    pub show_reasoning: bool,
    /// `display.tool_progress`: off | new | all | verbose.
    pub tool_progress: String,
    /// Quiet mode (-Q): only the final response is printed.
    pub quiet: bool,
    /// Master animation gate; false when `RenderCapability::NonInteractive`
    /// or user-disabled. Default true when interactive (FR-011).
    pub animations_enabled: bool,
    /// Override for tick rate (default 12). Read from config
    /// `display.animation_fps` when present.
    pub animation_fps: u32,
    /// Detected capability profile, resolved once at REPL startup.
    pub capability: crate::capability::RenderCapability,
}

impl Default for RenderOptions {
    fn default() -> Self {
        let capability = crate::capability::RenderCapability::detect();
        let level = capability.level();
        Self {
            show_reasoning: true,
            tool_progress: "all".to_string(),
            quiet: false,
            animations_enabled: !matches!(level, crate::capability::Capability::NonInteractive),
            animation_fps: capability.target_fps.max(12),
            capability,
        }
    }
}

fn term_width() -> usize {
    terminal_size::terminal_size()
        .map(|(terminal_size::Width(w), _)| w as usize)
        .unwrap_or(80)
}

fn box_width() -> usize {
    term_width().clamp(20, 80)
}

/// T044: Count the visual lines a delta string will occupy, accounting for
/// terminal width wrapping. Uses `unicode-width` for accurate glyph widths.
/// Each `\n` starts a new line; long lines without `\n` wrap across multiple
/// visual rows.
fn count_visual_lines(text: &str, width: usize) -> u32 {
    if width == 0 {
        return 1;
    }
    let mut lines: u32 = 0;
    for line in text.split('\n') {
        let line_w: usize = unicode_width::UnicodeWidthStr::width(line);
        if line_w == 0 {
            lines += 1;
        } else {
            lines += ((line_w + width - 1) / width) as u32;
        }
    }
    // split('\n') on "a\nb" gives ["a", "b"] = 2 visual lines, but the \n
    // only created one line break. Subtract 1 if we had any newlines.
    // Actually: "a\nb" IS 2 lines. "a\n" gives ["a", ""] = 2 (correct: a + blank).
    // This is the right count.
    lines.max(1)
}

/// Guard: if line count somehow overflows, skip caret rendering.
fn streamed_line_count_overflow(count: &u32) -> bool {
    *count > 5000
}

/// Consume agent events and render them live. Returns the final text.
pub async fn render_turn(mut rx: mpsc::UnboundedReceiver<AgentEvent>, opts: RenderOptions) -> String {
    let mut final_text = String::new();
    let mut streamed_any = false;
    let mut streamed_line_count: u32 = 0;
    let mut reasoning_open = false;
    let mut reasoning_buf = String::new();
    let mut last_tool_line: Option<String> = None;
    let mut total_prompt_tokens: u64 = 0;
    let mut total_completion_tokens: u64 = 0;

    // ── Animation state (FR-010: single interruptible tick loop) ──
    // When `!animations_enabled || !capability.is_interactive`, the tick arm
    // below is inert and behavior is identical to the pre-feature plain-text
    // path. When animations are on, a `tokio::time::interval` drives repaints
    // of any live `AnimationState` instances.
    let animations_on = opts.animations_enabled && opts.capability.is_interactive;
    let tick_interval_ms = if animations_on {
        (1000u64 / opts.animation_fps.max(1) as u64).max(20)
    } else {
        // Use a long (effectively dormant) interval when animations are off.
        // We still need *some* timer for `select!` to compile; it never fires
        // before an event arrives in practice because the recv arm dominates.
        3_600_000
    };
    let mut tick_rx = tokio::time::interval(std::time::Duration::from_millis(tick_interval_ms));
    // The first tick elapses immediately; consume it so we don't paint on
    // turn start before any event has arrived.
    tick_rx.tick().await;
    // Live animation states keyed by element kind (only one of each kind at a
    // time per turn; FR-010 mandates a single timer driving all elements).
    let mut spinner_state: Option<crate::animation::AnimationState> = None;
    let spinner_profile = if animations_on {
        crate::profile::AnimationProfile::for_kind(
            crate::profile::AnimationKind::ThinkingSpinner,
            opts.capability.level(),
        )
    } else {
        crate::profile::AnimationProfile::for_kind(
            crate::profile::AnimationKind::ThinkingSpinner,
            crate::capability::Capability::NonInteractive,
        )
    };

    // ── T040: Streaming caret state ──
    // While streaming text, we render a blinking caret at the cursor position
    // between deltas. The tick arm paints it; each ContentDelta erases it
    // before printing the new text.
    let caret_profile = if animations_on {
        crate::profile::AnimationProfile::for_kind(
            crate::profile::AnimationKind::StreamingCaret,
            opts.capability.level(),
        )
    } else {
        crate::profile::AnimationProfile::for_kind(
            crate::profile::AnimationKind::StreamingCaret,
            crate::capability::Capability::NonInteractive,
        )
    };
    let mut caret_active: bool = false;      // are we in streaming mode?
    let mut caret_visible: bool = false;     // is the caret glyph currently on screen?

    // ── T041: Per-tool animation state ──
    // When a tool starts, we capture the cursor row and start a ToolLine
    // spinner. The tick arm repaints it. When the tool ends, we rewrite the
    // SAME line in place with the resolved icon + summary.
    let tool_profile = if animations_on {
        crate::profile::AnimationProfile::for_kind(
            crate::profile::AnimationKind::ToolLine,
            opts.capability.level(),
        )
    } else {
        crate::profile::AnimationProfile::for_kind(
            crate::profile::AnimationKind::ToolLine,
            crate::capability::Capability::NonInteractive,
        )
    };
    // Active tool spinner: (row, AnimationState, name, summary). Only one tool
    // at a time because the agent loop is sequential. The summary is retained
    // so the running repaint (T046) can keep the name + summary visible.
    let mut active_tool: Option<(u16, crate::animation::AnimationState, String, String)> = None;

    // ── T042/T047: Persistent in-flight usage indicator ──
    // While a turn is in progress, the accumulated token counts are surfaced
    // inline on the spinner line (see `usage_suffix`). The turn-complete
    // summary on `Done` carries the final totals + duration.
    let mut turn_in_progress: bool = false;

    // ── T045: monotonic tick counter, incremented once per tick-arm firing,
    //    drives the streaming caret blink via `animation::tick_phase`. ──
    let mut tick_count: u64 = 0;

    // ── T048: turn-start timestamp, captured on TurnStart, used to compute
    //    the turn duration reported in the turn-complete summary (US5/AC2). ──
    let mut turn_start: Option<Instant> = None;

    let t = theme();

    // T047: builds the "· N in · M out" suffix (Pantera-colored) appended to
    // the spinner line to reflect in-flight usage while the agent works.
    // Returns empty when no tokens have accumulated yet.
    let usage_suffix = |prompt: u64, completion: u64| -> String {
        if prompt + completion == 0 {
            String::new()
        } else {
            format!(
                " · {} in · {} out",
                t.fg_most_subtle.ansi().paint(format_tokens(prompt)),
                t.fg_most_subtle.ansi().paint(format_tokens(completion)),
            )
        }
    };
    let close_reasoning = |open: &mut bool, buf: &mut String| {
        if *open {
            if !buf.is_empty() {
                println!("{}", t.fg_more_subtle.ansi().paint(buf.as_str()));
                buf.clear();
            }
            let w = box_width();
            let border = theme::gradient_diagonal_field(w.saturating_sub(2), t.info_most_subtle, t.fg_most_subtle);
            println!("{}", border);
            *open = false;
        }
    };

    loop {
        tokio::select! {
            // ── Primary arm: agent events ──
            maybe_ev = rx.recv() => {
                let Some(ev) = maybe_ev else { break; };
                match ev {
            AgentEvent::TurnStart { max_iterations } => {
                turn_start.get_or_insert_with(Instant::now);
                if !opts.quiet {
                    let label = format!("◆ Turn started (max {} iterations)", max_iterations);
                    let gradient = theme::gradient_fg_bold(&label, t.primary, t.secondary, true);
                    println!("{}", gradient);
                }
            }
            AgentEvent::IterationStart { iteration: it, max_iterations } => {
                if !opts.quiet {
                    let label = format!("[{}/{}]", it, max_iterations);
                    let colored = theme::gradient_fg(&label, t.primary, t.secondary);
                    print!("{} ", colored);
                    let _ = std::io::stdout().flush();
                }
            }
            AgentEvent::ApiCallStart => {
                if !opts.quiet {
                    turn_in_progress = true;
                    if animations_on {
                        // US2: start the thinking spinner. The tick arm repaints
                        // it each frame. It is finalized when the first token
                        // or tool event arrives.
                        spinner_state = Some(crate::animation::AnimationState::new(
                            crate::profile::AnimationKind::ThinkingSpinner,
                            std::time::Instant::now(),
                        ));
                        if let Some(s) = spinner_state.as_mut() {
                            s.running = true;
                        }
                        // Print the initial spinner frame immediately.
                        let profile = spinner_profile;
                        let frame = profile.frames[0];
                        let color = (profile.color)(&t);
                        let mut spinner_line = format!("\r{}", color.ansi().paint(frame));
                        if let Some(label) = profile.label {
                            spinner_line.push(' ');
                            spinner_line.push_str(&t.fg_more_subtle.ansi().paint(label).to_string());
                        }
                        // T047: reflect in-flight usage on the spinner line.
                        spinner_line.push_str(&usage_suffix(total_prompt_tokens, total_completion_tokens));
                        print!("{}", spinner_line);
                        let _ = std::io::stdout().flush();
                    } else {
                        let spinner_label = t.fg_more_subtle.ansi().paint("⟳ querying model...");
                        println!("{}", spinner_label);
                    }
                }
            }
            AgentEvent::ApiCallEnd { usage } => {
                total_prompt_tokens += usage.prompt_tokens;
                total_completion_tokens += usage.completion_tokens;
                if !opts.quiet && (usage.prompt_tokens > 0 || usage.completion_tokens > 0) {
                    let stats = format!(
                        "  {} {} in · {} out",
                        t.fg_most_subtle.ansi().paint("↪"),
                        t.fg_more_subtle.ansi().paint(format_tokens(usage.prompt_tokens)),
                        t.fg_more_subtle.ansi().paint(format_tokens(usage.completion_tokens)),
                    );
                    println!("{}", stats);
                }
            }
            AgentEvent::ReasoningDelta(d) => {
                if opts.quiet || !opts.show_reasoning {
                    continue;
                }
                if !reasoning_open {
                    reasoning_open = true;
                    let w = box_width();
                    let label = " Reasoning ";
                    let fill = w.saturating_sub(2 + label.len());
                    let label_styled = t.info.ansi().paint(label).to_string();
                    let fill_styled = theme::gradient_fg(
                        &"─".repeat(fill.saturating_sub(1)),
                        t.info_most_subtle,
                        t.fg_most_subtle,
                    );
                    println!("\n{}{}", t.fg_more_subtle.ansi().paint("┌"), label_styled);
                    println!("{}", fill_styled);
                }
                reasoning_buf.push_str(&d);
                while let Some(pos) = reasoning_buf.find('\n') {
                    let line: String = reasoning_buf.drain(..=pos).collect();
                    println!("{}", t.fg_more_subtle.ansi().paint(line.trim_end_matches('\n')));
                }
                if reasoning_buf.len() > 80 {
                    println!("{}", t.fg_more_subtle.ansi().paint(reasoning_buf.as_str()));
                    reasoning_buf.clear();
                }
                let _ = std::io::stdout().flush();
            }
            AgentEvent::ContentDelta(d) => {
                if opts.quiet {
                    continue;
                }
                // US2: finalize spinner before streaming content.
                if let Some(s) = spinner_state.as_mut() {
                    if s.running {
                        s.finalize();
                        use crossterm::{cursor, execute, terminal};
                        let _ = execute!(
                            std::io::stdout(),
                            cursor::MoveToColumn(0),
                            terminal::Clear(terminal::ClearType::CurrentLine)
                        );
                    }
                }
                // T040: erase the streaming caret if it's visible.
                if caret_visible {
                    use crossterm::{cursor, execute, terminal};
                    let _ = execute!(
                        std::io::stdout(),
                        cursor::MoveLeft(1),
                        terminal::Clear(terminal::ClearType::FromCursorDown),
                    );
                    caret_visible = false;
                }
                close_reasoning(&mut reasoning_open, &mut reasoning_buf);
                print!("{}", d);
                let _ = std::io::stdout().flush();
                streamed_any = true;
                caret_active = true;
                turn_in_progress = true;
                // Track visual lines for US3 markdown reflow on Done (T044: uses
                // unicode-width-aware wrapping count, not raw \n count).
                streamed_line_count += count_visual_lines(&d, box_width());
            }
            AgentEvent::AssistantMessage(text) => {
                final_text = text;
                if !opts.quiet && !streamed_any && !final_text.is_empty() {
                    close_reasoning(&mut reasoning_open, &mut reasoning_buf);
                    println!("{}", final_text);
                }
            }
            AgentEvent::ToolStart { name, emoji, summary } => {
                if streamed_any {
                    println!();
                    streamed_any = false;
                }
                // T040: stop caret when tool output begins.
                caret_active = false;
                close_reasoning(&mut reasoning_open, &mut reasoning_buf);
                // US2: finalize spinner before tool output.
                if let Some(s) = spinner_state.as_mut() {
                    if s.running {
                        s.finalize();
                        use crossterm::{cursor, execute, terminal};
                        let _ = execute!(
                            std::io::stdout(),
                            cursor::MoveToColumn(0),
                            terminal::Clear(terminal::ClearType::CurrentLine)
                        );
                    }
                }

                if !opts.quiet && opts.tool_progress != "off" {
                    let e = if emoji.is_empty() { "⚡" } else { &emoji };
                    let name_styled = theme::gradient_fg(&name, t.info, t.accent);

                    // T041: Start per-tool spinner on the same line.
                    if animations_on {
                        // Capture the current cursor row so we can rewrite this
                        // line in place when the tool resolves.
                        use crossterm::{cursor, queue};
                        let mut stdout = std::io::stdout();
                        // Get the cursor position.
                        if let Ok(pos) = cursor::position() {
                            // Print the entry line: spinner frame + name + summary.
                            let frame = tool_profile.frames[0];
                            let color = (tool_profile.color)(&t);
                            print!("  {} {}", color.ansi().paint(frame), name_styled);
                            if !summary.is_empty() {
                                let short_summary: String = summary.chars().take(60).collect();
                                print!(" {}", t.fg_most_subtle.ansi().paint(format!("({})", short_summary)));
                            }
                            let _ = stdout.flush();
                            // Move cursor to start of line so tick arm can repaint.
                            let _ = queue!(stdout, cursor::MoveToColumn(0));
                            // The row we just printed on is pos.1 (before we print).
                            // After println, cursor is on this line. We store the row.
                            // Actually, cursor::position() gave us where we ARE now
                            // (before printing). After printing (no newline yet), we
                            // are on the same row. We'll newline AFTER storing.
                            // Store the row we're on (the tool line row).
                            let tool_row = pos.1;
                            active_tool = Some((
                                tool_row,
                                crate::animation::AnimationState::new(
                                    crate::profile::AnimationKind::ToolLine,
                                    std::time::Instant::now(),
                                ),
                                name.clone(),
                                summary.clone(),
                            ));
                            if let Some((_, state, _, _)) = active_tool.as_mut() {
                                state.running = true;
                            }
                            // Move to next line so output doesn't overwrite.
                            println!();
                        } else {
                            // Fallback: no cursor position available.
                            print!("  {} {}", e, name_styled);
                            if !summary.is_empty() {
                                let short_summary: String = summary.chars().take(60).collect();
                                print!(" {}", t.fg_most_subtle.ansi().paint(format!("({})", short_summary)));
                            }
                            println!();
                        }
                    } else {
                        // Non-animated: plain line (unchanged from pre-feature).
                        print!("  {} {}", e, name_styled);
                        if !summary.is_empty() {
                            let short_summary: String = summary.chars().take(60).collect();
                            print!(" {}", t.fg_most_subtle.ansi().paint(format!("({})", short_summary)));
                        }
                        println!();
                    }
                }
            }
            AgentEvent::ToolProgress { name, progress } => {
                if !opts.quiet && opts.tool_progress == "verbose" {
                    println!("{}", t.fg_more_subtle.ansi().paint(format!("  ┊ {} {}", name, progress)));
                }
            }
            AgentEvent::ToolEnd { name, is_error, result_preview, duration_secs } => {
                let duration = duration_secs;
                if opts.quiet || opts.tool_progress == "off" {
                    continue;
                }
                if opts.tool_progress == "new" && last_tool_line.as_deref() == Some(name.as_str()) && !is_error {
                    continue;
                }
                last_tool_line = Some(name.clone());

                let status_icon = if is_error { "✗" } else { "✓" };
                let status_color = if is_error { t.error } else { t.success };
                let name_styled = t.fg_base.ansi().paint(&name);
                let dur = fmt_duration(duration);

                let line = if is_error {
                    format!(
                        "  {} {} {}",
                        status_color.ansi().paint(status_icon),
                        name_styled,
                        t.fg_more_subtle.ansi().paint(format!("failed ({})", dur))
                    )
                } else {
                    format!(
                        "  {} {} {}",
                        status_color.ansi().paint(status_icon),
                        name_styled,
                        t.fg_more_subtle.ansi().paint(format!("({})", dur))
                    )
                };

                // T041: If we have an active tool with a captured row, rewrite
                // the entry line in place with the resolved state.
                if let Some((tool_row, state, tool_name, _summary)) = active_tool.take() {
                    if tool_name == name && animations_on {
                        use crossterm::{cursor, queue, terminal};
                        let mut stdout = std::io::stdout();
                        // Move to the tool's row, clear the line, print resolved.
                        let _ = queue!(
                            stdout,
                            cursor::MoveTo(0, tool_row),
                            terminal::Clear(terminal::ClearType::CurrentLine),
                        );
                        let _ = stdout.flush();
                        print!("{}", line);
                        let _ = stdout.flush();
                        // Cursor is now at end of resolved line. Leave it; next
                        // event will move to a new line or overwrite.
                        let _ = state; // state consumed by take()
                    } else {
                        // Tool name mismatch or animations off — print normally.
                        println!("{}", line);
                    }
                } else {
                    println!("{}", line);
                }

                // Show result preview in verbose mode.
                if !is_error && opts.tool_progress == "verbose" && !result_preview.is_empty() {
                    let preview_trimmed: String = result_preview.chars().take(120).collect();
                    println!("    {} {}", t.fg_more_subtle.ansi().paint("└"), t.fg_most_subtle.ansi().paint(&preview_trimmed));
                }
            }
            AgentEvent::Notice(msg) => {
                if !opts.quiet {
                    println!("{}", t.warning.ansi().paint(format!("  · {}", msg)));
                }
            }
            AgentEvent::RetryAttempt { attempt, max_retries, error, wait_secs } => {
                if !opts.quiet {
                    let label = format!("  ↻ Retry {}/{} in {:.1}s — {}", attempt, max_retries, wait_secs, error);
                    println!("{}", t.warning.ansi().paint(label));
                }
            }
            AgentEvent::CompressionStart { reason, approx_tokens } => {
                if !opts.quiet {
                    let label = format!("  🗜️ Compressing (~{} tokens): {}", format_tokens(approx_tokens as u64), reason);
                    println!("{}", t.info.ansi().paint(label));
                }
            }
            AgentEvent::CompressionEnd { original_msgs, new_msgs } => {
                if !opts.quiet {
                    let label = format!("  ✅ Compressed {} → {} messages", original_msgs, new_msgs);
                    println!("{}", t.success_more_subtle.ansi().paint(label));
                }
            }
            AgentEvent::FallbackActivated { from_model, to_model } => {
                if !opts.quiet {
                    let label = format!("  🔄 Fallback: {} → {}", from_model, to_model);
                    println!("{}", t.warning.ansi().paint(label));
                }
            }
            AgentEvent::SubagentSpawn { goal, model, toolset_summary, depth } => {
                if !opts.quiet {
                    let indent = "  ".repeat(depth);
                    let label = format!("{}🤖 Subagent: {} ({}) [{}]", indent, goal, model, toolset_summary);
                    println!("{}", t.info.ansi().paint(label));
                }
            }
            AgentEvent::SubagentComplete { goal, success, summary_preview, token_usage, duration_secs } => {
                if !opts.quiet {
                    let status = if success { "✓" } else { "✗" };
                    let label = format!("  {} {} ({} tok, {:.1}s): {}", status, goal, token_usage.total_tokens, duration_secs, summary_preview);
                    println!("{}", t.success_more_subtle.ansi().paint(label));
                }
            }
            AgentEvent::SubagentFailed { goal, error, duration_secs } => {
                if !opts.quiet {
                    let label = format!("  ✗ {} ({:.1}s): {}", goal, duration_secs, error);
                    println!("{}", t.error.ansi().paint(label));
                }
            }
            AgentEvent::DelegationBatchComplete { total, succeeded, failed, total_duration_secs } => {
                if !opts.quiet {
                    let label = format!("  🤖 Batch: {}/{} succeeded, {} failed ({:.1}s)", succeeded, total, failed, total_duration_secs);
                    println!("{}", t.info_more_subtle.ansi().paint(label));
                }
            }
            AgentEvent::Done { final_text: text, usage: _, iterations } => {
                close_reasoning(&mut reasoning_open, &mut reasoning_buf);

                // T040: erase the streaming caret if visible.
                if caret_visible {
                    use crossterm::{cursor, execute, terminal};
                    let _ = execute!(
                        std::io::stdout(),
                        cursor::MoveLeft(1),
                        terminal::Clear(terminal::ClearType::FromCursorDown),
                    );
                }

                // US3: markdown reflow. If we streamed raw text and the final
                // text contains markdown, clear the streamed region and re-print
                // it once as formatted markdown. Only when interactive (cursor
                // control is required). NonInteractive keeps the raw stream.
                if streamed_any && animations_on && !text.is_empty() {
                    use crossterm::{cursor, queue, terminal};
                    let mut stdout = std::io::stdout();
                    // Move up and clear each streamed line (+1 for the current line).
                    let lines_to_clear = streamed_line_count + 1;
                    for _ in 0..lines_to_clear {
                        let _ = queue!(
                            stdout,
                            terminal::Clear(terminal::ClearType::CurrentLine),
                            cursor::MoveUp(1),
                        );
                    }
                    let _ = queue!(stdout,
                        terminal::Clear(terminal::ClearType::CurrentLine),
                        cursor::MoveToColumn(0),
                    );
                    let _ = stdout.flush();
                    // Re-render as markdown.
                    let rendered = crate::markdown::markdown_to_ansi(&text, &t);
                    print!("{}", rendered);
                    let _ = std::io::stdout().flush();
                    println!();
                } else if streamed_any {
                    println!();
                }

                if !text.is_empty() {
                    final_text = text;
                }
                // US5: Turn summary — printed on its own line below the
                // finalized text. Uses plain println! (not cursor control),
                // so it CANNOT overwrite streamed text. Includes turn duration
                // (T048/US5-AC2) sourced from `turn_start`.
                if !opts.quiet && iterations > 0 {
                    println!();
                    let dur_suffix = match turn_start {
                        Some(ts) => format!(
                            " · {}",
                            t.fg_subtle.ansi().paint(fmt_duration(ts.elapsed().as_secs_f64()))
                        ),
                        None => String::new(),
                    };
                    let summary = format!(
                        "  {} {} iteration{} · {} in · {} out{}",
                        t.fg_most_subtle.ansi().paint("⟶"),
                        t.fg_subtle.ansi().paint(format!("{}", iterations)),
                        if iterations == 1 { "" } else { "s" },
                        t.fg_subtle.ansi().paint(format_tokens(total_prompt_tokens)),
                        t.fg_subtle.ansi().paint(format_tokens(total_completion_tokens)),
                        dur_suffix,
                    );
                    println!("{}", summary);
                }
                break;
            }
            AgentEvent::Failed(err) => {
                close_reasoning(&mut reasoning_open, &mut reasoning_buf);
                if streamed_any {
                    println!();
                }
                println!("{}", t.error.ansi().paint(format!("Error: {}", err)));
                break;
            }
            // ── OMO orchestration events (additive) ──
            AgentEvent::AgentModeChanged { agent_name, model: _ } => {
                println!("{} agent → {}",
                    t.fg_subtle.ansi().paint("◆"),
                    t.fg_base.ansi().paint(&agent_name));
            }
            AgentEvent::CategoryDelegation { category, model } => {
                println!("{} [{}] → {}",
                    t.fg_subtle.ansi().paint("◇"),
                    category, model);
            }
            AgentEvent::BoulderWorkStarted { plan_name, work_id: _ } => {
                println!("{} started work: {}",
                    t.success.ansi().paint("▶"),
                    plan_name);
            }
            AgentEvent::BoulderWorkResumed { plan_name, work_id: _ } => {
                println!("{} resumed work: {}",
                    t.fg_subtle.ansi().paint("↻"),
                    plan_name);
            }
            AgentEvent::BoulderWorkCompleted { plan_name, work_id: _ } => {
                println!("{} completed: {}",
                    t.success.ansi().paint("✓"),
                    plan_name);
            }
            AgentEvent::GoalSet { objective } => {
                println!("{} goal set: {}",
                    t.success.ansi().paint("◎"),
                    objective);
            }
            AgentEvent::GoalCleared => {
                println!("{} goal cleared", t.fg_subtle.ansi().paint("○"));
            }
            AgentEvent::WisdomAccumulated { learnings_count } => {
                println!("{} {} learnings accumulated",
                    t.fg_subtle.ansi().paint("✦"),
                    learnings_count);
            }
                }
            }
            // ── Tick arm: advance live animations (FR-010) ──
            // When animations are off this arm is inert (no spinner state
            // is ever created). When on, it repaints the thinking spinner
            // while awaiting the first token, the streaming caret between
            // deltas, the per-tool spinner, and the persistent usage line.
            _ = tick_rx.tick(), if animations_on => {
                use crossterm::{cursor, execute, queue, terminal};
                tick_count = tick_count.wrapping_add(1);

                // ── Spinner: while awaiting first token ──
                if let Some(state) = spinner_state.as_mut() {
                    if state.running {
                        state.advance(spinner_profile);
                        let _ = execute!(
                            std::io::stdout(),
                            cursor::MoveToColumn(0),
                            terminal::Clear(terminal::ClearType::CurrentLine)
                        );
                        let frame = state.current_frame(spinner_profile);
                        let color = (spinner_profile.color)(&t);
                        let glyph = color.ansi().paint(frame);
                        if let Some(label) = spinner_profile.label {
                            print!("{} {}", glyph, t.fg_more_subtle.ansi().paint(label));
                        } else {
                            print!("{}", glyph);
                        }
                        // T047: refresh in-flight usage on the spinner line so
                        // accumulated tokens stay visible while the agent
                        // works (e.g. across agentic iterations).
                        print!("{}", usage_suffix(total_prompt_tokens, total_completion_tokens));
                        let _ = std::io::stdout().flush();
                    }
                }

                // ── T040: Streaming caret between deltas ──
                // When we're in streaming mode (caret_active) and no caret
                // is currently visible, paint one. This gives the impression
                // of a blinking caret at the end of the streamed text while
                // waiting for the next token.
                if caret_active && !caret_visible && !streamed_line_count_overflow(&streamed_line_count) {
                    let frame = caret_profile.frames
                        [usize::from(crate::animation::tick_phase(caret_profile, tick_count))];
                    let color = (caret_profile.color)(&t);
                    let _ = execute!(
                        std::io::stdout(),
                        cursor::SavePosition,
                    );
                    print!("{}", color.ansi().paint(frame));
                    let _ = std::io::stdout().flush();
                    caret_visible = true;
                }

                // ── T041/T046: Per-tool spinner ──
                // Repaint the running spinner ON the captured row, keeping the
                // tool name (+ summary) visible while the tool executes.
                if let Some((tool_row, state, tool_name, tool_summary)) = active_tool.as_mut() {
                    if state.running {
                        state.advance(tool_profile);
                        let row = *tool_row;
                        let _ = queue!(
                            std::io::stdout(),
                            cursor::MoveTo(0, row),
                            terminal::Clear(terminal::ClearType::CurrentLine),
                        );
                        let frame = state.current_frame(tool_profile);
                        let color = (tool_profile.color)(&t);
                        let _ = execute!(
                            std::io::stdout(),
                            cursor::MoveTo(0, row),
                            terminal::Clear(terminal::ClearType::CurrentLine)
                        );
                        let name_styled = theme::gradient_fg(tool_name.as_str(), t.info, t.accent);
                        print!("  {} {}", color.ansi().paint(frame), name_styled);
                        if !tool_summary.is_empty() {
                            let short: String = tool_summary.chars().take(60).collect();
                            print!(" {}", t.fg_most_subtle.ansi().paint(format!("({})", short)));
                        }
                        let _ = std::io::stdout().flush();
                    }
                }

                // ── T042/T047: in-flight usage is rendered on the spinner
                //    line above (see `usage_suffix` in the spinner repaint and
                //    the ApiCallStart initial print). A separate persistent
                //    row was rejected because, on an append-only line-based
                //    CLI, anchoring a row below the streaming region without
                //    clobbering streamed text is fragile (Constitution VII).
                //    The turn-complete summary on `Done` carries the final
                //    totals + duration. `turn_in_progress` is still tracked so
                //    the spinner knows when to show usage. ──
                let _ = turn_in_progress;
            }
        }
    }
    final_text
}

/// Format token counts with K/M suffixes for compact display.
fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn fmt_duration(secs: f64) -> String {
    if secs >= 60.0 {
        format!("{}m {:.0}s", (secs / 60.0) as u64, secs % 60.0)
    } else if secs >= 10.0 {
        format!("{:.0}s", secs)
    } else {
        format!("{:.1}s", secs)
    }
}

// ---------------------------------------------------------------------------
// Welcome banner — Crush-style with gradient logo, diagonal fields
// ---------------------------------------------------------------------------

pub struct BannerInfo<'a> {
    pub model: &'a str,
    pub context_length: Option<i64>,
    pub cwd: &'a str,
    pub session_id: &'a str,
    pub enabled_tools: &'a [String],
    pub yolo: bool,
}

fn format_context_length(tokens: i64) -> String {
    if tokens >= 1_000_000 {
        let val = tokens as f64 / 1_000_000.0;
        if (val - val.round()).abs() < 0.05 {
            format!("{}M", val.round() as i64)
        } else {
            format!("{:.1}M", val)
        }
    } else if tokens >= 1_000 {
        let val = tokens as f64 / 1_000.0;
        if (val - val.round()).abs() < 0.05 {
            format!("{}K", val.round() as i64)
        } else {
            format!("{:.1}K", val)
        }
    } else {
        tokens.to_string()
    }
}

/// Strip the `joey-` prefix style suffix for display (`_display_toolset_name`).
fn display_toolset_name(name: &str) -> String {
    name.strip_suffix("_tools").unwrap_or(name).to_string()
}

/// Group enabled tools by the first (sorted) leaf toolset containing them,
/// skipping platform composites (banner.py `get_toolset_for_tool` shape).
fn group_tools_by_toolset(enabled: &[String]) -> Vec<(String, Vec<String>)> {
    let mut groups: indexmap::IndexMap<String, Vec<String>> = indexmap::IndexMap::new();
    let toolsets: Vec<&str> = joey_tools::toolsets::names()
        .into_iter()
        .filter(|n| {
            !n.starts_with(branding::TOOLSET_PREFIX)
                && !matches!(*n, "all" | "coding" | "debugging" | "safe" | "search")
        })
        .collect();
    for tool in enabled {
        let mut owner: Option<&str> = None;
        for ts in &toolsets {
            if joey_tools::resolve_toolset(ts).iter().any(|t| t == tool) {
                owner = Some(ts);
                break;
            }
        }
        groups
            .entry(display_toolset_name(owner.unwrap_or("other")))
            .or_default()
            .push(tool.clone());
    }
    groups.sort_keys();
    groups.into_iter().collect()
}

/// Animated startup banner (FR-001, US1). Wraps the existing static `banner`
/// with a polished entrance animation that evokes claude-code's startup feel
/// while using Joey's own glyphs/branding and Crush/Pantera colors.
///
/// - Full capability: gradient wipe-in of the logo line, then the full static
///   banner. Bounded to ~900ms worst case; never blocks the prompt indefinitely.
/// - Reduced capability: print the static banner directly (no animation).
/// - NonInteractive: plain text only (delegate to `banner`; it emits no cursor
///   escapes).
pub fn banner_animated(info: &BannerInfo, opts: &RenderOptions) {
    use std::thread;
    use std::time::Duration;

    let level = opts.capability.level();
    match level {
        crate::capability::Capability::Full if opts.animations_enabled => {
            // Gradient wipe-in: print a brief entrance shimmer before the
            // full banner. 6 frames at ~80ms = ~480ms, well under the ~1.5s
            // budget.
            let t = theme();
            let profile = crate::profile::AnimationProfile::for_kind(
                crate::profile::AnimationKind::Banner,
                crate::capability::Capability::Full,
            );
            let width = box_width().max(40);
            // Print a gradient shimmer line that cycles a few frames.
            for i in 0..profile.frames.len().min(6) {
                let frame = profile.frames[i % profile.frames.len()];
                let color = (profile.color)(&t);
                let pad = width.saturating_sub(frame.chars().count() + 6);
                let pad_left = pad / 2;
                let line = format!("{}{}{}", " ".repeat(pad_left), color.ansi().paint(frame), "");
                use crossterm::{cursor, execute, terminal};
                let _ = execute!(
                    std::io::stdout(),
                    cursor::MoveToColumn(0),
                    terminal::Clear(terminal::ClearType::CurrentLine)
                );
                print!("\r{}", line);
                let _ = std::io::stdout().flush();
                thread::sleep(Duration::from_millis(80));
            }
            // Clear the shimmer line and print the full static banner.
            use crossterm::{cursor, execute, terminal};
            let _ = execute!(
                std::io::stdout(),
                cursor::MoveToColumn(0),
                terminal::Clear(terminal::ClearType::CurrentLine)
            );
            banner(info);
        }
        _ => {
            // Reduced or NonInteractive: no animation, just the static banner.
            banner(info);
        }
    }
}

pub fn banner(info: &BannerInfo) {
    let t = theme();
    let width = box_width().max(40);
    let inner = width - 2;

    // ── Logo: gradient wordmark + diagonal field (Crush-style) ──
    let logo_name = format!("{} v{}", branding::AGENT_NAME, branding::VERSION);
    let logo_line = theme::gradient_fg_bold(&logo_name, t.primary, t.secondary, true);

    let field_width = inner.saturating_sub(strip_ansi_width(&logo_line)).max(3);
    let field = theme::gradient_diagonal_field(field_width, t.fg_most_subtle, t.bg_less_visible);

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("{} {}", logo_line, field));
    lines.push(
        t.fg_more_subtle
            .ansi()
            .paint("· based on Hermes Agent by Nous Research")
            .to_string(),
    );
    lines.push(String::new());

    // ── Model line with accent gradient ──
    let model_short = info.model.rsplit('/').next().unwrap_or(info.model);
    let model_short: String = if model_short.chars().count() > 28 {
        format!("{}...", model_short.chars().take(25).collect::<String>())
    } else {
        model_short.to_string()
    };
    let ctx = info
        .context_length
        .filter(|n| *n > 0)
        .map(|n| format!(" · {} context", format_context_length(n)))
        .unwrap_or_default();
    lines.push(format!(
        "{}{}",
        theme::gradient_fg(
            if model_short.is_empty() {
                "(no model configured)"
            } else {
                &model_short
            },
            t.accent,
            t.info,
        ),
        t.fg_more_subtle.ansi().paint(ctx)
    ));

    if info.yolo {
        lines.push(format!(
            "{} {}",
            t.error.ansi().paint("⚠ YOLO mode"),
            t.fg_more_subtle.ansi().paint("— all approval prompts bypassed")
        ));
    }
    lines.push(t.fg_more_subtle.ansi().paint(info.cwd).to_string());
    lines.push(
        t.fg_more_subtle
            .ansi()
            .paint(format!("Session: {}", info.session_id))
            .to_string(),
    );
    lines.push(String::new());

    // ── Available Tools section ──
    lines.push(theme::gradient_fg_bold("Available Tools", t.primary, t.accent, true));
    let groups = group_tools_by_toolset(info.enabled_tools);
    let shown = groups.len().min(8);
    for (ts, tools) in groups.iter().take(8) {
        let mut names = tools.clone();
        names.sort();
        let mut joined = names.join(", ");
        if joined.len() > 45 {
            let mut short: Vec<String> = Vec::new();
            let mut len = 0usize;
            for n in &names {
                if len + n.len() + 2 > 42 {
                    short.push("...".to_string());
                    break;
                }
                len += n.len() + 2;
                short.push(n.clone());
            }
            joined = short.join(", ");
        }
        lines.push(format!("{} {}", t.fg_subtle.ansi().paint(format!("{}:", ts)), joined));
    }
    if groups.len() > shown {
        lines.push(
            t.fg_more_subtle
                .ansi()
                .paint(format!("(and {} more toolsets...)", groups.len() - shown))
                .to_string(),
        );
    }
    lines.push(String::new());

    // ── Tips section ──
    lines.push(theme::gradient_fg_bold("Tips", t.secondary, t.warning, true));
    lines.push(t.fg_more_subtle.ansi().paint("• /help for commands · /quit to exit").to_string());
    lines.push(
        t.fg_more_subtle
            .ansi()
            .paint("• Ctrl-C interrupts a running turn (press twice to force exit)")
            .to_string(),
    );
    lines.push(
        t.fg_more_subtle
            .ansi()
            .paint(format!("• {} -z \"...\" answers one-shot questions for scripts", branding::CLI_NAME))
            .to_string(),
    );

    // ── Panel with gradient top/bottom borders ──
    let top_border = theme::gradient_diagonal_field(inner, t.primary, t.secondary);
    let bot_border = theme::gradient_diagonal_field(inner, t.secondary, t.primary);
    println!("{}", top_border);
    for line in lines {
        let visible = strip_ansi_width(&line);
        let pad = inner.saturating_sub(visible + 2);
        println!("{} {}{} {}", t.fg_most_subtle.ansi().paint("│"), line, " ".repeat(pad), t.fg_most_subtle.ansi().paint("│"));
    }
    println!("{}", bot_border);
}

/// Display width of a string ignoring ANSI escape sequences.
fn strip_ansi_width(s: &str) -> usize {
    let mut plain = String::new();
    let mut in_escape = false;
    for ch in s.chars() {
        if in_escape {
            if ch == 'm' {
                in_escape = false;
            }
            continue;
        }
        if ch == '\u{1b}' {
            in_escape = true;
            continue;
        }
        plain.push(ch);
    }
    UnicodeWidthStr::width(plain.as_str())
}

// ---------------------------------------------------------------------------
// Exit outro (cli.py:12690-12727)
// ---------------------------------------------------------------------------

pub struct OutroInfo<'a> {
    pub session_id: &'a str,
    pub title: Option<String>,
    pub message_count: usize,
    pub user_messages: usize,
    pub tool_calls: usize,
    pub started: Instant,
    pub profile: &'a str,
}

pub fn exit_outro(info: &OutroInfo) {
    let t = theme();
    println!();
    if info.message_count > 0 {
        let elapsed = info.started.elapsed().as_secs();
        let (hours, rem) = (elapsed / 3600, elapsed % 3600);
        let (minutes, seconds) = (rem / 60, rem % 60);
        let duration_str = if hours > 0 {
            format!("{}h {}m {}s", hours, minutes, seconds)
        } else if minutes > 0 {
            format!("{}m {}s", minutes, seconds)
        } else {
            format!("{}s", seconds)
        };
        let profile_flag = if info.profile == "default" || info.profile == "custom" {
            String::new()
        } else {
            format!(" -p {}", info.profile)
        };

        // Gradient separator.
        let sep_width = 40usize;
        let sep = theme::gradient_diagonal_field(sep_width, t.primary, t.secondary);
        println!("{}", sep);
        println!("{}", t.fg_base.ansi().paint("Resume this session with:"));
        println!(
            "  {} {}",
            t.fg_more_subtle.ansi().paint("→"),
            theme::gradient_fg(
                &format!("{} --resume {}{}", branding::CLI_NAME, info.session_id, profile_flag),
                t.info,
                t.accent,
            )
        );
        if let Some(title) = &info.title {
            println!(
                "  {} {}",
                t.fg_more_subtle.ansi().paint("→"),
                theme::gradient_fg(
                    &format!("{} -c \"{}\"{}", branding::CLI_NAME, title, profile_flag),
                    t.info,
                    t.accent,
                )
            );
        }
        println!();
        println!("{} {}", t.fg_subtle.ansi().paint("Session:"), info.session_id);
        if let Some(title) = &info.title {
            println!("{} {}", t.fg_subtle.ansi().paint("Title:"), title);
        }
        println!("{} {}", t.fg_subtle.ansi().paint("Duration:"), duration_str);
        println!(
            "{} {} ({} user, {} tool calls)",
            t.fg_subtle.ansi().paint("Messages:"),
            info.message_count,
            info.user_messages,
            info.tool_calls
        );
        println!("{}", sep);
    } else {
        // Gradient farewell.
        let farewell = "Goodbye! ⚕".to_string();
        println!("{}", theme::gradient_fg(&farewell, t.primary, t.secondary));
    }
}

// ---------------------------------------------------------------------------
// Checkpoint display helpers
// ---------------------------------------------------------------------------

/// Render the checkpoint list in a Crush-styled table.
pub fn checkpoint_list(checkpoints: &[joey_tools::vcs::Checkpoint]) {
    let t = theme();
    if checkpoints.is_empty() {
        info("No checkpoints recorded yet.");
        info("Checkpoints are created automatically as you work, or via /checkpoint <message>.");
        return;
    }
    println!();
    let header = theme::gradient_fg_bold("Checkpoints", t.primary, t.secondary, true);
    println!("{}", header);
    println!(
        "  {}",
        t.fg_more_subtle.ansi().paint(format!(
            "{:<6} {:<8} {:<8} {}",
            "#", "Time", "Files", "Message"
        ))
    );
    let sep = theme::gradient_diagonal_field(60, t.fg_most_subtle, t.bg_less_visible);
    println!("  {}", sep);
    for cp in checkpoints {
        let time_short = cp.timestamp.get(..16).unwrap_or(&cp.timestamp);
        let num = theme::gradient_fg(&format!("#{}", cp.number), t.primary, t.secondary);
        println!(
            "  {} {:<8} {:<8} {}",
            num,
            time_short,
            cp.files_changed,
            t.fg_base.ansi().paint(&cp.message)
        );
    }
    println!();
    info("Revert with: /revert <number>");
}

/// Print a checkpoint creation confirmation.
pub fn checkpoint_created(number: usize, message: &str) {
    let t = theme();
    let label = format!("◆ Checkpoint #{} created", number);
    let gradient = theme::gradient_fg_bold(&label, t.success, t.accent, true);
    println!("  {} {}", gradient, t.fg_more_subtle.ansi().paint(message));
}

/// Print a revert confirmation.
pub fn checkpoint_reverted(number: usize) {
    let t = theme();
    let label = format!("◆ Reverted to checkpoint #{}", number);
    let gradient = theme::gradient_fg_bold(&label, t.info, t.accent, true);
    println!("  {}", gradient);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Capability, RenderCapability};
    use joey_agent_core::AgentEvent;
    use joey_providers::Usage;
    use tokio::sync::mpsc;

    /// Build RenderOptions with a given capability level and animations
    /// gated accordingly (mirrors the REPL startup logic in repl.rs).
    fn opts_for(cap: Capability) -> RenderOptions {
        match cap {
            Capability::NonInteractive => RenderOptions {
                show_reasoning: false,
                tool_progress: "all".to_string(),
                quiet: false,
                animations_enabled: false,
                animation_fps: 12,
                capability: RenderCapability {
                    is_interactive: false,
                    supports_truecolor: true,
                    supports_unicode: true,
                    term_width: 80,
                    target_fps: 0,
                },
            },
            Capability::Reduced => RenderOptions {
                show_reasoning: false,
                tool_progress: "all".to_string(),
                quiet: false,
                animations_enabled: true,
                animation_fps: 8,
                capability: RenderCapability {
                    is_interactive: true,
                    supports_truecolor: false,
                    supports_unicode: false,
                    term_width: 50,
                    target_fps: 8,
                },
            },
            Capability::Full => RenderOptions {
                show_reasoning: false,
                tool_progress: "all".to_string(),
                quiet: false,
                animations_enabled: true,
                animation_fps: 12,
                capability: RenderCapability {
                    is_interactive: true,
                    supports_truecolor: true,
                    supports_unicode: true,
                    term_width: 80,
                    target_fps: 12,
                },
            },
        }
    }

    /// Push a representative turn event stream through `render_turn` and
    /// return the final text. Used by the fallback / regression tests below.
    async fn run_synthetic_turn(opts: RenderOptions) -> String {
        let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
        // Minimal event sequence: TurnStart → ContentDelta → Done.
        let _ = tx.send(AgentEvent::TurnStart { max_iterations: 1 });
        let _ = tx.send(AgentEvent::ContentDelta("Hello world.".to_string()));
        let _ = tx.send(AgentEvent::Done {
            final_text: "Hello world.".to_string(),
            usage: Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                ..Default::default()
            },
            iterations: 1,
        });
        drop(tx);
        render_turn(rx, opts).await
    }

    // ── T011: plain-text fallback (NonInteractive) ──
    #[tokio::test]
    async fn noninteractive_returns_final_text_without_hang() {
        let opts = opts_for(Capability::NonInteractive);
        let text = run_synthetic_turn(opts).await;
        assert_eq!(text, "Hello world.");
    }

    // ── T011a: Reduced-capability path completes and returns final text ──
    #[tokio::test]
    async fn reduced_capability_completes_without_hang() {
        let opts = opts_for(Capability::Reduced);
        let text = run_synthetic_turn(opts).await;
        assert_eq!(text, "Hello world.");
    }

    // ── T035a: regression — the returned final_text is preserved across all
    //    capability levels (Constitution VII: non-regression of the public
    //    `render_turn` contract). ──
    #[tokio::test]
    async fn regression_final_text_identical_across_capabilities() {
        for cap in [Capability::NonInteractive, Capability::Reduced, Capability::Full] {
            let opts = opts_for(cap);
            let text = run_synthetic_turn(opts).await;
            assert_eq!(
                text, "Hello world.",
                "final_text must be identical across capability levels (cap={:?})",
                cap
            );
        }
    }

    /// A turn with a tool call must still return the assistant's final text,
    /// unchanged from the pre-feature behavior (Constitution VII).
    #[tokio::test]
    async fn regression_tool_turn_returns_final_text() {
        let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
        let _ = tx.send(AgentEvent::TurnStart { max_iterations: 2 });
        let _ = tx.send(AgentEvent::ApiCallStart);
        let _ = tx.send(AgentEvent::ApiCallEnd {
            usage: Usage {
                prompt_tokens: 10,
                completion_tokens: 0,
                ..Default::default()
            },
        });
        let _ = tx.send(AgentEvent::ToolStart {
            name: "read_file".to_string(),
            emoji: "📖".to_string(),
            summary: "reading file".to_string(),
        });
        let _ = tx.send(AgentEvent::ToolEnd {
            name: "read_file".to_string(),
            is_error: false,
            result_preview: "file contents".to_string(),
            duration_secs: 0.1,
        });
        let _ = tx.send(AgentEvent::ContentDelta("Done.".to_string()));
        let _ = tx.send(AgentEvent::Done {
            final_text: "Done.".to_string(),
            usage: Usage {
                prompt_tokens: 20,
                completion_tokens: 1,
                ..Default::default()
            },
            iterations: 2,
        });
        drop(tx);
        let opts = opts_for(Capability::NonInteractive);
        let text = render_turn(rx, opts).await;
        assert_eq!(text, "Done.");
    }

    // ── T012: banner_animated under NonInteractive delegates to plain banner ──
    // The NonInteractive path must not emit cursor-control escapes and must
    // not hang. We construct a minimal BannerInfo and verify it completes.
    #[test]
    fn banner_animated_noninteractive_does_not_hang() {
        let opts = opts_for(Capability::NonInteractive);
        let info = BannerInfo {
            model: "test-model",
            context_length: Some(8000),
            cwd: "/tmp",
            session_id: "test-session",
            enabled_tools: &[],
            yolo: false,
        };
        // Must complete without hanging and without emitting cursor escapes
        // (NonInteractive delegates to the static banner which uses plain
        // println!).
        banner_animated(&info, &opts);
    }

    // ── T016: thinking spinner starts on ApiCallStart and stops on first
    //    content/tool event. Under Full capability, ApiCallStart triggers
    //    the spinner state; a subsequent ContentDelta finalizes it. ──
    #[tokio::test]
    async fn spinner_starts_on_apicall_and_stops_on_content() {
        let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
        let _ = tx.send(AgentEvent::TurnStart { max_iterations: 1 });
        let _ = tx.send(AgentEvent::ApiCallStart);
        let _ = tx.send(AgentEvent::ContentDelta("response".to_string()));
        let _ = tx.send(AgentEvent::Done {
            final_text: "response".to_string(),
            usage: Usage::default(),
            iterations: 1,
        });
        drop(tx);
        let opts = opts_for(Capability::Full);
        // Must complete without hanging (spinner finalized by ContentDelta).
        let text = render_turn(rx, opts).await;
        assert_eq!(text, "response");
    }

    // ── T027: usage summary does not overwrite streamed text. The summary
    //    is printed via plain println! below the finalized text. We verify
    //    the turn completes and final_text is intact. ──
    #[tokio::test]
    async fn usage_summary_does_not_overwrite_streamed_text() {
        let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
        let _ = tx.send(AgentEvent::TurnStart { max_iterations: 3 });
        let _ = tx.send(AgentEvent::ApiCallStart);
        let _ = tx.send(AgentEvent::ApiCallEnd {
            usage: Usage {
                prompt_tokens: 100,
                completion_tokens: 50,
                ..Default::default()
            },
        });
        let _ = tx.send(AgentEvent::ContentDelta("The answer is 42.".to_string()));
        let _ = tx.send(AgentEvent::Done {
            final_text: "The answer is 42.".to_string(),
            usage: Usage {
                prompt_tokens: 100,
                completion_tokens: 50,
                ..Default::default()
            },
            iterations: 3,
        });
        drop(tx);
        // NonInteractive: no cursor manipulation, summary via println!.
        let opts = opts_for(Capability::NonInteractive);
        let text = render_turn(rx, opts).await;
        assert_eq!(text, "The answer is 42.");
    }

    // ── T012b: banner_animated under Full with animations disabled also
    //    delegates to the static banner (no animation when gate is off). ──
    #[test]
    fn banner_animated_full_but_disabled_delegates_to_static() {
        let mut opts = opts_for(Capability::Full);
        opts.animations_enabled = false;
        let info = BannerInfo {
            model: "test-model",
            context_length: Some(8000),
            cwd: "/tmp",
            session_id: "test-session",
            enabled_tools: &[],
            yolo: false,
        };
        banner_animated(&info, &opts);
    }

    // ── T044: count_visual_lines accounts for terminal width wrapping. ──
    #[test]
    fn count_visual_lines_wraps_long_lines() {
        // 80-char string in a 20-col terminal = 4 visual lines.
        let text = "a".repeat(80);
        assert_eq!(count_visual_lines(&text, 20), 4);

        // Short string = 1 visual line.
        assert_eq!(count_visual_lines("hello", 80), 1);

        // Two lines separated by \n.
        assert_eq!(count_visual_lines("hello\nworld", 80), 2);

        // Empty string still counts as 1 line.
        assert_eq!(count_visual_lines("", 80), 1);

        // Multi-byte: 3 emoji (each 2 cols wide) in a 4-col terminal.
        assert_eq!(count_visual_lines("😀😀😀", 4), 2);
    }

    // ── T040: StreamingCaret profile is addressable and has non-empty
    //    frames under Full/Reduced. ──
    #[test]
    fn streaming_caret_profile_has_blinking_frames() {
        use crate::profile::AnimationKind;
        let full = crate::profile::AnimationProfile::for_kind(
            AnimationKind::StreamingCaret,
            Capability::Full,
        );
        assert!(full.frames.len() >= 2, "caret must blink (>= 2 frames)");
        let reduced = crate::profile::AnimationProfile::for_kind(
            AnimationKind::StreamingCaret,
            Capability::Reduced,
        );
        assert!(reduced.frames.len() >= 2);
        // Reduced frames must be ASCII-safe.
        for frame in reduced.frames {
            for ch in frame.chars() {
                assert!(ch.is_ascii(), "reduced caret has non-ASCII: {:?}", ch);
            }
        }
    }

    // ── T041: ToolLine profile has non-empty frames for running state. ──
    #[test]
    fn tool_line_profile_has_spinner_frames() {
        use crate::profile::AnimationKind;
        let full = crate::profile::AnimationProfile::for_kind(
            AnimationKind::ToolLine,
            Capability::Full,
        );
        assert!(!full.frames.is_empty(), "tool line must have spinner frames");
        let reduced = crate::profile::AnimationProfile::for_kind(
            AnimationKind::ToolLine,
            Capability::Reduced,
        );
        assert!(!reduced.frames.is_empty());
    }

    // ── T040/T041 integration: a turn with ContentDelta under Full
    //    capability completes without hanging (caret state machine
    //    doesn't deadlock the select! loop). ──
    #[tokio::test]
    async fn streaming_with_caret_completes_under_full() {
        let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
        let _ = tx.send(AgentEvent::TurnStart { max_iterations: 1 });
        let _ = tx.send(AgentEvent::ApiCallStart);
        let _ = tx.send(AgentEvent::ContentDelta("Hello ".to_string()));
        let _ = tx.send(AgentEvent::ContentDelta("world\n".to_string()));
        let _ = tx.send(AgentEvent::Done {
            final_text: "Hello world\n".to_string(),
            usage: Usage::default(),
            iterations: 1,
        });
        drop(tx);
        let opts = opts_for(Capability::Full);
        let text = render_turn(rx, opts).await;
        assert_eq!(text, "Hello world\n");
    }

    // ── T041 integration: a turn with ToolStart+ToolEnd under Full
    //    capability completes without hanging. ──
    #[tokio::test]
    async fn tool_turn_with_animation_completes_under_full() {
        let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
        let _ = tx.send(AgentEvent::TurnStart { max_iterations: 1 });
        let _ = tx.send(AgentEvent::ApiCallStart);
        let _ = tx.send(AgentEvent::ToolStart {
            name: "search".to_string(),
            emoji: "🔍".to_string(),
            summary: "searching docs".to_string(),
        });
        let _ = tx.send(AgentEvent::ToolEnd {
            name: "search".to_string(),
            is_error: false,
            result_preview: "found 3 results".to_string(),
            duration_secs: 0.5,
        });
        let _ = tx.send(AgentEvent::ContentDelta("Done.".to_string()));
        let _ = tx.send(AgentEvent::Done {
            final_text: "Done.".to_string(),
            usage: Usage::default(),
            iterations: 1,
        });
        drop(tx);
        let opts = opts_for(Capability::Full);
        let text = render_turn(rx, opts).await;
        assert_eq!(text, "Done.");
    }

    // ── T047/T048 integration: an agentic (multi-call) turn under Full
    //    capability completes without hanging and the in-flight usage suffix
    //    + turn duration are wired (no panic, final_text intact). ──
    #[tokio::test]
    async fn agentic_turn_with_usage_and_duration_completes_under_full() {
        let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
        let _ = tx.send(AgentEvent::TurnStart { max_iterations: 2 });
        let _ = tx.send(AgentEvent::ApiCallStart);
        let _ = tx.send(AgentEvent::ApiCallEnd {
            usage: Usage {
                prompt_tokens: 40,
                completion_tokens: 10,
                ..Default::default()
            },
        });
        let _ = tx.send(AgentEvent::ToolStart {
            name: "search".to_string(),
            emoji: "🔍".to_string(),
            summary: "looking it up".to_string(),
        });
        let _ = tx.send(AgentEvent::ToolEnd {
            name: "search".to_string(),
            is_error: false,
            result_preview: "ok".to_string(),
            duration_secs: 0.2,
        });
        let _ = tx.send(AgentEvent::ApiCallStart);
        let _ = tx.send(AgentEvent::ApiCallEnd {
            usage: Usage {
                prompt_tokens: 60,
                completion_tokens: 5,
                ..Default::default()
            },
        });
        let _ = tx.send(AgentEvent::ContentDelta("Answer.".to_string()));
        let _ = tx.send(AgentEvent::Done {
            final_text: "Answer.".to_string(),
            usage: Usage::default(),
            iterations: 2,
        });
        drop(tx);
        let opts = opts_for(Capability::Full);
        let text = render_turn(rx, opts).await;
        assert_eq!(text, "Answer.");
    }

    // ── T050: the markdown-reflow layout helper (`count_visual_lines`)
    //    adapts to terminal width changes (FR-007 / T033 — lazy width
    //    re-read chosen in research.md). It returns a pure count, so it
    //    cannot emit partial-frame escapes; this locks the width-adaptation
    //    behavior for the reflow path. ──
    #[test]
    fn reflow_line_count_adapts_to_width() {
        let text = "x".repeat(120);
        // Wide terminal: one line.
        assert_eq!(count_visual_lines(&text, 120), 1);
        // Narrower widths split across more lines, monotonically.
        assert!(count_visual_lines(&text, 80) > 1);
        assert!(count_visual_lines(&text, 40) > count_visual_lines(&text, 80));
        assert!(count_visual_lines(&text, 20) > count_visual_lines(&text, 40));
        // Narrower ⇒ at least as many visual lines (monotonic in width).
        let mut prev = count_visual_lines(&text, 120);
        for w in (1..120).rev().step_by(7) {
            let n = count_visual_lines(&text, w);
            assert!(n >= prev, "narrower width must not reduce line count: w={} n={} prev={}", w, n, prev);
            prev = n;
        }
    }
}
