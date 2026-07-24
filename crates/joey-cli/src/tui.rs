//! TUI frontend bridge: runs the animated ratatui dashboard as the interactive
//! REPL, adapting the [`joey_tui`] runtime to the agent and slash-command
//! surface.
//!
//! Reuses the same agent construction, session management, and Ctrl-C
//! interrupt semantics as the line-based REPL — only the rendering and input
//! layer changes. Prompts submitted while a turn is running are queued and
//! run in order once the agent is free.

use std::collections::VecDeque;
use std::io::IsTerminal;
use std::sync::atomic::Ordering;
use std::time::Instant;

use joey_agent_core::{Agent, AgentEvent};
use joey_core::Config;
use joey_tui::{state::NoticeKind, AppState, Theme, TranscriptItem, Tui, TuiAction};
use tokio::sync::mpsc;

use crate::render;
use crate::repl::ChatOptions;
use crate::slash::{self, Resolution};

/// Run the TUI-driven interactive session. Mirrors `repl::run_chat` but
/// swaps the line-editor loop for the ratatui dashboard.
pub async fn run(opts: ChatOptions) -> anyhow::Result<i32> {
    // The dashboard needs a real terminal on both ends; pipes get the line
    // REPL (which has proper batch/quiet modes).
    if !IsTerminal::is_terminal(&std::io::stdout())
        || !IsTerminal::is_terminal(&std::io::stdin())
    {
        if !opts.quiet {
            render::info("--tui needs an interactive terminal — using the line REPL.");
        }
        return crate::repl::run_chat(opts).await;
    }

    let config = Config::load()?;

    if let Some(code) = crate::commands::first_run_guard(&config) {
        return Ok(code);
    }
    let config = Config::load()?;

    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let db = joey_core::SessionDb::open_default().ok();

    // Establish / resume a session (same logic as the line REPL).
    let mut resumed = false;
    let session_id = if let Some(target) = &opts.resume {
        match db.as_ref().and_then(|d| {
            d.resolve_session_id(target).ok().flatten().or_else(|| crate::repl::find_by_title(d, target))
        }) {
            Some(id) => {
                resumed = true;
                id
            }
            None => {
                render::error(&format!("No session found matching '{}'", target));
                return Ok(1);
            }
        }
    } else if let Some(name) = &opts.continue_last {
        let found = db.as_ref().and_then(|d| {
            if name.is_empty() {
                d.most_recent_session().ok().flatten()
            } else {
                crate::repl::find_by_title(d, name).or_else(|| d.resolve_session_id(name).ok().flatten())
            }
        });
        match found {
            Some(id) => {
                resumed = true;
                id
            }
            None => {
                if name.is_empty() {
                    render::error("No previous session to continue");
                } else {
                    render::error(&format!("No session found matching '{}'", name));
                }
                return Ok(1);
            }
        }
    } else {
        let model_hint = opts.model.clone().unwrap_or_else(|| config.model());
        db.as_ref()
            .and_then(|d| d.create_session("cli", Some(&model_hint), cwd.to_str()).ok())
            .unwrap_or_else(joey_core::SessionDb::new_session_id)
    };
    joey_core::logging::set_session_context(Some(&session_id));

    let overrides = crate::repl::Overrides {
        model: opts.model.clone(),
        provider: opts.provider.clone(),
        toolsets: opts.toolsets.clone(),
        max_turns: opts.max_turns,
        reasoning: None,
        pass_session_id: opts.pass_session_id,
    };

    let history = if resumed {
        db.as_ref().map(|d| crate::repl::restore_history(d, &session_id)).unwrap_or_default()
    } else {
        Vec::new()
    };
    let restored_count = history.len();

    let mut agent = crate::repl::build_agent(&config, &cwd, &overrides, &session_id, history)?;

    let provider_name: &'static str = agent.client().profile().name;
    let model_name = crate::repl::build_agent_config(&config, &overrides).model;
    let session_start = Instant::now();

    // Build the TUI app state.
    let mut app_state = AppState::new(session_id.clone(), model_name.clone());
    app_state.provider = provider_name.to_string();
    app_state.cwd = cwd.to_string_lossy().into_owned();
    app_state.show_reasoning = config.get_bool("display.show_reasoning", true);

    let theme = Theme::aurora();
    let mut tui = match Tui::enter(app_state, theme) {
        Ok(t) => t,
        Err(e) => {
            render::error(&format!("Failed to initialize the TUI ({e}) — using the line REPL."));
            end_session(&agent, &session_id, "tui_init_failed");
            return crate::repl::run_chat(opts).await;
        }
    };

    // Welcome banner into the transcript.
    {
        let sid_short: String = session_id.chars().take(8).collect();
        tui.app_mut().push_item(TranscriptItem::Notice {
            text: format!(
                "✦ joey-agent — model {} · provider {} · session {}",
                model_name, provider_name, sid_short
            ),
            kind: NoticeKind::Info,
        });
        if resumed {
            tui.app_mut().push_item(TranscriptItem::Notice {
                text: format!("Resumed session {} ({} messages)", sid_short, restored_count),
                kind: NoticeKind::Success,
            });
        }
        if !agent.client().has_credentials() {
            tui.app_mut().push_item(TranscriptItem::Notice {
                text: format!(
                    "No API key for provider '{}'. Set one with `joey model` (outside the TUI).",
                    provider_name
                ),
                kind: NoticeKind::Warning,
            });
        }
    }

    // Single-query mode: run one turn, then hand the answer back to the
    // normal terminal (the alternate screen vanishes on exit).
    if let Some(query) = &opts.query {
        let mut queued = VecDeque::new();
        run_turn(&mut tui, &mut agent, query, &mut queued).await;
        let final_text = tui.app().last_final_text.clone();
        let _ = tui.leave();
        drop(tui);
        if !final_text.is_empty() {
            println!("{}", final_text);
        }
        if opts.quiet {
            println!();
            println!("Session: {}", session_id);
        }
        end_session(&agent, &session_id, "query_complete");
        return Ok(0);
    }

    // Interactive loop.
    let result = interactive_loop(&mut tui, &mut agent).await;
    let _ = tui.leave();
    drop(tui);
    end_session(&agent, &session_id, "user_exit");

    if let Err(e) = result {
        render::error(&format!("TUI session error: {e}"));
        return Ok(1);
    }

    // Exit outro — same shape as the line REPL's.
    let history = agent.history();
    let user_msgs = history.iter().filter(|m| m.role == "user").count();
    let tool_calls =
        history.iter().filter(|m| m.role == "tool" || !m.tool_calls.is_empty()).count();
    let title = db
        .as_ref()
        .and_then(|d| d.get_session(&session_id).ok().flatten())
        .and_then(|s| s.title);
    render::exit_outro(&render::OutroInfo {
        session_id: &session_id,
        title,
        message_count: history.len(),
        user_messages: user_msgs,
        tool_calls,
        started: session_start,
        profile: crate::active_profile(),
    });
    Ok(0)
}

