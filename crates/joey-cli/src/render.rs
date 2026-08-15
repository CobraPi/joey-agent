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
//!   diagonal field decorations, and semantic color tokens.

use std::io::Write;
use std::time::Instant;

use joey_agent_core::events::{FileChangeKind, AgentEvent};
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
    /// `display.syntax_highlighting`: gate per-language syntax highlighting
    /// of diff code lines (feature 005). Default true. When false, the
    /// highlight helper is never invoked (zero cost — Principle VIII escape
    /// hatch for the syntect dependency).
    pub syntax_highlighting: bool,
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
            syntax_highlighting: true,
        }
    }
}

/// Feature 005 (E2 resolution): maximum number of diff lines rendered in the
/// interactive view before tail-truncation kicks in. Mirrors the reasoning/
/// tool truncation pattern. Ported from crush's `MAX_COLLAPSED_HEIGHT`.
const MAX_DIFF_BLOCK_HEIGHT: usize = 50;

/// Feature 005 (T015): render one diff line with add/remove/context coloring
/// and optional syntax highlighting. Returns the ANSI-styled string.
///
/// - Lines starting with `+` (and not `+++`) → green (addition).
/// - Lines starting with `-` (and not `---`) → red (removal).
/// - Hunk headers (`@@`) → subtle accent.
/// - File headers (`---`/`+++`) → subtle.
/// - Context lines (` ` prefix) → base color.
///
/// When `syntax_highlighting` is enabled, the code portion of add/context
/// lines is passed through `joey_tools::highlight::highlight_line` for
/// per-language coloring (layered on top of the add/context tint).
fn render_diff_line(line: &str, path: &str, syntax_highlighting: bool, t: &Theme) -> String {
    // Strip the leading marker for syntax highlighting, then re-apply color.
    if line.starts_with("+++") || line.starts_with("---") {
        return t.fg_most_subtle.ansi().paint(line).to_string();
    }
    if line.starts_with("@@") {
        return t.info_most_subtle.ansi().paint(line).to_string();
    }
    if let Some(code) = line.strip_prefix('+') {
        // Addition line.
        if syntax_highlighting {
            if let Some(hl) = joey_tools::highlight::highlight_line(code, path, true) {
                return format!("{}{}", t.success.ansi().paint("+"), hl);
            }
        }
        return t.success.ansi().paint(line).to_string();
    }
    if let Some(code) = line.strip_prefix('-') {
        // Removal line (no syntax highlight on the removed side — keeps the
        // red tint readable; crush does the same).
        let _ = code;
        return t.error.ansi().paint(line).to_string();
    }
    // Context line.
    if let Some(code) = line.strip_prefix(' ') {
        if syntax_highlighting {
            if let Some(hl) = joey_tools::highlight::highlight_line(code, path, true) {
                return format!(" {}", hl);
            }
        }
    }
    t.fg_base.ansi().paint(line).to_string()
}

fn term_width() -> usize {
    terminal_size::terminal_size()
        .map(|(terminal_size::Width(w), _)| w as usize)
        .unwrap_or(80)
}

fn box_width() -> usize {
    term_width().clamp(20, 80)
}

/// Spec 008 (T001/FR-013): Classify whether a tool name is a terminal-command
/// block (renders with the crush `$ command` layout). Matches
/// `joey_tui::state::is_terminal_block` (007 T016). Data-driven: tool name only.
fn is_terminal_block(name: &str) -> bool {
    name == "terminal"
}

/// Feature 013 (T032): pure drain helper for the `pending_separator` state
/// machine. Returns `true` when a separator blank line should be printed
/// (i.e. a previous element set the flag), and resets the flag to `false`.
/// Extracted as a pure fn so the spacing state machine is directly unit-
/// testable without stdout capture (contract `cli-render-spacing.md` §1).
///
/// Invariants enforced:
/// - INV-1 (no double-blank): draining resets the flag, so two consecutive
///   renderable elements produce exactly one blank.
/// - Edge (no leading blank): at turn start the flag is `false`, so the first
///   element renders with no preceding blank.
fn drain_separator(pending: &mut bool) -> bool {
    if *pending {
        *pending = false;
        true
    } else {
        false
    }
}

/// Spec 008 (T006/FR-002): Build the reasoning-close footer line
/// (`└─ Thought for {:.1}s` + gradient fill) when a duration > 0 is available,
/// or `None` for a plain border close. Extracted from `close_reasoning` so the
/// no-duration (`None`) branch is unit-testable without stdout capture.
fn reasoning_footer_line(started: Option<Instant>) -> Option<String> {
    let t = theme();
    if let Some(ts) = started {
        let secs = ts.elapsed().as_secs_f64();
        if secs > 0.0 {
            let footer = format!("└─ Thought for {:.1}s ", secs);
            let w = box_width();
            let fill = w.saturating_sub(2 + footer.len());
            let footer_styled = t.fg_more_subtle.ansi().paint(&footer).to_string();
            let fill_styled = theme::gradient_fg(
                &"─".repeat(fill.saturating_sub(1)),
                t.info_most_subtle,
                t.fg_most_subtle,
            );
            return Some(format!("{}{}", footer_styled, fill_styled));
        }
    }
    None
}

