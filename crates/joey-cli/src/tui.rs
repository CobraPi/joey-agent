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
        // Populate status-bar fields (cwd, provider, model) on the AppState
        // so the bar isn't blank.
        tui.app_mut().provider = provider_name.to_string();
        tui.app_mut().model = model_name.to_string();
        tui.app_mut().cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

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

    // Populate the Tab agent roster from the OMO registry, resolved against the
    // connected provider + active model (T140). Without this, Tab opens an
    // empty picker that renders nothing — the visible "Tab does nothing" bug.
    populate_agent_roster(&mut tui, &agent);

    // Single-query mode: run one turn, then hand the answer back to the
    // normal terminal (the alternate screen vanishes on exit).
    if let Some(query) = &opts.query {
        let mut queued = VecDeque::new();
        apply_intent_gate(&mut tui, &mut agent, query);
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

/// Build the Tab-picker agent roster from the OMO registry, resolved against
/// the currently connected provider + active model (T140). The first entry is
/// always "Default" (the live joey-agent); followed by each available primary
/// OMO agent in canonical Tab order.
fn populate_agent_roster(tui: &mut Tui, agent: &Agent) {
    let available = joey_omo::AvailableModelSet::from_connected(
        agent.client().profile(),
        agent.model(),
    );
    let overrides = joey_omo::agents::registry::ModelOverrides::new();
    let registry = joey_omo::AgentRegistry::build(available, &overrides);
    tui.app_mut().agent_roster =
        joey_tui::widgets::build_agent_roster_from_registry(&registry);
    // Stamp the Default entry's runtime so the picker shows the live model.
    if let Some(default) = tui.app_mut().agent_roster.first_mut() {
        default.resolved_model = Some(agent.model().to_string());
    }
}

/// Look up the agent's model requirement in the OMO registry and switch the
/// live runtime to it (T033/BC-015). Returns a human-readable notice.
/// "Default" reverts to the model the session started with (saved on the App).
fn switch_agent(tui: &mut Tui, agent: &mut Agent, agent_name: &str) {
    // "Default" → restore the session's original model. We stash it in the
    // App the first time we switch AWAY from it.
    if agent_name == "default" {
        let target = tui.app().default_model.clone();
        let target = match target {
            Some(m) if !m.is_empty() => m,
            _ => {
                // Nothing saved (shouldn't happen post-startup) — stay put.
                tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "Already on the default agent".into(),
                    kind: NoticeKind::Info,
                });
                return;
            }
        };
        // Clear the OMO identity — Default uses the joey-agent base prompt.
        apply_model_switch(tui, agent, "default", &target, "auto", None);
        return;
    }

    // Rebuild a registry to read the agent's resolved model + provider.
    let available = joey_omo::AvailableModelSet::from_connected(
        agent.client().profile(),
        agent.model(),
    );
    let overrides = joey_omo::agents::registry::ModelOverrides::new();
    let registry = joey_omo::AgentRegistry::build(available, &overrides);
    let Some(omo_agent) = registry.get(agent_name) else {
        tui.app_mut().push_item(TranscriptItem::Notice {
            text: format!("Unknown agent: {agent_name}"),
            kind: NoticeKind::Warning,
        });
        return;
    };
    let Some(model) = omo_agent.resolved_model.clone() else {
        tui.app_mut().push_item(TranscriptItem::Notice {
            text: format!(
                "{} is unavailable with the current provider/model",
                omo_agent.display_name
            ),
            kind: NoticeKind::Warning,
        });
        return;
    };
    // Build the OMO identity prompt for this agent (model-family-aware variant).
    // This is what makes Tab-switching actually activate the agent's persona
    // rather than just swapping the model (BC-004/FR-006).
    let identity = joey_omo::dispatch_system_prompt(agent_name, &model);
    // Let the provider auto-resolve from the model (provider="auto"), matching
    // how an explicit `--model` is handled at startup.
    apply_model_switch(
        tui,
        agent,
        &omo_agent.display_name,
        &model,
        "auto",
        Some(identity),
    );
}