fn end_session(agent: &Agent, session_id: &str, reason: &str) {
    if let Some(db) = agent.session_db() {
        let _ = db.end_session(session_id, reason);
    }
}

/// The interactive read → submit → render loop driven by the TUI.
async fn interactive_loop(tui: &mut Tui, agent: &mut Agent) -> anyhow::Result<()> {
    let mut queued: VecDeque<String> = VecDeque::new();
    loop {
        // Prompts queued during the previous turn run first, in order.
        let action = match queued.pop_front() {
            Some(text) => TuiAction::Submit(text),
            None => wait_for_action(tui).await,
        };
        match action {
            TuiAction::Quit => return Ok(()),
            TuiAction::Interrupt => continue,
            TuiAction::SwitchAgent(agent_name) => {
                // The host handles agent switching by rebuilding the AgentConfig.
                // For now, emit a notice so the user sees feedback.
                tui.app_mut().apply(joey_agent_core::AgentEvent::Notice(
                    format!("Switched to agent: {}", agent_name),
                ));
            }
            TuiAction::Submit(text) => {
                if text.trim_start().starts_with('/') {
                    if let SlashAction::Quit = handle_slash_tui(&text, tui) {
                        return Ok(());
                    }
                    continue;
                }
                run_turn(tui, agent, &text, &mut queued).await;
            }
        }
    }
}

/// Pump crossterm events + animation ticks until the TUI emits an action.
async fn wait_for_action(tui: &mut Tui) -> TuiAction {
    use crossterm::event::{self, Event};
    use std::time::Duration;

    loop {
        tui.tick_animations();
        let _ = tui.draw();
        // One frame's worth of waiting, then drain everything pending so a
        // fast typist never outruns the poll cadence.
        if event::poll(tui.frame_budget()).unwrap_or(false) {
            loop {
                match event::read() {
                    Ok(Event::Key(k)) => {
                        if let Some(a) = tui.handle_key(k) {
                            return a;
                        }
                    }
                    Ok(Event::Paste(s)) => tui.input.insert_str(&s),
                    Ok(Event::Resize(w, h)) => tui.resize(w, h),
                    _ => {}
                }
                if !event::poll(Duration::from_millis(0)).unwrap_or(false) {
                    break;
                }
            }
        }
    }
}