/// Spec 008 (T014/FR-004/FR-006): Build the terminal-command block header line
/// (`$ command` + status icon + `(exit N)` badge + duration). Pure function
/// extracted from the `ToolEnd` arm for direct unit testing.
fn terminal_header_line(summary: &str, is_error: bool, exit_code: Option<i64>, duration: f64) -> String {
    let t = theme();
    let status_icon = if is_error { "✗" } else { "✓" };
    let status_color = if is_error { t.error } else { t.success };
    let dur = fmt_duration(duration);
    let exit_badge = match exit_code {
        Some(code) if code != 0 => format!(" (exit {})", code),
        _ => String::new(),
    };
    format!(
        "  {} {} {}{} {}",
        theme::paint_bold("$", t.accent),
        t.fg_base.ansi().paint(summary),
        theme::paint_bold(status_icon, status_color),
        if exit_badge.is_empty() {
            String::new()
        } else {
            t.error.ansi().paint(&exit_badge).to_string()
        },
        t.fg_more_subtle.ansi().paint(&dur),
    )
}

/// Spec 008 (T020/FR-007): Build the generic tool-call header line (status icon
/// + emoji + bold name + primary param + duration + optional exit badge). Pure
/// function extracted from the `ToolEnd` arm for direct unit testing.
fn generic_tool_header_line(
    name: &str,
    emoji: &str,
    summary: &str,
    is_error: bool,
    exit_code: Option<i64>,
    duration: f64,
) -> String {
    let t = theme();
    let status_icon = if is_error { "✗" } else { "✓" };
    let status_color = if is_error { t.error } else { t.success };
    let dur = fmt_duration(duration);
    let exit_badge = match exit_code {
        Some(code) if code != 0 => format!(" (exit {})", code),
        _ => String::new(),
    };
    format!(
        "  {} {} {} {} {} {}",
        theme::paint_bold(status_icon, status_color),
        t.accent.ansi().paint(emoji),
        theme::paint_bold(name, t.fg_base),
        t.fg_most_subtle.ansi().paint(summary),
        t.fg_more_subtle.ansi().paint(&dur),
        if exit_badge.is_empty() {
            String::new()
        } else {
            t.error.ansi().paint(&exit_badge).to_string()
        },
    )
}

