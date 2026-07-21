//! Terminal rendering for the CLI: streaming output, tool progress, banners
//! (port of the rich/prompt_toolkit rendering surface in `cli.py`, adapted to
//! a line-based terminal UI).

use std::io::Write;

use joey_agent_core::AgentEvent;
use joey_core::branding;
use nu_ansi_term::Color;
use tokio::sync::mpsc;

/// Consume agent events and render them live to stdout. Returns the final text.
pub async fn render_turn(mut rx: mpsc::UnboundedReceiver<AgentEvent>) -> String {
    let mut final_text = String::new();
    let mut streamed_any = false;
    let mut in_reasoning = false;

    while let Some(ev) = rx.recv().await {
        match ev {
            AgentEvent::ContentDelta(d) => {
                if in_reasoning {
                    println!();
                    in_reasoning = false;
                }
                print!("{}", d);
                let _ = std::io::stdout().flush();
                streamed_any = true;
            }
            AgentEvent::ReasoningDelta(d) => {
                if !in_reasoning {
                    print!("{}", Color::DarkGray.paint("\n[thinking] "));
                    in_reasoning = true;
                }
                print!("{}", Color::DarkGray.paint(d));
                let _ = std::io::stdout().flush();
            }
            AgentEvent::AssistantMessage(text) => {
                final_text = text;
                // If nothing streamed (non-streaming mode), print the message now.
                if !streamed_any {
                    println!("{}", final_text);
                }
            }
            AgentEvent::ToolStart { name, emoji, summary } => {
                if streamed_any || in_reasoning {
                    println!();
                    streamed_any = false;
                    in_reasoning = false;
                }
                let label = if emoji.is_empty() {
                    name.clone()
                } else {
                    format!("{} {}", emoji, name)
                };
                let line = if summary.is_empty() {
                    format!("  {}", label)
                } else {
                    format!("  {} {}", label, Color::DarkGray.paint(summary))
                };
                println!("{}", Color::Cyan.paint(line));
            }
            AgentEvent::ToolEnd { name, is_error } => {
                if is_error {
                    println!("{}", Color::Red.paint(format!("  ✗ {} failed", name)));
                }
            }
            AgentEvent::Notice(msg) => {
                println!("{}", Color::Yellow.paint(format!("  · {}", msg)));
            }
            AgentEvent::Done { final_text: text, usage } => {
                if streamed_any || in_reasoning {
                    println!();
                }
                if !text.is_empty() {
                    final_text = text;
                }
                if usage.total_tokens > 0 {
                    let stat = format!(
                        "  [{}↑ {}↓ tokens]",
                        usage.prompt_tokens, usage.completion_tokens
                    );
                    println!("{}", Color::DarkGray.paint(stat));
                }
                break;
            }
            AgentEvent::Failed(err) => {
                println!("{}", Color::Red.paint(format!("\nError: {}", err)));
                break;
            }
        }
    }
    final_text
}

/// Print the startup banner.
pub fn banner(model: &str) {
    let title = Color::Cyan.bold().paint(format!("{} ☤", branding::AGENT_NAME));
    println!("\n{}  {}", title, Color::DarkGray.paint(format!("v{}", branding::VERSION)));
    println!(
        "{}",
        Color::DarkGray.paint(format!("model: {}   ·   /help for commands, /quit to exit", model))
    );
    println!();
}

/// Print an assistant label before non-streamed output.
pub fn info(msg: &str) {
    println!("{}", Color::DarkGray.paint(msg));
}

pub fn error(msg: &str) {
    eprintln!("{}", Color::Red.paint(format!("error: {}", msg)));
}

pub fn success(msg: &str) {
    println!("{}", Color::Green.paint(msg));
}

/// A green check mark for doctor-style output.
pub fn check_mark() -> String {
    Color::Green.paint("✓").to_string()
}

/// A yellow warning mark for doctor-style output.
pub fn warn_mark() -> String {
    Color::Yellow.paint("!").to_string()
}
