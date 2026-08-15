//! TUI frontend bridge: runs the animated ratatui dashboard as the interactive
//! REPL, adapting the [`joey_tui`] runtime to the agent and slash-command
//! surface.
//!
//! Reuses the same agent construction, session management, and Ctrl-C
//! interrupt semantics as the line-based REPL — only the rendering and input
//! layer changes. Prompts submitted while a turn is running are queued and
//! run in order once the agent is free.

use std::io::IsTerminal;
use std::time::Instant;

use joey_agent_core::Agent;
use joey_core::Config;
use joey_tui::{state::NoticeKind, AppState, SlashCommandInfo, Theme, TranscriptItem, Tui, TuiAction};

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

    let agent = crate::repl::build_agent(&config, &cwd, &overrides, &session_id, history)?;

    let provider_name: &'static str = agent.client().profile().name;
    let model_name = crate::repl::build_agent_config(&config, &overrides).model;
    let session_start = Instant::now();

    // Build the TUI app state.
    let mut app_state = AppState::new(session_id.clone(), model_name.clone());
    app_state.provider = provider_name.to_string();
    app_state.cwd = cwd.to_string_lossy().into_owned();
    app_state.show_reasoning = config.get_bool("display.show_reasoning", true);
    // Slash-command popup catalog: inject the shared registry (single source
    // of truth in crate::slash — the TUI crate cannot depend on joey-cli).
    app_state.slash_commands = crate::slash::REGISTRY
        .iter()
        .map(|c| SlashCommandInfo {
            name: c.name.to_string(),
            aliases: c.aliases.iter().map(|a| a.to_string()).collect(),
            description: c.description.to_string(),
            args_hint: c.args_hint.to_string(),
            implemented: c.implemented,
        })
        .collect();
    // Shared input history with the CLI surface (~/.joey/.joey_history —
    // reedline-compatible format, so entries made in either surface are
    // recallable in both).
    app_state.input_history = crate::history::load();
    // Feature 015 (NeuroCode): when the engine is enabled in config,
    // build_agent wires it into the agent's turn loop. Surface the state in
    // the TUI immediately (status badge + bottom-right live context panel).
    if crate::neurocode_wiring::try_build_engine(&config).is_some() {
        app_state.apply(joey_agent_core::AgentEvent::NeuroCodeActive { active: true });
        app_state.push_item(TranscriptItem::Notice {
            text: "⚡ NeuroCode active — dependency-aware context injection is ON (live feed: bottom-right panel)".into(),
            kind: NoticeKind::Success,
        });
    }

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

    // ── Engine-actor decoupling ────────────────────────────────────────
    // The agent moves into a dedicated engine task; this task only renders
    // and dispatches commands. A hung turn or tool can never freeze the GUI
    // (Ctrl-C escalation: interrupt ��� force-kill + fresh engine).
    let engine_spec = crate::engine::EngineSpec {
        config: config.clone(),
        cwd: cwd.clone(),
        overrides: overrides.clone(),
        session_id: session_id.clone(),
    };
    let (ev_tx, ev_rx) = tokio::sync::mpsc::unbounded_channel::<crate::engine::EngineEvent>();
    let (engine, interrupt) = crate::engine::spawn_engine(agent, ev_tx);

    // Single-query mode: submit, pump until done, hand the answer back.
    if let Some(query) = &opts.query {
        let mut session = TuiSession {
            tui,
            ev_rx,
            engine: Some(engine),
            interrupt,
            engine_spec,
            busy: false,
            last_ctrlc: None,
            queued_forwarded: 0,
        };
        session.submit(query.clone());
        loop {
            match pump_one(&mut session).await {
                Some(PumpOutcome::TurnDone) | Some(PumpOutcome::EngineGone) => break,
                _ => {}
            }
        }
        let final_text = session.tui.app().last_final_text.clone();
        let _ = session.tui.leave();
        drop(session.tui);
        if !final_text.is_empty() {
            println!("{}", final_text);
        }
        if opts.quiet {
            println!();
            println!("Session: {}", session_id);
        }
        end_session_by_id(&session_id, "query_complete");
        return Ok(0);
    }

    // Interactive loop.
    let session = TuiSession {
        tui,
        ev_rx,
        engine: Some(engine),
        interrupt,
        engine_spec,
        busy: false,
        last_ctrlc: None,
        queued_forwarded: 0,
    };
    let (result, outro) = interactive_loop(session).await;

    if let Err(e) = result {
        render::error(&format!("TUI session error: {e}"));
        return Ok(1);
    }

    // Exit outro — same shape as the line REPL's.
    render::exit_outro(&render::OutroInfo {
        session_id: &session_id,
        title: outro.title,
        message_count: outro.message_count,
        user_messages: outro.user_messages,
        tool_calls: outro.tool_calls,
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

/// End a session by id when the Agent lives inside the engine task (the DB
/// is reopened here on the UI side; SQLite WAL handles the concurrency).
fn end_session_by_id(session_id: &str, reason: &str) {
    if let Ok(db) = joey_core::SessionDb::open_default() {
        let _ = db.end_session(session_id, reason);
    }
}

/// Build the Tab-picker agent roster from the OMO registry, resolved against
/// the currently connected provider + active model (T140). The first entry is
/// always "Default" (the live joey-agent); followed by each available primary
/// OMO agent in canonical Tab order.
fn populate_agent_roster(tui: &mut Tui, agent: &Agent) {
    let available = joey_omo::AvailableModelSet::from_connected_with_catalog(
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

/// The interactive read → submit → render loop driven by the TUI.
/// The decoupled TUI session: UI task state only. All compute lives in the
/// engine task (see engine.rs); this struct owns the Tui, the engine handle,
/// and the busy/kill bookkeeping.
pub struct TuiSession {
    pub tui: Tui,
    pub ev_rx: tokio::sync::mpsc::UnboundedReceiver<crate::engine::EngineEvent>,
    pub engine: Option<crate::engine::EngineHandle>,
    pub interrupt: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub engine_spec: crate::engine::EngineSpec,
    /// A turn or heavy job is running on the engine.
    pub busy: bool,
    pub last_ctrlc: Option<Instant>,
    /// Prompts forwarded to a busy engine (queued engine-side). If the
    /// engine is force-killed, those queued prompts die with it — the
    /// engine owns its queue and they cannot be recovered. This counter
    /// lets the UI report exactly how many were discarded (fix: silent
    /// queue loss on ForceKill). Reset on every TurnFinished.
    pub queued_forwarded: usize,
}

impl TuiSession {
    /// Returns true when the app should quit (slash /quit).
    pub fn submit(&mut self, prompt: String) -> bool {
        if prompt.trim().is_empty() {
            return false;
        }
        // Record to the shared CLI/TUI history file (the App's in-memory
        // copy was already recorded by the input handler).
        crate::history::record(&prompt);
        if prompt.trim_start().starts_with('/') {
            return self.handle_slash(&prompt);
        }
        let active_agent = self
            .tui
            .app()
            .agent_roster
            .get(self.tui.app().active_agent_index)
            .map(|a| a.name.clone())
            .unwrap_or_else(|| "default".to_string());
        // Pending OMO context (from /start-work) prepends once.
        let turn_text = match self.tui.app_mut().pending_context_injection.take() {
            Some(ctx) if !prompt.is_empty() => format!("{ctx}\n{prompt}"),
            Some(ctx) => ctx,
            None => prompt,
        };
        self.tui.app_mut().record_user(&turn_text);
        // Only flip busy when there's actually an engine to run the turn —
        // otherwise the app wedges in Busy with no TurnFinished coming.
        if self.engine.is_some() {
            self.busy = true;
            // Flip the App to Busy immediately so is_busy()-gated keys
            // (Ctrl-C escalation, input styling) apply before TurnStart
            // arrives.
            self.tui.app_mut().mode = joey_tui::state::RunMode::Busy;
        }
        if let Some(engine) = &self.engine {
            engine.send(crate::engine::EngineCommand::Submit {
                prompt: turn_text,
                active_agent,
            });
        }
        false
    }

    /// /start-work: activate Atlas on a plan. The OMO bookkeeping +
    /// context injection stay UI-side; the Atlas identity switch goes to
    /// the engine (it mutates the agent).
    fn handle_start_work(&mut self, args: &str) {
        let cwd = std::path::PathBuf::from(self.tui.app().cwd.clone());
        let omo_dir = cwd.join(".omo");
        let session_id = self.tui.app().session_id.clone();
        let plan_name_opt: Option<&str> = if args.trim().is_empty() { None } else { Some(args.trim()) };
        match joey_omo::start_work(&omo_dir, &session_id, plan_name_opt) {
            Ok(result) => {
                if let Some(engine) = &self.engine {
                    engine.send(crate::engine::EngineCommand::SwitchAgent("atlas".into()));
                }
                self.tui.app_mut().pending_context_injection = Some(result.context_injection);
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: format!("🪨 Started work (agent: {}) — Atlas mode active", result.agent),
                    kind: NoticeKind::Success,
                });
            }
            Err(e) => {
                self.tui.app_mut().push_item(TranscriptItem::Error {
                    text: format!("start-work failed: {e}"),
                });
            }
        }
    }

    /// Interrupt escalation: 1st press = cooperative interrupt; 2nd press
    /// within 2s = FORCE KILL — abandon the engine task (it may be stuck in
    /// a blocking tool; the interrupt flag is set but a truly hung tool
    /// ignores it) and spawn a fresh engine with a rebuilt agent.
    pub fn interrupt_pressed(&mut self) {
        let now = Instant::now();
        let second = self
            .last_ctrlc
            .map(|t| now.duration_since(t).as_secs_f64() < 2.0)
            .unwrap_or(false);
        if second {
            self.force_kill_engine("user force-kill (double Ctrl-C)");
            return;
        }
        self.last_ctrlc = Some(now);
        self.interrupt.store(true, std::sync::atomic::Ordering::SeqCst);
        self.tui.app_mut().push_item(TranscriptItem::Notice {
            text: "⚡ Interrupting… (press Ctrl-C again to KILL & restart the engine)".into(),
            kind: NoticeKind::Warning,
        });
    }

    /// Abandon the current engine (its stuck task leaks until process exit —
    /// deliberate: the GUI must survive no matter what) and build a fresh
    /// engine around a rebuilt agent (history restored from the session DB).
    pub fn force_kill_engine(&mut self, reason: &str) {
        // Prompts forwarded while busy lived in the killed engine's queue
        // and die with it — the honest fix is to tell the user (they can't
        // be recovered; the engine owns its queue).
        let discarded = self.queued_forwarded;
        if let Some(engine) = self.engine.take() {
            // Signal, then abandon: drop the command channel + detach task.
            engine.send(crate::engine::EngineCommand::ForceKill);
            engine.abandon();
        }
        self.queued_forwarded = 0;
        // The fresh engine must not inherit a stale 2s Ctrl-C kill window
        // (a second Ctrl-C right after restart would instantly kill it).
        self.last_ctrlc = None;
        // Flush any stale engine events so they can't leak into the new one.
        while let Ok(_) = self.ev_rx.try_recv() {}
        if discarded > 0 {
            self.tui.app_mut().push_item(TranscriptItem::Error {
                text: format!(
                    "{discarded} queued prompt(s) discarded with the killed engine — resubmit them if still needed"
                ),
            });
        }
        // Fresh engine, fresh agent.
        let agent = match self.engine_spec.build_agent() {
            Ok(a) => a,
            Err(e) => {
                self.tui.app_mut().push_item(TranscriptItem::Error {
                    text: format!("engine restart failed: {e}"),
                });
                self.engine = None;
                self.busy = false;
                self.tui.app_mut().mode = joey_tui::state::RunMode::Input;
                return;
            }
        };
        let (ev_tx, ev_rx) = tokio::sync::mpsc::unbounded_channel::<crate::engine::EngineEvent>();
        let (engine, interrupt) = crate::engine::spawn_engine(agent, ev_tx);
        self.ev_rx = ev_rx;
        self.engine = Some(engine);
        self.interrupt = interrupt;
        self.busy = false;
        // The killed turn never sends Done — reset the RunMode ourselves.
        self.tui.app_mut().mode = joey_tui::state::RunMode::Input;
        self.tui.app_mut().push_item(TranscriptItem::Notice {
            text: format!("☠ engine killed & restarted ({reason}) — GUI stayed live"),
            kind: NoticeKind::Warning,
        });
    }
}

/// Outcome of one pump step.
pub enum PumpOutcome {
    TurnDone,
    HeavyDone,
    EngineGone,
    /// A terminal action fired (caller decides; only used by pump_one).
    Action(TuiAction),
}

/// The unified UI pump: ONE select over engine events / terminal input /
/// frame timer. The UI task never awaits engine compute — this is the core
/// of the GUI/compute decoupling.
async fn pump_one(session: &mut TuiSession) -> Option<PumpOutcome> {
    use crossterm::event::{self, Event};
    use std::time::Duration;

    session.tui.tick_animations();
    let _ = session.tui.draw();

    let ev = tokio::select! {
        ev = session.ev_rx.recv() => ev,
        _ = tokio::time::sleep(session.tui.frame_budget()) => {
            // Frame tick: drain all pending terminal input (non-blocking).
            while event::poll(Duration::from_millis(0)).unwrap_or(false) {
                match event::read() {
                    Ok(Event::Key(k)) => {
                        if let Some(a) = session.tui.handle_key(k) {
                            return Some(PumpOutcome::Action(a));
                        }
                    }
                    Ok(Event::Paste(s)) => {
                        session.tui.input.insert_str(&s);
                        let text = session.tui.input.text();
                        session.tui.app_mut().update_slash_menu(&text);
                    }
                    Ok(Event::Resize(w, h)) => session.tui.resize(w, h),
                    Ok(Event::Mouse(m)) => {
                        use crossterm::event::{MouseEventKind, MouseButton};
                        match m.kind {
                            MouseEventKind::ScrollUp => {
                                session.tui.handle_mouse_scroll(m.row, m.column, true);
                            }
                            MouseEventKind::ScrollDown => {
                                session.tui.handle_mouse_scroll(m.row, m.column, false);
                            }
                            MouseEventKind::Down(MouseButton::Left) => {
                                session.tui.handle_mouse_click(m.row, m.column);
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            return None;
        }
    };

    match ev {
        Some(crate::engine::EngineEvent::Agent(agent_ev)) => {
            session.tui.app_mut().apply(agent_ev);
            None
        }
        Some(crate::engine::EngineEvent::Notice(text)) => {
            session.tui.app_mut().push_item(TranscriptItem::Notice {
                text,
                kind: NoticeKind::Info,
            });
            None
        }
        Some(crate::engine::EngineEvent::TurnFinished { .. }) => {
            session.busy = false;
            // Anything forwarded while busy has now been drained by the
            // engine's internal queue — reset the loss counter.
            session.queued_forwarded = 0;
            // Honor a pending agent switch now that the turn is done.
            if let Some(agent_name) = session.tui.app_mut().pending_agent_switch.take() {
                if let Some(engine) = &session.engine {
                    engine.send(crate::engine::EngineCommand::SwitchAgent(agent_name));
                }
            }
            Some(PumpOutcome::TurnDone)
        }
        Some(crate::engine::EngineEvent::HeavyJobFinished { label: _, text }) => {
            session.busy = false;
            // A heavy job never sends AgentEvent::Done, so reset the
            // RunMode ourselves — without this the status bar stays BUSY
            // forever after /neurocode completes.
            session.tui.app_mut().mode = joey_tui::state::RunMode::Input;
            for line in text.lines() {
                session.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: line.to_string(),
                    kind: NoticeKind::Info,
                });
            }
            Some(PumpOutcome::HeavyDone)
        }
        Some(crate::engine::EngineEvent::AgentSwitched { model, provider, notice, .. }) => {
            session.tui.app_mut().model = model;
            session.tui.app_mut().provider = provider;
            session.tui.app_mut().push_item(TranscriptItem::Notice {
                text: notice,
                kind: NoticeKind::Success,
            });
            // Refresh the Default roster entry's model stamp.
            let model_now = session.tui.app().model.clone();
            if let Some(default) = session.tui.app_mut().agent_roster.first_mut() {
                default.resolved_model = Some(model_now);
            }
            None
        }
        Some(crate::engine::EngineEvent::EngineGone(msg)) => {
            session.busy = false;
            // Engine death never sends Done — reset the RunMode ourselves
            // or the status bar stays BUSY forever.
            session.tui.app_mut().mode = joey_tui::state::RunMode::Input;
            session.tui.app_mut().push_item(TranscriptItem::Error { text: msg });
            Some(PumpOutcome::EngineGone)
        }
        None => {
            session.busy = false;
            session.tui.app_mut().mode = joey_tui::state::RunMode::Input;
            Some(PumpOutcome::EngineGone)
        }
    }
}

/// Snapshot for the exit outro (counts gathered engine-side at quit).
pub struct OutroSnapshot {
    pub title: Option<String>,
    pub message_count: usize,
    pub user_messages: usize,
    pub tool_calls: usize,
}

/// The interactive loop: pure UI. Pumps events, dispatches actions; all
/// compute is engine-side. On quit it leaves the terminal, ends the
/// session, and gathers the outro snapshot from the session DB.
async fn interactive_loop(mut session: TuiSession) -> (anyhow::Result<()>, OutroSnapshot) {
    loop {
        match pump_one(&mut session).await {
            Some(PumpOutcome::Action(TuiAction::Quit)) => break,
            Some(PumpOutcome::Action(TuiAction::Interrupt)) => {
                session.interrupt_pressed();
            }
            Some(PumpOutcome::Action(TuiAction::Submit(text))) => {
                // Read-only slash commands are safe (and useful) while a
                // turn runs — answer them inline instead of forwarding a
                // raw "/status" prompt to the engine as if it were chat.
                let light_slash = session.busy
                    && text.trim_start().starts_with('/')
                    && slash_is_light(&text);
                if light_slash {
                    session.handle_slash(&text);
                } else if session.busy && text.trim_start().starts_with("/queue") {
                    // Hermes /queue: queue for the NEXT turn — never
                    // interrupts the running turn.
                    let queued_text = text.trim_start_matches("/queue").trim().to_string();
                    if queued_text.is_empty() {
                        session.tui.app_mut().push_item(TranscriptItem::Notice {
                            text: "Usage: /queue <prompt>".into(),
                            kind: NoticeKind::Warning,
                        });
                    } else if let Some(engine) = &session.engine {
                        let active_agent = session
                            .tui
                            .app()
                            .agent_roster
                            .get(session.tui.app().active_agent_index)
                            .map(|a| a.name.clone())
                            .unwrap_or_else(|| "default".to_string());
                        engine.send(crate::engine::EngineCommand::Submit {
                            prompt: queued_text.clone(),
                            active_agent,
                        });
                        session.queued_forwarded += 1;
                        let preview: String = queued_text.chars().take(48).collect();
                        session.tui.app_mut().push_item(TranscriptItem::Notice {
                            text: format!("⧗ queued for next turn: {preview}"),
                            kind: NoticeKind::Busy,
                        });
                    }
                } else if session.busy && text.trim_start().starts_with("/steer") {
                    // Hermes parity: /steer mid-turn injects WITHOUT
                    // interrupting — the message lands inside the marker on
                    // the current turn's next tool result.
                    let steer_text = text.trim_start_matches("/steer").trim().to_string();
                    if steer_text.is_empty() {
                        session.tui.app_mut().push_item(TranscriptItem::Notice {
                            text: "Usage: /steer <message>".into(),
                            kind: NoticeKind::Warning,
                        });
                    } else if let Some(engine) = &session.engine {
                        engine.send(crate::engine::EngineCommand::Steer(steer_text));
                    }
                } else if session.busy {
                    // Hermes parity (busy_input_mode: interrupt — the
                    // upstream default): a plain message mid-turn
                    // INTERRUPTS the running turn; the engine unwinds it
                    // and runs the new message as the next turn.
                    let preview: String = text.chars().take(48).collect();
                    session.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: format!("⚡ interrupting — your message runs next: {preview}"),
                        kind: NoticeKind::Warning,
                    });
                    session.interrupt.store(true, std::sync::atomic::Ordering::SeqCst);
                    if let Some(engine) = &session.engine {
                        // Queued AFTER the interrupt lands; the engine runs
                        // it when the turn unwinds.
                        let active_agent = session
                            .tui
                            .app()
                            .agent_roster
                            .get(session.tui.app().active_agent_index)
                            .map(|a| a.name.clone())
                            .unwrap_or_else(|| "default".to_string());
                        engine.send(crate::engine::EngineCommand::Submit {
                            prompt: text,
                            active_agent,
                        });
                        // Still tracked: a force-kill before the turn
                        // unwinds would drop this queued prompt too.
                        session.queued_forwarded += 1;
                    }
                } else if session.submit(text) {
                    break;
                }
            }
            Some(PumpOutcome::Action(TuiAction::SwitchAgent(name))) => {
                if session.busy {
                    session.tui.app_mut().pending_agent_switch = Some(name.clone());
                    session.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: format!("⧗ will switch to {name} next turn"),
                        kind: NoticeKind::Busy,
                    });
                } else if let Some(engine) = &session.engine {
                    engine.send(crate::engine::EngineCommand::SwitchAgent(name));
                }
            }
            Some(PumpOutcome::Action(TuiAction::CopyItem(idx))) => {
                let text = session.tui.app().transcript.get(idx).and_then(|item| match item {
                    TranscriptItem::User { text }
                    | TranscriptItem::Assistant { text }
                    | TranscriptItem::Reasoning { text, .. } => Some(text.clone()),
                    TranscriptItem::Tool { full_result, result_preview, .. } => {
                        full_result.clone().or_else(|| Some(result_preview.clone()))
                    }
                    TranscriptItem::FileDiff { path, lines, .. } => {
                        Some(format!("# {}\n{}", path, lines.join("\n")))
                    }
                    TranscriptItem::Notice { text, .. }
                    | TranscriptItem::Error { text } => Some(text.clone()),
                });
                if let Some(t) = text {
                    match crate::clipboard::copy_to_clipboard(&t) {
                        Ok(()) => session.tui.app_mut().push_item(TranscriptItem::Notice {
                            text: format!("✓ Copied {} chars to clipboard", t.chars().count()),
                            kind: NoticeKind::Success,
                        }),
                        Err(e) => session.tui.app_mut().push_item(TranscriptItem::Error {
                            text: format!("Copy failed: {e}"),
                        }),
                    }
                }
            }
            _ => {}
        }
    }

    // Quit: leave the terminal, end the session, snapshot the outro.
    let session_id = session.engine_spec.session_id.clone();
    let _ = session.tui.leave();
    end_session_by_id(&session_id, "user_exit");
    let (message_count, user_messages, tool_calls, title) = outro_stats(&session_id);
    (
        Ok(()),
        OutroSnapshot { title, message_count, user_messages, tool_calls },
    )
}

/// Gather exit-outro stats from the session DB (the Agent lives engine-side).
fn outro_stats(session_id: &str) -> (usize, usize, usize, Option<String>) {
    let Ok(db) = joey_core::SessionDb::open_default() else {
        return (0, 0, 0, None);
    };
    let msgs = db.messages(session_id).unwrap_or_default();
    let user_messages = msgs.iter().filter(|m| m.role == joey_core::state::Role::User).count();
    let tool_calls = msgs
        .iter()
        .filter(|m| m.role == joey_core::state::Role::Tool || m.tool_calls.is_some())
        .count();
    let title = db
        .get_session(session_id)
        .ok()
        .flatten()
        .and_then(|s| s.title);
    (msgs.len(), user_messages, tool_calls, title)
}

/// Slash-command handling inside the TUI. A few commands work natively;
/// the rest answer honestly instead of pretending to run.
///
/// Async so heavy subcommands (`/neurocode index` parses the whole tree and
/// bulk-upserts to SQLite) can run on `spawn_blocking` while the UI keeps
/// rendering (see `run_neurocode_tui`) — the GUI no longer freezes until the
/// command completes.
impl TuiSession {
/// Slash-command handling on the UI side. Light commands render inline;
/// heavy ones (neurocode index/ingest…) are dispatched to the engine task.
/// Returns true when the app should quit.
pub fn handle_slash(&mut self, input: &str) -> bool {
    match slash::resolve(input) {
        Resolution::Unknown => {
            self.tui.app_mut().push_item(TranscriptItem::Error {
                text: format!("Unknown command: {}", input),
            });
        }
        Resolution::Ambiguous(matches) => {
            self.tui.app_mut().push_item(TranscriptItem::Notice {
                text: format!("Ambiguous: did you mean {}?", matches.join(", ")),
                kind: NoticeKind::Warning,
            });
        }
        Resolution::Command { def, .. } if !def.implemented => {
            self.tui.app_mut().push_item(TranscriptItem::Notice {
                text: format!("/{} is not available in joey-agent yet.", def.name),
                kind: NoticeKind::Warning,
            });
        }
        Resolution::Command { def, .. } => match def.name {
            "quit" | "exit" => return true,
            "help" => self.tui.toggle_help(),
            // T114: /start-work — activate Atlas on a plan (CLI/TUI parity).
            "start-work" => {
                let args = slash_args_after(input, "start-work");
                self.handle_start_work(args);
            }
            // T114: /goal — persistent per-session objective (CLI/TUI parity).
            "goal" => {
                let args = slash_args_after(input, "goal");
                handle_goal_tui(&mut self.tui, args);
            }
            "clear" => {
                self.tui.app_mut().transcript.clear();
                self.tui.app_mut().scroll = None;
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "view cleared — conversation history is unchanged".into(),
                    kind: NoticeKind::Info,
                });
            }
            "agents" => {
                self.tui.app_mut().agent_picker_open = true;
            }
            "model" => {
                let model_now = self.tui.app().model.clone();
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: format!("Current model: {model_now} — use `joey model` outside the TUI to change"),
                    kind: NoticeKind::Info,
                });
            }
            "status" => {
                let (sid, mdl, tok_prompt, tok_comp, tok_iter, msg_count) = {
                    let app = self.tui.app();
                    (
                        app.session_id.clone(),
                        app.model.clone(),
                        app.tokens.prompt,
                        app.tokens.completion,
                        app.tokens.iterations,
                        app.transcript.len(),
                    )
                };
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: format!(
                        "session {} | model {} | tokens in:{} out:{} api:{} | messages {}",
                        sid, mdl, tok_prompt, tok_comp, tok_iter, msg_count,
                    ),
                    kind: NoticeKind::Info,
                });
            }
            "timestamps" | "ts" => {
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "Timestamps are always shown inline in the TUI transcript".into(),
                    kind: NoticeKind::Info,
                });
            }
            "tools" => {
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "Use `joey tools list` outside the TUI to manage tools".into(),
                    kind: NoticeKind::Info,
                });
            }
            "new" | "reset" => {
                self.tui.app_mut().transcript.clear();
                self.tui.app_mut().scroll = None;
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "New session — history cleared (start a new joey session for a fresh ID)".into(),
                    kind: NoticeKind::Info,
                });
            }
            "verbose" => {
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "Tool progress is always shown in the TUI transcript".into(),
                    kind: NoticeKind::Info,
                });
            }
            "changes" => {
                use joey_tools::file_tracker::FileTracker;
                let summary = FileTracker::change_summary();
                if summary.files_modified == 0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: "No files changed in this session.".into(),
                        kind: NoticeKind::Info,
                    });
                } else {
                    let paths = summary.modified_paths.join(", ");
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: format!(
                            "{} file(s) read, {} modified: {}",
                            summary.files_read, summary.files_modified, paths,
                        ),
                        kind: NoticeKind::Info,
                    });
                }
            }
            // /copy — copy the last agent response to the clipboard (with an
            // optional 1-based message number, counting assistant messages
            // oldest→newest; negative numbers count from the newest).
            "copy" => {
                let args = slash_args_after(input, "copy");
                copy_in_tui(&mut self.tui, args);
            }
            "history" => {
                // Show conversation history summary in the transcript.
                let count = self.tui.app().transcript_len();
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: format!(
                        "{} transcript item(s) this session — scroll with ↑/↓ or PgUp/PgDn",
                        count,
                    ),
                    kind: NoticeKind::Info,
                });
            }
            "version" | "v" => {
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: format!("joey-agent {}", env!("CARGO_PKG_VERSION")),
                    kind: NoticeKind::Info,
                });
            }
            "llm-selector" => {
                // Reuse the CLI handler, funneling its terminal output into
                // the transcript (it prints; we capture by running the same
                // underlying call when it returns Result, else we show a
                // pointer to the CLI).
                let args = slash_args_after(input, "llm-selector");
                match crate::llm_selector::llm_selector_slash(args) {
                    Ok(()) => {}
                    Err(e) => self.tui.app_mut().push_item(TranscriptItem::Error { text: e }),
                }
            }
            // ── Spec-Kit workflow ──
            "speckit-status" => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                match crate::speckit_slash::status(&cwd) {
                    Ok(st) => {
                        for line in crate::speckit_slash::render_status(&st).lines() {
                            self.tui.app_mut().push_item(TranscriptItem::Notice {
                                text: line.to_string(),
                                kind: NoticeKind::Info,
                            });
                        }
                    }
                    Err(e) => self.tui.app_mut().push_item(TranscriptItem::Error { text: e }),
                }
            }
            "speckit-help" => {
                for line in crate::speckit_slash::render_help().lines() {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: line.to_string(),
                        kind: NoticeKind::Info,
                    });
                }
            }
            name if name.starts_with("speckit-") => {
                // Lifecycle steps: pre-flight script + one full agent turn.
                // The turn runs on the ENGINE (like any turn) — prepare the
                // prompt on the UI side (scripts are fast) and submit.
                let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                let args = slash_args_after(input, name).to_string();
                match (
                    crate::speckit_slash::find_repo_root(&cwd),
                    crate::speckit_slash::step_by_name(name),
                ) {
                    (Some(root), Some(step)) => {
                        match crate::speckit_slash::prepare_step(step, &root, &args, Some(step.skill)) {
                            Ok(prep) => {
                                self.tui.app_mut().push_item(TranscriptItem::Notice {
                                    text: format!(
                                        "🧭 /{} — starting {} workflow (agent turn)",
                                        step.name, step.skill
                                    ),
                                    kind: NoticeKind::Info,
                                });
                                if let Some(engine) = &self.engine {
                                    let active_agent = self
                                        .tui
                                        .app()
                                        .agent_roster
                                        .get(self.tui.app().active_agent_index)
                                        .map(|a| a.name.clone())
                                        .unwrap_or_else(|| "default".to_string());
                                    engine.send(crate::engine::EngineCommand::Submit {
                                        prompt: prep.prompt,
                                        active_agent,
                                    });
                                    self.busy = true;
                                    self.tui.app_mut().mode = joey_tui::state::RunMode::Busy;
                                }
                            }
                            Err(e) => self.tui.app_mut().push_item(TranscriptItem::Error { text: e }),
                        }
                    }
                    _ => {
                        self.tui.app_mut().push_item(TranscriptItem::Error {
                            text: "not a spec-kit repository (no .specify/ directory), or unknown step. See /speckit-help.".into(),
                        });
                    }
                }
            }
            "neurocode" => {
                // The handler builds its own engine and returns plain text —
                // perfect for the transcript (Constitution II parity).
                // Heavy subcommands (index/ingest strict form) parse the
                // whole tree + bulk-upsert SQLite, so run the handler off
                // the UI task — the GUI must not freeze. Natural-language
                // ingest instead hands off to a full agent turn (submitted
                // through the engine like any turn).
                let args = slash_args_after(input, "neurocode").to_string();
                match crate::commands::neurocode::neurocode_slash_outcome(&args) {
                    crate::commands::neurocode::NeurocodeOutcome::AgentIngest(prompt) => {
                        self.tui.app_mut().push_item(TranscriptItem::Notice {
                            text: "🧭 natural-language ingest — the agent will locate the \
                                   source and call neurocode_ingest"
                                .into(),
                            kind: NoticeKind::Info,
                        });
                        if let Some(engine) = &self.engine {
                            let active_agent = self
                                .tui
                                .app()
                                .agent_roster
                                .get(self.tui.app().active_agent_index)
                                .map(|a| a.name.clone())
                                .unwrap_or_else(|| "default".to_string());
                            engine.send(crate::engine::EngineCommand::Submit {
                                prompt,
                                active_agent,
                            });
                            self.busy = true;
                            self.tui.app_mut().mode = joey_tui::state::RunMode::Busy;
                        }
                    }
                    crate::commands::neurocode::NeurocodeOutcome::Text(_) => {
                        self.busy = true;
                        self.tui.app_mut().mode = joey_tui::state::RunMode::Busy;
                        self.tui.app_mut().push_item(TranscriptItem::Notice {
                            text: "⧗ /neurocode running on the engine… (GUI stays live)".into(),
                            kind: NoticeKind::Busy,
                        });
                        if let Some(engine) = &self.engine {
                            engine.send(crate::engine::EngineCommand::HeavyJob {
                                label: "neurocode".into(),
                                args,
                            });
                        }
                    }
                }
            }
            "toolsets" => {
                let names = joey_tools::toolsets::names();
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: format!("Toolsets: {}", names.join(", ")),
                    kind: NoticeKind::Info,
                });
            }
            "skills" => {
                let skills = joey_tools::tools::skills_tool::discover();
                if skills.is_empty() {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: "No skills installed.".into(),
                        kind: NoticeKind::Info,
                    });
                } else {
                    let listing = skills
                        .iter()
                        .map(|s| s.name.clone())
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: format!("Skills: {}", listing),
                        kind: NoticeKind::Info,
                    });
                }
            }
            name => {
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: format!(
                        "/{} isn't wired into the TUI yet — run joey without --tui to use it.",
                        name
                    ),
                    kind: NoticeKind::Warning,
                });
            }
        },
    }
    false
}
}

