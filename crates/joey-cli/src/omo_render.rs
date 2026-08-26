//! OMO CLI inline activity renderer (T144, FR-040).
//!
//! Prints one-line ANSI-colored summaries as orchestration events arrive,
//! matching the TUI's activity panel output for users running joey in CLI
//! mode (no TUI). Routes AgentEvents to compact, readable lines.

#![allow(dead_code)]

use joey_agent_core::AgentEvent;

/// Render an orchestration AgentEvent as an optional one-line summary.
/// Returns None for events that don't warrant CLI inline output.
pub fn render_event(event: &AgentEvent) -> Option<String> {
    match event {
        AgentEvent::SubagentSpawn {
            goal,
            model,
            toolset_summary,
            ..
        } => {
            let goal_preview: String = goal.chars().take(48).collect();
            Some(format!(
                "  {} {} {} ({})",
                colorize("⟳", Ansi::Cyan),
                colorize("spawned", Ansi::DarkGray),
                colorize(&goal_preview, Ansi::White),
                colorize(&format!("model: {}, tools: {}", model, toolset_summary), Ansi::DarkGray),
            ))
        }
        AgentEvent::SubagentComplete {
            goal,
            success,
            summary_preview,
            duration_secs,
            ..
        } => {
            let icon = if *success { "✓" } else { "✗" };
            let color = if *success { Ansi::Green } else { Ansi::Red };
            let goal_preview: String = goal.chars().take(40).collect();
            let summary_preview: String = summary_preview.chars().take(60).collect();
            Some(format!(
                "  {} {} {} ({:.1}s) {}",
                colorize(icon, color),
                colorize("done", Ansi::DarkGray),
                colorize(&goal_preview, Ansi::White),
                duration_secs,
                colorize(&summary_preview, Ansi::DarkGray),
            ))
        }
        AgentEvent::SubagentFailed {
            id: _,
            goal,
            error,
            duration_secs,
        } => {
            let goal_preview: String = goal.chars().take(40).collect();
            Some(format!(
                "  {} {} {} ({:.1}s) {}",
                colorize("✗", Ansi::Red),
                colorize("failed", Ansi::DarkGray),
                colorize(&goal_preview, Ansi::White),
                duration_secs,
                colorize(error, Ansi::Red),
            ))
        }
        AgentEvent::DelegationBatchComplete {
            total,
            succeeded,
            failed,
            total_duration_secs,
        } => {
            let icon = if *failed > 0 { "⚠" } else { "✓" };
            let color = if *failed > 0 { Ansi::Yellow } else { Ansi::Green };
            Some(format!(
                "  {} batch: {}/{} done, {} failed ({:.1}s)",
                colorize(icon, color),
                succeeded,
                total,
                failed,
                total_duration_secs,
            ))
        }
        AgentEvent::CategoryDelegation { category, model } => Some(format!(
            "  {} [{}] → {}",
            colorize("◎", Ansi::Magenta),
            colorize(category, Ansi::White),
            colorize(model, Ansi::DarkGray),
        )),
        AgentEvent::AgentModeChanged { agent_name, model } => Some(format!(
            "  {} agent: {} [{}]",
            colorize("★", Ansi::Yellow),
            colorize(agent_name, Ansi::White),
            colorize(model, Ansi::DarkGray),
        )),
        AgentEvent::BoulderWorkStarted { plan_name, .. } => Some(format!(
            "  {} work started: {}",
            colorize("⛰", Ansi::Green),
            colorize(plan_name, Ansi::White),
        )),
        AgentEvent::BoulderWorkCompleted { plan_name, .. } => Some(format!(
            "  {} work done: {}",
            colorize("⛰", Ansi::Green),
            colorize(plan_name, Ansi::White),
        )),
        AgentEvent::GoalSet { objective } => Some(format!(
            "  {} goal: {}",
            colorize("◎", Ansi::Cyan),
            colorize(&objective.chars().take(60).collect::<String>(), Ansi::White),
        )),
        AgentEvent::GoalCleared => Some(format!(
            "  {} goal cleared",
            colorize("◎", Ansi::DarkGray),
        )),
        AgentEvent::WisdomAccumulated { learnings_count } => Some(format!(
            "  {} {} learnings",
            colorize("♾", Ansi::Purple),
            learnings_count,
        )),
        AgentEvent::FallbackActivated {
            from_model,
            to_model,
            ..
        } => Some(format!(
            "  {} fallback: {} → {}",
            colorize("↻", Ansi::Yellow),
            colorize(from_model, Ansi::DarkGray),
            colorize(to_model, Ansi::White),
        )),
        // Events not relevant to inline CLI activity display.
        _ => None,
    }
}

/// Print an event to stdout if it produces a summary line.
pub fn print_event(event: &AgentEvent) {
    if let Some(line) = render_event(event) {
        println!("{}", line);
    }
}

// ── Minimal ANSI color helpers ──────────────────────────────────────────────

#[derive(Clone, Copy)]
enum Ansi {
    Red,
    Green,
    Yellow,
    Cyan,
    Magenta,
    Purple,
    White,
    DarkGray,
}

fn colorize(text: &str, color: Ansi) -> String {
    // Respect NO_COLOR / non-TTY: strip escape codes if not a terminal.
    if std::env::var("NO_COLOR").is_ok() || !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        return text.to_string();
    }
    let code = match color {
        Ansi::Red => "31",
        Ansi::Green => "32",
        Ansi::Yellow => "33",
        Ansi::Cyan => "36",
        Ansi::Magenta => "35",
        Ansi::Purple => "95",
        Ansi::White => "37",
        Ansi::DarkGray => "90",
    };
    format!("\x1b[{}m{}\x1b[0m", code, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_subagent_spawn() {
        let event = AgentEvent::SubagentSpawn {
            id: 1,
            goal: "search codebase".into(),
            model: "glm-5".into(),
            toolset_summary: "read,search".into(),
            depth: 1,
        };
        let line = render_event(&event);
        assert!(line.is_some());
        assert!(line.unwrap().contains("spawned"));
    }

    #[test]
    fn render_subagent_complete() {
        let event = AgentEvent::SubagentComplete {
            id: 1,
            goal: "search codebase".into(),
            success: true,
            summary_preview: "found 3 files".into(),
            token_usage: joey_providers::Usage::default(),
            duration_secs: 4.2,
        };
        let line = render_event(&event);
        assert!(line.is_some());
        let l = line.unwrap();
        assert!(l.contains("done"));
        assert!(l.contains("4.2s"));
    }

    #[test]
    fn render_category_delegation() {
        let event = AgentEvent::CategoryDelegation {
            category: "deep".into(),
            model: "opus-4".into(),
        };
        let line = render_event(&event);
        assert!(line.unwrap().contains("deep"));
    }

    #[test]
    fn render_irrelevant_event_returns_none() {
        let event = AgentEvent::TurnStart { max_iterations: 50 };
        assert!(render_event(&event).is_none());
    }
}
