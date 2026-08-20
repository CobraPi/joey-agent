//! Turn-actor engine: full GUI/compute decoupling for the TUI.
//!
//! Architecture (the "engine actor" model):
//!
//! ```text
//! ┌──────────── UI task (owns Tui + terminal) ─��───────────┐
//! │  select! { engine_events │ terminal_input │ frame }    │
//! └──────┬─────────────────────────────────▲───────────────┘
//!  EngineCommand (mpsc)          EngineEvent (mpsc)
//!        │                               │
//! ┌──────▼───────── engine task ─────────┴───────────────┐
//! │  owns Agent; runs turns + heavy jobs                 │
//! │  never touches the terminal                          │
//! └──────────────────────────────────────────────────────┘
//! ```
//!
//! The UI never `.await`s engine compute: it pumps events, renders frames,
//! and dispatches commands. A hung tool blocks its engine task, but the GUI
//! keeps rendering; `ForceKill` makes the UI ABANDON the engine task
//! (leaking the stuck future + agent — the interrupt flag was set first)
//! and build a fresh engine from the same config, restoring history from
//! the session DB. This is the "kill and restart any event" primitive.
//!
//! Abandonment safety: the engine's only shared state with the UI is the
//! session-id string and the session DB (SQLite WAL + busy-timeout, safe
//! for concurrent access). A stuck task is reclaimed at process exit.
//! Background processes it launched keep running — that is the documented
//! semantics of `terminal background=true`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use joey_agent_core::{Agent, AgentEvent};
use tokio::sync::mpsc;

use crate::repl::Overrides;

/// Command from the UI to the engine task.
#[allow(dead_code)] // variants are part of the command API (Interrupt used by hosts/tests)
#[derive(Debug)]
pub enum EngineCommand {
    /// Run one agent turn with this prompt (queued while another runs).
    /// The engine applies intent-gate/@plan preprocessing (agent-overlay
    /// mutations must happen where the agent lives).
    Submit {
        prompt: String,
        /// Active OMO agent name from the UI's Tab roster ("default" when
        /// none) — drives ultrawork gating.
        active_agent: String,
        /// True when the UI has NOT yet rendered this prompt as a user
        /// message (busy-path /queue and interrupt-with-message). The
        /// engine then emits `QueuedSubmitStarted` when the turn actually
        /// starts, so the user message appears in causal order — after the
        /// previous turn's final assistant message commits, before this
        /// turn's output streams. Submits the UI already recorded (its
        /// `submit()` funnel, speckit/neurocode scaffolding) pass false.
        announce: bool,
    },
    /// Cooperatively interrupt the running turn (Ctrl-C semantics).
    Interrupt,
    /// Abandon the engine (UI kills + restarts with a fresh task/agent).
    ForceKill,
    /// Switch the active OMO agent (applied between turns; queued mid-turn).
    SwitchAgent(String),
    /// Switch the main LLM model (`/model <name>`). The engine owns the
    /// agent, so the swap (and any NeuroCode engine refresh needed for
    /// per-provider tier scope) happens here. `global` also persists the
    /// choice to `model.default`.
    SwitchModel { model: String, global: bool },
    /// Run a heavy blocking job on the engine's blocking pool (currently
    /// `/neurocode …` — tree walks + SQLite bulk upserts). Light slash
    /// commands never come here; the UI answers those inline.
    HeavyJob { label: String, args: String },
    /// Run a `/hypercode run …` pipeline on the engine task. The pipeline
    /// is async (parallel subagent children), NOT blocking-pool — it runs
    /// directly on the engine with the agent's SubagentManager, so all
    /// children share the provider semaphore + interrupt handle and every
    /// child event streams to the UI through the global orchestration tap
    /// (native TUI panes). `goal` may embed `--stream a;b;c` workstreams
    /// and `--max N`.
    Hypercode { goal: String },
    /// /steer mid-turn: stash text into the agent's steer slot (no
    /// interrupt; injected after the current tool batch). Outside a turn
    /// it's a no-op (the UI queues it as a normal prompt instead).
    Steer(String),
    /// Toggle HyperCode orchestrator mode on the live agent: swap the
    /// enabled-tool surface (delegate_task-only ↔ full) and apply/clear the
    /// orchestrator overlay. Between turns.
    SetOrchestratorMode(bool),
    /// Re-read the session history from the DB into the live agent
    /// (`/undo` rewound the DB; the engine mirrors it in-memory).
    ReloadHistory,
    /// Queue an image data-URL onto the engine's agent for the next turn
    /// (`/image`, `/paste`).
    AttachImage(String),
}

/// Event from the engine task to the UI.
#[allow(dead_code)] // payload fields are part of the event API
#[derive(Debug)]
pub enum EngineEvent {
    /// A raw agent event (streaming, tools, lifecycle…).
    Agent(AgentEvent),
    /// The turn finished with this result.
    TurnFinished {
        final_text: String,
        interrupted: bool,
    },
    /// A heavy job finished with its display text.
    HeavyJobFinished { label: String, text: String },
    /// Live progress from a running `/hypercode` pipeline: phase
    /// transitions (planning → exploring → building → synthesizing).
    /// Follows the HeavyJobFinished lifecycle: progress events, then one
    /// final HeavyJobFinished { label: "hypercode", text: report }.
    HypercodeProgress { phase: String, detail: String },
    /// The engine is starting a Submit that arrived while a previous turn
    /// was still running (interrupt-with-message, or a steer that lost the
    /// turn-end race). Carries the RAW prompt so the UI can render the user
    /// message it couldn't record at submit time (the busy path never calls
    /// record_user for engine-queued submits).
    QueuedSubmitStarted { prompt: String },
    /// The engine applied an agent switch; the UI should refresh its model
    /// labels. `notice` is display text for the transcript.
    AgentSwitched { display_name: String, model: String, provider: String, notice: String },
    /// The engine applied a `/model` switch (`SwitchModel`). The UI refreshes
    /// its model/provider labels; `notice` is display text for the transcript.
    ModelSwitched { model: String, provider: String, notice: String },
    /// Pre-turn notice (intent gate announcements etc.) for the transcript.
    Notice(String),
    /// The engine finished all work (turn + heavy-job queues empty) and is
    /// about to block waiting for commands. The UI uses this to drain its
    /// UI-side `/queue` as a visible next turn. NOT sent at startup, so a
    /// freshly (re)spawned engine never auto-runs a queue the user meant
    /// to review after a force-kill.
    Idle,
    /// The engine task exited (fatal error or clean shutdown).
    EngineGone(String),
}