/// True for slash commands that are pure UI-side reads/writes — safe to
/// run while the engine is mid-turn (they never touch the agent). Anything
/// else submitted while busy is queued (or, for unknown commands, answered
/// by handle_slash as usual when idle).
fn slash_is_light(input: &str) -> bool {
    let stripped = input.trim_start().trim_start_matches('/').to_ascii_lowercase();
    let name = stripped.split_whitespace().next().unwrap_or("");
    matches!(name, "status" | "help" | "copy" | "model" | "version" | "v")
}

/// Extract the argument substring after `/command` in a slash input string.
/// E.g. "/start-work my-plan" → "my-plan", "/goal set Ship it" → "set Ship it".
fn slash_args_after<'a>(input: &'a str, command: &str) -> &'a str {
    // input starts with '/'; strip it, then the command name (case-insensitive).
    let stripped = input.trim_start_matches('/');
    let lower = stripped.to_ascii_lowercase();
    // "/command <rest>" → "<rest>"
    let prefix = format!("{command} ");
    if lower.starts_with(&prefix) {
        return &stripped[prefix.len()..];
    }
    // Bare "/command" with no args → empty.
    if lower == command {
        return "";
    }
    // Fallback: anything after the first space.
    stripped.split_once(' ').map(|(_, r)| r).unwrap_or("")
}