/// Apply the model swap, surfacing the result as a transcript notice and
/// syncing the TUI's model label. When `identity` is Some, the OMO agent's
/// system prompt is injected as the agent identity overlay (BC-004/FR-006);
/// None clears it (reverting to the default joey-agent identity).
fn apply_model_switch(
    tui: &mut Tui,
    agent: &mut Agent,
    display_name: &str,
    model: &str,
    provider: &str,
    identity: Option<String>,
) {
    // Stash the session's original model the first time we switch away.
    if tui.app().default_model.is_none() {
        tui.app_mut().default_model = Some(agent.model().to_string());
    }
    match agent.switch_model(provider, "", model, None) {
        Ok(msg) => {
            // Inject (or clear) the OMO agent identity AFTER switch_model
            // succeeds. switch_model clears the ultrawork overlay (BC-016);
            // the identity is a separate layer that persists until the next
            // agent switch.
            agent.set_agent_identity(identity);
            tui.app_mut().model = agent.model().to_string();
            tui.app_mut().provider = agent.provider_name().to_string();
            tui.app_mut().push_item(TranscriptItem::Notice {
                text: format!("{msg} — agent mode: {display_name}"),
                kind: NoticeKind::Success,
            });
        }
        Err(e) => {
            tui.app_mut().push_item(TranscriptItem::Error {
                text: format!("Could not switch to {display_name}: {e}"),
            });
        }
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
                // T033/BC-015: rebuild the runtime onto the chosen agent's model.
                switch_agent(tui, agent, &agent_name);
            }
            TuiAction::Submit(text) => {
                if text.trim_start().starts_with('/') {
                    if let SlashAction::Quit = handle_slash_tui(&text, tui, agent) {
                        return Ok(());
                    }
                    continue;
                }
                apply_intent_gate(tui, agent, &text);
                run_turn(tui, agent, &text, &mut queued).await;
                // BC-016: honor an agent switch requested mid-turn, now that
                // the turn's mutable borrow of `agent` has been released.
                if let Some(agent_name) = tui.app_mut().pending_agent_switch.take() {
                    switch_agent(tui, agent, &agent_name);
                }
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
                    Ok(Event::Mouse(m)) => {
                        use crossterm::event::MouseEventKind;
                        match m.kind {
                            MouseEventKind::ScrollUp => {
                                tui.handle_mouse_scroll(m.row, m.column, true);
                            }
                            MouseEventKind::ScrollDown => {
                                tui.handle_mouse_scroll(m.row, m.column, false);
                            }
                            _ => {}
                        }
                    }
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
                                        // BC-016: a live turn holds a mutable
                                        // borrow of the agent, and the switch
                                        // targets the NEXT turn anyway. Stash
                                        // it; applied once the turn ends.
                                        tui.app_mut().pending_agent_switch =
                                            Some(agent_name.clone());
                                        tui.app_mut().push_item(TranscriptItem::Notice {
                                            text: format!(
                                                "⧗ will switch to {} next turn",
                                                agent_name
                                            ),
                                            kind: NoticeKind::Busy,
                                        });
                                    }
                                }
                            }
                        }
                        Ok(Event::Paste(s)) => tui.input.insert_str(&s),
                        Ok(Event::Resize(w, h)) => tui.resize(w, h),
                        Ok(Event::Mouse(m)) => {
                            use crossterm::event::MouseEventKind;
                            match m.kind {
                                MouseEventKind::ScrollUp => {
                                    tui.handle_mouse_scroll(m.row, m.column, true);
                                }
                                MouseEventKind::ScrollDown => {
                                    tui.handle_mouse_scroll(m.row, m.column, false);
                                }
                                _ => {}
                            }
                        }
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