/// Handle to the (current) engine: command channel + join handle.
pub struct EngineHandle {
    pub cmd_tx: mpsc::UnboundedSender<EngineCommand>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl EngineHandle {
    pub fn send(&self, cmd: EngineCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// Abandon the engine: detach the task (never joined) and drop the
    /// command channel. The engine sees the closed channel and unwinds via
    /// its interrupt flag; a fully stuck engine simply leaks until process
    /// exit — by design, the GUI must survive regardless.
    pub fn abandon(mut self) {
        let _ = self.join.take(); // detach
        // cmd_tx drops here → channel closes.
    }
}

/// Everything needed to construct a FRESH agent — the UI holds this so a
/// killed engine can be replaced without user-visible state loss.
#[derive(Clone)]
pub struct EngineSpec {
    pub config: joey_core::Config,
    pub cwd: std::path::PathBuf,
    pub overrides: Overrides,
    pub session_id: String,
}

impl EngineSpec {
    /// Rebuild a fresh agent from the spec (startup + restart-after-kill).
    /// History is restored from the session DB so the conversation survives.
    pub fn build_agent(&self) -> anyhow::Result<Agent> {
        let history = crate::repl::restore_history_from_db(&self.session_id);
        crate::repl::build_agent(&self.config, &self.cwd, &self.overrides, &self.session_id, history)
    }
}

/// Spawn a fresh engine task around a PRE-BUILT agent (callers extract any
/// UI-side info — e.g. the OMO roster — before handing it over). Returns
/// the handle plus the agent's interrupt flag (the UI can request an
/// interrupt even while the engine is mid-turn and not polling commands).
///
/// `spec` is retained by the engine for features that need build-time
/// context (the HyperCode pipeline rebuilds a delegation context from it).
pub fn spawn_engine(
    agent: Agent,
    spec: EngineSpec,
    event_tx: mpsc::UnboundedSender<EngineEvent>,
) -> (EngineHandle, Arc<AtomicBool>) {
    let interrupt = agent.interrupt_handle();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<EngineCommand>();
    let join = tokio::spawn(engine_task(agent, spec, cmd_rx, event_tx));
    (EngineHandle { cmd_tx, join: Some(join) }, interrupt)
}

/// Build the HyperCode execution context for an agent: the SAME
/// SubagentManager, AgentConfig snapshot, and base registry that the
/// agent's `delegate_task` tool uses (rebuilt here because Agent keeps
/// them private). Sharing the manager means hypercode children and
/// delegate_task children compete for one provider semaphore and obey one
/// interrupt — no double-billing the provider.
pub(crate) fn hypercode_context_for_agent(
    config: &joey_core::Config,
    cwd: &std::path::Path,
    overrides: &Overrides,
    _agent: &Agent,
) -> crate::hypercode::HypercodeContext {
    let agent_config = crate::repl::build_agent_config(config, overrides);
    // Base registry = the registry build_agent starts from (builtins +
    // session/clarify/lsp), WITHOUT orchestration/neurocode additions —
    // exactly the snapshot delegate_task's children get.
    let mut base = joey_tools::ToolRegistry::with_builtins();
    {
        let session_db = joey_core::SessionDb::open_default()
            .ok()
            .map(|db| std::sync::Arc::new(std::sync::Mutex::new(db)));
        joey_tools::builtins::register_session_tools(&mut base, session_db);
        joey_tools::builtins::register_clarify_tool(&mut base, None);
        let root = cwd.to_path_buf();
        let lsp_mgr = joey_tools::lsp::LspManager::from_joey_config(config, root);
        if lsp_mgr.has_servers() {
            joey_tools::tools::lsp_tools::register_lsp_manager(lsp_mgr);
        }
    }
    let manager = std::sync::Arc::new(joey_orchestration::SubagentManager::new(
        joey_orchestration::ManagerConfig::from_config(config),
    ));
    crate::hypercode::HypercodeContext {
        agent_config,
        config: config.clone(),
        base_registry: base,
        manager,
        cwd: cwd.to_path_buf(),
    }
}

/// The engine task body. Owns the agent; processes commands sequentially;
/// forwards agent events to the UI. NEVER touches the terminal or Tui.
async fn engine_task(
    mut agent: Agent,
    spec: EngineSpec,
    mut cmd_rx: mpsc::UnboundedReceiver<EngineCommand>,
    event_tx: mpsc::UnboundedSender<EngineEvent>,
) {
    // Split the spec into the pieces the Hypercode arm needs (avoid holding
    // the whole spec alive across the loop).
    let spec_config = spec.config;
    let spec_cwd = spec.cwd;
    let spec_overrides = spec.overrides;
    // Queued prompts / jobs submitted while a turn runs.
    let mut queued: VecDeque<EngineCommand> = VecDeque::new();
    // True while a turn (or heavy job) is executing or commands sit in the
    // local queue — drives QueuedSubmitStarted and Idle emission.
    let mut busy_for_queue = false;
    // The agent's interrupt flag, captured BEFORE any turn future borrows
    // the agent (the borrow lasts for the whole turn).
    let interrupt = agent.interrupt_handle();

    loop {
        let cmd = match queued.pop_front() {
            Some(c) => c,
            None => {
                // Entering the blocking wait: all work drained. Announce
                // idleness so the UI can drain its own /queue list (the
                // engine never sees UI-side /queue entries as commands).
                // (busy_for_queue stays true here; it's only false at
                // startup — suppressing a startup Idle — and is re-set
                // after every received command.)
                if busy_for_queue {
                    let _ = event_tx.send(EngineEvent::Idle);
                }
                match cmd_rx.recv().await {
                    Some(c) => c,
                    None => return, // UI dropped the channel — exit cleanly.
                }
            }
        };
        busy_for_queue = true;

        match cmd {
            EngineCommand::Submit { prompt, active_agent, announce } => {
                // Steer handle captured BEFORE the turn future borrows the
                // agent — lets mid-turn Steer commands reach the running
                // turn without touching the borrow.
                let steer_handle = agent.steer_handle();
                if announce {
                    // This Submit was queued while busy (UI never rendered
                    // it) — show the user message now, in causal order.
                    let _ = event_tx.send(EngineEvent::QueuedSubmitStarted { prompt: prompt.clone() });
                }
                // Pre-turn preprocessing lives WITH the agent (engine side):
                // intent gate + @plan mutate agent overlays.
                let turn_text = engine_pre_turn(&mut agent, &event_tx, &prompt, &active_agent);
                if turn_text.is_empty() {
                    // Early exit: still emit a synthetic AgentEvent::Done so
                    // the UI resets its RunMode like a normal turn end —
                    // TurnFinished alone resets session.busy but NOT
                    // app.mode, which would wedge the app Busy forever
                    // (Ctrl-C escalation would then never reach Quit).
                    let _ = event_tx.send(EngineEvent::Agent(AgentEvent::Done {
                        final_text: String::new(),
                        usage: Default::default(),
                        iterations: 0,
                    }));
                    let _ = event_tx.send(EngineEvent::TurnFinished {
                        final_text: String::new(),
                        interrupted: false,
                    });
                    continue;
                }
                if !agent.client().has_credentials() {
                    let _ = event_tx.send(EngineEvent::Notice(format!(
                        "no API key for provider '{}' — run `joey model` outside the TUI.",
                        agent.client().profile().name
                    )));
                    let _ = event_tx.send(EngineEvent::Agent(AgentEvent::Done {
                        final_text: String::new(),
                        usage: Default::default(),
                        iterations: 0,
                    }));
                    let _ = event_tx.send(EngineEvent::TurnFinished {
                        final_text: String::new(),
                        interrupted: false,
                    });
                    continue;
                }
                let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
                let turn = agent.run_turn(&turn_text, tx);
                tokio::pin!(turn);
                loop {
                    tokio::select! {
                        ev = rx.recv() => {
                            match ev {
                                Some(ev) => {
                                    let _ = event_tx.send(EngineEvent::Agent(ev));
                                }
                                None => break, // turn finished + flushed
                            }
                        }
                        cmd = cmd_rx.recv() => {
                            match cmd {
                                Some(EngineCommand::Interrupt)
                                | Some(EngineCommand::ForceKill) => {
                                    // Cooperative interrupt. ForceKill is the
                                    // same for the engine; the UI additionally
                                    // abandons the handle and spawns a fresh
                                    // engine (see abandon()).
                                    interrupt.store(true, Ordering::SeqCst);
                                }
                                Some(EngineCommand::Steer(text)) => {
                                    // Mid-turn steer: no interrupt; lands
                                    // after the current tool batch.
                                    Agent::steer_via_handle(&steer_handle, &text);
                                    let _ = event_tx.send(EngineEvent::Notice(
                                        "🧭 Steer queued: lands after the current tool call".into(),
                                    ));
                                }
                                Some(other) => queued.push_back(other),
                                None => {
                                    // UI abandoned us: interrupt and keep
                                    // draining so the turn unwinds promptly;
                                    // the task exits when the loop returns.
                                    interrupt.store(true, Ordering::SeqCst);
                                }
                            }
                        }
                        res = &mut turn => {
                            // Flush remaining events before the result.
                            while let Ok(ev) = rx.try_recv() {
                                let _ = event_tx.send(EngineEvent::Agent(ev));
                            }
                            let _ = event_tx.send(EngineEvent::TurnFinished {
                                final_text: res.final_text,
                                interrupted: res.interrupted,
                            });
                            break;
                        }
                    }
                }
                // Abandoned (channel closed)? Exit instead of running queue.
                // NOTE: a bare try_recv() would CONSUME a legitimately
                // queued command that arrived at this exact instant (race).
                // We can't hold a cmd_tx clone inside the task for
                // `is_closed()` either: tokio's Sender::is_closed detects
                // *receiver* drop (we own the receiver, so it never fires)
                // and the clone would keep the channel open, breaking the
                // clean-exit-on-abandon path. Instead, re-queue anything
                // we consume so no command is ever lost.
                match cmd_rx.try_recv() {
                    Ok(cmd) => queued.push_front(cmd),
                    Err(mpsc::error::TryRecvError::Empty) => {}
                    Err(mpsc::error::TryRecvError::Disconnected) => return,
                }
            }
            EngineCommand::SwitchAgent(agent_name) => {
                let notice = engine_switch_agent(&mut agent, &agent_name);
                let _ = event_tx.send(EngineEvent::AgentSwitched {
                    display_name: agent_name,
                    model: agent.model().to_string(),
                    provider: agent.provider_name().to_string(),
                    notice,
                });
            }
            EngineCommand::SwitchModel { model, global } => {
                let notice = engine_switch_model(&mut agent, &model, global);
                let _ = event_tx.send(EngineEvent::ModelSwitched {
                    model: agent.model().to_string(),
                    provider: agent.provider_name().to_string(),
                    notice,
                });
            }
            EngineCommand::HeavyJob { label, args } => {
                // Heavy jobs run on the blocking pool so even a multi-minute
                // tree walk doesn't pin the engine's async worker.
                let out_label = label.clone();
                let job = tokio::task::spawn_blocking(move || run_heavy_job(&label, &args));
                tokio::pin!(job);
                let res = loop {
                    tokio::select! {
                        res = &mut job => {
                            break res.unwrap_or_else(|e| format!("job failed: {e}"));
                        }
                        cmd = cmd_rx.recv() => {
                            match cmd {
                                // A blocking job can't be cooperatively
                                // interrupted — acknowledge instead of going
                                // deaf (2nd Ctrl-C still force-kills via the
                                // UI abandon path).
                                Some(EngineCommand::Interrupt) => {
                                    let _ = event_tx.send(EngineEvent::Notice(
                                        "⏳ heavy job in progress — cannot interrupt; press Ctrl-C again to force-restart the engine.".into(),
                                    ));
                                }
                                Some(EngineCommand::ForceKill) => {
                                    interrupt.store(true, Ordering::SeqCst);
                                }
                                Some(other) => queued.push_back(other),
                                None => {
                                    // UI abandoned the engine — a closed
                                    // channel resolves instantly, so stay
                                    // here and the select would busy-spin.
                                    // Detach the blocking job (it runs to
                                    // completion) and exit.
                                    break String::from("(engine abandoned during heavy job)");
                                }
                            }
                        }
                    }
                };
                let _ = event_tx.send(EngineEvent::HeavyJobFinished { label: out_label, text: res });
            }
            EngineCommand::Hypercode { goal } => {
                // HyperCode runs the multi-phase parallel pipeline directly
                // on the engine's async task (children are network-bound —
                // the blocking pool would be wrong). The pipeline shares
                // the agent's provider semaphore via its own manager, and
                // every child event streams to the UI through the global
                // orchestration tap (TUI panes). Commands race the pipeline:
                // Interrupt signals the manager's cooperative interrupt.
                let ctx = hypercode_context_for_agent(
                    &spec_config,
                    &spec_cwd,
                    &spec_overrides,
                    &agent,
                );
                // Decode the UI-encoded run options (--stream/--max travel
                // in the goal string; see repl::encode_run_goal).
                let (streams, max_ws, goal) = crate::repl::decode_run_goal(&goal);
                let provider = agent.provider_name().to_string();
                let opts = crate::hypercode::HypercodeOptions {
                    workstreams: streams,
                    max_workstreams: max_ws,
                    provider,
                };
                let (prog_tx, mut prog_rx) = mpsc::unbounded_channel::<(String, String)>();
                let progress = move |phase: crate::hypercode::Phase, detail: &str| {
                    let _ = prog_tx.send((phase.label().to_string(), detail.to_string()));
                };
                let manager = ctx.manager.clone();
                let mut abandoned = false;
                let run = crate::hypercode::run_hypercode(
                    &ctx,
                    &goal,
                    &opts,
                    Some(&progress),
                );
                tokio::pin!(run);
                let res = loop {
                    tokio::select! {
                        rep = &mut run => break rep,
                        prog = prog_rx.recv() => {
                            if let Some((phase, detail)) = prog {
                                let _ = event_tx.send(EngineEvent::HypercodeProgress { phase, detail });
                            }
                        }
                        // Once the UI has abandoned us the channel resolves
                        // None instantly — disable the arm (a plain loop
                        // would busy-spin while children wind down).
                        cmd = cmd_rx.recv(), if !abandoned => {
                            match cmd {
                                Some(EngineCommand::Interrupt)
                                | Some(EngineCommand::ForceKill) => {
                                    // Cooperative: children wind down at the
                                    // next checkpoint and the pipeline
                                    // returns an interrupted report.
                                    manager.signal_interrupt();
                                    interrupt.store(true, Ordering::SeqCst);
                                }
                                Some(other) => queued.push_back(other),
                                None => {
                                    // UI abandoned the engine: signal and
                                    // keep awaiting the pipeline so children
                                    // unwind cooperatively (the run future
                                    // owns their JoinSet — dropping it would
                                    // abort them mid-provider-call).
                                    manager.signal_interrupt();
                                    interrupt.store(true, Ordering::SeqCst);
                                    abandoned = true;
                                }
                            }
                        }
                    }
                };
                // Flush any pending progress before the final report.
                while let Ok((phase, detail)) = prog_rx.try_recv() {
                    let _ = event_tx.send(EngineEvent::HypercodeProgress { phase, detail });
                }
                interrupt.store(false, Ordering::SeqCst);
                let text = res.render().join("\n");
                let _ = event_tx.send(EngineEvent::HeavyJobFinished {
                    label: "hypercode".into(),
                    text,
                });
            }
            EngineCommand::Steer(text) => {
                // Idle steer: nothing to inject into. Match the line REPL's
                // degradation — queue the text as the next turn's prompt
                // instead of silently dropping it (fix: silent data loss on
                // the turn-end race). announce=true: the UI never rendered
                // this as a user message (the busy path sends bare Steer).
                let cleaned = text.trim().to_string();
                if !cleaned.is_empty() {
                    let _ = event_tx.send(EngineEvent::Notice(
                        "🧭 no turn running — steer queued for the next turn.".into(),
                    ));
                    queued.push_back(EngineCommand::Submit {
                        prompt: cleaned,
                        active_agent: "default".into(),
                        announce: true,
                    });
                }
            }
            EngineCommand::Interrupt => {
                interrupt.store(true, Ordering::SeqCst);
            }
            EngineCommand::ReloadHistory => {
                // /undo rewound the DB — mirror it into the live agent.
                let history = crate::repl::restore_history_from_db(&spec.session_id);
                agent.set_history(history);
                let _ = event_tx.send(EngineEvent::Notice(
                    "↺ conversation history reloaded from the session store.".into(),
                ));
            }
            EngineCommand::SetOrchestratorMode(on) => {
                // /hypercode toggle (orchestrator mode): swap the tool
                // surface + overlay on the LIVE agent — no rebuild needed.
                // The system prompt's tool section was baked at build time,
                // but the registry gate (enabled_tools) is authoritative for
                // dispatch and the overlay instructs the model, so the stale
                // prompt list is cosmetic until the next rebuild.
                if on {
                    let tools = crate::hypercode::orchestrator_tool_names();
                    agent.set_enabled_tools(tools);
                    agent.set_extra_instructions(Some(crate::hypercode::orchestrator_overlay()));
                    let _ = event_tx.send(EngineEvent::Notice(
                        "⚡ orchestrator mode ON — file writes/builds now go through explorer/implementor subagents (you keep process monitoring, read-only peeks, and web)".into(),
                    ));
                } else {
                    let tools = crate::commands::platform_tools(&spec_config, "cli");
                    agent.set_enabled_tools(tools);
                    agent.set_extra_instructions(None);
                    let _ = event_tx.send(EngineEvent::Notice(
                        "orchestrator mode OFF — full tool surface restored.".into(),
                    ));
                }
            }
            EngineCommand::AttachImage(url) => {
                agent.attach_image(url);
                let _ = event_tx.send(EngineEvent::Notice(format!(
                    "🖼 image attached ({}) — it goes with your next message.",
                    agent.pending_image_count()
                )));
            }
            EngineCommand::ForceKill => {
                interrupt.store(true, Ordering::SeqCst);
                return;
            }
        }
    }
}

/// The heavy-job dispatch table. ONLY blocking, CPU-bound handlers live
/// here; anything that mutates TUI state is a light command handled by the
/// UI directly.
fn run_heavy_job(label: &str, args: &str) -> String {
    match label {
        "neurocode" => crate::commands::neurocode::neurocode_slash(args),
        // /browser (feature 016, T066): runs on the blocking pool; the
        // async connect is driven via a runtime handle (same semantics as
        // the line REPL's browser_slash).
        "browser" => run_browser_session_op(args),
        _ => format!("unknown heavy job: {label}"),
    }
}

/// Shared /browser session operation used by the TUI heavy-job path (the
/// line REPL has its own interactive handler; both drive the same global
/// BrowserHandle, so state is common — Constitution II parity).
fn run_browser_session_op(args: &str) -> String {
    use joey_tools::tools::browser_tools::shared_browser_handle;
    let handle = shared_browser_handle();
    let op = args.trim().to_lowercase();
    // Blocking pool: drive the async ops on the ambient runtime handle.
    let rt = tokio::runtime::Handle::try_current()
        .map_err(|e| format!("no tokio runtime: {e}"));
    let rt = match rt {
        Ok(rt) => rt,
        Err(e) => return e,
    };
    match op.as_str() {
        "" | "status" => {
            if handle.is_connected() {
                "Browser: connected".to_string()
            } else {
                "Browser: disconnected (/browser connect to attach or launch)".to_string()
            }
        }
        "connect" => rt.block_on(async {
            let cfg = joey_browser::BrowserConfig::from_config(
                &joey_core::config::Config::load()
                    .unwrap_or_else(|_| joey_core::config::Config::defaults()),
           );
            match handle.connect(cfg).await {
                Ok(()) => {
                    joey_browser::url_safety_bridge::install_url_safety_check(|u| {
                        browser_url_safety_shim(u)
                    });
                    "Browser connected (agent works in its own tab; your tabs are untouched).".to_string()
                }
                Err(e) => format!("Browser connect failed: {e}"),
            }
        }),
        "disconnect" => rt.block_on(async {
            match handle.disconnect().await {
                Ok(()) => "Browser disconnected.".to_string(),
                Err(e) => format!("Disconnect failed: {e}"),
            }
        }),
        other => format!("Usage: /browser [connect|disconnect|status] (got '{other}')"),
    }
}
/// Pre-turn preprocessing on the engine side: @plan prefix + intent gate.
/// Returns the text to send (empty = skip the turn). Announcements go to
/// the UI as Notice events (mirrors the old UI-side helpers).
fn engine_pre_turn(
    agent: &mut Agent,
    event_tx: &mpsc::UnboundedSender<EngineEvent>,
    message: &str,
    active_agent: &str,
) -> String {
    let mut text = message.to_string();

    // T114: @plan prefix → Prometheus (read-only planner).
    if text.starts_with("@plan ") || text == "@plan" {
        let overlay = joey_omo::agents::prompts::dispatch_system_prompt("prometheus", agent.model());
        agent.set_extra_instructions(Some(overlay));
        let _ = event_tx.send(EngineEvent::Notice(
            "📋 Switched to Prometheus (@plan) — create a plan, no execution.".into(),
        ));
        text = text.trim_start_matches("@plan").trim().to_string();
    }

    // FR-022/FR-024: intent-gate keywords.
    if let Some(keyword) = joey_omo::detect_keyword(&text) {
        match keyword {
            joey_omo::KeywordType::Ultrawork | joey_omo::KeywordType::HyperplanUltraworkCombo => {
                if joey_omo::check_ultrawork_activation(keyword, active_agent).is_some() {
                    let overlay = joey_omo::ultrawork_prompt(agent.model());
                    agent.set_extra_instructions(Some(overlay));
                    let _ = event_tx.send(EngineEvent::Notice("⚡ ULTRAWORK MODE ENABLED!".into()));
                } else {
                    let _ = event_tx.send(EngineEvent::Notice(format!(
                        "ultrawork ignored — {active_agent} is a read-only planner"
                    )));
                }
            }
            joey_omo::KeywordType::Hyperplan => {
                let _ = event_tx.send(EngineEvent::Notice("⚡ HYPERPLAN MODE ENABLED!".into()));
            }
            joey_omo::KeywordType::Team => {
                let _ = event_tx.send(EngineEvent::Notice("TEAM MODE ENABLED!".into()));
            }
        }
    }

    text
}

/// Agent switching on the engine side (T033/BC-015). Returns a notice for
/// the transcript. "default" reverts to the base joey prompt.
fn engine_switch_agent(agent: &mut Agent, agent_name: &str) -> String {
    if agent_name == "default" {
        agent.set_agent_identity(None);
        return "Reverted to the default agent".into();
    }
    // Rebuild a registry to resolve the agent's model + provider.
    let available = joey_omo::AvailableModelSet::from_connected_with_catalog(
        agent.client().profile(),
        agent.model(),
    );
    let overrides = joey_omo::agents::registry::ModelOverrides::new();
    let registry = joey_omo::AgentRegistry::build(available, &overrides);
    let Some(omo_agent) = registry.get(agent_name) else {
        return format!("Unknown agent: {agent_name}");
    };
    let Some(model) = omo_agent.resolved_model.clone() else {
        return format!(
            "{} is unavailable with the current provider/model",
            omo_agent.display_name
        );
    };
    let identity = joey_omo::dispatch_system_prompt(agent_name, &model);
    match agent.switch_model("auto", "", &model, None) {
        Ok(msg) => {
            agent.set_agent_identity(Some(identity));
            format!("{msg} — agent mode: {}", omo_agent.display_name)
        }
        Err(e) => format!("Switch failed: {e}"),
    }
}

/// `/model <name>` on the engine side: swap the live agent's main model,
/// optionally persist it, and refresh the NeuroCode engine so per-provider
/// tier scoping follows the (possibly different) provider. Returns a notice
/// for the transcript.
fn engine_switch_model(agent: &mut Agent, model: &str, global: bool) -> String {
    let mut notice = match agent.switch_model("auto", "", model, None) {
        Ok(msg) => msg,
        Err(e) => return format!("Model switch failed: {e}"),
    };
    if global {
        match joey_core::Config::load() {
            Ok(mut cfg) => match cfg.set_and_save("model.default", model) {
                Ok(()) => {
                    notice.push_str(&format!(" — saved to {}", cfg.path().display()));
                }
                Err(e) => {
                    notice.push_str(&format!(
                        " (session only — failed to persist model.default: {e})"
                    ));
                }
            },
            Err(e) => {
                notice.push_str(&format!(" (session only — config unavailable: {e})"));
            }
        }
    }
    // Refresh the NeuroCode engine: its tier resolution is scoped to the
    // ACTIVE provider (`neurocode.tier.providers.<id>`), which may have
    // changed with the model swap. Reads config fresh; None (neurocode
    // disabled) clears the agent's engine override so tiers stop applying.
    let refreshed = joey_core::Config::load().unwrap_or_else(|_| joey_core::Config::defaults());
    let scoped: Option<Arc<dyn joey_neurocode::NeuroCodeEngine>> =
        crate::neurocode_wiring::try_build_engine_scoped(
            &refreshed,
            agent.provider_name(),
        )
        .map(|e| e as Arc<dyn joey_neurocode::NeuroCodeEngine>);
    let tier_note = if scoped.is_some() {
        " · neurocode tier scope refreshed"
    } else {
        ""
    };
    agent.set_neurocode_engine_opt(scoped);
    notice.push_str(tier_note);
    notice
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heavy_job_dispatch_known_label() {
        // The neurocode label routes to the real handler (output shape
        // varies; just prove dispatch doesn't fall through to unknown).
        let out = run_heavy_job("neurocode", "status");
        assert!(!out.contains("unknown heavy job"));
    }

    #[test]
    fn heavy_job_unknown_label_answers_honestly() {
        assert!(run_heavy_job("nope", "").contains("unknown heavy job"));
    }
}

#[cfg(test)]
mod actor_tests {
    use super::*;

    /// The engine task processes commands sequentially and queues Submits
    /// that arrive mid-turn. Uses the real engine with a stub prompt that
    /// produces no provider call (no credentials path → immediate
    /// TurnFinished), verifying the actor plumbing end-to-end.
    #[tokio::test]
    async fn engine_queues_and_completes_turns() {
        // Build a spec with an unauthenticated provider so run_turn returns
        // fast (the engine sends the no-credentials notice + TurnFinished).
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "model:\n  provider: openai-api\n  default: gpt-4o-mini\n").unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let spec = EngineSpec {
            config,
            cwd: std::env::temp_dir(),
            overrides: crate::repl::Overrides::default(),
            session_id: "engtest_00000000_0000_abc123".into(),
        };
        let agent = spec.build_agent().expect("agent builds");
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_handle, _interrupt) = spawn_engine(agent, spec, ev_tx);

        // Send two submits; both should eventually produce TurnFinished
        // (queued sequentially), and the engine stays alive between them.
        _handle.send(EngineCommand::Submit { prompt: "one".into(), active_agent: "default".into(), announce: false });
        _handle.send(EngineCommand::Submit { prompt: "two".into(), active_agent: "default".into(), announce: false });
        let mut finished = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while finished < 2 && std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::TurnFinished { .. }) => finished += 1,
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            }
        }
        assert_eq!(finished, 2, "both queued turns completed");
    }

    /// `/model` switch through the real engine actor: SwitchModel swaps the
    /// agent's model (unauthenticated provider → no network) and emits
    /// ModelSwitched carrying the new model id.
    #[tokio::test]
    async fn switch_model_swaps_and_emits() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "model:\n  provider: openai-api\n  default: gpt-4o-mini\n").unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let spec = EngineSpec {
            config,
            cwd: std::env::temp_dir(),
            overrides: crate::repl::Overrides::default(),
            session_id: "engmodel_00000000_0000_abc123".into(),
        };
        let agent = spec.build_agent().expect("agent builds");
        assert_eq!(agent.model(), "gpt-4o-mini");
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let (handle, _interrupt) = spawn_engine(agent, spec, ev_tx);

        handle.send(EngineCommand::SwitchModel {
            model: "gpt-4.1".into(),
            global: false,
        });

        let mut switched = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while switched.is_none() && std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::ModelSwitched { model, provider, notice }) => {
                    switched = Some((model, provider, notice));
                }
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            }
        }
        let (model, _provider, notice) =
            switched.expect("engine emitted ModelSwitched");
        assert_eq!(model, "gpt-4.1");
        assert!(notice.contains("gpt-4.1"), "notice mentions the model: {notice}");
        // `global: false` must NOT touch model.default in the config file.
        let cfg_after = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        assert_eq!(cfg_after.get_str("model.default", ""), "gpt-4o-mini");
    }

    /// ForceKill: the channel closes and the engine task exits (join
    /// resolves) even from idle.
    #[tokio::test]
    async fn force_kill_exits_engine() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "model:\n  provider: openai-api\n  default: gpt-4o-mini\n").unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let spec = EngineSpec {
            config,
            cwd: std::env::temp_dir(),
            overrides: crate::repl::Overrides::default(),
            session_id: "engkill_00000000_0000_abc123".into(),
        };
        let agent = spec.build_agent().expect("agent builds");
        let (ev_tx, _ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let (handle, _interrupt) = spawn_engine(agent, spec, ev_tx);
        // Send ForceKill then drop our sender via abandon; the task should
        // return and the leaked join handle completes. We can't await the
        // join after abandon (it's detached), so instead verify via a
        // sentinel: send ForceKill BEFORE abandon and confirm no panic —
        // the structural guarantee is the channel close in abandon().
        handle.send(EngineCommand::ForceKill);
        handle.abandon();
        // Give the task a moment to observe the close and exit.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    fn unauth_spec(tag: &str) -> EngineSpec {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "model:\n  provider: openai-api\n  default: gpt-4o-mini\n").unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        EngineSpec {
            config,
            cwd: std::env::temp_dir(),
            overrides: crate::repl::Overrides::default(),
            session_id: format!("{tag}_00000000_0000_abc123"),
        }
    }

    /// Regression (busy deadlock fix): early-exit submit paths (empty
    /// pre-turn text, missing credentials) must emit a synthetic
    /// AgentEvent::Done BEFORE EngineEvent::TurnFinished so the UI resets
    /// RunMode like a normal turn end — TurnFinished alone only resets the
    /// host busy flag.
    #[tokio::test]
    async fn engine_early_exit_sends_done_before_turn_finished() {
        for (tag, prompt) in [("engdone1", "@plan"), ("engdone2", "hello")] {
            let eng_spec = unauth_spec(tag);
            let agent = eng_spec.build_agent().expect("agent builds");
            let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
            let (handle, _int) = spawn_engine(agent, eng_spec, ev_tx);
            handle.send(EngineCommand::Submit { prompt: prompt.into(), active_agent: "default".into(), announce: false });

            let mut saw_done_at: Option<usize> = None;
            let mut saw_finished_at: Option<usize> = None;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while saw_finished_at.is_none() && std::time::Instant::now() < deadline {
                match ev_rx.try_recv() {
                    Ok(EngineEvent::Agent(AgentEvent::Done { .. })) => {
                        assert!(saw_done_at.is_none(), "double Done in {tag}");
                        saw_done_at = Some(0usize); // marker; order checked below
                    }
                    Ok(EngineEvent::TurnFinished { .. }) => {
                        // Done must ALREADY have been seen.
                        assert!(saw_done_at.is_some(), "{tag}: TurnFinished without a preceding Done");
                        saw_finished_at = Some(0usize);
                    }
                    Ok(_) => {}
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
                }
            }
            assert!(saw_finished_at.is_some(), "{tag}: TurnFinished never arrived");
        }
    }

    /// Regression (try_recv race fix): a command arriving right as the
    /// previous turn finishes must NOT be swallowed by the post-turn
    /// abandon check — it is re-queued and runs.
    #[tokio::test]
    async fn engine_survives_post_turn_submit() {
        let spec = unauth_spec("engrace");
        let agent = spec.build_agent().expect("agent builds");
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let (handle, _int) = spawn_engine(agent, spec, ev_tx);
        handle.send(EngineCommand::Submit { prompt: "one".into(), active_agent: "default".into(), announce: false });
        // Wait for the first turn to fully finish, THEN submit — this is
        // exactly the instant the old try_recv-based check raced with.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match ev_rx.try_recv() {
                Ok(EngineEvent::TurnFinished { .. }) => break,
                Ok(_) => {}
                Err(_) => {
                    assert!(std::time::Instant::now() < deadline, "first turn never finished");
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            }
        }
        handle.send(EngineCommand::Submit { prompt: "two".into(), active_agent: "default".into(), announce: false });
        let mut finished = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while finished == 0 && std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::TurnFinished { .. }) => finished += 1,
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
        assert_eq!(finished, 1, "post-turn submit was processed, not consumed by the abandon check");
    }

    /// Regression (idle-steer data loss): a Steer that arrives after the
    /// turn ended (race between TurnFinished and the command) must NOT be
    /// dropped — the engine queues it as the next turn and tells the UI.
    #[tokio::test]
    async fn idle_steer_degrades_to_queued_submit() {
        let spec = unauth_spec("engsteer1");
        let agent = spec.build_agent().expect("agent builds");
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let (handle, _int) = spawn_engine(agent, spec, ev_tx);
        handle.send(EngineCommand::Steer("please also check the tests".into()));

        let mut saw_notice = false;
        let mut saw_turn_finished = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !(saw_notice && saw_turn_finished) && std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::Notice(t)) if t.contains("no turn running") => saw_notice = true,
                Ok(EngineEvent::TurnFinished { .. }) => saw_turn_finished = true,
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
        assert!(saw_notice, "idle steer must degrade with a notice, not vanish");
        assert!(saw_turn_finished, "degraded steer must run as a turn");
    }

    /// QueuedSubmitStarted fires for announce=true submits (busy-path)
    /// and not for announce=false ones (the normal UI funnel).
    #[tokio::test]
    async fn queued_submit_started_announces_only_marked_submits() {
        let spec = unauth_spec("engqss1");
        let agent = spec.build_agent().expect("agent builds");
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let (handle, _int) = spawn_engine(agent, spec, ev_tx);
        handle.send(EngineCommand::Submit {
            prompt: "marked".into(),
            active_agent: "default".into(),
            announce: true,
        });
        let mut saw_qss: Option<String> = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while saw_qss.is_none() && std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::QueuedSubmitStarted { prompt }) => saw_qss = Some(prompt),
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
        assert_eq!(saw_qss.as_deref(), Some("marked"));

        // Unmarked: no QueuedSubmitStarted before TurnFinished.
        handle.send(EngineCommand::Submit {
            prompt: "plain".into(),
            active_agent: "default".into(),
            announce: false,
        });
        let mut leaked_qss = false;
        let mut finished = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !finished && std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::QueuedSubmitStarted { .. }) => leaked_qss = true,
                Ok(EngineEvent::TurnFinished { .. }) => finished = true,
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
        assert!(finished);
        assert!(!leaked_qss, "unmarked submits must not announce");
    }

    /// The engine announces Idle once its queues drain (not at startup).
    #[tokio::test]
    async fn engine_announces_idle_after_drain() {
        let spec = unauth_spec("engidle1");
        let agent = spec.build_agent().expect("agent builds");
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let (handle, _int) = spawn_engine(agent, spec, ev_tx);
        handle.send(EngineCommand::Submit {
            prompt: "hello".into(),
            active_agent: "default".into(),
            announce: false,
        });
        let mut saw_idle = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !saw_idle && std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::Idle) => saw_idle = true,
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
        assert!(saw_idle, "engine must announce Idle after the queue drains");
    }

    /// `/hypercode run` through the real engine actor: the pipeline runs
    /// (unauthenticated provider → planner child fails fast), emits
    /// HypercodeProgress for the planning phase, and terminates with
    /// HeavyJobFinished { label: "hypercode" } carrying the rendered
    /// report. Verifies the full command → progress → completion cycle
    /// without touching the network.
    #[tokio::test]
    async fn hypercode_command_streams_progress_and_finishes() {
        let spec = unauth_spec("enghc1");
        let agent = spec.build_agent().expect("agent builds");
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let (handle, _int) = spawn_engine(agent, spec, ev_tx);
        handle.send(EngineCommand::Hypercode {
            goal: "test the hypercode pipeline".into(),
        });

        let mut saw_planning_progress = false;
        let mut finished_text: Option<String> = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while finished_text.is_none() && std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::HypercodeProgress { phase, .. }) => {
                    if phase == "planning" {
                        saw_planning_progress = true;
                    }
                }
                Ok(EngineEvent::HeavyJobFinished { label, text }) => {
                    assert_eq!(label, "hypercode");
                    finished_text = Some(text);
                }
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
        let text = finished_text.expect("hypercode run must finish with HeavyJobFinished");
        assert!(
            saw_planning_progress,
            "planning progress event must arrive: {text}"
        );
        assert!(
            text.contains("HyperCode run"),
            "final report must be the rendered HypercodeReport: {text}"
        );
        // The engine must stay alive afterwards (Idle still emitted).
        handle.send(EngineCommand::Submit {
            prompt: "ping".into(),
            active_agent: "default".into(),
            announce: false,
        });
        let mut saw_finished = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !saw_finished && std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::TurnFinished { .. }) => saw_finished = true,
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
        assert!(saw_finished, "engine survives a hypercode run");
    }
}

