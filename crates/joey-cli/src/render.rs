//! Terminal rendering: streaming output with the dim `┌─ Reasoning` box
//! (cli.py:5651-5697), tool-completion lines honoring `display.tool_progress`
//! (cli.py:10652-10761), the welcome banner (banner.py:580+), and the exit
//! outro (cli.py:12690-12727).

use std::collections::HashMap;
use std::io::Write;
use std::time::Instant;

use joey_agent_core::AgentEvent;
use joey_core::branding;
use nu_ansi_term::Color;
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;

// ---------------------------------------------------------------------------
// Basic styled prints
// ---------------------------------------------------------------------------

pub fn info(msg: &str) {
    println!("{}", Color::DarkGray.paint(msg));
}

pub fn error(msg: &str) {
    eprintln!("{}", Color::Red.paint(format!("error: {}", msg)));
}

pub fn success(msg: &str) {
    println!("{}", Color::Green.paint(msg));
}

pub fn check_ok(text: &str, detail: &str) {
    let d = if detail.is_empty() { String::new() } else { format!(" {}", Color::DarkGray.paint(detail)) };
    println!("  {} {}{}", Color::Green.paint("✓"), text, d);
}

pub fn check_warn(text: &str, detail: &str) {
    let d = if detail.is_empty() { String::new() } else { format!(" {}", Color::DarkGray.paint(detail)) };
    println!("  {} {}{}", Color::Yellow.paint("⚠"), text, d);
}

pub fn check_fail(text: &str, detail: &str) {
    let d = if detail.is_empty() { String::new() } else { format!(" {}", Color::DarkGray.paint(detail)) };
    println!("  {} {}{}", Color::Red.paint("✗"), text, d);
}

pub fn check_info(text: &str) {
    println!("    {} {}", Color::Cyan.paint("→"), text);
}

/// A `◆ Section` banner (doctor.py:192-196 `_section`).
pub fn section(title: &str) {
    println!();
    println!("{}", Color::Cyan.bold().paint(format!("◆ {}", title)));
}