/// IntentGate (FR-022/FR-024, T141): scan the user's message for OMO
/// keyword triggers before the turn runs. When `ultrawork`/`ulw` is
/// detected and the active agent supports it, inject the ultrawork
/// instruction set as an overlay on the system prompt. Prometheus and
/// unknown agents silently ignore ultrawork (BC-025).
///
/// The active agent name is resolved from the TUI's agent roster +
/// active_agent_index (populated by `populate_agent_roster` / Tab cycling).
/// When no roster entry exists (e.g. the default agent at startup), the
/// agent is treated as "default", which is ultrawork-valid.
fn apply_intent_gate(tui: &mut Tui, agent: &mut Agent, message: &str) {
    let Some(keyword) = joey_omo::detect_keyword(message) else {
        return;
    };

    // Resolve the active agent's canonical name from the Tab picker state.
    // Clone to release the immutable borrow of `tui` before we mutate it.
    let active_agent_name = tui
        .app()
        .agent_roster
        .get(tui.app().active_agent_index)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| "default".to_string());

    match keyword {
        joey_omo::KeywordType::Ultrawork | joey_omo::KeywordType::HyperplanUltraworkCombo => {
            if let Some(_announcement) =
                joey_omo::check_ultrawork_activation(keyword, &active_agent_name)
            {
                // Inject the ultrawork overlay (model-family-aware variant).
                let overlay = joey_omo::ultrawork_prompt(agent.model());
                agent.set_extra_instructions(Some(overlay));
                tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "⚡ ULTRAWORK MODE ENABLED!".into(),
                    kind: NoticeKind::Success,
                });
            } else {
                // Prometheus or other incompatible agent — silently ignored.
                tui.app_mut().push_item(TranscriptItem::Notice {
                    text: format!(
                        "ultrawork ignored — {} is a read-only planner",
                        active_agent_name
                    ),
                    kind: NoticeKind::Warning,
                });
            }
        }
        joey_omo::KeywordType::Hyperplan => {
            tui.app_mut().push_item(TranscriptItem::Notice {
                text: "⚡ HYPERPLAN MODE ENABLED!".into(),
                kind: NoticeKind::Info,
            });
        }
        joey_omo::KeywordType::Team => {
            tui.app_mut().push_item(TranscriptItem::Notice {
                text: "TEAM MODE ENABLED!".into(),
                kind: NoticeKind::Info,
            });
        }
    }
}

/// Slash-command handling inside the TUI. A few commands work natively;
/// the rest answer honestly instead of pretending to run.
fn handle_slash_tui(input: &str, tui: &mut Tui, agent: &Agent) -> SlashAction {
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
            "agents" => {
                tui.app_mut().agent_picker_open = true;
            }
            "model" => {
                tui.app_mut().push_item(TranscriptItem::Notice {
                    text: format!("Current model: {} — use `joey model` outside the TUI to change", agent.model()),
                    kind: NoticeKind::Info,
                });
            }
            "status" => {
                let (sid, mdl, tok_prompt, tok_comp, tok_iter, msg_count) = {
                    let app = tui.app();
                    (
                        app.session_id.clone(),
                        app.model.clone(),
                        app.tokens.prompt,
                        app.tokens.completion,
                        app.tokens.iterations,
                        app.transcript.len(),
                    )
                };
                tui.app_mut().push_item(TranscriptItem::Notice {
                    text: format!(
                        "session {} | model {} | tokens in:{} out:{} api:{} | messages {}",
                        sid, mdl, tok_prompt, tok_comp, tok_iter, msg_count,
                    ),
                    kind: NoticeKind::Info,
                });
            }
            "timestamps" | "ts" => {
                tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "Timestamps are always shown inline in the TUI transcript".into(),
                    kind: NoticeKind::Info,
                });
            }
            "tools" => {
                tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "Use `joey tools list` outside the TUI to manage tools".into(),
                    kind: NoticeKind::Info,
                });
            }
            "new" | "reset" => {
                tui.app_mut().transcript.clear();
                tui.app_mut().scroll = None;
                tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "New session — history cleared (start a new joey session for a fresh ID)".into(),
                    kind: NoticeKind::Info,
                });
            }
            "verbose" => {
                tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "Tool progress is always shown in the TUI transcript".into(),
                    kind: NoticeKind::Info,
                });
            }
            "changes" => {
                use joey_tools::file_tracker::FileTracker;
                let summary = FileTracker::change_summary();
                if summary.files_modified == 0 {
                    tui.app_mut().push_item(TranscriptItem::Notice {
                        text: "No files changed in this session.".into(),
                        kind: NoticeKind::Info,
                    });
                } else {
                    let paths = summary.modified_paths.join(", ");
                    tui.app_mut().push_item(TranscriptItem::Notice {
                        text: format!(
                            "{} file(s) read, {} modified: {}",
                            summary.files_read, summary.files_modified, paths,
                        ),
                        kind: NoticeKind::Info,
                    });
                }
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