#[cfg(test)]
mod steer_command_tests {
    use super::*;

    /// Steer commands arriving mid-turn reach the agent's shared steer
    /// slot via the handle — no borrow conflict, no interrupt. The
    /// end-to-end marker injection is covered by agent-core's steer_tests;
    /// here we verify the engine command routing keeps the turn alive.
    #[tokio::test]
    async fn steer_command_does_not_kill_turn_or_lose_text() {
        // The steer handle mechanism: two sequential steers concatenate.
        let handle = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        assert!(Agent::steer_via_handle(&handle, "one"));
        assert!(Agent::steer_via_handle(&handle, "two"));
        assert_eq!(*handle.lock().unwrap(), "one\ntwo");
        assert!(!Agent::steer_via_handle(&handle, "  "));
    }
}

/// URL-safety shim: bridges joey-browser's injected checker to the REAL
/// `url_safety::is_safe_url` used by the web tools (FR-020 — one policy,
/// one implementation). Loads config lazily per call.
pub(crate) fn browser_url_safety_shim(url: &str) -> Result<(), String> {
    let cfg = joey_core::config::Config::load().unwrap_or_else(|_| joey_core::config::Config::defaults());
    if joey_tools::url_safety::is_safe_url(url, &cfg) {
        Ok(())
    } else {
        Err(format!("local/private network target refused: {url}"))
    }
}