/// A boxed cyan header (doctor.py:652-654 / config.py:8291-8293 shape).
pub fn boxed_header(title: &str) {
    let inner_width = 57usize;
    println!("{}", Color::Cyan.paint(format!("┌{}┐", "─".repeat(inner_width))));
    let pad_total = inner_width.saturating_sub(UnicodeWidthStr::width(title));
    let left = pad_total / 2;
    let right = pad_total - left;
    println!(
        "{}",
        Color::Cyan.paint(format!("│{}{}{}│", " ".repeat(left), title, " ".repeat(right)))
    );
    println!("{}", Color::Cyan.paint(format!("└{}┘", "─".repeat(inner_width))));
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
    let mut tool_starts: HashMap<String, Vec<Instant>> = HashMap::new();
    let mut tool_emoji: HashMap<String, String> = HashMap::new();
    let mut last_tool_line: Option<String> = None;

    let close_reasoning = |open: &mut bool, buf: &mut String| {
        if *open {
            if !buf.is_empty() {
                println!("{}", Color::DarkGray.paint(buf.as_str()));
                buf.clear();
            }
            let w = box_width();
            println!("{}", Color::DarkGray.paint(format!("└{}┘", "─".repeat(w.saturating_sub(2)))));
            *open = false;
        }
    };

    while let Some(ev) = rx.recv().await {
        match ev {
            AgentEvent::ReasoningDelta(d) => {
                if opts.quiet || !opts.show_reasoning {
                    continue;
                }
                // Open the dim reasoning box on the first token (cli.py:5651-5686).
                if !reasoning_open {
                    reasoning_open = true;
                    let w = box_width();
                    let label = " Reasoning ";
                    let fill = w.saturating_sub(2 + label.len());
                    println!(
                        "\n{}",
                        Color::DarkGray.paint(format!("┌─{}{}┐", label, "─".repeat(fill.saturating_sub(1))))
                    );
                }
                reasoning_buf.push_str(&d);
                while let Some(pos) = reasoning_buf.find('\n') {
                    let line: String = reasoning_buf.drain(..=pos).collect();
                    println!("{}", Color::DarkGray.paint(line.trim_end_matches('\n')));
                }
                if reasoning_buf.len() > 80 {
                    println!("{}", Color::DarkGray.paint(reasoning_buf.as_str()));
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
            AgentEvent::ToolStart { name, emoji, .. } => {
                if streamed_any {
                    println!();
                    streamed_any = false;
                }
                close_reasoning(&mut reasoning_open, &mut reasoning_buf);
                tool_starts.entry(name.clone()).or_default().push(Instant::now());
                tool_emoji.insert(name, emoji);
            }
            AgentEvent::ToolProgress { name, progress } => {
                if !opts.quiet && opts.tool_progress == "verbose" {
                    println!("{}", Color::DarkGray.paint(format!("  ┊ {} {}", name, progress)));
                }
            }
            AgentEvent::ToolEnd { name, is_error } => {
                // Stacked scrollback line on completion, honoring the
                // off/new/all/verbose modes (cli.py:10707-10736).
                let duration = tool_starts
                    .get_mut(&name)
                    .and_then(|v| if v.is_empty() { None } else { Some(v.remove(0)) })
                    .map(|t| t.elapsed().as_secs_f64())
                    .unwrap_or(0.0);
                if opts.quiet || opts.tool_progress == "off" {
                    continue;
                }
                if opts.tool_progress == "new" && last_tool_line.as_deref() == Some(name.as_str()) && !is_error {
                    continue;
                }
                last_tool_line = Some(name.clone());
                let emoji = tool_emoji.get(&name).cloned().unwrap_or_default();
                let emoji = if emoji.is_empty() { "⚡".to_string() } else { emoji };
                let line = if is_error {
                    Color::Red
                        .paint(format!("  ✗ {} failed ({})", name, fmt_duration(duration)))
                        .to_string()
                } else {
                    format!(
                        "  {} {} {}",
                        emoji,
                        name,
                        Color::DarkGray.paint(format!("({})", fmt_duration(duration)))
                    )
                };
                println!("{}", line);
            }
            AgentEvent::Notice(msg) => {
                if !opts.quiet {
                    println!("{}", Color::Yellow.paint(format!("  · {}", msg)));
                }
            }
            AgentEvent::Done { final_text: text, .. } => {
                close_reasoning(&mut reasoning_open, &mut reasoning_buf);
                if streamed_any {
                    println!();
                }
                if !text.is_empty() {
                    final_text = text;
                }
                // No per-turn token stat line: upstream prints usage only via
                // /usage and the status bar.
                break;
            }
            AgentEvent::Failed(err) => {
                close_reasoning(&mut reasoning_open, &mut reasoning_buf);
                if streamed_any {
                    println!();
                }
                println!("{}", Color::Red.paint(format!("Error: {}", err)));
                break;
            }
        }
    }
    final_text
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
// Welcome banner (banner.py:580+ content parity, plain-ANSI panel)
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
                // Skip composite/subset sets so each tool lands in its
                // canonical leaf group (banner.py get_toolset_for_tool).
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
        groups.entry(display_toolset_name(owner.unwrap_or("other"))).or_default().push(tool.clone());
    }
    groups.sort_keys();
    groups.into_iter().collect()
}

pub fn banner(info: &BannerInfo) {
    let accent = Color::Yellow;
    let dim = Color::DarkGray;
    let width = box_width().max(40);
    let inner = width - 2;

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "{} {}",
        accent.bold().paint(format!("☤ {} v{}", branding::AGENT_NAME, branding::VERSION)).to_string(),
        dim.paint("· Nous Research heritage").to_string()
    ));

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
        accent.paint(if model_short.is_empty() { "(no model configured)".to_string() } else { model_short }),
        dim.paint(ctx)
    ));
    if info.yolo {
        lines.push(format!(
            "{} {}",
            Color::Red.bold().paint("⚠ YOLO mode"),
            dim.paint("— all approval prompts bypassed")
        ));
    }
    lines.push(dim.paint(info.cwd).to_string());
    lines.push(dim.paint(format!("Session: {}", info.session_id)).to_string());
    lines.push(String::new());

    lines.push(accent.bold().paint("Available Tools").to_string());
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
        lines.push(format!("{} {}", dim.paint(format!("{}:", ts)), joined));
    }
    if groups.len() > shown {
        lines.push(dim.paint(format!("(and {} more toolsets...)", groups.len() - shown)).to_string());
    }
    lines.push(String::new());

    lines.push(accent.bold().paint("Tips").to_string());
    lines.push(dim.paint("• /help for commands · /quit to exit").to_string());
    lines.push(dim.paint("• Ctrl-C interrupts a running turn (press twice to force exit)").to_string());
    lines.push(dim.paint("• joey -z \"...\" answers one-shot questions for scripts").to_string());

    // Panel.
    println!("{}", dim.paint(format!("╭{}╮", "─".repeat(inner))));
    for line in lines {
        let visible = strip_ansi_width(&line);
        let pad = inner.saturating_sub(visible + 2);
        println!(
            "{} {}{} {}",
            dim.paint("│"),
            line,
            " ".repeat(pad),
            dim.paint("│")
        );
    }
    println!("{}", dim.paint(format!("╰{}╯", "─".repeat(inner))));
}

/// Display width of a string ignoring ANSI escape sequences.
fn strip_ansi_width(s: &str) -> usize {
    let mut width = 0usize;
    let mut in_escape = false;
    let mut plain = String::new();
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
    width += UnicodeWidthStr::width(plain.as_str());
    width
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
        println!("Resume this session with:");
        println!("  joey --resume {}{}", info.session_id, profile_flag);
        if let Some(title) = &info.title {
            println!("  joey -c \"{}\"{}", title, profile_flag);
        }
        println!();
        println!("Session:        {}", info.session_id);
        if let Some(title) = &info.title {
            println!("Title:          {}", title);
        }
        println!("Duration:       {}", duration_str);
        println!(
            "Messages:       {} ({} user, {} tool calls)",
            info.message_count, info.user_messages, info.tool_calls
        );
    } else {
        println!("Goodbye! ⚕");
    }
}
