//! System prompt construction (port of `agent/prompt_builder.py` core).
//!
//! Assembles the base identity, environment context, memory/user files, and the
//! `<available_skills>` index. Branded for joey-agent.

use std::fmt::Write as _;

use joey_core::branding;
use joey_tools::ToolContext;

/// Build the system prompt for a turn.
pub fn build_system_prompt(ctx: &ToolContext, model: &str) -> String {
    let mut p = String::new();

    // ── Identity ──
    let _ = writeln!(
        p,
        "You are {name}, an autonomous AI agent that helps with software \
         engineering and general tasks. You operate through tools: you can read and \
         write files, run shell commands, search the web, and manage your own memory \
         and skills.",
        name = branding::AGENT_NAME
    );
    p.push('\n');

    // ── Environment ──
    let _ = writeln!(p, "<environment>");
    let _ = writeln!(p, "Working directory: {}", ctx.cwd().display());
    let _ = writeln!(p, "Model: {}", model);
    let _ = writeln!(p, "Date: {}", joey_core::time::now().format("%Y-%m-%d"));
    let os = std::env::consts::OS;
    let _ = writeln!(p, "Platform: {}", os);
    let _ = writeln!(p, "</environment>");
    p.push('\n');

    // ── Operating guidance ──
    p.push_str(
        "Guidelines:\n\
         - When you need to act on the world, call a tool. Do not describe an action \
         you could take with a tool — take it.\n\
         - Prefer the dedicated file tools (read_file, write_file, patch, search_files) \
         over shell equivalents.\n\
         - Keep responses concise. Lead with the outcome.\n\
         - After finishing a task, stop calling tools and give a short summary.\n",
    );
    p.push('\n');

    // ── Memory files ──
    if let Some(mem) = read_memory_file("MEMORY.md") {
        let _ = writeln!(p, "<memory>\n{}\n</memory>", mem);
        p.push('\n');
    }
    if let Some(user) = read_memory_file("USER.md") {
        let _ = writeln!(p, "<user_profile>\n{}\n</user_profile>", user);
        p.push('\n');
    }

    // ── Skills index (tier 1) ──
    let skills = joey_tools::tools::skills_tool::discover();
    if !skills.is_empty() {
        p.push_str("<available_skills>\n");
        p.push_str(
            "Before acting on a task a skill covers, load it with skill_view. \
             Available skills:\n",
        );
        for s in skills {
            let _ = writeln!(p, "- {}: {}", s.name, s.description);
        }
        p.push_str("</available_skills>\n");
    }

    p
}

fn read_memory_file(name: &str) -> Option<String> {
    let path = joey_core::constants::joey_home().join("memories").join(name);
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