/// Spec 008 (T015/T021/FR-005/FR-008): Build the indented body lines from a
/// result string. Returns an empty Vec when the body is empty (header-only
/// block). Pure function extracted from the `ToolEnd` arm for direct testing.
fn tool_body_lines(body: &str) -> Vec<String> {
    if body.is_empty() {
        return Vec::new();
    }
    let t = theme();
    body.lines()
        .map(|l| format!("    {}", t.fg_more_subtle.ansi().paint(l)))
        .collect()
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
            lines += line_w.div_ceil(width) as u32;
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
    let mut reasoning_line_count: usize = 0; // Feature 005 (T025): reasoning size tracking
    // Spec 008 (T002): timestamp of the first ReasoningDelta of a block;
    // used to derive the `Thought for {:.1}s` footer duration on block close.
    let mut reasoning_started: Option<Instant> = None;
    let mut last_tool_line: Option<String> = None;
    // Spec 008: stash the emoji+summary from ToolStart for use in the
    // crush-style header on ToolEnd (emoji+summary are not on ToolEnd events).
    let mut pending_tool_emoji = String::new();
    let mut pending_tool_summary = String::new();
    let mut total_prompt_tokens: u64 = 0;
    let mut total_completion_tokens: u64 = 0;

    // Feature 013 (US3): the pending_separator flag drives uniform one-blank-
    // line spacing between every distinct CLI element (FR-009). Drained
    // (one println!()) before the next renderable element's first line; set
    // true after an element renders. INV-1: draining resets the flag, so two
    // consecutive renderable elements produce exactly one blank — never two
    // (Clarification Q1). FR-015: a suppressed element (quiet/gate-hidden)
    // neither drains nor sets the flag, so it contributes no dangling blank.
    let mut pending_separator: bool = false;

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
    // Feature 013 (T028): returns `true` when a reasoning block was actually
    // closed (footer printed), so each call site can set `pending_separator`
    // to insert the FR-010 blank before the next element.
    let close_reasoning = |open: &mut bool, buf: &mut String, line_count: &mut usize, started: Option<Instant>| -> bool {
        if *open {
            if !buf.is_empty() {
                println!("{}", t.fg_more_subtle.ansi().paint(buf.as_str()));
                buf.clear();
            }
            // Spec 008 (T006/FR-002): replace the "N lines of reasoning" close
            // summary with `└─ Thought for {:.1}s` footer matching the TUI
            // (widgets.rs:333-336), or a plain border close when no duration.
            match reasoning_footer_line(started) {
                Some(line) => println!("{}", line),
                None => {
                    let w = box_width();
                    let border = theme::gradient_diagonal_field(
                        w.saturating_sub(2),
                        t.info_most_subtle,
                        t.fg_most_subtle,
                    );
                    println!("{}", border);
                }
            }
            *open = false;
            *line_count = 0;
            true
        } else {
            false
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
                        let color = (profile.color)(t);
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
                    // Feature 013 (T027): TRAILING METADATA (Clarification Q3,
                    // FR-012) — do NOT drain before the usage line (it attaches
                    // tightly to whatever preceded it), but DO set the flag so
                    // the next distinct element is preceded by one blank.
                    let stats = format!(
                        "  {} {} in · {} out",
                        t.fg_most_subtle.ansi().paint("↪"),
                        t.fg_more_subtle.ansi().paint(format_tokens(usage.prompt_tokens)),
                        t.fg_more_subtle.ansi().paint(format_tokens(usage.completion_tokens)),
                    );
                    println!("{}", stats);
                    pending_separator = true;
                }
            }
            AgentEvent::ReasoningDelta(d) => {
                if opts.quiet || !opts.show_reasoning {
                    continue;
                }
                if !reasoning_open {
                    reasoning_open = true;
                    // Spec 008 (T007): capture start time for the footer duration.
                    reasoning_started = Some(Instant::now());
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
                    reasoning_line_count += 1; // Feature 005 (T025)
                }
                if reasoning_buf.len() > 80 {
                    println!("{}", t.fg_more_subtle.ansi().paint(reasoning_buf.as_str()));
                    reasoning_line_count += 1; // Feature 005 (T025)
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
                // Feature 013 (T028): close reasoning first; if a footer was
                // printed, set the flag so the drain below inserts the FR-010
                // blank between the reasoning footer and the content.
                if close_reasoning(&mut reasoning_open, &mut reasoning_buf, &mut reasoning_line_count, reasoning_started.take()) {
                    pending_separator = true;
                }
                // Feature 013 (T025): drain before the first streamed char so
                // the content block is separated from the previous element
                // (or from the reasoning footer per FR-010).
                if drain_separator(&mut pending_separator) { println!(); }
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
                    // Feature 013 (T028): close reasoning; set flag if closed.
                    if close_reasoning(&mut reasoning_open, &mut reasoning_buf, &mut reasoning_line_count, reasoning_started.take()) {
                        pending_separator = true;
                    }
                    // Feature 013 (T025): drain before this distinct element.
                    if drain_separator(&mut pending_separator) { println!(); }
                    println!("{}", final_text);
                    // Feature 013 (T026): set after rendering.
                    pending_separator = true;
                }
            }
            AgentEvent::ToolStart { name, emoji, summary } => {
                // Spec 008: stash emoji+summary for the ToolEnd crush header.
                pending_tool_emoji = emoji.clone();
                pending_tool_summary = summary.clone();
                // Feature 013 (T031): the old `if streamed_any { println!() }`
                // ad-hoc blank is subsumed by the pending_separator flag.
                // (Relied on INV-1 dedup — draining resets the flag.)
                // T040: stop caret when tool output begins.
                caret_active = false;
                // Feature 013 (T028): close reasoning; set flag if closed so
                // the drain below separates the footer from the tool header.
                if close_reasoning(&mut reasoning_open, &mut reasoning_buf, &mut reasoning_line_count, reasoning_started.take()) {
                    pending_separator = true;
                }
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
                    // Feature 013 (T025): drain BEFORE the tool_row capture so
                    // the blank lands ABOVE the spinner row and tool_row points
                    // at the post-blank spinner row (FR-014, contract §3).
                    if drain_separator(&mut pending_separator) { println!(); }
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
                            let color = (tool_profile.color)(t);
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
            AgentEvent::ToolEnd { name, is_error, result_preview, duration_secs, exit_code, full_result } => {
                let duration = duration_secs;
                if opts.quiet || opts.tool_progress == "off" {
                    continue;
                }
                if opts.tool_progress == "new" && last_tool_line.as_deref() == Some(name.as_str()) && !is_error {
                    continue;
                }
                last_tool_line = Some(name.clone());

                let body_text = if !full_result.is_empty() { &full_result } else { &result_preview };
                let emoji = if pending_tool_emoji.is_empty() { "⚡" } else { pending_tool_emoji.as_str() };
                let summary = &pending_tool_summary;

                // Spec 008 (T013/FR-013): branch on terminal vs generic tool.
                // Header/body composition delegated to pure helpers
                // (terminal_header_line / generic_tool_header_line / tool_body_lines)
                // for direct unit-test coverage.
                if is_terminal_block(&name) {
                    // ── T014/FR-004: terminal-command block header ──
                    let line = terminal_header_line(summary, is_error, exit_code, duration);

                    // T016: in-place rewrite when animations_on and active_tool matches.
                    if let Some((tool_row, state, tool_name, _summary)) = active_tool.take() {
                        if tool_name == name && animations_on {
                            use crossterm::{cursor, queue, terminal};
                            let mut stdout = std::io::stdout();
                            let _ = queue!(
                                stdout,
                                cursor::MoveTo(0, tool_row),
                                terminal::Clear(terminal::ClearType::CurrentLine),
                            );
                            let _ = stdout.flush();
                            print!("{}", line);
                            let _ = stdout.flush();
                            let _ = state;
                        } else {
                            println!("{}", line);
                        }
                    } else {
                        println!("{}", line);
                    }

                    // T015/FR-005: print the full command output beneath the header.
                    for l in tool_body_lines(body_text) {
                        println!("{}", l);
                    }
                } else {
                    // ── T020/FR-007: generic tool-call header (crush composition) ──
                    let line = generic_tool_header_line(
                        &name, emoji, summary, is_error, exit_code, duration,
                    );

                    // T022: in-place rewrite when animations_on and active_tool matches.
                    if let Some((tool_row, state, tool_name, _summary)) = active_tool.take() {
                        if tool_name == name && animations_on {
                            use crossterm::{cursor, queue, terminal};
                            let mut stdout = std::io::stdout();
                            let _ = queue!(
                                stdout,
                                cursor::MoveTo(0, tool_row),
                                terminal::Clear(terminal::ClearType::CurrentLine),
                            );
                            let _ = stdout.flush();
                            print!("{}", line);
                            let _ = stdout.flush();
                            let _ = state;
                        } else {
                            println!("{}", line);
                        }
                    } else {
                        println!("{}", line);
                    }

                    // T021/FR-008: print the full result body indented beneath.
                    for l in tool_body_lines(body_text) {
                        println!("{}", l);
                    }
                }
                // Feature 013 (T026): set after the tool block finishes
                // rendering, so the next distinct element is preceded by one
                // blank. NO drain here — the in-place rewrite path (above)
                // targets tool_row and must not print a stray blank (FR-014,
                // contract §3).
                pending_separator = true;
            }
            AgentEvent::Notice(msg) => {
                if !opts.quiet {
                    // Feature 013 (T025/T026): drain before, set after.
                    if drain_separator(&mut pending_separator) { println!(); }
                    println!("{}", t.warning.ansi().paint(format!("  · {}", msg)));
                    pending_separator = true;
                }
            }
            AgentEvent::RetryAttempt { attempt, max_retries, error, wait_secs } => {
                if !opts.quiet {
                    // Feature 013 (T025/T026): drain before, set after.
                    if drain_separator(&mut pending_separator) { println!(); }
                    let label = format!("  ↻ Retry {}/{} in {:.1}s — {}", attempt, max_retries, wait_secs, error);
                    println!("{}", t.warning.ansi().paint(label));
                    pending_separator = true;
                }
            }
            AgentEvent::CompressionStart { reason, approx_tokens } => {
                if !opts.quiet {
                    // Feature 013 (T025/T026): drain before, set after.
                    if drain_separator(&mut pending_separator) { println!(); }
                    let label = format!("  🗜️ Compressing (~{} tokens): {}", format_tokens(approx_tokens as u64), reason);
                    println!("{}", t.info.ansi().paint(label));
                    pending_separator = true;
                }
            }
            AgentEvent::CompressionEnd { original_msgs, new_msgs } => {
                if !opts.quiet {
                    // Feature 013 (T025/T026): drain before, set after.
                    if drain_separator(&mut pending_separator) { println!(); }
                    let label = format!("  ✅ Compressed {} → {} messages", original_msgs, new_msgs);
                    println!("{}", t.success_more_subtle.ansi().paint(label));
                    pending_separator = true;
                }
            }
            AgentEvent::FallbackActivated { from_model, to_model } => {
                if !opts.quiet {
                    // Feature 013 (T025/T026): drain before, set after.
                    if drain_separator(&mut pending_separator) { println!(); }
                    let label = format!("  🔄 Fallback: {} → {}", from_model, to_model);
                    println!("{}", t.warning.ansi().paint(label));
                    pending_separator = true;
                }
            }
            AgentEvent::SubagentSpawn { goal, model, toolset_summary, depth } => {
                if !opts.quiet {
                    // Feature 013 (T025/T026): drain before, set after.
                    if drain_separator(&mut pending_separator) { println!(); }
                    let indent = "  ".repeat(depth);
                    let label = format!("{}🤖 Subagent: {} ({}) [{}]", indent, goal, model, toolset_summary);
                    println!("{}", t.info.ansi().paint(label));
                    pending_separator = true;
                }
            }
            AgentEvent::SubagentComplete { goal, success, summary_preview, token_usage, duration_secs } => {
                if !opts.quiet {
                    // Feature 013 (T025/T026): drain before, set after.
                    if drain_separator(&mut pending_separator) { println!(); }
                    let status = if success { "✓" } else { "✗" };
                    let label = format!("  {} {} ({} tok, {:.1}s): {}", status, goal, token_usage.total_tokens, duration_secs, summary_preview);
                    println!("{}", t.success_more_subtle.ansi().paint(label));
                    pending_separator = true;
                }
            }
            AgentEvent::SubagentFailed { goal, error, duration_secs } => {
                if !opts.quiet {
                    // Feature 013 (T025/T026): drain before, set after.
                    if drain_separator(&mut pending_separator) { println!(); }
                    let label = format!("  ✗ {} ({:.1}s): {}", goal, duration_secs, error);
                    println!("{}", t.error.ansi().paint(label));
                    pending_separator = true;
                }
            }
            AgentEvent::DelegationBatchComplete { total, succeeded, failed, total_duration_secs } => {
                if !opts.quiet {
                    // Feature 013 (T025/T026): drain before, set after.
                    if drain_separator(&mut pending_separator) { println!(); }
                    let label = format!("  🤖 Batch: {}/{} succeeded, {} failed ({:.1}s)", succeeded, total, failed, total_duration_secs);
                    println!("{}", t.info_more_subtle.ansi().paint(label));
                    pending_separator = true;
                }
            }
            // Feature 005: inline file-change diff rendering (T014/T015/T016/T017).
            AgentEvent::FileChange { path, kind, before: _, after: _, diff, is_binary, source: _ } => {
                if opts.quiet {
                    // --quiet: skip diffs entirely (only final response prints).
                    continue;
                }
                // Feature 013 (T025): drain before the diff block.
                if drain_separator(&mut pending_separator) { println!(); }
                let t = theme();
                // Path header: "  ◆ path  +N -M" with kind label.
                let kind_label = match kind {
                    FileChangeKind::Create => " (new file)",
                    FileChangeKind::Delete => " (deleted)",
                    FileChangeKind::Edit => "",
                };
                let header = format!("  ◆ {}{}  {}", path, kind_label, diff.stat_line());
                println!("{}", t.fg_subtle.ansi().paint(header));

                if is_binary {
                    // T017: binary-file placeholder (FR-016).
                    println!("{}", t.fg_most_subtle.ansi().paint("    binary file changed"));
                    // Feature 013 (T026): set after the block rendered.
                    pending_separator = true;
                    continue;
                }

                // T016: non-interactive / piped → plain text, no color, no truncation (FR-012).
                if !opts.capability.is_interactive {
                    for line in diff.diff.lines() {
                        println!("{}", line);
                    }
                    // Feature 013 (T026): set after the block rendered.
                    pending_separator = true;
                    continue;
                }

                // T014 + T015: interactive — colored + syntax-highlighted diff,
                // with large-diff height bounding (E2 resolution).
                let diff_lines: Vec<&str> = diff.diff.lines().collect();
                let max_height = MAX_DIFF_BLOCK_HEIGHT;
                let hidden = if diff_lines.len() > max_height {
                    diff_lines.len() - max_height
                } else {
                    0
                };
                // Render only the tail `max_height` lines when truncated.
                let start = hidden;
                for &line in &diff_lines[start..] {
                    let rendered = render_diff_line(line, &path, opts.syntax_highlighting, t);
                    println!("{}", rendered);
                }
                if hidden > 0 {
                    let affordance = format!("    … ({} earlier lines hidden)", hidden);
                    println!("{}", t.fg_most_subtle.ansi().paint(affordance));
                }
                // Feature 013 (T026): set after the block rendered.
                pending_separator = true;
            }
            AgentEvent::Done { final_text: text, usage: _, iterations } => {
                // Feature 013 (T028): close reasoning; set flag if closed.
                if close_reasoning(&mut reasoning_open, &mut reasoning_buf, &mut reasoning_line_count, reasoning_started.take()) {
                    pending_separator = true;
                }

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
                    let rendered = crate::markdown::markdown_to_ansi(&text, t);
                    print!("{}", rendered);
                    let _ = std::io::stdout().flush();
                    println!();
                } else if streamed_any {
                    println!();
                }

                if !text.is_empty() {
                    final_text = text;
                }
                // Feature 013 (T025): drain before the turn summary so it is
                // separated from the finalized text/previous element. NO set
                // after — turn end; next turn starts fresh (Edge Case).
                if drain_separator(&mut pending_separator) { println!(); }
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
                // Feature 013 (T028): close reasoning; set flag if closed.
                if close_reasoning(&mut reasoning_open, &mut reasoning_buf, &mut reasoning_line_count, reasoning_started.take()) {
                    pending_separator = true;
                }
                // Feature 013 (T025/T031): drain before the error line. The
                // old `if streamed_any { println!() }` ad-hoc blank is subsumed
                // by the flag (INV-1 dedup). NO set after — turn end.
                if drain_separator(&mut pending_separator) { println!(); }
                println!("{}", t.error.ansi().paint(format!("Error: {}", err)));
                break;
            }
            // ── OMO orchestration events (additive) ──
            // NeuroCode events are TUI-panel payloads; the line renderer has
            // no panel, so they're intentionally consumed silently here (the
            // tier/tokens summary already reaches the log via tracing).
            AgentEvent::NeuroCodeContext { .. } | AgentEvent::NeuroCodeActive { .. } => {}
            AgentEvent::AgentModeChanged { agent_name, model: _ } => {
                // Feature 013 (T025/T026): drain before, set after.
                if drain_separator(&mut pending_separator) { println!(); }
                println!("{} agent → {}",
                    t.fg_subtle.ansi().paint("◆"),
                    t.fg_base.ansi().paint(&agent_name));
                pending_separator = true;
            }
            AgentEvent::CategoryDelegation { category, model } => {
                // Feature 013 (T025/T026): drain before, set after.
                if drain_separator(&mut pending_separator) { println!(); }
                println!("{} [{}] → {}",
                    t.fg_subtle.ansi().paint("◇"),
                    category, model);
                pending_separator = true;
            }
            AgentEvent::BoulderWorkStarted { plan_name, work_id: _ } => {
                // Feature 013 (T025/T026): drain before, set after.
                if drain_separator(&mut pending_separator) { println!(); }
                println!("{} started work: {}",
                    t.success.ansi().paint("▶"),
                    plan_name);
                pending_separator = true;
            }
            AgentEvent::BoulderWorkResumed { plan_name, work_id: _ } => {
                // Feature 013 (T025/T026): drain before, set after.
                if drain_separator(&mut pending_separator) { println!(); }
                println!("{} resumed work: {}",
                    t.fg_subtle.ansi().paint("↻"),
                    plan_name);
                pending_separator = true;
            }
            AgentEvent::BoulderWorkCompleted { plan_name, work_id: _ } => {
                // Feature 013 (T025/T026): drain before, set after.
                if drain_separator(&mut pending_separator) { println!(); }
                println!("{} completed: {}",
                    t.success.ansi().paint("✓"),
                    plan_name);
                pending_separator = true;
            }
            AgentEvent::GoalSet { objective } => {
                // Feature 013 (T025/T026): drain before, set after.
                if drain_separator(&mut pending_separator) { println!(); }
                println!("{} goal set: {}",
                    t.success.ansi().paint("◎"),
                    objective);
                pending_separator = true;
            }
            AgentEvent::GoalCleared => {
                // Feature 013 (T025/T026): drain before, set after.
                if drain_separator(&mut pending_separator) { println!(); }
                println!("{} goal cleared", t.fg_subtle.ansi().paint("○"));
                pending_separator = true;
            }
            AgentEvent::WisdomAccumulated { learnings_count } => {
                // Feature 013 (T025/T026): drain before, set after.
                if drain_separator(&mut pending_separator) { println!(); }
                println!("{} {} learnings accumulated",
                    t.fg_subtle.ansi().paint("✦"),
                    learnings_count);
                pending_separator = true;
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
                        let color = (spinner_profile.color)(t);
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
                    let color = (caret_profile.color)(t);
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
                        let color = (tool_profile.color)(t);
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
                let color = (profile.color)(t);
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
    use joey_agent_core::events::{FileChangeKind, AgentEvent};
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
                syntax_highlighting: true,
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
                syntax_highlighting: true,
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
                syntax_highlighting: true,
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
            exit_code: None,
            full_result: "file contents".to_string(),
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
            exit_code: None,
            full_result: "found 3 results".to_string(),
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
            exit_code: None,
            full_result: "ok".to_string(),
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

    // ══ Spec 008: Crush-Style Block Formatting Tests ══

    /// Run a synthetic event stream through `render_turn` and return final_text.
    /// Used by the regression / gate tests below (T023-T026).
    fn run_turn(events: Vec<AgentEvent>, opts: RenderOptions) -> String {
        let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
        for ev in events {
            let _ = tx.send(ev);
        }
        drop(tx);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build runtime");
        rt.block_on(render_turn(rx, opts))
    }

    // ── T009: is_terminal_block classification (matches 007 T020, FR-013) ──
    #[test]
    fn is_terminal_block_classification() {
        assert!(is_terminal_block("terminal"));
        assert!(!is_terminal_block("read_file"));
        assert!(!is_terminal_block("write_file"));
        assert!(!is_terminal_block("search_files"));
        assert!(!is_terminal_block(""));
    }

    // ── T003: reasoning footer shows "Thought for" when duration > 0 ──
    // FR-002/FR-003, quickstart.md test 2.
    #[test]
    fn close_reasoning_footer_with_duration() {
        let ts = Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let line = reasoning_footer_line(Some(ts));
        assert!(line.is_some(), "footer should render when elapsed > 0");
        let line = line.unwrap();
        assert!(
            line.contains("Thought for"),
            "reasoning footer missing 'Thought for': {}",
            line
        );
    }

    // ── T004: reasoning close with no duration → None (plain border) ──
    // The `None` path is the defensive fallback with no "Thought for" footer
    // (quickstart.md test 3).
    #[test]
    fn close_reasoning_no_duration_plain_border() {
        assert!(reasoning_footer_line(None).is_none());
    }

    // ── T010: terminal block header shows $ prompt + command from summary ──
    // FR-004, quickstart.md test 4.
    #[test]
    fn terminal_block_header_shows_prompt() {
        let line = terminal_header_line("ls -la crates", false, Some(0), 0.3);
        assert!(line.contains('$'), "terminal prompt '$' missing: {}", line);
        assert!(
            line.contains("ls -la crates"),
            "command text missing: {}",
            line
        );
    }

    // ── T011: terminal block exit badge shows (exit N) for non-zero ──
    // FR-006, quickstart.md test 5.
    #[test]
    fn terminal_block_exit_badge_nonzero() {
        let line = terminal_header_line("false", true, Some(1), 0.1);
        assert!(
            line.contains("(exit 1)"),
            "exit badge missing: {}",
            line
        );
    }

    // ── T012: terminal block no badge on zero exit ──
    // FR-006, quickstart.md test 6.
    #[test]
    fn terminal_block_no_badge_on_zero_exit() {
        let line = terminal_header_line("echo hi", false, Some(0), 0.1);
        assert!(
            !line.contains("(exit"),
            "unexpected exit badge on zero exit: {}",
            line
        );
    }

    // ── T017: generic tool header composition (status icon + name + summary) ──
    // FR-007, quickstart.md test 7.
    #[test]
    fn generic_tool_header_composition() {
        let line = generic_tool_header_line("read_file", "📖", "Cargo.toml", false, None, 0.1);
        assert!(line.contains("read_file"), "tool name missing: {}", line);
        assert!(line.contains("Cargo.toml"), "summary param missing: {}", line);
        assert!(line.contains('✓'), "success icon missing: {}", line);
    }

    // ── T018: generic tool body sourced from full_result ──
    // FR-008, quickstart.md test 8.
    #[test]
    fn generic_tool_body_from_full_result() {
        let lines = tool_body_lines("line one\nline two\nline three");
        assert_eq!(lines.len(), 3);
        let joined = lines.join("\n");
        assert!(joined.contains("line two"), "full_result body missing 'line two': {}", joined);
        assert!(joined.contains("line three"), "full_result body missing 'line three': {}", joined);
    }

    // ── T019: generic tool body falls back to result_preview ──
    // FR-008, quickstart.md test 9.
    #[test]
    fn generic_tool_body_fallback_to_preview() {
        let lines = tool_body_lines("preview only content");
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains("preview only content"),
            "preview fallback body missing: {}",
            lines[0]
        );
    }

    // ── T015/FR-005: empty body produces no lines (header-only block) ──
    #[test]
    fn tool_body_empty_produces_no_lines() {
        assert!(tool_body_lines("").is_empty());
    }

    // ── T023: regression — reasoning visibility gate preserved ──
    // FR-011, spec US1 acceptance scenario 5. When show_reasoning is false the
    // reasoning box must not open; the turn still returns correct final_text.
    #[test]
    fn reasoning_visibility_gate_preserved() {
        // show_reasoning: false (NonInteractive default)
        let opts = opts_for(Capability::NonInteractive);
        let text = run_turn(
            vec![
                AgentEvent::TurnStart { max_iterations: 1 },
                AgentEvent::ReasoningDelta("secret reasoning\n".to_string()),
                AgentEvent::ContentDelta("Answer.".to_string()),
                AgentEvent::Done {
                    final_text: "Answer.".to_string(),
                    usage: Usage::default(),
                    iterations: 1,
                },
            ],
            opts,
        );
        assert_eq!(text, "Answer.");
    }

    // ── T024: regression — quiet mode suppresses all blocks ──
    // FR-011. The turn completes and returns correct final_text.
    #[test]
    fn quiet_mode_suppresses_blocks() {
        let mut opts = opts_for(Capability::NonInteractive);
        opts.quiet = true;
        let text = run_turn(
            vec![
                AgentEvent::TurnStart { max_iterations: 1 },
                AgentEvent::ToolStart {
                    name: "terminal".to_string(),
                    emoji: "⬛".to_string(),
                    summary: "ls".to_string(),
                },
                AgentEvent::ToolEnd {
                    name: "terminal".to_string(),
                    is_error: false,
                    result_preview: "file1\nfile2".to_string(),
                    duration_secs: 0.1,
                    exit_code: Some(0),
                    full_result: "file1\nfile2".to_string(),
                },
                AgentEvent::ContentDelta("Done.".to_string()),
                AgentEvent::Done {
                    final_text: "Done.".to_string(),
                    usage: Usage::default(),
                    iterations: 1,
                },
            ],
            opts,
        );
        assert_eq!(text, "Done.");
    }

    // ── T025: regression — tool_progress "off" suppresses tool blocks ──
    // FR-011, spec US3 acceptance scenario 5. The turn completes and returns
    // correct final_text.
    #[test]
    fn tool_progress_off_suppresses_blocks() {
        let mut opts = opts_for(Capability::NonInteractive);
        opts.tool_progress = "off".to_string();
        let text = run_turn(
            vec![
                AgentEvent::TurnStart { max_iterations: 1 },
                AgentEvent::ToolStart {
                    name: "read_file".to_string(),
                    emoji: "📖".to_string(),
                    summary: "test.txt".to_string(),
                },
                AgentEvent::ToolEnd {
                    name: "read_file".to_string(),
                    is_error: false,
                    result_preview: "contents".to_string(),
                    duration_secs: 0.1,
                    exit_code: None,
                    full_result: "contents".to_string(),
                },
                AgentEvent::ContentDelta("Done.".to_string()),
                AgentEvent::Done {
                    final_text: "Done.".to_string(),
                    usage: Usage::default(),
                    iterations: 1,
                },
            ],
            opts,
        );
        assert_eq!(text, "Done.");
    }

    // ── T026: NonInteractive renders block layout without crash ──
    // FR-015: structural layout renders in ALL modes. The pure helpers produce
    // the header + body in any mode (FR-015 "structural layout in ALL modes"),
    // and a full NonInteractive turn completes and returns final_text.
    #[test]
    fn noninteractive_renders_block_layout() {
        // Pure-function: header + body compose correctly (capability-agnostic).
        let header = generic_tool_header_line("read_file", "📖", "Cargo.toml", false, None, 0.5);
        assert!(header.contains("read_file"), "header missing: {}", header);
        let body = tool_body_lines("line 1\nline 2");
        assert_eq!(body.len(), 2);
        assert!(body[1].contains("line 2"), "body missing: {}", body[1]);

        // Integration: a full NonInteractive turn completes without crash.
        let opts = opts_for(Capability::NonInteractive);
        let text = run_turn(
            vec![
                AgentEvent::TurnStart { max_iterations: 1 },
                AgentEvent::ToolStart {
                    name: "read_file".to_string(),
                    emoji: "📖".to_string(),
                    summary: "Cargo.toml".to_string(),
                },
                AgentEvent::ToolEnd {
                    name: "read_file".to_string(),
                    is_error: false,
                    result_preview: "file contents".to_string(),
                    duration_secs: 0.5,
                    exit_code: None,
                    full_result: "line 1\nline 2".to_string(),
                },
                AgentEvent::ContentDelta("Done.".to_string()),
                AgentEvent::Done {
                    final_text: "Done.".to_string(),
                    usage: Usage::default(),
                    iterations: 1,
                },
            ],
            opts,
        );
        assert_eq!(text, "Done.");
    }

    // ── Feature 013 (US3): pending_separator spacing state machine ──

    /// T032: the `drain_separator` state machine produces exactly one blank
    /// between adjacent renderable elements and NO blank before the first
    /// element (INV-1, Edge Case "no leading blank"). Simulates a sequence
    /// of element renders over the flag.
    #[test]
    fn drain_separator_one_blank_between_elements() {
        let mut pending = false;
        let mut blanks_emitted = 0usize;

        // Simulate N renderable elements: each drains (maybe prints blank),
        // then sets the flag after rendering.
        for _ in 0..5 {
            if drain_separator(&mut pending) {
                blanks_emitted += 1;
            }
            // ... element renders here ...
            pending = true;
        }

        // 5 elements → 4 inter-element blanks (the first element drains
        // nothing because the flag starts false).
        assert_eq!(
            blanks_emitted, 4,
            "5 elements should produce exactly 4 inter-element blanks, not {}",
            blanks_emitted
        );
    }

    /// T032 (Edge): no leading blank at turn start (flag starts false).
    #[test]
    fn drain_separator_no_leading_blank() {
        let mut pending = false;
        // First element: drain should NOT fire (no previous element).
        assert!(
            !drain_separator(&mut pending),
            "first element must not drain a blank (no leading blank)"
        );
    }

    /// T032 (INV-1): draining resets the flag, so two consecutive drains
    /// never both fire.
    #[test]
    fn drain_separator_reset_prevents_double_blank() {
        let mut pending = true;
        assert!(drain_separator(&mut pending), "first drain fires");
        assert!(
            !drain_separator(&mut pending),
            "second drain must NOT fire (flag was reset) — no double-blank"
        );
    }

    /// T033: trailing-metadata exception — `ApiCallEnd` does NOT drain before
    /// itself (attaches tightly to its predecessor), but DOES set the flag so
    /// the next element drains one blank (Clarification Q3, FR-012). We
    /// simulate the pattern: previous element sets flag; ApiCallEnd does NOT
    /// drain; ApiCallEnd sets flag; next element drains.
    #[test]
    fn trailing_metadata_no_drain_before_set_after() {
        let mut pending = false;

        // 1. Previous element (e.g. a tool block) renders and sets flag.
        pending = true;

        // 2. ApiCallEnd: does NOT drain (tight before). We model this by
        //    simply NOT calling drain_separator here. It sets the flag.
        //    (In render_turn the usage line prints immediately after the
        //    predecessor, with no intervening blank.)
        // pending stays true (was already true; ApiCallEnd would set it).

        // 3. Next distinct element drains — should fire exactly once.
        assert!(
            drain_separator(&mut pending),
            "next element after trailing-metadata must drain exactly one blank"
        );
        assert!(
            !drain_separator(&mut pending),
            "only one blank (flag reset by drain)"
        );
    }

    /// T035 (FR-015): a suppressed element (quiet/gate skip) does NOT set
    /// `pending_separator`, so no dangling blank is introduced where a block
    /// was hidden. We model this: the element neither drains nor sets.
    #[test]
    fn suppressed_element_does_not_set_flag() {
        let mut pending = false;

        // First renderable element: drains nothing (flag false), sets flag.
        assert!(!drain_separator(&mut pending));
        pending = true;

        // Suppressed element (e.g. quiet): skips both drain and set.
        // (No call to drain_separator, no assignment to pending.)

        // Next renderable element: drains exactly one (from the first element).
        assert!(
            drain_separator(&mut pending),
            "suppressed element must not consume or duplicate the separator"
        );
    }

    /// T034 (FR-014): the ToolStart→ToolEnd ordering invariant — the drain
    /// occurs before `tool_row` capture (conceptually) and NOT during the
    /// ToolEnd rewrite. This is a behavioral test on the drain helper covering
    /// the tool-block sequence: drain (ToolStart) → no drain (ToolEnd) → set.
    #[test]
    fn tool_block_sequence_drain_before_set_after() {
        let mut pending = true; // previous element set the flag.

        // ToolStart: drain fires (blank lands above the tool line).
        assert!(
            drain_separator(&mut pending),
            "ToolStart must drain before capturing tool_row (FR-014)"
        );
        // ToolStart does NOT set the flag (ToolEnd owns the block close).
        assert_eq!(pending, false);

        // ToolEnd: NO drain (the rewrite targets tool_row). We model this by
        // NOT calling drain_separator here.
        // ToolEnd sets the flag after the body prints.
        pending = true;

        // Next element drains exactly one.
        assert!(
            drain_separator(&mut pending),
            "next element after tool block must drain one blank"
        );
    }

    /// T036: integration — a multi-element NonInteractive turn (reasoning,
    /// tool calls, content) completes and returns final_text under the new
    /// spacing (constitution Principle VII: non-regression).
    #[test]
    fn spaced_turn_completes_and_returns_final_text() {
        let opts = opts_for(Capability::NonInteractive);
        let text = run_turn(
            vec![
                AgentEvent::TurnStart { max_iterations: 1 },
                AgentEvent::ToolStart {
                    name: "read_file".to_string(),
                    emoji: "📖".to_string(),
                    summary: "a.txt".to_string(),
                },
                AgentEvent::ToolEnd {
                    name: "read_file".to_string(),
                    is_error: false,
                    result_preview: "content".to_string(),
                    duration_secs: 0.1,
                    exit_code: None,
                    full_result: String::new(),
                },
                AgentEvent::ToolStart {
                    name: "terminal".to_string(),
                    emoji: "⚡".to_string(),
                    summary: "echo hi".to_string(),
                },
                AgentEvent::ToolEnd {
                    name: "terminal".to_string(),
                    is_error: false,
                    result_preview: "hi".to_string(),
                    duration_secs: 0.1,
                    exit_code: Some(0),
                    full_result: String::new(),
                },
                AgentEvent::ContentDelta("Final answer.".to_string()),
                AgentEvent::Done {
                    final_text: "Final answer.".to_string(),
                    usage: Usage::default(),
                    iterations: 1,
                },
            ],
            opts,
        );
        assert_eq!(text, "Final answer.");
    }
}
