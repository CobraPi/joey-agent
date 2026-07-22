//! Terminal rendering: streaming output with the dim `┌─ Reasoning` box
//! (cli.py:5651-5697), tool-completion lines honoring `display.tool_progress`
//! (cli.py:10652-10761), the welcome banner (banner.py:580+), and the exit
//! outro (cli.py:12690-12727).
//!
//! Crush-inspired visual style: CharmTone Pantera theme, gradient text,
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
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { show_reasoning: true, tool_progress: "all".to_string(), quiet: false }
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

/// Consume agent events and render them live. Returns the final text.
pub async fn render_turn(mut rx: mpsc::UnboundedReceiver<AgentEvent>, opts: RenderOptions) -> String {
    let mut final_text = String::new();
    let mut streamed_any = false;
    let mut reasoning_open = false;
    let mut reasoning_buf = String::new();
    let mut last_tool_line: Option<String> = None;
    let mut total_prompt_tokens: u64 = 0;
    let mut total_completion_tokens: u64 = 0;

    let t = theme();
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

    while let Some(ev) = rx.recv().await {
        match ev {
            AgentEvent::TurnStart { max_iterations } => {
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
                    let spinner_label = t.fg_more_subtle.ansi().paint("⟳ querying model...");
                    println!("{}", spinner_label);
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
                close_reasoning(&mut reasoning_open, &mut reasoning_buf);
                print!("{}", d);
                let _ = std::io::stdout().flush();
                streamed_any = true;
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
                close_reasoning(&mut reasoning_open, &mut reasoning_buf);

                if !opts.quiet && opts.tool_progress != "off" {
                    let e = if emoji.is_empty() { "⚡" } else { &emoji };
                    let name_styled = theme::gradient_fg(&name, t.info, t.accent);
                    print!("  {} {}", e, name_styled);
                    if !summary.is_empty() {
                        let short_summary: String = summary.chars().take(60).collect();
                        print!(" {}", t.fg_most_subtle.ansi().paint(format!("({})", short_summary)));
                    }
                    println!();
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
                println!("{}", line);

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
            AgentEvent::Done { final_text: text, usage: _, iterations } => {
                close_reasoning(&mut reasoning_open, &mut reasoning_buf);
                if streamed_any {
                    println!();
                }
                if !text.is_empty() {
                    final_text = text;
                }
                // Turn summary with iteration count and token usage.
                if !opts.quiet && iterations > 0 {
                    println!();
                    let summary = format!(
                        "  {} {} iteration{} · {} in · {} out",
                        t.fg_most_subtle.ansi().paint("⟶"),
                        t.fg_subtle.ansi().paint(format!("{}", iterations)),
                        if iterations == 1 { "" } else { "s" },
                        t.fg_subtle.ansi().paint(format_tokens(total_prompt_tokens)),
                        t.fg_subtle.ansi().paint(format_tokens(total_completion_tokens)),
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

pub fn banner(info: &BannerInfo) {
    let t = theme();
    let width = box_width().max(40);
    let inner = width - 2;

    // ── Logo: gradient wordmark + diagonal field (Crush-style) ──
    let logo_name = format!("{} {}", branding::AGENT_NAME, format!("v{}", branding::VERSION));
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
        let farewell = format!("Goodbye! ⚕");
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