#[cfg(test)]
mod tui_tests {
    /// Read-only slash commands are UI-safe while the engine is busy;
    /// everything else (prompts, heavy commands) must queue.
    #[test]
    fn slash_is_light_classifies_read_only_commands() {
        for ok in ["/status", "/help", "/copy", "/copy 2", "/model", "/version", "/v", "  /status"] {
            assert!(super::slash_is_light(ok), "expected light: {ok}");
        }
        for heavy in ["/neurocode index", "/start-work", "/quit", "/clear", "hello", ""] {
            assert!(!super::slash_is_light(heavy), "expected not light: {heavy}");
        }
    }
}


/// `/copy [n]` in the TUI — copy an assistant message to the clipboard.
///
/// No argument: the most recent assistant message. A positive 1-based n:
/// the nth assistant message (oldest→newest). A negative n: counting from
/// the newest (-1 = last). Uses the native clipboard chain with an OSC 52
/// fallback so it also works over SSH.
fn copy_in_tui(tui: &mut Tui, args: &str) {
    // Collect assistant message texts from the transcript.
    let assistant_texts: Vec<String> = tui
        .app()
        .transcript
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::Assistant { text } => Some(text.clone()),
            _ => None,
        })
        .collect();

    let selected: Option<String> = if args.trim().is_empty() {
        assistant_texts.last().cloned()
    } else {
        match args.trim().parse::<i64>() {
            Ok(n) if n > 0 => assistant_texts.get((n - 1) as usize).cloned(),
            Ok(n) if n < 0 => {
                let idx = assistant_texts.len().checked_sub(n.unsigned_abs() as usize);
                idx.and_then(|i| assistant_texts.get(i).cloned())
            }
            _ => None,
        }
    };

    let Some(text) = selected else {
        tui.app_mut().push_item(TranscriptItem::Notice {
            text: if assistant_texts.is_empty() {
                "Nothing to copy yet.".into()
            } else {
                format!("No assistant message matches '{}'.", args.trim())
            },
            kind: NoticeKind::Warning,
        });
        return;
    };

    let preview: String = text.chars().take(60).collect();
    match crate::clipboard::copy_to_clipboard(&text) {
        Ok(()) => tui.app_mut().push_item(TranscriptItem::Notice {
            text: format!("✓ Copied to clipboard: {}…", preview),
            kind: NoticeKind::Success,
        }),
        Err(e) => tui.app_mut().push_item(TranscriptItem::Error {
            text: format!("Copy failed: {e}"),
        }),
    }
}

