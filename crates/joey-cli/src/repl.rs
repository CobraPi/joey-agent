//! The interactive chat REPL (port of the `cli.py` REPL, line-based).
//!
//! Reads user input with a line editor, dispatches slash commands, and runs
//! agent turns with live streaming. Conversation history persists in the
//! session DB.

use std::path::PathBuf;

use anyhow::Result;
use joey_agent_core::{Agent, AgentConfig};
use joey_core::{Config, Role, SessionDb, StoredMessage};
use joey_providers::Message;
use joey_tools::{ToolContext, ToolRegistry};
use reedline::{DefaultPrompt, DefaultPromptSegment, Reedline, Signal};

use crate::render;

/// Run the interactive REPL.
pub async fn run(config: Config, cwd: PathBuf, resume: Option<String>) -> Result<()> {
    let db = SessionDb::open_default().ok();
    let agent_cfg = AgentConfig::from_config(&config);

    if !has_credentials(&agent_cfg) {
        render::info(
            "No API key found for the current provider. Set one with `joey config set <PROVIDER>_API_KEY <key>` \
             or `joey model`. You can still use slash commands.",
        );
    }

    render::banner(&agent_cfg.model);

    // Establish or resume a session.
    let session_id = match &resume {
        Some(prefix) => db
            .as_ref()
            .and_then(|d| d.resolve_session_id(prefix).ok().flatten())
            .unwrap_or_else(SessionDb::new_session_id),
        None => db
            .as_ref()
            .and_then(|d| d.create_session("cli", Some(&agent_cfg.model), cwd.to_str()).ok())
            .unwrap_or_else(SessionDb::new_session_id),
    };

    let ctx = ToolContext::new(cwd.clone(), config.clone(), session_id.clone());
    let registry = ToolRegistry::with_builtins();
    let mut agent = Agent::new(agent_cfg.clone(), registry, ctx)?;

    // Restore history on resume.
    if resume.is_some() {
        if let Some(d) = &db {
            let restored = restore_history(d, &session_id);
            if !restored.is_empty() {
                render::info(&format!("Resumed session {} ({} messages).", &session_id[..8.min(session_id.len())], restored.len()));
                agent.set_history(restored);
            }
        }
    }

    let mut editor = Reedline::create();
    let prompt = DefaultPrompt::new(
        DefaultPromptSegment::Basic("you".to_string()),
        DefaultPromptSegment::Empty,
    );

    loop {
        let sig = editor.read_line(&prompt);
        let input = match sig {
            Ok(Signal::Success(line)) => line,
            Ok(Signal::CtrlC) => {
                println!("(interrupted — /quit to exit)");
                continue;
            }
            Ok(Signal::CtrlD) => {
                println!("bye");
                break;
            }
            Err(e) => {
                render::error(&format!("input error: {}", e));
                break;
            }
        };
        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        if let Some(cmd) = input.strip_prefix('/') {
            match handle_slash(cmd, &mut agent, &config).await {
                SlashOutcome::Quit => break,
                SlashOutcome::Continue => continue,
            }
        }

        // A chat turn.
        if !agent.client().has_credentials() {
            render::error("no API key configured for this provider — set one with `joey model` or `joey config set`.");
            continue;
        }

        // Persist the user message.
        if let Some(d) = &db {
            let _ = d.add_message(&StoredMessage::new(&session_id, Role::User, input));
        }

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let render_handle = tokio::spawn(render::render_turn(rx));
        let result = agent.run_turn(input, tx).await;
        let _ = render_handle.await;

        // Persist the assistant reply.
        if let Some(d) = &db {
            if !result.final_text.is_empty() {
                let _ = d.add_message(&StoredMessage::new(
                    &session_id,
                    Role::Assistant,
                    &result.final_text,
                ));
            }
        }
        println!();
    }

    if let Some(d) = &db {
        let _ = d.end_session(&session_id, "user_exit");
    }
    Ok(())
}

fn has_credentials(cfg: &AgentConfig) -> bool {
    joey_providers::build_client(&cfg.provider, &cfg.base_url, &cfg.model, cfg.api_key.clone())
        .map(|c| c.has_credentials())
        .unwrap_or(false)
}

fn restore_history(db: &SessionDb, session_id: &str) -> Vec<Message> {
    db.messages(session_id)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| match m.role {
            Role::User => Some(Message::user(m.content)),
            Role::Assistant => Some(Message::assistant(m.content)),
            _ => None,
        })
        .collect()
}

enum SlashOutcome {
    Continue,
    Quit,
}

async fn handle_slash(cmd: &str, agent: &mut Agent, config: &Config) -> SlashOutcome {
    let mut parts = cmd.split_whitespace();
    let name = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();

    match name {
        "quit" | "exit" | "q" => return SlashOutcome::Quit,
        "help" | "commands" => print_help(),
        "new" | "reset" | "clear" => {
            agent.set_history(Vec::new());
            render::success("Started a fresh conversation.");
        }
        "model" => {
            render::info(&format!("Current model: {}", config.model()));
            render::info("Change it with `joey model` or `joey config set model.default <name>`.");
        }
        "tools" => {
            let names = joey_tools::ToolRegistry::with_builtins().names();
            render::info(&format!("Enabled tools: {}", names.join(", ")));
        }
        "toolsets" => {
            for ts in joey_tools::toolsets::names() {
                let desc = joey_tools::toolsets::description(ts).unwrap_or("");
                println!("  {:<12} {}", ts, desc);
            }
        }
        "skills" => {
            let skills = joey_tools::tools::skills_tool::discover();
            if skills.is_empty() {
                render::info("No skills installed.");
            } else {
                for s in skills {
                    println!("  {:<24} {}", s.name, s.description);
                }
            }
        }
        "version" | "v" => render::info(&format!("{} v{}", joey_core::branding::AGENT_NAME, joey_core::branding::VERSION)),
        "reasoning" => {
            render::info(&format!("reasoning level: {:?}", rest.first()));
        }
        other => render::error(&format!("unknown command: /{} (try /help)", other)),
    }
    SlashOutcome::Continue
}

fn print_help() {
    println!("Slash commands:");
    let cmds = [
        ("/help", "show this help"),
        ("/new", "start a fresh conversation (alias /reset, /clear)"),
        ("/model", "show the current model"),
        ("/tools", "list enabled tools"),
        ("/toolsets", "list available toolsets"),
        ("/skills", "list installed skills"),
        ("/reasoning <level>", "show/set reasoning effort"),
        ("/version", "show version"),
        ("/quit", "exit (alias /exit, /q)"),
    ];
    for (c, d) in cmds {
        println!("  {:<20} {}", c, d);
    }
}
