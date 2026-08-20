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
            render::info("the TUI needs an interactive terminal — using the line REPL.");
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
    // Load HyperCode enabled state from config
    app_state.hypercode_enabled = config.get_bool("hypercode.enabled", false);
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
    let neurocode_active = crate::neurocode_wiring::try_build_engine(&config).is_some();
    if neurocode_active {
        app_state.apply(joey_agent_core::AgentEvent::NeuroCodeActive { active: true });
        app_state.push_item(TranscriptItem::Notice {
            text: "⚡ NeuroCode active — dependency-aware context injection is ON (live feed: bottom-right panel)".into(),
            kind: NoticeKind::Success,
        });
    }
    // HyperCode additive: show if it's enabled alongside NeuroCode
    if app_state.hypercode_enabled && neurocode_active {
        app_state.push_item(TranscriptItem::Notice {
            text: "⚡ HyperCode also active — parallel task optimization is ON (works with NeuroCode context)".into(),
            kind: NoticeKind::Success,
        });
    } else if app_state.hypercode_enabled {
        app_state.push_item(TranscriptItem::Notice {
            text: "⚡ HyperCode active — parallel task optimization is ON".into(),
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
    let (engine, interrupt) = crate::engine::spawn_engine(agent, engine_spec.clone(), ev_tx);

    // Parallel-subagent feature: install the process-global delegation
    // event tap. Every SubagentManager in this process (including engines
    // rebuilt after a force-kill) mirrors subagent lifecycle + wrapped
    // child events here, driving the right-rail panes.
    let (tap_tx, tap_rx) =
        tokio::sync::mpsc::unbounded_channel::<joey_agent_core::AgentEvent>();
    joey_orchestration::tap::set_global_tap(Some(tap_tx));

    // Single-query mode: submit, pump until done, hand the answer back.
    if let Some(query) = &opts.query {
        let mut session = TuiSession {
            tui,
            ev_rx,
            tap_rx,
            engine: Some(engine),
            interrupt,
            engine_spec,
            busy: false,
            last_ctrlc: None,
            queued: Vec::new(),
            engine_queued: Vec::new(),
            engine_generation: 0,
            busy_enter_mode: joey_core::Config::load()
                .map(|c| c.get_str("display.busy_enter", "interrupt"))
                .unwrap_or_else(|_| "interrupt".to_string()),
        };
        session.submit(query.clone());
        loop {
            let gen_at_start = session.engine_generation;
            match pump_one(&mut session).await {
                Some(PumpOutcome::TurnDone) | Some(PumpOutcome::EngineGone) => break,
                Some(PumpOutcome::Action(TuiAction::Quit)) => break,
                // The waiting turn died with a force-killed engine — its
                // TurnFinished will never arrive from the fresh engine.
                // Escape instead of waiting forever.
                _ => {
                    if session.engine_generation != gen_at_start {
                        break;
                    }
                }
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
        tap_rx,
        engine: Some(engine),
        interrupt,
        engine_spec,
        busy: false,
        last_ctrlc: None,
        queued: Vec::new(),
        engine_queued: Vec::new(),
        engine_generation: 0,
        busy_enter_mode: joey_core::Config::load()
            .map(|c| c.get_str("display.busy_enter", "interrupt"))
            .unwrap_or_else(|_| "interrupt".to_string()),
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
    /// Parallel-subagent feature: delegation event tap receiver. The global
    /// tap (joey-orchestration) sends orchestration + wrapped child events
    /// here; pumped alongside engine events so per-subagent panes update
    /// live even while the engine is mid-turn.
    pub tap_rx: tokio::sync::mpsc::UnboundedReceiver<joey_agent_core::AgentEvent>,
    pub engine: Option<crate::engine::EngineHandle>,
    pub interrupt: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub engine_spec: crate::engine::EngineSpec,
    /// A turn or heavy job is running on the engine.
    pub busy: bool,
    pub last_ctrlc: Option<Instant>,
    /// The UI-side `/queue` stash. Owned HERE (not engine-side) so it
    /// survives a force-kill — the engine can be killed and restarted
    /// without losing what the user deliberately deferred.
    /// - `/queue` while BUSY: entry runs as its own turn when the engine
    ///   announces `Idle` (auto-drain, oldest first).
    /// - `/queue` while IDLE: entries join the next submitted input,
    ///   newline-separated (line-REPL parity, repl.rs process_input).
    /// - Interrupt-with-message pushes to the FRONT (runs next).
    pub queued: Vec<String>,
    /// Mirror of prompts sitting in the ENGINE's queue (busy /queue,
    /// interrupt-with-message). Display + force-kill accounting only —
    /// the engine owns the real queue. Element removed when the engine
    /// announces the submit started (QueuedSubmitStarted); on force-kill
    /// the remainder is what gets discarded.
    pub engine_queued: Vec<String>,
    /// Increments each time the engine is force-killed and restarted.
    /// Interactive-mode pump loops compare against the generation they
    /// started with; the single-query loop uses it to detect that the
    /// turn it was waiting on died with the killed engine (no
    /// TurnFinished will ever arrive from the fresh one).
    pub engine_generation: u64,
    /// What Enter does while a turn runs (`/busy`): "queue", "steer", or
    /// "interrupt" (backed by config display.busy_enter; upstream default
    /// interrupt).
    pub busy_enter_mode: String,
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
        // Line-REPL parity (repl.rs process_input): prompts stashed via
        // /queue join the next submitted turn, separated by newlines.
        // (The /queue-while-idle and engine-Idle drains bypass submit(),
        // so this only fires when the user submits fresh input with a
        // pending stash — same semantics as upstream.)
        let turn_text = if self.queued.is_empty() {
            prompt
        } else {
            let mut joined = std::mem::take(&mut self.queued);
            joined.push(prompt);
            joined.join("\n")
        };
        // Pending OMO context (from /start-work) prepends once.
        let turn_text = match self.tui.app_mut().pending_context_injection.take() {
            Some(ctx) if !turn_text.is_empty() => format!("{ctx}\n{turn_text}"),
            Some(ctx) => ctx,
            None => turn_text,
        };
        self.tui.app_mut().record_user(&turn_text);
        self.dispatch_turn(turn_text, active_agent);
        false
    }

    /// Send an already-rendered turn text to the engine and flip busy.
    /// Shared by the submit() funnel and the Idle auto-drain (which must
    /// NOT re-join the remaining stash into the popped prompt).
    fn dispatch_turn(&mut self, turn_text: String, active_agent: String) {
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
                announce: false,
            });
        }
    }

    /// Interrupt-with-message: interrupt the running turn and run this
    /// prompt next (busy_input_mode=interrupt, upstream default). The
    /// submit is queued engine-side behind the interrupt; mirrored
    /// locally for force-kill accounting, and announced with
    /// QueuedSubmitStarted when the turn actually starts so the user
    /// message renders in causal order.
    fn send_queued_submit(&mut self, prompt: String) {
        if let Some(engine) = &self.engine {
            let active_agent = self
                .tui
                .app()
                .agent_roster
                .get(self.tui.app().active_agent_index)
                .map(|a| a.name.clone())
                .unwrap_or_else(|| "default".to_string());
            engine.send(crate::engine::EngineCommand::Submit {
                prompt: prompt.clone(),
                active_agent,
                announce: true,
            });
            self.engine_queued.push(prompt);
        }
    }

    /// Queue a prompt for the next turn WITHOUT sending anything to the
    /// engine — the UI-side stash survives force-kills. Used by /queue.
    fn stash_ui_queue(&mut self, prompt: String) {
        let preview: String = prompt.chars().take(48).collect();
        let pos = self.queued.len() + 1;
        self.tui.app_mut().push_item(TranscriptItem::Notice {
            text: format!("⧗ queued (#{pos}) for the next turn: {preview}"),
            kind: NoticeKind::Busy,
        });
        self.queued.push(prompt);
    }

    /// Render the queue listing (bare /queue — line-REPL parity, plus the
    /// engine-side backlog mirror while a turn runs).
    fn show_ui_queue(&mut self) {
        let total = self.queued.len() + self.engine_queued.len();
        if total == 0 {
            self.tui.app_mut().push_item(TranscriptItem::Notice {
                text: "No prompts queued. Usage: /queue <prompt>".into(),
                kind: NoticeKind::Info,
            });
            return;
        }
        self.tui.app_mut().push_item(TranscriptItem::Notice {
            text: format!("{} prompt(s) queued for the next turn:", total),
            kind: NoticeKind::Info,
        });
        let mut i = 1;
        for q in self.engine_queued.iter().chain(self.queued.iter()) {
            let preview: String = q.chars().take(48).collect();
            self.tui.app_mut().push_item(TranscriptItem::Notice {
                text: format!("  {i}. {preview}"),
                kind: NoticeKind::Info,
            });
            i += 1;
        }
    }

    /// /model: main-model switch + NeuroCode per-provider tier config.
    ///
    /// Grammar:
    ///   /model                                — show current model/provider
    ///                                          + tier config for the provider
    ///   /model <name> [--global]              — switch the main model (the
    ///                                          engine swaps the live agent;
    ///                                          --global persists model.default)
    ///   /model neurocode                      — show tier models for the
    ///                                          active provider
    ///   /model neurocode frontier <name>      — set the frontier tier model
    ///                                          (persisted per provider)
    ///   /model neurocode economical <name>    — set the economical tier model
    ///   /model neurocode reset                — clear this provider's tier
    ///                                          overrides (fall back to flat)
    fn handle_model_slash(&mut self, args: &str) {
        match ModelSlash::parse(args) {
            ModelSlash::Show => {
                // Show: current model + provider + neurocode tiers.
                let model = self.tui.app().model.clone();
                let provider = self.tui.app().provider.clone();
                let mut text = format!("Current model: {model} (provider: {provider})");
                if let Ok(cfg) = joey_core::Config::load() {
                    let nc = joey_neurocode::NeuroCodeConfig::from_config(&cfg);
                    if nc.enabled {
                        let tiers = nc.tier.tiers_for_provider(&provider);
                        let f = if tiers.frontier.is_empty() { "(unset)".into() } else { tiers.frontier };
                        let e = if tiers.economical.is_empty() { "(unset)".into() } else { tiers.economical };
                        text.push_str(&format!(
                            "\nneurocode tiers [{provider}]: frontier={f} · economical={e} \
                             (ambiguous→{})",
                            nc.tier.ambiguous_default,
                        ));
                    } else {
                        text.push_str("\nneurocode: disabled");
                    }
                }
                text.push_str("\nUsage: /model <name> [--global] · /model neurocode <frontier|economical> <name> · /model neurocode reset");
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text,
                    kind: NoticeKind::Info,
                });
            }
            ModelSlash::Neurocode { sub } => {
                self.handle_model_neurocode(sub);
            }
            ModelSlash::Switch { model, global } => {
                if self.busy {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: "⧗ model switch queued — applies when the running turn finishes.".into(),
                        kind: NoticeKind::Busy,
                    });
                }
                if let Some(engine) = &self.engine {
                    engine.send(crate::engine::EngineCommand::SwitchModel { model, global });
                } else {
                    self.tui.app_mut().push_item(TranscriptItem::Error {
                        text: "engine unavailable — cannot switch model".into(),
                    });
                }
            }
        }
    }

    /// `/model neurocode …` sub-handler: per-provider tier model config.
    fn handle_model_neurocode(&mut self, sub: ModelNcSub) {
        let provider = self.tui.app().provider.clone();
        match sub {
            ModelNcSub::Show => {
                let mut text = match joey_core::Config::load() {
                    Ok(cfg) => {
                        let nc = joey_neurocode::NeuroCodeConfig::from_config(&cfg);
                        if nc.enabled {
                            let tiers = nc.tier.tiers_for_provider(&provider);
                            format!(
                                "neurocode tiers [{provider}]:\n  frontier: {}\n  economical: {}\n  ambiguous_default: {}",
                                if tiers.frontier.is_empty() { "(unset)".into() } else { tiers.frontier },
                                if tiers.economical.is_empty() { "(unset)".into() } else { tiers.economical },
                                nc.tier.ambiguous_default,
                            )
                        } else {
                            "neurocode is disabled (neurocode.enabled=false) — tier models \
                             are stored but not used."
                                .to_string()
                        }
                    }
                    Err(e) => format!("config unavailable: {e}"),
                };
                text.push_str("\nUsage: /model neurocode <frontier|economical> <name> · /model neurocode reset");
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text,
                    kind: NoticeKind::Info,
                });
            }
            ModelNcSub::Frontier(model) => {
                self.set_neurocode_tier(&provider, "frontier", &model);
            }
            ModelNcSub::Economical(model) => {
                self.set_neurocode_tier(&provider, "economical", &model);
            }
            ModelNcSub::Reset => {
                let mut cleared = Vec::new();
                match joey_core::Config::load() {
                    Ok(mut cfg) => {
                        for tier in ["frontier", "economical"] {
                            let key = format!("neurocode.tier.providers.{provider}.{tier}");
                            match cfg.unset(&key) {
                                Ok(true) => cleared.push(tier),
                                Ok(false) => {}
                                Err(e) => {
                                    self.tui.app_mut().push_item(TranscriptItem::Error {
                                        text: format!("failed to unset {key}: {e}"),
                                    });
                                    return;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        self.tui.app_mut().push_item(TranscriptItem::Error {
                            text: format!("config unavailable: {e}"),
                        });
                        return;
                    }
                }
                let what = if cleared.is_empty() {
                    "no per-provider overrides were set".to_string()
                } else {
                    format!("cleared: {}", cleared.join(", "))
                };
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: format!("neurocode tier overrides for {provider} — {what} (flat keys now apply)"),
                    kind: NoticeKind::Success,
                });
            }
            ModelNcSub::Unknown(other) => {
                self.tui.app_mut().push_item(TranscriptItem::Error {
                    text: format!(
                        "unknown neurocode subcommand '{other}'. Use: show | frontier <name> | economical <name> | reset"
                    ),
                });
            }
        }
    }

    /// Persist a neurocode tier model for `provider` (`frontier`/`economical`).
    fn set_neurocode_tier(&mut self, provider: &str, tier: &str, model: &str) {
        if model.is_empty() {
            self.tui.app_mut().push_item(TranscriptItem::Error {
                text: format!("Usage: /model neurocode {tier} <model-name>"),
            });
            return;
        }
        let key = format!("neurocode.tier.providers.{provider}.{tier}");
        match joey_core::Config::load() {
            Ok(mut cfg) => match cfg.set_and_save(&key, model) {
                Ok(()) => {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: format!(
                            "✓ neurocode {tier} model for {provider} → {model} \
                             (saved; applies from the next turn)",
                        ),
                        kind: NoticeKind::Success,
                    });
                }
                Err(e) => {
                    self.tui.app_mut().push_item(TranscriptItem::Error {
                        text: format!("failed to save {key}: {e}"),
                    });
                }
            },
            Err(e) => {
                self.tui.app_mut().push_item(TranscriptItem::Error {
                    text: format!("config unavailable: {e}"),
                });
            }
        }
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

    /// /hypercode: parallel task optimization status and configuration.
    fn handle_hypercode_slash(&mut self, args: &str) {
        let provider = self.tui.app().provider.clone();
        match crate::repl::hypercode_slash_with_provider(&provider, args) {
            Ok(crate::hypercode::HyperCodeOutput::Text(lines)) => {
                for line in lines {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: line,
                        kind: NoticeKind::Info,
                    });
                }
            }
            Ok(crate::hypercode::HyperCodeOutput::Toggle(new_state)) => {
                self.tui.app_mut().hypercode_enabled = new_state;
                // Live-apply orchestrator mode on the engine's agent (tool
                // surface + overlay swap; no rebuild needed).
                if let Some(engine) = &self.engine {
                    engine.send(crate::engine::EngineCommand::SetOrchestratorMode(new_state));
                }
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: format!(
                        "⚡ HyperCode mode toggled: {} (saved to config.yaml){}",
                        if new_state { "ON" } else { "OFF" },
                        if new_state { " — orchestrator mode: file writes/builds go through subagents; main agent keeps process monitoring + read-only peeks + web" } else { "" }
                    ),
                    kind: NoticeKind::Success,
                });
            }
            Ok(crate::hypercode::HyperCodeOutput::Configured(msg)) => {
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: msg,
                    kind: NoticeKind::Success,
                });
            }
            Ok(crate::hypercode::HyperCodeOutput::Run { goal }) => {
                // Execute on the engine: children stream live through the
                // global orchestration tap into native TUI panes + rail +
                // job board; phase progress arrives as HypercodeProgress.
                if self.engine.is_none() {
                    self.tui.app_mut().push_item(TranscriptItem::Error {
                        text: "engine unavailable — cannot start hypercode run".into(),
                    });
                    return;
                }
                self.busy = true;
                self.tui.app_mut().mode = joey_tui::state::RunMode::Busy;
                self.tui.app_mut().hypercode_phase = None;
                self.tui.app_mut().job_board_visible = true;
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "⚡ HyperCode run starting on the engine — subagents appear on the right rail; Ctrl-C interrupts".into(),
                    kind: NoticeKind::Busy,
                });
                if let Some(engine) = &self.engine {
                    engine.send(crate::engine::EngineCommand::Hypercode { goal });
                }
            }
            Err(e) => {
                self.tui.app_mut().push_item(TranscriptItem::Error {
                    text: e,
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
        // be recovered; the engine owns its queue). The UI-side stash
        // survives (that's why /queue-while-idle stashes UI-side).
        let discarded = std::mem::take(&mut self.engine_queued);
        if let Some(engine) = self.engine.take() {
            // Signal, then abandon: drop the command channel + detach task.
            engine.send(crate::engine::EngineCommand::ForceKill);
            engine.abandon();
        }
        // The fresh engine must not inherit a stale 2s Ctrl-C kill window
        // (a second Ctrl-C right after restart would instantly kill it).
        self.last_ctrlc = None;
        // Flush any stale engine events so they can't leak into the new one.
        while let Ok(_) = self.ev_rx.try_recv() {}
        if !discarded.is_empty() {
            let previews: Vec<String> = discarded
                .iter()
                .map(|p| p.chars().take(48).collect())
                .collect();
            self.tui.app_mut().push_item(TranscriptItem::Error {
                text: format!(
                    "{} queued prompt(s) discarded with the killed engine — resubmit if still needed: {}",
                    discarded.len(),
                    previews.join(" | ")
                ),
            });
        }
        // Fresh engine, fresh agent.
        self.engine_generation += 1;
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
        let (engine, interrupt) = crate::engine::spawn_engine(
            agent,
            self.engine_spec.clone(),
            ev_tx,
        );
        self.ev_rx = ev_rx;
        self.engine = Some(engine);
        self.interrupt = interrupt;
        self.busy = false;
        // The killed turn never sends Done — reset the RunMode ourselves.
        self.tui.app_mut().mode = joey_tui::state::RunMode::Input;
        // A hypercode run dying with the engine never emits its final
        // HeavyJobFinished — clear the live phase badge.
        self.tui.app_mut().hypercode_phase = None;
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
        tap_ev = session.tap_rx.recv() => {
            // Parallel-subagent feature: delegation tap events (spawn /
            // wrapped child stream / complete) update the App directly.
            // They never carry the parent's turn lifecycle, so they cannot
            // wedge the busy state.
            if let Some(agent_ev) = tap_ev {
                session.tui.app_mut().apply(agent_ev);
            }
            return None;
        }
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
            // NOTE: queued_forwarded is NOT reset here anymore — the engine
            // may still hold queued submits behind this TurnFinished (the
            // flag only reaches zero as each QueuedSubmitStarted lands or
            // at a force-kill, which reports the true discard count).
            // Honor a pending agent switch now that the turn is done.
            if let Some(agent_name) = session.tui.app_mut().pending_agent_switch.take() {
                if let Some(engine) = &session.engine {
                    engine.send(crate::engine::EngineCommand::SwitchAgent(agent_name));
                }
            }
            Some(PumpOutcome::TurnDone)
        }
        Some(crate::engine::EngineEvent::QueuedSubmitStarted { prompt }) => {
            // A busy-path submit (queue/interrupt-with-message) is starting
            // now — render the user message we couldn't show at submit
            // time, in causal order. Pop it from the mirror (by value, in
            // order; a mismatch just clears the head defensively).
            pop_engine_queued_head(&mut session.engine_queued, &prompt);
            session.busy = true;
            session.tui.app_mut().mode = joey_tui::state::RunMode::Busy;
            session.tui.app_mut().record_user(&prompt);
            None
        }
        Some(crate::engine::EngineEvent::Idle) => {
            // All engine work drained. Pop the UI-side /queue stash — one
            // prompt per turn, oldest first, submitted through
            // dispatch_turn (not the submit() funnel, which would re-join
            // the remaining stash into this prompt).
            if session.busy {
                session.busy = false;
                session.tui.app_mut().mode = joey_tui::state::RunMode::Input;
            }
            if let Some(next) = session.queued.first().cloned() {
                session.queued.remove(0);
                let active_agent = session
                    .tui
                    .app()
                    .agent_roster
                    .get(session.tui.app().active_agent_index)
                    .map(|a| a.name.clone())
                    .unwrap_or_else(|| "default".to_string());
                let preview: String = next.chars().take(48).collect();
                session.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: format!("⧗ running queued prompt: {preview}"),
                    kind: NoticeKind::Busy,
                });
                session.tui.app_mut().record_user(&next);
                session.dispatch_turn(next, active_agent);
            }
            None
        }
        Some(crate::engine::EngineEvent::HypercodeProgress { phase, detail }) => {
            // Live phase banner for the running hypercode pipeline. The
            // badge picks this up (⚡ PLAN/EXPL/BUILD/SYNTH) and the
            // transcript records the transition.
            session.tui.app_mut().hypercode_phase = Some(phase.clone());
            session.tui.app_mut().push_item(TranscriptItem::Notice {
                text: format!("⚡ hypercode: {phase} — {detail}"),
                kind: NoticeKind::Busy,
            });
            None
        }
        Some(crate::engine::EngineEvent::HeavyJobFinished { label: _, text }) => {
            session.busy = false;
            // A heavy job never sends AgentEvent::Done, so reset the
            // RunMode ourselves — without this the status bar stays BUSY
            // forever after /neurocode completes.
            session.tui.app_mut().mode = joey_tui::state::RunMode::Input;
            session.tui.app_mut().hypercode_phase = None;
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
        Some(crate::engine::EngineEvent::ModelSwitched { model, provider, notice }) => {
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
            // Anything mirrored as engine-queued died with the engine.
            session.engine_queued.clear();
            // Engine death never sends Done — reset the RunMode ourselves
            // or the status bar stays BUSY forever.
            session.tui.app_mut().mode = joey_tui::state::RunMode::Input;
            session.tui.app_mut().hypercode_phase = None;
            session.tui.app_mut().push_item(TranscriptItem::Error { text: msg });
            Some(PumpOutcome::EngineGone)
        }
        None => {
            session.busy = false;
            session.engine_queued.clear();
            session.tui.app_mut().mode = joey_tui::state::RunMode::Input;
            session.tui.app_mut().hypercode_phase = None;
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
                // Resolution (not raw starts_with) so aliases (/q),
                // prefixes (/qu), and case variants all classify the same.
                let is_slash = text.trim_start().starts_with('/');
                let light_slash = session.busy && is_slash && slash_is_light(&text);
                let resolved_busy = session.busy && is_slash;
                if light_slash {
                    session.handle_slash(&text);
                } else if resolved_busy
                    && matches!(
                        slash::resolve(&text),
                        Resolution::Command { def, .. } if def.name == "steer"
                    )
                {
                    // Hermes parity: /steer mid-turn injects WITHOUT
                    // interrupting — the message lands inside the marker on
                    // the current turn's next tool result.
                    let steer_text = slash::resolve(&text).rest_or_empty();
                    if steer_text.is_empty() {
                        session.tui.app_mut().push_item(TranscriptItem::Notice {
                            text: "Usage: /steer <message> — inject mid-turn after the next tool call".into(),
                            kind: NoticeKind::Warning,
                        });
                    } else if let Some(engine) = &session.engine {
                        engine.send(crate::engine::EngineCommand::Steer(steer_text));
                    }
                } else if session.busy {
                    // /busy selects what Enter does mid-turn (upstream
                    // busy_input_mode; default interrupt).
                    let preview: String = text.chars().take(48).collect();
                    match session.busy_enter_mode.as_str() {
                        "queue" => {
                            session.stash_ui_queue(text);
                        }
                        "steer" => {
                            if let Some(engine) = &session.engine {
                                engine.send(crate::engine::EngineCommand::Steer(text.clone()));
                            }
                            session.tui.app_mut().push_item(TranscriptItem::Notice {
                                text: format!("🧭 steering the running turn: {preview}"),
                                kind: NoticeKind::Info,
                            });
                        }
                        _ => {
                            // Hermes parity (busy_input_mode: interrupt — the
                            // upstream default): a plain message mid-turn
                            // INTERRUPTS the running turn; the engine unwinds
                            // it and runs the new message as the next turn.
                            session.tui.app_mut().push_item(TranscriptItem::Notice {
                                text: format!("⚡ interrupting — your message runs next: {preview}"),
                                kind: NoticeKind::Warning,
                            });
                            session.interrupt.store(true, std::sync::atomic::Ordering::SeqCst);
                            session.send_queued_submit(text);
                        }
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
                text: format!("/{} has no handler in this build (registry inconsistency).", def.name),
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
            // /queue — UI-side stash in BOTH modes: defers to the next
            // turn, never interrupts, and survives force-kills. While a
            // turn runs, slash_is_light routes here too; the engine's Idle
            // event then auto-drains entries one turn each.
            "queue" => {
                let args = slash_args_after(input, "queue");
                if args.trim().is_empty() {
                    self.show_ui_queue();
                } else {
                    self.stash_ui_queue(args.trim().to_string());
                }
            }
            // /steer while idle: nothing is running — degrade to a queued
            // prompt (line-REPL parity; the busy path in interactive_loop
            // does true mid-turn steering via EngineCommand::Steer).
            "steer" => {
                let args = slash_args_after(input, "steer");
                if args.trim().is_empty() {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: "Usage: /steer <message> — inject mid-turn after the next tool call".into(),
                        kind: NoticeKind::Warning,
                    });
                } else {
                    self.stash_ui_queue(args.trim().to_string());
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: "No turn running — queued for the next turn. (Mid-turn /steer works while a turn is live.)".into(),
                        kind: NoticeKind::Info,
                    });
                }
            }
            "model" => {
                let args = slash_args_after(input, "model");
                self.handle_model_slash(args);
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
                                        announce: false,
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
                                announce: false,
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
            // /browser (feature 016, T066): session control with CLI parity.
            // connect can launch a browser + open a websocket (slow), so it
            // runs as an engine heavy job; the result lands in the transcript
            // via HeavyJobFinished. The global BrowserHandle is shared with
            // the line REPL and the browser_* tools (one session everywhere).
            "browser" => {
                let args = slash_args_after(input, "browser").to_string();
                self.busy = true;
                self.tui.app_mut().mode = joey_tui::state::RunMode::Busy;
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "⧗ /browser running on the engine…".into(),
                    kind: NoticeKind::Busy,
                });
                if let Some(engine) = &self.engine {
                    engine.send(crate::engine::EngineCommand::HeavyJob {
                        label: "browser".into(),
                        args,
                    });
                }
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
            "hypercode" => {
                // HyperCode parallel optimization — TUI version.
                let args = slash_args_after(input, "hypercode");
                self.handle_hypercode_slash(args);
            }
            // ── Newly-wired commands (slash_extra.rs shared handlers) ──
            "redraw" => {
                // ratatui repaints fully every frame — clear the stale
                // transcript scroll state and force an immediate redraw tick.
                self.tui.app_mut().scroll = None;
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: "screen repainted".into(),
                    kind: NoticeKind::Info,
                });
            }
            "save" => {
                let sid = self.tui.app().session_id.clone();
                let db = joey_core::SessionDb::open_default().ok();
                let lines = crate::slash_extra::save_session_markdown(&sid, db.as_ref());
                for l in lines.0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "retry" => {
                // Re-send the last user message from the transcript as a new
                // turn (the engine owns the agent + its history).
                let last_user = self
                    .tui
                    .app()
                    .transcript
                    .iter()
                    .rev()
                    .find_map(|item| match item {
                        TranscriptItem::User { text } if !text.trim().is_empty() => {
                            Some(text.clone())
                        }
                        _ => None,
                    });
                match last_user {
                    Some(text) => {
                        self.tui.app_mut().push_item(TranscriptItem::Notice {
                            text: format!("↻ retrying: {}", text.chars().take(60).collect::<String>()),
                            kind: NoticeKind::Info,
                        });
                        self.submit(text);
                    }
                    None => self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: "Nothing to retry yet.".into(),
                        kind: NoticeKind::Warning,
                    }),
                }
            }
            "prompt" | "compose" => {
                let args = slash_args_after(input, "prompt").to_string();
                // Leave the alternate screen so $EDITOR owns the terminal.
                let _ = self.tui.leave();
                let composed = crate::slash_extra::compose_in_editor(&args);
                let back = self.tui.enter_from_leave();
                let quit = match composed {
                    Some(text) => self.submit(text),
                    None => {
                        self.tui.app_mut().push_item(TranscriptItem::Notice {
                            text: "(editor cancelled or empty — nothing sent)".into(),
                            kind: NoticeKind::Info,
                        });
                        false
                    }
                };
                debug_assert!(back.is_ok(), "re-entering the TUI after $EDITOR must succeed");
                if quit {
                    return true;
                }
            }
            "undo" => {
                let sid = self.tui.app().session_id.clone();
                let n: usize = slash_args_after(input, "undo")
                    .trim()
                    .parse()
                    .unwrap_or(1)
                    .max(1);
                if let Ok(db) = joey_core::SessionDb::open_default() {
                    match db.rewind_last_user_exchanges(&sid, n) {
                        Ok(0) => self.tui.app_mut().push_item(TranscriptItem::Notice {
                            text: "Nothing to undo (no active user exchange left).".into(),
                            kind: NoticeKind::Warning,
                        }),
                        Ok(removed) => {
                            // Signal the engine to drop its tail too; it
                            // rebuilds history from the DB on the next turn.
                            if let Some(engine) = &self.engine {
                                engine.send(crate::engine::EngineCommand::ReloadHistory);
                            }
                            self.tui.app_mut().push_item(TranscriptItem::Notice {
                                text: format!("Undid {removed} message(s) ({n} exchange(s)). History reloaded."),
                                kind: NoticeKind::Success,
                            });
                        }
                        Err(e) => self.tui.app_mut().push_item(TranscriptItem::Error {
                            text: format!("rewind failed: {e}"),
                        }),
                    }
                }
            }
            "title" => {
                let args = slash_args_after(input, "title").trim().to_string();
                if args.is_empty() {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: "Usage: /title <name>".into(),
                        kind: NoticeKind::Warning,
                    });
                } else if let Ok(db) = joey_core::SessionDb::open_default() {
                    let sid = self.tui.app().session_id.clone();
                    match db.set_title(&sid, &args) {
                        Ok(()) => self.tui.app_mut().push_item(TranscriptItem::Notice {
                            text: format!("✓ Title set: {args}"),
                            kind: NoticeKind::Success,
                        }),
                        Err(e) => self.tui.app_mut().push_item(TranscriptItem::Error {
                            text: format!("failed to set title: {e}"),
                        }),
                    }
                }
            }
            "handoff" => {
                let args = slash_args_after(input, "handoff").to_string();
                let sid = self.tui.app().session_id.clone();
                for l in crate::slash_extra::handoff_lines(&args, &sid).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "branch" | "fork" => {
                let args = slash_args_after(input, "branch").to_string();
                let sid = self.tui.app().session_id.clone();
                if let Ok(db) = joey_core::SessionDb::open_default() {
                    let (lines, _) = crate::slash_extra::branch_session(&sid, &args, Some(&db));
                    for l in lines.0 {
                        self.tui.app_mut().push_item(TranscriptItem::Notice {
                            text: l,
                            kind: NoticeKind::Info,
                        });
                    }
                }
            }
            "snapshot" => {
                let args = slash_args_after(input, "snapshot").to_string();
                let config = joey_core::Config::load().unwrap_or_else(|_| Config::defaults());
                for l in crate::slash_extra::snapshot::handle(&args, &config).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "stop" => {
                for l in crate::slash_extra::stop_background_processes().0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "background" | "bg" | "btw" => {
                let args = slash_args_after(input, "background").trim().to_string();
                if args.is_empty() {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: "Usage: /background <prompt>".into(),
                        kind: NoticeKind::Warning,
                    });
                } else {
                    // Queue without interrupting — the engine drains it when
                    // the current turn ends (same as /queue semantics).
                    self.stash_ui_queue(args);
                }
            }
            "journey" | "learning" | "memory-graph" => {
                let args = slash_args_after(input, "journey").to_string();
                let cwd = std::env::current_dir().unwrap_or_default();
                for l in crate::slash_extra::journey_lines(&cwd, &args).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "moa" => {
                let args = slash_args_after(input, "moa").trim().to_string();
                if args.is_empty() {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: "Usage: /moa <prompt>".into(),
                        kind: NoticeKind::Warning,
                    });
                } else {
                    let (lines, composed) = crate::slash_extra::moa_prompt(&args);
                    for l in lines.0 {
                        self.tui.app_mut().push_item(TranscriptItem::Notice {
                            text: l,
                            kind: NoticeKind::Info,
                        });
                    }
                    self.submit(composed);
                }
            }
            "subgoal" => {
                let args = slash_args_after(input, "subgoal").to_string();
                let cwd = std::env::current_dir().unwrap_or_default();
                for l in crate::slash_extra::subgoal_lines(&cwd, &args).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "whoami" => {
                for l in crate::slash_extra::whoami_lines().0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "profile" => {
                for l in crate::slash_extra::profile_lines().0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "codex-runtime" | "codex_runtime" => {
                let args = slash_args_after(input, "codex-runtime").to_string();
                let mut config = joey_core::Config::load().unwrap_or_else(|_| Config::defaults());
                for l in crate::slash_extra::codex_runtime_lines(&mut config, &args).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "personality" => {
                let args = slash_args_after(input, "personality").to_string();
                let mut config = joey_core::Config::load().unwrap_or_else(|_| Config::defaults());
                let (lines, _overlay) = crate::slash_extra::personality::handle(&mut config, &args);
                for l in lines.0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
                // The overlay applies engine-side on the next agent rebuild;
                // switching personalities mid-session re-reads config there.
            }
            "statusbar" | "sb" => {
                let mut config = joey_core::Config::load().unwrap_or_else(|_| Config::defaults());
                for l in crate::slash_extra::statusbar_lines(&mut config).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
                // Apply live: the TUI reads display.statusbar at render time
                // via App::statusbar_visible (set below).
                let visible = config.get_bool("display.statusbar", true);
                self.tui.app_mut().show_status_bar = visible;
            }
            "footer" => {
                let args = slash_args_after(input, "footer").to_string();
                let mut config = joey_core::Config::load().unwrap_or_else(|_| Config::defaults());
                for l in crate::slash_extra::footer_lines(&mut config, &args).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "yolo" => {
                for l in crate::slash_extra::yolo_lines().0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Warning,
                    });
                }
            }
            "fast" => {
                let args = slash_args_after(input, "fast").to_string();
                let mut config = joey_core::Config::load().unwrap_or_else(|_| Config::defaults());
                for l in crate::slash_extra::fast_lines(&mut config, &args).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "skin" => {
                let args = slash_args_after(input, "skin").to_string();
                let mut config = joey_core::Config::load().unwrap_or_else(|_| Config::defaults());
                for l in crate::slash_extra::skin::handle(&mut config, &args).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "indicator" => {
                let args = slash_args_after(input, "indicator").to_string();
                let mut config = joey_core::Config::load().unwrap_or_else(|_| Config::defaults());
                for l in crate::slash_extra::indicator::handle(&mut config, &args).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "voice" => {
                let args = slash_args_after(input, "voice").to_string();
                let mut config = joey_core::Config::load().unwrap_or_else(|_| Config::defaults());
                for l in crate::slash_extra::voice_lines(&mut config, &args).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "busy" => {
                let args = slash_args_after(input, "busy").to_string();
                let mut config = joey_core::Config::load().unwrap_or_else(|_| Config::defaults());
                for l in crate::slash_extra::busy::handle(&mut config, &args).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
                // Live-apply: the interactive loop reads this for busy-Enter.
                self.busy_enter_mode = config.get_str("display.busy_enter", "interrupt");
            }
            "reload" => {
                for l in crate::slash_extra::reload_env().0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "memory" => {
                let args = slash_args_after(input, "memory").to_string();
                let mut config = joey_core::Config::load().unwrap_or_else(|_| Config::defaults());
                for l in crate::slash_extra::memory_lines(&mut config, &args).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "bundles" => {
                let config = joey_core::Config::load().unwrap_or_else(|_| Config::defaults());
                for l in crate::slash_extra::bundles_lines(&config).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "pet" => {
                let args = slash_args_after(input, "pet").to_string();
                let mut config = joey_core::Config::load().unwrap_or_else(|_| Config::defaults());
                for l in crate::slash_extra::pet::handle(&mut config, &args).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "hatch" | "generate-pet" => {
                let args = slash_args_after(input, "hatch").to_string();
                for l in crate::slash_extra::pet::hatch(&args).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "learn" => {
                let args = slash_args_after(input, "learn").trim().to_string();
                if args.is_empty() {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: "Usage: /learn <what to learn from>".into(),
                        kind: NoticeKind::Warning,
                    });
                } else {
                    let (lines, prompt) = crate::slash_extra::learn_prompt(&args);
                    for l in lines.0 {
                        self.tui.app_mut().push_item(TranscriptItem::Notice {
                            text: l,
                            kind: NoticeKind::Info,
                        });
                    }
                    self.submit(prompt);
                }
            }
            "cron" => {
                let args = slash_args_after(input, "cron").to_string();
                for l in crate::slash_extra::cron::handle(&args).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "suggestions" | "suggest" => {
                let args = slash_args_after(input, "suggestions").to_string();
                let mut config = joey_core::Config::load().unwrap_or_else(|_| Config::defaults());
                for l in crate::slash_extra::suggestions::handle(&mut config, &args).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "blueprint" | "bp" => {
                let args = slash_args_after(input, "blueprint").to_string();
                for l in crate::slash_extra::blueprint::handle(&args).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "curator" => {
                let args = slash_args_after(input, "curator").to_string();
                let job = args.split_whitespace().next().unwrap_or("").to_string();
                for l in crate::slash_extra::curator_lines(&args).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
                if job == "dedupe" || job == "refresh" {
                    let prompt = crate::slash_extra::curator_prompt(&job);
                    self.submit(prompt);
                }
            }
            "kanban" => {
                let cwd = std::env::current_dir().unwrap_or_default();
                for l in crate::slash_extra::kanban_lines(&cwd).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "reload-mcp" | "reload_mcp" => {
                for l in crate::slash_extra::reload_mcp_lines().0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "reload-skills" | "reload_skills" => {
                for l in crate::slash_extra::reload_skills_lines().0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "plugins" => {
                for l in crate::slash_extra::plugins_lines().0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "subscription" | "upgrade" => {
                for l in crate::slash_extra::subscription_lines().0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "topup" => {
                for l in crate::slash_extra::topup_lines().0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "insights" => {
                let args = slash_args_after(input, "insights").trim().to_string();
                let days: i64 = args.parse().unwrap_or(7).max(1);
                let db = joey_core::SessionDb::open_default().ok();
                for l in crate::slash_extra::insights_lines(days, db.as_ref()).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "platforms" | "gateway" => {
                for l in crate::slash_extra::platforms_lines().0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "image" => {
                let args = slash_args_after(input, "image").trim().to_string();
                if args.is_empty() {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: "Usage: /image <path> (png/jpg/gif/webp, ≤15MB)".into(),
                        kind: NoticeKind::Warning,
                    });
                } else {
                    match crate::slash_extra::image_data_url(&args) {
                        Ok(url) => {
                            if let Some(engine) = &self.engine {
                                engine.send(crate::engine::EngineCommand::AttachImage(url.clone()));
                                self.tui.app_mut().push_item(TranscriptItem::Notice {
                                    text: "✓ Image attached — it goes with your next message.".into(),
                                    kind: NoticeKind::Success,
                                });
                            }
                        }
                        Err(e) => self.tui.app_mut().push_item(TranscriptItem::Error { text: e }),
                    }
                }
            }
            "paste" => {
                #[cfg(target_os = "macos")]
                {
                    let dir = std::env::temp_dir();
                    let out_path = dir.join(format!("joey-paste-{}.png", std::process::id()));
                    let script = format!(
                        "set theClipboard to the clipboard as «class PNGf»\nset theFile to open for access POSIX file \"{}\" with write permission\nwrite theClipboard to theFile\nclose access theFile",
                        out_path.display()
                    );
                    let status = std::process::Command::new("osascript")
                        .arg("-e")
                        .arg(&script)
                        .stderr(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .status();
                    let ok = matches!(status, Ok(s) if s.success()) && out_path.exists();
                    if ok {
                        match crate::slash_extra::image_data_url(out_path.to_str().unwrap_or_default()) {
                            Ok(url) => {
                                if let Some(engine) = &self.engine {
                                    engine.send(crate::engine::EngineCommand::AttachImage(url.clone()));
                                }
                                let _ = std::fs::remove_file(&out_path);
                                self.tui.app_mut().push_item(TranscriptItem::Notice {
                                    text: "✓ Clipboard image attached — it goes with your next message.".into(),
                                    kind: NoticeKind::Success,
                                });
                            }
                            Err(e) => self.tui.app_mut().push_item(TranscriptItem::Error { text: e }),
                        }
                    } else {
                        self.tui.app_mut().push_item(TranscriptItem::Error {
                            text: "no image on the clipboard (or clipboard access denied)".into(),
                        });
                    }
                }
                #[cfg(not(target_os = "macos"))]
                {
                    self.tui.app_mut().push_item(TranscriptItem::Error {
                        text: "clipboard image paste needs macOS (osascript) — use /image <path> instead".into(),
                    });
                }
            }
            "update" => {
                for l in crate::slash_extra::update_lines().0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            "debug" => {
                let args = slash_args_after(input, "debug").trim().to_string();
                let mode = if args.is_empty() { "local".to_string() } else { args };
                for l in crate::slash_extra::debug_lines(&mode).0 {
                    self.tui.app_mut().push_item(TranscriptItem::Notice {
                        text: l,
                        kind: NoticeKind::Info,
                    });
                }
            }
            name => {
                self.tui.app_mut().push_item(TranscriptItem::Notice {
                    text: format!(
                        "/{} isn't wired into the TUI yet — run joey --cli to use it.",
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
/// run while the engine is mid-turn (they never touch the agent). Uses
/// slash::resolve so aliases (/q), and case variants classify the same as
/// the full name. Anything else submitted while busy is queued (or, for
/// unknown commands, answered by handle_slash as usual when idle).
/// Parsed `/model` arguments (pure parser — unit-testable without a session).
#[derive(Debug, PartialEq)]
enum ModelSlash {
    /// `/model` — show current model + tiers.
    Show,
    /// `/model <name> [--global]`.
    Switch { model: String, global: bool },
    /// `/model neurocode …` (alias: `nc`).
    Neurocode { sub: ModelNcSub },
}

/// `/model neurocode` subcommand.
#[derive(Debug, PartialEq)]
enum ModelNcSub {
    Show,
    Frontier(String),
    Economical(String),
    Reset,
    Unknown(String),
}

impl ModelSlash {
    fn parse(args: &str) -> Self {
        let mut parts: Vec<String> = args
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();
        let global = parts.iter().any(|p| p == "--global" || p == "-g");
        parts.retain(|p| p != "--global" && p != "-g");

        let Some(first) = parts.first().cloned() else {
            return Self::Show;
        };
        match first.as_str() {
            "neurocode" | "nc" => {
                let sub = match parts.get(1).map(|s| s.as_str()) {
                    None | Some("show") => ModelNcSub::Show,
                    Some("frontier") => ModelNcSub::Frontier(
                        parts[2..].join(" "),
                    ),
                    Some("economical") | Some("eco") => ModelNcSub::Economical(
                        parts[2..].join(" "),
                    ),
                    Some("reset") => ModelNcSub::Reset,
                    Some(other) => ModelNcSub::Unknown(other.to_string()),
                };
                Self::Neurocode { sub }
            }
            _ => Self::Switch {
                model: parts.join(" "),
                global,
            },
        }
    }
}

fn slash_is_light(input: &str) -> bool {
    if !input.trim_start().starts_with('/') {
        return false;
    }
    match slash::resolve(input) {
        // /queue is UI-side in BOTH modes: while busy it stashes into the
        // kill-survivable UI queue (never interrupts); while idle the same
        // stash joins the next submitted turn. Bare /queue lists the queue.
        Resolution::Command { def, .. } => {
            matches!(def.name, "status" | "help" | "copy" | "model" | "version" | "queue")
        }
        _ => false,
    }
}

/// Pop the head of the engine-queue mirror when it matches the announced
/// prompt (in-order drain). A mismatch clears the head defensively so the
/// mirror can never grow stale.
fn pop_engine_queued_head(mirror: &mut Vec<String>, announced: &str) {
    if mirror.is_empty() {
        return;
    }
    if mirror[0] == announced {
        mirror.remove(0);
    } else {
        // Out-of-order announcement (shouldn't happen — the engine runs
        // FIFO): drop the stale head so the count stays honest.
        mirror.remove(0);
    }
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
        for ok in [
            "/status", "/help", "/copy", "/copy 2", "/model", "/version", "/v",
            "  /status", "/queue while busy stashes", "/q alias stashes",
        ] {
            assert!(super::slash_is_light(ok), "expected light: {ok}");
        }
        // /qu is ambiguous (queue/quit) → unique-shortest → /quit (not
        // light); /steer is a mid-turn engine command (not light).
        for heavy in ["/neurocode index", "/start-work", "/quit", "/clear", "hello", "", "/steer x", "/qu x"] {
            assert!(!super::slash_is_light(heavy), "expected not light: {heavy}");
        }
    }

    /// /model grammar parsing: the four forms the TUI handler routes on.
    #[test]
    fn model_slash_parses_all_forms() {
        use super::ModelSlash;
        // Bare show.
        assert!(matches!(ModelSlash::parse(""), ModelSlash::Show));
        assert!(matches!(ModelSlash::parse("   "), ModelSlash::Show));
        // Plain model switch (session + global; --global stripped from name).
        match ModelSlash::parse("gpt-5.4") {
            ModelSlash::Switch { model, global } => {
                assert_eq!(model, "gpt-5.4");
                assert!(!global);
            }
            other => panic!("expected Switch, got {other:?}"),
        }
        match ModelSlash::parse("claude-opus-4.6 --global") {
            ModelSlash::Switch { model, global } => {
                assert_eq!(model, "claude-opus-4.6");
                assert!(global);
            }
            other => panic!("expected Switch, got {other:?}"),
        }
        // Multi-word model names stay joined.
        match ModelSlash::parse("openai gpt-5.4") {
            ModelSlash::Switch { model, .. } => assert_eq!(model, "openai gpt-5.4"),
            other => panic!("expected Switch, got {other:?}"),
        }
        // Neurocode forms.
        assert!(matches!(ModelSlash::parse("neurocode"), ModelSlash::Neurocode { .. }));
        match ModelSlash::parse("neurocode frontier glm-4.6") {
            ModelSlash::Neurocode { sub } => {
                assert_eq!(sub, super::ModelNcSub::Frontier("glm-4.6".into()));
            }
            other => panic!("expected Neurocode, got {other:?}"),
        }
        match ModelSlash::parse("nc economical glm-4.5-air") {
            ModelSlash::Neurocode { sub } => {
                assert_eq!(sub, super::ModelNcSub::Economical("glm-4.5-air".into()));
            }
            other => panic!("expected Neurocode, got {other:?}"),
        }
        assert!(matches!(
            ModelSlash::parse("neurocode reset"),
            ModelSlash::Neurocode { sub: super::ModelNcSub::Reset }
        ));
        assert!(matches!(
            ModelSlash::parse("neurocode show"),
            ModelSlash::Neurocode { sub: super::ModelNcSub::Show }
        ));
        match ModelSlash::parse("neurocode bogus x") {
            ModelSlash::Neurocode { sub: super::ModelNcSub::Unknown(w) } => {
                assert_eq!(w, "bogus");
            }
            other => panic!("expected Neurocode Unknown, got {other:?}"),
        }
        // Tier with no model name is Unknown-shaped usage error (frontier
        // with empty model → handled at render; parser keeps the variant).
        match ModelSlash::parse("neurocode frontier") {
            ModelSlash::Neurocode { sub } => {
                assert_eq!(sub, super::ModelNcSub::Frontier(String::new()));
            }
            other => panic!("expected Neurocode, got {other:?}"),
        }
    }

    /// The busy path must classify queue/steer via slash::resolve — the
    /// old raw starts_with checks mis-routed the /q alias (→ interrupt!)
    /// and case variants. This pins the exact resolution behavior the
    /// interactive_loop branches rely on.
    #[test]
    fn busy_path_resolution_routes_queue_steer_correctly() {
        use crate::slash::{self, Resolution};
        // Every queue form resolves to the queue command with its args
        // (`rest` preserves the tail exactly as typed — leading space).
        for input in ["/queue do the thing", "/q do the thing", "/Queue do the thing"] {
            match slash::resolve(input) {
                Resolution::Command { def, rest } => {
                    assert_eq!(def.name, "queue", "{input}");
                    assert_eq!(rest.trim(), "do the thing", "{input}");
                }
                other => panic!("{input} resolved to {other:?}"),
            }
        }
        // Steer forms likewise.
        for input in ["/steer redirect here", "/Steer redirect here"] {
            match slash::resolve(input) {
                Resolution::Command { def, rest } => {
                    assert_eq!(def.name, "steer", "{input}");
                    assert_eq!(rest.trim(), "redirect here", "{input}");
                }
                other => panic!("{input} resolved to {other:?}"),
            }
        }
        // Bare forms carry empty rest (after trim — rest_or_empty).
        assert_eq!(slash::resolve("/queue").rest_or_empty(), "");
        assert_eq!(slash::resolve("/q").rest_or_empty(), "");
    }

    /// The engine-queue mirror drains in order on announcements.
    #[test]
    fn engine_queued_mirror_drains_in_order() {
        let mut mirror = vec!["one".to_string(), "two".to_string()];
        super::pop_engine_queued_head(&mut mirror, "one");
        assert_eq!(mirror, vec!["two".to_string()]);
        super::pop_engine_queued_head(&mut mirror, "two");
        assert!(mirror.is_empty());
        // Empty mirror: no panic.
        super::pop_engine_queued_head(&mut mirror, "anything");
        assert!(mirror.is_empty());
    }

    /// slash_args_after must extract queue/steer arguments exactly,
    /// including multi-word and edge forms (used by the idle handle_slash
    /// arms).
    #[test]
    fn slash_args_extraction_for_queue_and_steer() {
        assert_eq!(super::slash_args_after("/queue check the tests", "queue"), "check the tests");
        assert_eq!(super::slash_args_after("/steer look at this instead", "steer"), "look at this instead");
        assert_eq!(super::slash_args_after("/queue", "queue"), "");
        assert_eq!(super::slash_args_after("/q check", "queue"), "check");
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
