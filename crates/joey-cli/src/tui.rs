//! TUI frontend bridge: runs the animated ratatui dashboard as the interactive
//! REPL, adapting the [`joey_tui`] runtime to the agent and slash-command
//! surface.
//!
//! Reuses the same agent construction, session management, and Ctrl-C
//! interrupt semantics as the line-based REPL — only the rendering and input
//! layer changes.

use std::io::IsTerminal;
use std::sync::atomic::Ordering;

use joey_agent_core::{Agent, AgentEvent};
use joey_core::Config;
use joey_tui::{state::NoticeKind, AppState, RunMode, Theme, Tui, TuiAction, TranscriptItem};
use tokio::sync::mpsc;

use crate::render;
use crate::repl::ChatOptions;
use crate::slash::{self, Resolution};

/// Run the TUI-driven interactive session. Mirrors `repl::run_chat` but
/// swaps the line-editor loop for the ratatui dashboard.
pub async fn run(opts: ChatOptions) -> anyhow::Result<i32> {
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
                eprintln!("No session found matching '{}'", target);
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
            None => return Ok(0),
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

    let agent = crate::repl::build_agent(&config, &cwd, &overrides, &session_id, history)?;

    if !opts.quiet && !IsTerminal::is_terminal(&std::io::stdout()) {
        render::info("Starting TUI on a non-TTY stdout — output may be garbled.");
    }

    let provider_name: &'static str = agent.client().profile().name;
    let model_name = crate::repl::build_agent_config(&config, &overrides).model;

    // Build the TUI app state.
    let mut app_state = AppState::new(session_id.clone(), model_name.clone());
    app_state.provider = provider_name.to_string();
    app_state.cwd = cwd.to_string_lossy().into_owned();
    app_state.show_reasoning = config.get_bool("display.show_reasoning", true);

    let theme = Theme::aurora();
    let mut tui = match Tui::enter(app_state, theme) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Failed to initialize TUI: {e}");
            eprintln!("Falling back to the line-based REPL. Use `joey` without --tui.");
            return Ok(1);
        }
    };

    // Welcome banner into the transcript.
    {
        let sid_short = &session_id[..session_id.len().min(8)];
        tui.app_mut().push_item(TranscriptItem::Notice {
            text: format!(
                "✦ joey-agent — model {} · provider {} · session {}",
                model_name, provider_name, sid_short
            ),
            kind: NoticeKind::Info,
        });
        if resumed {
            tui.app_mut().push_item(TranscriptItem::Notice {
                text: format!("Resumed session {}", sid_short),
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

    // Single-query mode: run one turn then exit.
    if let Some(query) = &opts.query {
        let mut agent = agent;
        run_turn(&mut tui, &mut agent, query).await;
        tui.leave().ok();
        return Ok(0);
    }

    // Interactive loop: the agent is owned mutably here.
    let mut agent = agent;
    let result = interactive_loop(&mut tui, &mut agent).await;
    let _ = tui.leave();

    if let Err(e) = result {
        eprintln!("TUI session error: {e}");
        return Ok(1);
    }
    Ok(0)
}

/// The interactive read → submit → render loop driven by the TUI.
async fn interactive_loop(tui: &mut Tui, agent: &mut Agent) -> anyhow::Result<()> {
    loop {
        // Wait for the next user action (Submit / Quit).
        let action = wait_for_action(tui).await;
        match action {
            TuiAction::Quit => return Ok(()),
            TuiAction::Interrupt => continue,
            TuiAction::Submit(text) => {
                if text.trim_start().starts_with('/') {
                    handle_slash_tui(&text, tui).await;
                    continue;
                }
                run_turn(tui, agent, &text).await;
            }
        }
    }
}

/// Drain crossterm events + animation ticks until the TUI emits an action.
async fn wait_for_action(tui: &mut Tui) -> TuiAction {
    use crossterm::event::{self, Event};
    use std::time::Duration;

    let frame_budget = Duration::from_millis(16);
    loop {
        tui.tick_animations();
        let _ = tui.draw();
        if event::poll(frame_budget).unwrap_or(false) {
            match event::read() {
                Ok(Event::Key(k)) => {
                    if let Some(a) = tui.handle_key(k) {
                        return a;
                    }
                }
                Ok(Event::Resize(w, h)) => {
                    tui.resize(w, h);
                }
                _ => {}
            }
        }
    }
}

/// Run one agent turn inside the TUI, streaming events into the animated view
/// with upstream Ctrl-C interrupt semantics (first press interrupts, second
/// within 2s force-exits).
async fn run_turn(tui: &mut Tui, agent: &mut Agent, prompt: &str) {
    if !agent.client().has_credentials() {
        tui.app_mut().push_item(TranscriptItem::Error {
            text: format!(
                "no API key for provider '{}' — run `joey model` outside the TUI.",
                agent.client().profile().name
            ),
        });
        return;
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
    let interrupt = agent.interrupt_handle();

    let turn = agent.run_turn(prompt, tx);
    tokio::pin!(turn);

    use crossterm::event::{self, Event};
    use std::time::{Duration, Instant as StdInstant};
    let mut last_ctrlc: Option<StdInstant> = None;
    let frame_budget = Duration::from_millis(16);

    loop {
        // Drain events into the model.
        while let Ok(ev) = rx.try_recv() {
            tui.app_mut().apply(ev);
        }
        tokio::select! {
            _res = &mut turn => {
                while let Ok(ev) = rx.try_recv() {
                    tui.app_mut().apply(ev);
                }
                break;
            }
            _ = tokio::time::sleep(frame_budget) => {
                // Pump input: interrupt / quit handling.
                while event::poll(Duration::from_millis(0)).unwrap_or(false) {
                    if let Ok(Event::Key(k)) = event::read() {
                        if let Some(a) = tui.handle_key(k) {
                            match a {
                                TuiAction::Interrupt => {
                                    let now = StdInstant::now();
                                    if last_ctrlc
                                        .map(|t| now.duration_since(t).as_secs_f64() < 2.0)
                                        .unwrap_or(false)
                                    {
                                        // Force exit.
                                        let _ = tui.leave();
                                        std::process::exit(0);
                                    }
                                    last_ctrlc = Some(now);
                                    interrupt.store(true, Ordering::SeqCst);
                                    tui.app_mut().push_item(TranscriptItem::Notice {
                                        text: "⚡ Interrupting… (Ctrl+C again to force exit)".into(),
                                        kind: NoticeKind::Warning,
                                    });
                                }
                                TuiAction::Quit => {
                                    tui.app_mut().mode = RunMode::Quitting;
                                    let _ = tui.draw();
                                    let _ = tui.leave();
                                    std::process::exit(0);
                                }
                                _ => {}
                            }
                        }
                    } else if let Ok(Event::Resize(w, h)) = event::read() {
                        tui.resize(w, h);
                    }
                }
            }
        }
        tui.tick_animations();
        let _ = tui.draw();
    }
}

/// Slash-command handling inside the TUI.
async fn handle_slash_tui(input: &str, tui: &mut Tui) {
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
        Resolution::Command { def, rest } => {
            if matches!(def.name.as_ref(), "quit" | "exit") {
                tui.app_mut().mode = RunMode::Quitting;
                let _ = tui.draw();
                let _ = tui.leave();
                std::process::exit(0);
            }
            tui.app_mut().push_item(TranscriptItem::Notice {
                text: format!("/{} {}", def.name, rest),
                kind: NoticeKind::Info,
            });
        }
    }
}

/// Build a minimal ReplState placeholder. The TUI owns its own agent; the
/// ReplState here is only for checkpoint/timer parity and is unused in the
/// turn path. Exposed via `pub(super)` so `repl` can reach helpers if needed.
#[allow(dead_code)]
pub(super) fn placeholder_state(
    _config: Config,
    _cwd: std::path::PathBuf,
    _session_id: String,
) {
    // No-op: the TUI path does not require a ReplState. Kept as a stub for
    // future checkpoint/timer parity wiring.
}