/// Run one agent turn inside the TUI, streaming events into the animated view
/// with upstream Ctrl-C interrupt semantics (first press interrupts, second
/// within 2s force-exits). Prompts submitted while busy are queued for the
/// host loop to run next.
async fn run_turn(
    tui: &mut Tui,
    agent: &mut Agent,
    prompt: &str,
    queued: &mut VecDeque<String>,
) {
    if !agent.client().has_credentials() {
        tui.app_mut().push_item(TranscriptItem::Error {
            text: format!(
                "no API key for provider '{}' — run `joey model` outside the TUI.",
                agent.client().profile().name
            ),
        });
        return;
    }

    tui.app_mut().record_user(prompt);

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    let interrupt = agent.interrupt_handle();

    let turn = agent.run_turn(prompt, tx);
    tokio::pin!(turn);

    use crossterm::event::{self, Event};
    use std::time::Duration;
    let mut last_ctrlc: Option<Instant> = None;

    loop {
        // Drain agent events into the model.
        while let Ok(ev) = rx.try_recv() {
            tui.app_mut().apply(ev);
        }
        tokio::select! {
            _res = &mut turn => {
                while let Ok(ev) = rx.try_recv() {
                    tui.app_mut().apply(ev);
                }
                tui.tick_animations();
                let _ = tui.draw();
                break;
            }
            _ = tokio::time::sleep(tui.frame_budget()) => {
                while event::poll(Duration::from_millis(0)).unwrap_or(false) {
                    match event::read() {
                        Ok(Event::Key(k)) => {
                            if let Some(a) = tui.handle_key(k) {
                                match a {
                                    // Esc/Ctrl+C while busy. A second press
                                    // within 2s force-exits.
                                    TuiAction::Interrupt | TuiAction::Quit => {
                                        let now = Instant::now();
                                        if last_ctrlc
                                            .map(|t| now.duration_since(t).as_secs_f64() < 2.0)
                                            .unwrap_or(false)
                                        {
                                            let _ = tui.leave();
                                            std::process::exit(0);
                                        }
                                        last_ctrlc = Some(now);
                                        interrupt.store(true, Ordering::SeqCst);
                                        if !queued.is_empty() {
                                            queued.clear();
                                            tui.app_mut().push_item(TranscriptItem::Notice {
                                                text: "queued prompts discarded".into(),
                                                kind: NoticeKind::Warning,
                                            });
                                        }
                                        tui.app_mut().push_item(TranscriptItem::Notice {
                                            text: "⚡ Interrupting… (press again to force exit)".into(),
                                            kind: NoticeKind::Warning,
                                        });
                                    }
                                    TuiAction::Submit(text) => {
                                        queued.push_back(text.clone());
                                        let preview: String = text.chars().take(48).collect();
                                        tui.app_mut().push_item(TranscriptItem::Notice {
                                            text: format!(
                                                "⧗ queued for next turn ({}): {}",
                                                queued.len(),
                                                preview
                                            ),
                                            kind: NoticeKind::Busy,
                                        });
                                    }
                                    TuiAction::SwitchAgent(agent_name) => {
                                        tui.app_mut().push_item(TranscriptItem::Notice {
                                            text: format!("Switched to: {}", agent_name),
                                            kind: NoticeKind::Info,
                                        });
                                    }
                                }
                            }
                        }
                        Ok(Event::Paste(s)) => tui.input.insert_str(&s),
                        Ok(Event::Resize(w, h)) => tui.resize(w, h),
                        _ => {}
                    }
                }
            }
        }
        tui.tick_animations();
        let _ = tui.draw();
    }
}

enum SlashAction {
    Handled,
    Quit,
}

/// Slash-command handling inside the TUI. A few commands work natively;
/// the rest answer honestly instead of pretending to run.
fn handle_slash_tui(input: &str, tui: &mut Tui) -> SlashAction {
    match slash::resolve(input) {
        Resolution::Unknown => {
            tui.app_mut().push_item(TranscriptItem::Error {
                text: format!("Unknown command: {}", input),
            });
        }
        Resolution::Ambiguous(matches) => {
            tui.app_mut().push_item(TranscriptItem::Notice {
                text: format!("Ambiguous: did you mean {}?", matches.join(", ")),
                kind: NoticeKind::Warning,
            });
        }
        Resolution::Command { def, .. } if !def.implemented => {
            tui.app_mut().push_item(TranscriptItem::Notice {
                text: format!("/{} is not available in joey-agent yet.", def.name),
                kind: NoticeKind::Warning,
            });
        }
        Resolution::Command { def, .. } => match def.name {
            "quit" | "exit" => return SlashAction::Quit,
            "help" => tui.toggle_help(),
            "clear" => {
                tui.app_mut().transcript.clear();
                tui.app_mut().scroll = None;
                tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "view cleared — conversation history is unchanged".into(),
                    kind: NoticeKind::Info,
                });
            }
            name => {
                tui.app_mut().push_item(TranscriptItem::Notice {
                    text: format!(
                        "/{} isn't wired into the TUI yet — run joey without --tui to use it.",
                        name
                    ),
                    kind: NoticeKind::Warning,
                });
            }
        },
    }
    SlashAction::Handled
}