/// T114: `/goal <subcommand>` in the TUI — mirrors the CLI handler
/// (`omo_goal_slash`). Manages the persistent per-session objective stored in
/// `.omo/goal.json` (set/pause/resume/clear/show).
fn handle_goal_tui(tui: &mut Tui, args: &str) {
    let cwd = std::path::PathBuf::from(tui.app().cwd.clone());
    let omo_dir = cwd.join(".omo");
    let session_id = tui.app().session_id.clone();
    let action = joey_omo::parse_goal_command(args);

    match action {
        joey_omo::GoalAction::Set { objective } => {
            if objective.is_empty() {
                tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "Usage: /goal set <objective text>".into(),
                    kind: NoticeKind::Info,
                });
                return;
            }
            let goal = joey_omo::GoalState::new(session_id, objective.clone());
            match goal.write(&omo_dir) {
                Ok(_) => tui.app_mut().push_item(TranscriptItem::Notice {
                    text: format!("Goal set: {objective}"),
                    kind: NoticeKind::Success,
                }),
                Err(e) => tui.app_mut().push_item(TranscriptItem::Error {
                    text: format!("Failed to write goal: {e}"),
                }),
            }
        }
        joey_omo::GoalAction::Pause => {
            if let Some(mut goal) = joey_omo::GoalState::read(&omo_dir) {
                goal.status = joey_omo::GoalStatus::Paused;
                let _ = goal.write(&omo_dir);
                tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "Goal paused (no continuation injection).".into(),
                    kind: NoticeKind::Info,
                });
            } else {
                tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "No goal set. Use /goal set <text>.".into(),
                    kind: NoticeKind::Info,
                });
            }
        }
        joey_omo::GoalAction::Resume => {
            if let Some(mut goal) = joey_omo::GoalState::read(&omo_dir) {
                goal.status = joey_omo::GoalStatus::Active;
                let _ = goal.write(&omo_dir);
                tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "Goal resumed (continuation injection active).".into(),
                    kind: NoticeKind::Info,
                });
            } else {
                tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "No goal set. Use /goal set <text>.".into(),
                    kind: NoticeKind::Info,
                });
            }
        }
        joey_omo::GoalAction::Clear => {
            joey_omo::GoalState::clear(&omo_dir);
            tui.app_mut().push_item(TranscriptItem::Notice {
                text: "Goal cleared.".into(),
                kind: NoticeKind::Info,
            });
        }
        joey_omo::GoalAction::Show => match joey_omo::GoalState::read(&omo_dir) {
            Some(goal) => {
                let status = match goal.status {
                    joey_omo::GoalStatus::Active => "Active",
                    joey_omo::GoalStatus::Paused => "Paused",
                };
                tui.app_mut().push_item(TranscriptItem::Notice {
                    text: format!("Goal [{}]: {}", status, goal.objective),
                    kind: NoticeKind::Info,
                });
            }
            None => tui.app_mut().push_item(TranscriptItem::Notice {
                text: "No goal set. Usage: /goal set <text>".into(),
                kind: NoticeKind::Info,
            }),
        },
    }
}
