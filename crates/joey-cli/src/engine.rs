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

/// The synthetic prompt the idle-wake turn runs with (T013, FR-003). Worded
/// to look like a system notification, not a user request, and to avoid every
/// intent-gate keyword (ultrawork/hyperplan/team) so `engine_pre_turn` never
/// mutates overlays on a wake. The turn's real payload is the
/// pending-completions drain at `run_turn` start; the prompt text merely
/// gives the model something to acknowledge if it IS dispatched (credentialed
/// provider) while keeping the early-exit path (no credentials) trivially
/// harmless.
pub(crate) const WAKE_PROMPT: &str =
    "(system) A background delegation finished while you were idle. Review the completion notice above and act on it if warranted; otherwise acknowledge briefly.";

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
    /// Ignored (with a Notice) when no turn is running: a stale idle
    /// store would poison the next turn (Agent::run_turn clears the
    /// flag at start, then checks it shortly after — a true landing in
    /// that window aborts a brand-new turn as "interrupted").
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
    /// A background delegation finished while the engine was idle (spec 020
    /// T013, FR-003 "idle wake"). Sent by the TUI pump when the delegation
    /// tap reports a terminal child lifecycle event and no turn/heavy job is
    /// running. The engine starts a synthetic turn so `run_turn` drains the
    /// agent's pending-completions queue (notices are injected as context at
    /// turn start). Mid-turn it is a deliberate no-op: FR-003 routes
    /// mid-turn completions to the next turn boundary, and a turn that is
    /// already polling drains the queue when the NEXT turn starts.
    DelegationNoticePending,
    /// T024 (US6, FR-017, spec 020): stop the delegation child `id`
    /// (operator-requested — focused-pane `x` in the TUI). Routed to the
    /// session manager with `StopReason::OperatorRequested`; the engine
    /// acks (or surfaces the manager's error) as an `EngineEvent::Notice`.
    /// Handled IMMEDIATELY even mid-turn: control ops are synchronous and
    /// never acquire a semaphore permit (SC-007), and the target is the
    /// CHILD, not the parent turn.
    StopSubagent { id: u64 },
    /// T024 (US6, spec 020): steer the delegation child `id` — deliver
    /// `text` before the child's next action (focused-pane `s` overlay in
    /// the TUI). Routed to the session manager's `steer_child`; ordering
    /// (steer lands before the child's next action) is the manager's
    /// concern — the engine merely must not reorder or drop the command.
    SteerSubagent { id: u64, text: String },
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
    /// T024 (spec 020): the session's SubagentManager — the SAME Arc the
    /// agent's `delegate_task` tool holds (from `repl::build_agent_parts`).
    /// Children are keyed by process-global ids and tracked in the shared
    /// registry, so a manager built independently of the agent's tools
    /// would see an EMPTY registry and stop/steer would always miss. The
    /// engine routes `StopSubagent`/`SteerSubagent` here (FR-017), and the
    /// TUI exit path awaits its bounded `shutdown` (T025 wind-down).
    pub subagent_manager: std::sync::Arc<joey_orchestration::SubagentManager>,
}

impl EngineSpec {
    /// Rebuild a fresh agent from the spec (startup + restart-after-kill).
    /// History is restored from the session DB so the conversation survives.
    pub fn build_agent(&self) -> anyhow::Result<Agent> {
        let history = crate::repl::restore_history_from_db(&self.session_id);
        crate::repl::build_agent(&self.config, &self.cwd, &self.overrides, &self.session_id, history)
    }

    /// Wind-down timeout (T025, FR-015) for the exit-path `shutdown` call:
    /// the manager's own config (`delegation.wind_down_timeout_secs`,
    /// default 10s) — the same bound `SubagentManager::shutdown` enforces.
    pub fn wind_down_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            self.subagent_manager.config().wind_down_timeout_secs,
        )
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
    agent: &Agent,
) -> crate::hypercode::HypercodeContext {
    let agent_config = crate::repl::build_agent_config(config, overrides);
    // Capture the LIVE effective main-turn model (tier-routed / allocator
    // / image-routed — NOT the raw config default) so hypercode children
    // inherit what the parent actually dispatches with.
    let parent_effective_model = Some(agent.effective_main_turn_model());
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
        parent_effective_model,
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
    // Live view of HyperCode orchestrator mode. The spec's config snapshot
    // goes stale the moment /hypercode toggles (it persists to disk and
    // sends SetOrchestratorMode; the spec is not rebuilt), so switch_model
    // re-application below must consult THIS flag, not re-read the snapshot.
    let mut orchestrator_on = crate::hypercode::orchestrator_active(&spec_config);
    let spec_cwd = spec.cwd;
    let spec_overrides = spec.overrides;
    // T024: the session's SubagentManager for stop/steer routing (FR-017).
    let spec_manager = spec.subagent_manager;
    // T024: while a /hypercode pipeline runs, ITS manager (built fresh by
    // hypercode_context_for_agent) owns the live children — stop/steer for
    // ids the session manager doesn't know fall back to it.
    let mut hypercode_manager: Option<std::sync::Arc<joey_orchestration::SubagentManager>> = None;
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
                // Idle→running transition, at the top of the arm: the actor
                // loop is between futures here — no turn future exists and
                // no mid-turn select can race us — so this is the atomic
                // point to wipe any residual interrupt flag. A queued
                // Submit is only popped AFTER the previous turn finished
                // (its interrupt already consumed), so this never cancels
                // a legit in-flight interrupt; it only removes poison set
                // while idle (defense-in-depth: Agent::run_turn clears the
                // flag at start and checks it shortly after, so a stale
                // true landing in that window aborts a brand-new turn at
                // birth). Covers the early-exit paths too.
                interrupt.store(false, Ordering::SeqCst);
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
                                Some(EngineCommand::Interrupt) => {
                                    // Always mid-turn here (the turn future
                                    // is being polled in this select).
                                    match interrupt_action_for(true) {
                                        InterruptAction::Signal => {
                                            interrupt.store(true, Ordering::SeqCst);
                                        }
                                        InterruptAction::Ignore => {}
                                    }
                                }
                                Some(EngineCommand::ForceKill) => {
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
                                Some(EngineCommand::DelegationNoticePending) => {
                                    // Mid-turn idle-wake request (T013):
                                    // deliberate NO-OP. The pending-completions
                                    // queue drains at the NEXT turn's start
                                    // (run_turn drains before its first
                                    // provider call), and FR-003 routes
                                    // mid-turn completions to the next turn
                                    // boundary — never preempts this turn.
                                }
                                Some(EngineCommand::StopSubagent { id }) => {
                                    // T024: child control ops target the
                                    // CHILD, not this turn — act immediately
                                    // (permit-free, synchronous; SC-007)
                                    // instead of queueing behind the turn.
                                    engine_stop_subagent(&[spec_manager.clone()], id, &event_tx);
                                }
                                Some(EngineCommand::SteerSubagent { id, text }) => {
                                    // T024: same immediacy for steer — the
                                    // child must receive it before ITS next
                                    // action, whenever that is.
                                    engine_steer_subagent(&[spec_manager.clone()], id, &text, &event_tx);
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
                let notice = engine_switch_agent(&mut agent, &agent_name, orchestrator_on);
                let _ = event_tx.send(EngineEvent::AgentSwitched {
                    display_name: agent_name,
                    model: agent.model().to_string(),
                    provider: agent.provider_name().to_string(),
                    notice,
                });
            }
            EngineCommand::SwitchModel { model, global } => {
                let notice = engine_switch_model(&mut agent, &model, global, orchestrator_on);
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
                // Scope NeuroCode tier resolution to the LIVE agent provider
                // (the engine actor owns the agent, so this survives /model
                // switches — same scope /model neurocode writes its keys under).
                let live_provider = agent.provider_name().to_string();
                let job =
                    tokio::task::spawn_blocking(move || run_heavy_job(&label, &args, &live_provider));
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
                // T024: the hypercode pipeline's children live in THIS
                // manager's registry — record it so stop/steer routing
                // (idle or mid-pipeline) consults both it and the session
                // manager.
                hypercode_manager = Some(manager.clone());
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
                                Some(EngineCommand::StopSubagent { id }) => {
                                    // T024: operator stop of ONE hypercode
                                    // child — not the whole pipeline. Act
                                    // immediately (permit-free; SC-007).
                                    engine_stop_subagent(
                                        &[spec_manager.clone(), manager.clone()],
                                        id,
                                        &event_tx,
                                    );
                                }
                                Some(EngineCommand::SteerSubagent { id, text }) => {
                                    // T024: operator steer of one child.
                                    engine_steer_subagent(
                                        &[spec_manager.clone(), manager.clone()],
                                        id,
                                        &text,
                                        &event_tx,
                                    );
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
                // Idle path (no turn future is being polled — mid-turn
                // Interrupts are consumed by the Submit arm's select and
                // never reach the local queue). A turn-less Interrupt must
                // NOT store the flag: Agent::run_turn clears it at start
                // and checks it shortly after, so a stale true poisons the
                // NEXT turn (born interrupted — "the agent interrupted
                // itself"). Acknowledge instead, matching the heavy-job
                // arm's notice style.
                match interrupt_action_for(false) {
                    InterruptAction::Signal => {
                        interrupt.store(true, Ordering::SeqCst);
                    }
                    InterruptAction::Ignore => {
                        let _ = event_tx.send(EngineEvent::Notice(
                            "⚡ no turn running — interrupt ignored.".into(),
                        ));
                    }
                }
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
                // Keep the live flag authoritative for later
                // switch_model/switch_agent overlay re-application.
                orchestrator_on = on;
                // /hypercode toggle (orchestrator mode): swap the tool
                // surface + overlay on the LIVE agent — no agent rebuild
                // needed. The system prompt's tool section was baked at
                // build time, so re-bake it from the new enabled list:
                // otherwise the stale tool guidance contradicts the
                // reduced schemas + overlay on provider wires that weigh
                // the baked prompt heavily.
                if on {
                    let tools = crate::hypercode::orchestrator_tool_names();
                    agent.set_enabled_tools(tools);
                    agent.rebuild_system_prompt();
                    agent.set_extra_instructions(Some(crate::hypercode::orchestrator_overlay()));
                    let _ = event_tx.send(EngineEvent::Notice(
                        "⚡ orchestrator mode ON — file writes/builds now go through explorer/implementor subagents (you keep process monitoring, read-only peeks, and web)".into(),
                    ));
                } else {
                    let tools = crate::commands::platform_tools(&spec_config, "cli");
                    agent.set_enabled_tools(tools);
                    agent.rebuild_system_prompt();
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
            EngineCommand::DelegationNoticePending => {
                // Idle wake (spec 020 T013, FR-003): a background delegation
                // finished while no turn was running. Start a synthetic turn
                // whose ONLY job is to drain the agent's pending-completions
                // queue — `run_turn` injects each notice into the
                // conversation as context at turn start, so the orchestrator
                // autonomously processes completions without user input.
                // Reuses the Submit machinery (announce=true so the UI
                // renders the wake prompt in causal order); if nothing is
                // actually pending, the wake is harmless — a no-notice turn
                // the UI records as an autonomous tick.
                let _ = event_tx.send(EngineEvent::Notice(
                    "🔔 background delegation finished — waking the orchestrator to process the completion notice.".into(),
                ));
                queued.push_front(EngineCommand::Submit {
                    prompt: WAKE_PROMPT.to_string(),
                    active_agent: "default".into(),
                    announce: true,
                });
            }
            EngineCommand::StopSubagent { id } => {
                // T024 (US6, FR-017): operator stop of one delegation child.
                // Synchronous control op — runs immediately even from idle.
                let candidates = hypercode_manager
                    .as_ref()
                    .map(|m| vec![spec_manager.clone(), m.clone()])
                    .unwrap_or_else(|| vec![spec_manager.clone()]);
                engine_stop_subagent(&candidates, id, &event_tx);
            }
            EngineCommand::SteerSubagent { id, text } => {
                // T024 (US6): operator steer of one delegation child.
                let candidates = hypercode_manager
                    .as_ref()
                    .map(|m| vec![spec_manager.clone(), m.clone()])
                    .unwrap_or_else(|| vec![spec_manager.clone()]);
                engine_steer_subagent(&candidates, id, &text, &event_tx);
            }
            EngineCommand::ForceKill => {
                interrupt.store(true, Ordering::SeqCst);
                return;
            }
        }
    }
}

/// T024 (US6, FR-017, spec 020): TUI-initiated child stop → the manager's
/// control plane with the OPERATOR-requested reason. FR-017 distinguishes
/// human stops from model stops in the child's terminal record, so this
/// must be `StopReason::OperatorRequested` — never `OrchestratorRequested`
/// (that variant is reserved for the model's own `subagent_control` tool).
/// Synchronous and permit-free (SC-007): safe to run mid-turn without
/// disturbing the parent's turn future. Acks/errors surface as
/// `EngineEvent::Notice` so the operator sees them in the transcript.
///
/// `candidates` are the managers that may own the child (the session
/// manager from the spec, plus the transient hypercode pipeline manager
/// while one runs): the first that KNOWS the id (running or historical)
/// gets the call; none knowing it yields the unknown-id error.
fn engine_stop_subagent(
    candidates: &[std::sync::Arc<joey_orchestration::SubagentManager>],
    id: u64,
    event_tx: &mpsc::UnboundedSender<EngineEvent>,
) {
    for manager in candidates {
        if manager.child_status(id).is_some() {
            match manager.stop_child(id, joey_orchestration::StopReason::OperatorRequested) {
                Ok(()) => {
                    let _ = event_tx.send(EngineEvent::Notice(format!(
                        "🛑 stop requested for subagent {id} (operator) — winding down at its next checkpoint"
                    )));
                }
                Err(e) => {
                    let _ = event_tx.send(EngineEvent::Notice(format!(
                        "🛑 cannot stop subagent {id}: {e}"
                    )));
                }
            }
            return;
        }
    }
    let _ = event_tx.send(EngineEvent::Notice(format!(
        "🛑 cannot stop subagent {id}: no such child in this session"
    )));
}

/// T024 (US6, spec 020): TUI-initiated child steer → the manager's
/// `steer_child`, which appends `text` to the child's steer slot for
/// delivery before its next action. Ordering (steer-before-next-action)
/// is the manager's concern; the engine's contract is only to route the
/// command promptly (never queue it behind the running turn) and not drop
/// it. Acks/errors surface as `EngineEvent::Notice`. Same candidate-manager
/// resolution as [`engine_stop_subagent`].
fn engine_steer_subagent(
    candidates: &[std::sync::Arc<joey_orchestration::SubagentManager>],
    id: u64,
    text: &str,
    event_tx: &mpsc::UnboundedSender<EngineEvent>,
) {
    for manager in candidates {
        if manager.child_status(id).is_some() {
            match manager.steer_child(id, text) {
                Ok(()) => {
                    let _ = event_tx.send(EngineEvent::Notice(format!(
                        "🧭 steer queued for subagent {id} — lands before its next action"
                    )));
                }
                Err(e) => {
                    let _ = event_tx.send(EngineEvent::Notice(format!(
                        "🛑 cannot steer subagent {id}: {e}"
                    )));
                }
            }
            return;
        }
    }
    let _ = event_tx.send(EngineEvent::Notice(format!(
        "🛑 cannot steer subagent {id}: no such child in this session"
    )));
}

/// How the engine actor reacts to an Interrupt-family command, given
/// whether a turn is currently executing. Pure decision — unit-tested;
/// the actor loop sites (mid-turn select arm, idle top-level arm) both
/// route through [`interrupt_action_for`].
///
/// Stale-interrupt poisoning (the bug this gates): the interrupt flag is
/// Arc-shared with the Agent, and `Agent::run_turn` clears it at start
/// then first checks it shortly after. An Interrupt stored while NO turn
/// is running therefore sits latent and, if a Submit lands in the
/// clear→check window (queued/delayed command, then a submit), the
/// brand-new turn is instantly "interrupted" — the agent appears to
/// interrupt itself. Gating idle Interrupts to `Ignore` (plus clearing
/// the flag at the idle→running transition as defense-in-depth) removes
/// the engine-side poison path.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum InterruptAction {
    /// Set the shared flag: a turn is live and must cooperatively unwind.
    Signal,
    /// No turn is running: dropping the request (a Notice is emitted by
    /// the caller). Storing would poison the next turn.
    Ignore,
}

/// Decision for an Interrupt command arriving at the engine actor.
/// `turn_running` is true only while a Submit's turn future is being
/// polled (the idle top-level arm and the between-commands path pass
/// false). ForceKill is NOT routed here: it must stay effective from any
/// state (the task then exits and the UI builds a fresh engine/agent).
pub(crate) fn interrupt_action_for(turn_running: bool) -> InterruptAction {
    if turn_running {
        InterruptAction::Signal
    } else {
        InterruptAction::Ignore
    }
}

/// The heavy-job dispatch table. ONLY blocking, CPU-bound handlers live
/// here; anything that mutates TUI state is a light command handled by
/// the UI directly. `live_provider` scopes NeuroCode tier resolution to
/// the agent's actual provider.
fn run_heavy_job(label: &str, args: &str, live_provider: &str) -> String {
    match label {
        "neurocode" => crate::commands::neurocode::neurocode_slash_provider_scoped_text(
            args,
            live_provider,
        ),
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
///
/// `orchestrator_on` is the engine's LIVE view of whether the HyperCode
/// orchestrator overlay should be present: `Agent::switch_model` clears
/// `extra_instructions` (intentional — identity overlays must not leak
/// across a model swap), so the orchestrator overlay must be re-applied
/// here when orchestrator mode is active.
fn engine_switch_agent(agent: &mut Agent, agent_name: &str, orchestrator_on: bool) -> String {
    if agent_name == "default" {
        // No switch_model here, so extra_instructions survives — nothing to
        // re-apply (the orchestrator overlay, if active, is still in place).
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
            reapply_orchestrator_overlay(agent, orchestrator_on);
            format!("{msg} — agent mode: {}", omo_agent.display_name)
        }
        Err(e) => format!("Switch failed: {e}"),
    }
}

/// Re-apply the HyperCode orchestrator overlay after a switch that cleared
/// it (`Agent::switch_model` resets `extra_instructions` by design). No-op
/// when the orchestrator is not active — the same overlay the
/// `SetOrchestratorMode` arm applies via `hypercode::orchestrator_overlay`.
fn reapply_orchestrator_overlay(agent: &mut Agent, orchestrator_on: bool) {
    if orchestrator_on {
        agent.set_extra_instructions(Some(crate::hypercode::orchestrator_overlay()));
    }
}

/// `/model <name>` on the engine side: swap the live agent's main model,
/// optionally persist it, and refresh the NeuroCode engine so per-provider
/// tier scoping follows the (possibly different) provider. Returns a notice
/// for the transcript.
fn engine_switch_model(
    agent: &mut Agent,
    model: &str,
    global: bool,
    orchestrator_on: bool,
) -> String {
    let mut notice = match agent.switch_model("auto", "", model, None) {
        Ok(msg) => msg,
        Err(e) => return format!("Model switch failed: {e}"),
    };
    // switch_model cleared extra_instructions (by design) — restore the
    // orchestrator overlay when HyperCode orchestrator mode is active.
    reapply_orchestrator_overlay(agent, orchestrator_on);
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
        let out = run_heavy_job("neurocode", "status", "zai");
        assert!(!out.contains("unknown heavy job"));
    }

    #[test]
    fn heavy_job_unknown_label_answers_honestly() {
        assert!(run_heavy_job("nope", "", "zai").contains("unknown heavy job"));
    }

    #[test]
    fn interrupt_action_signals_only_during_a_live_turn() {
        // Mid-turn (turn future being polled): the flag must be stored.
        assert_eq!(interrupt_action_for(true), InterruptAction::Signal);
        // Idle: storing would poison the next turn (run_turn clears the
        // flag at start, then checks it shortly after — a stale true in
        // that window aborts a brand-new turn at birth).
        assert_eq!(interrupt_action_for(false), InterruptAction::Ignore);
    }
}

#[cfg(test)]
mod actor_tests {
    use super::*;

    /// Shared lock for actor tests that (transitively) touch the
    /// process-global environment. `spec.build_agent()` ->
    /// `repl::build_agent_parts` -> `joey_omo::AvailableModelSet::
    /// from_connected_with_catalog` performs a REAL network catalog fetch
    /// (models.dev via model_catalog.rs) whenever `copilot::custom_endpoint()`
    /// is Some — which it is in a developer shell exporting
    /// AI_USAGE_HUD_BASE_URL / COPILOT_API_BASE_URL. Sibling test modules in
    /// this binary also call `Config::load()` whose `.env` import applies
    /// user values with OVERRIDE semantics, so every actor test must hold
    /// this lock for its whole body to keep the scrubbed env stable.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII test-env guard (self-contained port of llm_selector.rs's
    /// TestEnvGuard). While held:
    /// 1. ENV_LOCK is held, serializing env-touching tests in this module
    ///    against each other and against this module's Config::load_from
    ///    readers under the same lock. joey-core's cross-crate
    ///    TEST_HOME_OVERRIDE_LOCK is held for the guard's whole lifetime
    ///    too, so the process-global home override can't race other test
    ///    modules that install one (e.g. model_catalog.rs).
    /// 2. A HomeOverrideGuard pins `joey_home()` to a fresh tempdir. The
    ///    override beats both the JOEY_HOME env var and the platform
    ///    default and is re-read on every call, so NO concurrent env
    ///    mutation can make `models_dev_cache_path()` resolve to the REAL
    ///    ~/.joey — whose stale models_dev_cache.json would make stage 3
    ///    fetch https://models.dev/api.json over the network. This is the
    ///    race-free seam replacing reliance on the env var alone.
    /// 3. COPILOT_API_BASE_URL and AI_USAGE_HUD_BASE_URL are scrubbed (and
    ///    restored on drop), so a developer shell's exports can't leak into
    ///    agent construction and trigger the copilot/models.dev catalog
    ///    fetch path.
    /// 4. JOEY_HOME is still pointed at the SAME tempdir (restored on
    ///    drop): `load_joey_dotenv` (joey-core config.rs) resolves the
    ///    `.env` to import from the env var, NOT from the home override,
    ///    so this env redirect is what keeps any transitive
    ///    `Config::load()` — ours or a sibling module's — importing the
    ///    EMPTY temp `.env` instead of the developer's real one (which
    ///    sets AI_USAGE_HUD_BASE_URL and would re-magnetize provider
    ///    resolution mid-test). If load_joey_dotenv ever honors the
    ///    override, this env machinery can be dropped entirely.
    /// 5. The tempdir is seeded with an EMPTY `.env` and a fresh
    ///    (mtime=now) `models_dev_cache.json` containing `{"_":{}}`: the
    ///    cache makes the 1h-TTL stage-2 disk-cache check hit under the
    ///    overridden home, so models.dev is never fetched over the network.
    ///
    /// Drop order: the manual Drop body restores the env vars while both
    /// locks are still held; fields then drop in declaration order — the
    /// home override releases BEFORE TEST_HOME_OVERRIDE_LOCK, and ENV_LOCK
    /// (declared last) releases last.
    pub(super) struct TestEnvGuard {
        _home: joey_core::constants::HomeOverrideGuard,
        _override_lock: std::sync::MutexGuard<'static, ()>,
        prev_copilot: Option<String>,
        prev_hud: Option<String>,
        prev_home: Option<std::ffi::OsString>,
        _dir: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl TestEnvGuard {
        // pub(super): shared with sibling #[cfg(test)] modules (e.g.
        // subagent_control_tests) — test-only, never leaves the crate.
        pub(super) fn new() -> Self {
            // ENV_LOCK is a static, so the guard's lifetime is 'static.
            // Lock order ENV_LOCK → TEST_HOME_OVERRIDE_LOCK matches every
            // other taker of the pair in the workspace — no inversion.
            let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let _override_lock = joey_core::constants::TEST_HOME_OVERRIDE_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let prev_home = std::env::var_os("JOEY_HOME");
            let dir = tempfile::tempdir().expect("temp joey home");
            std::fs::write(dir.path().join(".env"), "").expect("seed empty .env");
            // Seed a fresh (mtime=now) empty models.dev disk cache: stage 2
            // serves it within the 1h TTL and stage 3 (network fetch of
            // https://models.dev/api.json) is unreachable.
            std::fs::write(dir.path().join("models_dev_cache.json"), "{\"_\":{}}")
                .expect("seed models.dev disk cache");
            // Race-free home redirect FIRST: every `joey_home()` reader
            // (models_dev_cache_path, session/auth stores) now sees the
            // tempdir even if the env var below is momentarily unset or
            // restored by a concurrent guard, so the stale-real-cache
            // models.dev fetch can never fire.
            let _home =
                joey_core::constants::HomeOverrideGuard::new(dir.path().to_path_buf());
            // The env var still matters for `load_joey_dotenv` (it resolves
            // `.env` from the env var, not the override): redirect it to the
            // SAME tempdir so any concurrent sibling `Config::load()`
            // imports the EMPTY temp `.env` — an OVERRIDE write of nothing —
            // instead of the developer's real one, so it can no longer
            // resurrect AI_USAGE_HUD_BASE_URL after the scrub below.
            std::env::set_var("JOEY_HOME", dir.path());
            // Now scrub the endpoint vars (saved for restore-on-drop). A
            // test that legitimately needs them can set its own afterwards.
            let prev_copilot = std::env::var("COPILOT_API_BASE_URL").ok();
            let prev_hud = std::env::var("AI_USAGE_HUD_BASE_URL").ok();
            std::env::remove_var("COPILOT_API_BASE_URL");
            std::env::remove_var("AI_USAGE_HUD_BASE_URL");
            Self {
                _home,
                _override_lock,
                prev_copilot,
                prev_hud,
                prev_home,
                _dir: dir,
                _lock,
            }
        }
    }

    impl Drop for TestEnvGuard {
        fn drop(&mut self) {
            // Env restores run first, while both locks are still held; the
            // fields below then release the override and the locks.
            match &self.prev_home {
                Some(v) => std::env::set_var("JOEY_HOME", v),
                None => std::env::remove_var("JOEY_HOME"),
            }
            for (k, v) in [
                ("COPILOT_API_BASE_URL", &self.prev_copilot),
                ("AI_USAGE_HUD_BASE_URL", &self.prev_hud),
            ] {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// The engine task processes commands sequentially and queues Submits
    /// that arrive mid-turn. Uses the real engine with a stub prompt that
    /// produces no provider call (no credentials path → immediate
    /// TurnFinished), verifying the actor plumbing end-to-end.
    #[tokio::test]
    async fn engine_queues_and_completes_turns() {
        let _env_guard = TestEnvGuard::new();
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
            subagent_manager: std::sync::Arc::new(joey_orchestration::SubagentManager::new(
                Default::default(),
            )),
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

    /// Regression (orchestrator overlay loss): `/model` must not drop the
    /// HyperCode orchestrator overlay. `Agent::switch_model` clears
    /// `extra_instructions` BY DESIGN (identity/personality overlays must
    /// not leak across a model swap) — the engine's switch handler has to
    /// re-apply the orchestrator overlay while orchestrator mode is active.
    /// Pre-fix this silently reverted the main agent to full-tool mode,
    /// so it opened turns with delegate_task(explorer) instead of a plan.
    #[test]
    fn switch_model_preserves_orchestrator_overlay_when_active() {
        let _env_guard = TestEnvGuard::new();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            "model:\n  provider: openai-api\n  default: gpt-4o-mini\nhypercode:\n  enabled: true\n  orchestrator_mode: true\n",
        )
        .unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let spec = EngineSpec {
            config,
            cwd: std::env::temp_dir(),
            overrides: crate::repl::Overrides::default(),
            session_id: "engorch1_00000000_0000_abc123".into(),
            subagent_manager: std::sync::Arc::new(joey_orchestration::SubagentManager::new(
                Default::default(),
            )),
        };
        let mut agent = spec.build_agent().expect("agent builds");
        // Sanity: build_agent_parts applied the overlay at construction.
        assert!(
            agent
                .effective_system_prompt()
                .contains(crate::hypercode::ORCHESTRATOR_PROMPT),
            "overlay present after build"
        );
        // Real model swap (different id -> the switch_model path that
        // clears extra_instructions actually runs).
        let notice = engine_switch_model(&mut agent, "gpt-4.1", false, true);
        assert_eq!(agent.model(), "gpt-4.1", "model actually swapped: {notice}");
        assert!(
            agent
                .effective_system_prompt()
                .contains(crate::hypercode::ORCHESTRATOR_PROMPT),
            "orchestrator overlay survives /model while orchestrator mode is active"
        );
        // Control: with the orchestrator off, a switch must NOT inject it.
        let _ = engine_switch_model(&mut agent, "gpt-4o-mini", false, false);
        assert!(
            !agent
                .effective_system_prompt()
                .contains(crate::hypercode::ORCHESTRATOR_PROMPT),
            "overlay must not be re-applied when orchestrator mode is off"
        );
    }

    /// Regression (orchestrator overlay loss): `/agents <name>` must not
    /// drop the HyperCode orchestrator overlay either. The OMO identity
    /// goes into the agent_identity slot and the overlay is re-applied on
    /// top after switch_model cleared it. Uses zai so Sisyphus resolves to
    /// glm-5.2 (exact fallback-chain hit ≠ active glm-4.5-flash), proving
    /// the real clearing path runs (an identical model would no-op).
    #[test]
    fn switch_agent_preserves_orchestrator_overlay_when_active() {
        let _env_guard = TestEnvGuard::new();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            "model:\n  provider: zai\n  default: glm-4.5-flash\nhypercode:\n  enabled: true\n  orchestrator_mode: true\n",
        )
        .unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let spec = EngineSpec {
            config,
            cwd: std::env::temp_dir(),
            overrides: crate::repl::Overrides::default(),
            session_id: "engorch2_00000000_0000_abc123".into(),
            subagent_manager: std::sync::Arc::new(joey_orchestration::SubagentManager::new(
                Default::default(),
            )),
        };
        let mut agent = spec.build_agent().expect("agent builds");
        assert!(
            agent
                .effective_system_prompt()
                .contains(crate::hypercode::ORCHESTRATOR_PROMPT),
            "overlay present after build"
        );
        let notice = engine_switch_agent(&mut agent, "sisyphus", true);
        assert!(notice.contains("agent mode: Sisyphus"), "switch ok: {notice}");
        assert_eq!(
            agent.model(),
            "glm-5.2",
            "model actually swapped (the extra_instructions-clearing path ran)"
        );
        assert!(
            agent
                .effective_system_prompt()
                .contains(crate::hypercode::ORCHESTRATOR_PROMPT),
            "orchestrator overlay survives /agents while orchestrator mode is active"
        );
        // The OMO persona landed in its own slot (stacks with the overlay).
        assert!(agent.agent_identity().is_some(), "identity slot populated");
    }

    /// `/model` switch through the real engine actor: SwitchModel swaps the
    /// agent's model (unauthenticated provider → no network) and emits
    /// ModelSwitched carrying the new model id.
    #[tokio::test]
    async fn switch_model_swaps_and_emits() {
        let _env_guard = TestEnvGuard::new();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "model:\n  provider: openai-api\n  default: gpt-4o-mini\n").unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let spec = EngineSpec {
            config,
            cwd: std::env::temp_dir(),
            overrides: crate::repl::Overrides::default(),
            session_id: "engmodel_00000000_0000_abc123".into(),
            subagent_manager: std::sync::Arc::new(joey_orchestration::SubagentManager::new(
                Default::default(),
            )),
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
        let _env_guard = TestEnvGuard::new();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "model:\n  provider: openai-api\n  default: gpt-4o-mini\n").unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let spec = EngineSpec {
            config,
            cwd: std::env::temp_dir(),
            overrides: crate::repl::Overrides::default(),
            session_id: "engkill_00000000_0000_abc123".into(),
            subagent_manager: std::sync::Arc::new(joey_orchestration::SubagentManager::new(
                Default::default(),
            )),
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

    /// Idle wake (spec 020 T013, FR-003) — end-to-end through the REAL
    /// completion path: turn 1 launches `terminal background=true
    /// notify_on_complete=true` (via a scripted mock provider returning a
    /// tool_call), the process reaper pushes a BackgroundCompletion into the
    /// engine agent's ToolContext queue, and once the engine is idle a
    /// DelegationNoticePending must start a wake turn that DRAINS the queue
    /// (the completion Notice fires inside the wake turn) and finishes.
    #[tokio::test]
    async fn idle_wake_drains_pending_completion() {
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::{TcpListener, TcpStream};

        let _env_guard = TestEnvGuard::new();
        // Credential the openai-api profile so the Submit arm's
        // has_credentials gate passes and run_turn actually dispatches to
        // the mock (restored manually; ENV_LOCK holds siblings off).
        let prev_key = std::env::var("OPENAI_API_KEY").ok();
        std::env::set_var("OPENAI_API_KEY", "test-key-wake");

        // Scripted sequential mock provider: each connection gets the next
        // response. Turn 1 = tool_call (background terminal) then final
        // text; the wake turn = one final text.
        let openai_tool_call = |args: &str| {
            serde_json::json!({
                "id": "chatcmpl-mock", "object": "chat.completion", "model": "test-model",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant", "content": null,
                        "tool_calls": [{
                            "id": "call_wake1", "type": "function",
                            "function": { "name": "terminal", "arguments": args }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
            })
            .to_string()
        };
        let openai_text = |text: &str| {
            serde_json::json!({
                "id": "chatcmpl-mock", "object": "chat.completion", "model": "test-model",
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": text },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
            })
            .to_string()
        };
        let spawn_args = serde_json::json!({
            "command": "sleep 0.2",
            "background": true,
            "notify_on_complete": true
        })
        .to_string();
        let script = vec![
            openai_tool_call(&spawn_args),
            openai_text("spawned the background job"),
            openai_text("wake acknowledged"),
        ];

        async fn read_one_request(stream: &mut TcpStream) -> std::io::Result<()> {
            let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos;
                }
                if buf.len() > 256 * 1024 {
                    return Ok(());
                }
                let n = match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await
                {
                    Ok(Ok(n)) => n,
                    _ => return Ok(()),
                };
                if n == 0 {
                    return Ok(());
                }
                buf.extend_from_slice(&chunk[..n]);
            };
            let headers = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
            let content_length = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while buf.len() < header_end + 4 + content_length {
                let n = match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await
                {
                    Ok(Ok(n)) => n,
                    _ => break,
                };
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            Ok(())
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for body in script {
                let Ok((mut stream, _)) = listener.accept().await else { break };
                let _ = read_one_request(&mut stream).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        // The atomic was removed; the script is strictly sequential per
        // connection (each accept() consumes exactly one scripted body).

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            format!(
                "model:\n  provider: openai-api\n  default: test-model\n  base_url: http://{addr}\nagent:\n  max_turns: 4\n  api_max_retries: 1\n  tool_delay: 0.0\ndisplay:\n  streaming: false\n"
            ),
        )
        .unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let spec = EngineSpec {
            config,
            cwd: std::env::temp_dir(),
            overrides: crate::repl::Overrides::default(),
            session_id: "engwake1_00000000_0000_abc123".into(),
            subagent_manager: std::sync::Arc::new(joey_orchestration::SubagentManager::new(
                Default::default(),
            )),
        };
        let agent = spec.build_agent().expect("agent builds");
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let (handle, _int) = spawn_engine(agent, spec, ev_tx);

        // Turn 1: launches the background process (mock returns the
        // tool_call, then the post-tool final text). Wait it out fully.
        handle.send(EngineCommand::Submit {
            prompt: "start a background sleep".into(),
            active_agent: "default".into(),
            announce: false,
        });
        let mut turn1_done = false;
        let mut saw_idle = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while (!turn1_done || !saw_idle) && std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::TurnFinished { .. }) => turn1_done = true,
                Ok(EngineEvent::Idle) => saw_idle = true,
                Ok(_) => {}
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        assert!(turn1_done, "turn 1 finished");
        assert!(saw_idle, "engine announced idle after turn 1");

        // Give the reaper time to observe the 0.2s process exit (50ms poll)
        // and seed the agent's pending-completions queue.
        tokio::time::sleep(Duration::from_millis(1200)).await;

        // Idle wake: the TUI-side signal (tap terminal event + not busy).
        handle.send(EngineCommand::DelegationNoticePending);

        let mut finished_after_wake = 0;
        let mut saw_wake_announce = false;
        let mut saw_wake_prompt = false;
        let mut saw_completion_notice = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while finished_after_wake < 1 && std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::TurnFinished { .. }) => finished_after_wake += 1,
                Ok(EngineEvent::Notice(t)) if t.contains("waking the orchestrator") => {
                    saw_wake_announce = true;
                }
                Ok(EngineEvent::QueuedSubmitStarted { prompt }) => {
                    assert_eq!(prompt, WAKE_PROMPT, "wake turn uses the synthetic prompt");
                    saw_wake_prompt = true;
                }
                Ok(EngineEvent::Agent(AgentEvent::Notice(t))) => {
                    if t.contains("completed: exit 0") {
                        saw_completion_notice = true;
                    }
                }
                Ok(_) => {}
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        assert_eq!(finished_after_wake, 1, "wake turn ran to completion");
        assert!(saw_wake_announce, "wake announcement emitted");
        assert!(saw_wake_prompt, "wake turn announced with the synthetic prompt");
        assert!(
            saw_completion_notice,
            "pending completion was DRAINED inside the wake turn (Notice with the completion)"
        );

        match prev_key {
            Some(v) => std::env::set_var("OPENAI_API_KEY", v),
            None => std::env::remove_var("OPENAI_API_KEY"),
        }
        handle.send(EngineCommand::ForceKill);
    }

    /// Idle wake, mid-turn arrival (T013): a DelegationNoticePending that
    /// lands while a turn is being polled is a deliberate no-op — FR-003
    /// routes mid-turn completions to the next turn boundary, and no extra
    /// (wake) turn may preempt or follow the in-flight one.
    #[tokio::test]
    async fn mid_turn_delegation_notice_pending_is_noop() {
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let _env_guard = TestEnvGuard::new();
        let prev_key = std::env::var("OPENAI_API_KEY").ok();
        std::env::set_var("OPENAI_API_KEY", "test-key-wake2");

        // One SLOW response: the single provider call of turn 1 takes ~1s,
        // giving the test a guaranteed mid-turn window.
        let body = serde_json::json!({
            "id": "chatcmpl-mock", "object": "chat.completion", "model": "test-model",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": "slow turn done" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
        })
        .to_string();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
                let mut chunk = [0u8; 4096];
                // headers + body (single read pass is enough at this size)
                let _ = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await;
                buf.extend_from_slice(&chunk);
                let _ = buf;
                tokio::time::sleep(Duration::from_millis(1000)).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(), body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            format!(
                "model:\n  provider: openai-api\n  default: test-model\n  base_url: http://{addr}\nagent:\n  max_turns: 2\n  api_max_retries: 1\n  tool_delay: 0.0\ndisplay:\n  streaming: false\n"
            ),
        )
        .unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let spec = EngineSpec {
            config,
            cwd: std::env::temp_dir(),
            overrides: crate::repl::Overrides::default(),
            session_id: "engwake2_00000000_0000_abc123".into(),
            subagent_manager: std::sync::Arc::new(joey_orchestration::SubagentManager::new(
                Default::default(),
            )),
        };
        let agent = spec.build_agent().expect("agent builds");
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let (handle, _int) = spawn_engine(agent, spec, ev_tx);

        handle.send(EngineCommand::Submit {
            prompt: "slow prompt".into(),
            active_agent: "default".into(),
            announce: false,
        });

        // Wait until the turn is provably mid-flight (TurnStart observed),
        // then fire the wake command INTO the running turn.
        let mut turn_started = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !turn_started && std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::Agent(AgentEvent::TurnStart { .. })) => turn_started = true,
                Ok(_) => {}
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
        assert!(turn_started, "turn 1 started");
        handle.send(EngineCommand::DelegationNoticePending);

        // Exactly ONE TurnFinished must ever arrive, and no wake turn may
        // run (no QueuedSubmitStarted, no wake announcement) — not during
        // the turn and not in the idle window after it.
        let mut finished = 0;
        let mut leaked_wake = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::TurnFinished { .. }) => finished += 1,
                Ok(EngineEvent::QueuedSubmitStarted { .. }) => leaked_wake = true,
                Ok(EngineEvent::Notice(t)) if t.contains("waking the orchestrator") => {
                    leaked_wake = true;
                }
                Ok(_) => {}
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
        assert_eq!(finished, 1, "exactly the original turn finished");
        assert!(!leaked_wake, "mid-turn DelegationNoticePending must not spawn a wake turn");

        match prev_key {
            Some(v) => std::env::set_var("OPENAI_API_KEY", v),
            None => std::env::remove_var("OPENAI_API_KEY"),
        }
        handle.send(EngineCommand::ForceKill);
    }

    /// The synthetic wake prompt must never trip the intent gate (a wake
    /// must not, e.g., enable ULTRAWORK) — pins the WAKE_PROMPT wording.
    #[test]
    fn wake_prompt_avoids_intent_gate_keywords() {
        assert!(joey_omo::detect_keyword(WAKE_PROMPT).is_none());
    }

    pub(super) fn unauth_spec(tag: &str) -> EngineSpec {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "model:\n  provider: openai-api\n  default: gpt-4o-mini\n").unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        EngineSpec {
            config,
            cwd: std::env::temp_dir(),
            overrides: crate::repl::Overrides::default(),
            session_id: format!("{tag}_00000000_0000_abc123"),
            subagent_manager: std::sync::Arc::new(joey_orchestration::SubagentManager::new(
                Default::default(),
            )),
        }
    }

    /// Regression (busy deadlock fix): early-exit submit paths (empty
    /// pre-turn text, missing credentials) must emit a synthetic
    /// AgentEvent::Done BEFORE EngineEvent::TurnFinished so the UI resets
    /// RunMode like a normal turn end — TurnFinished alone only resets the
    /// host busy flag.
    #[tokio::test]
    async fn engine_early_exit_sends_done_before_turn_finished() {
        let _env_guard = TestEnvGuard::new();
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

    /// Regression (stale-interrupt poisoning): an Interrupt arriving while
    /// NO turn is running must not poison the NEXT turn. Pre-fix, the idle
    /// top-level arm did `interrupt.store(true)` unconditionally; the flag
    /// is Arc-shared with the Agent, whose run_turn clears it at start and
    /// first checks it shortly after — so a Submit landing in that window
    /// was born interrupted ("the agent interrupted itself"). Post-fix the
    /// idle arm ignores the request with a Notice, and the idle→running
    /// transition additionally clears any residual flag (defense-in-depth
    /// against non-engine sources like the UI's shared Arc).
    #[tokio::test]
    async fn idle_interrupt_does_not_poison_next_turn() {
        let _env_guard = TestEnvGuard::new();
        let spec = unauth_spec("engstale");
        let agent = spec.build_agent().expect("agent builds");
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let (handle, interrupt) = spawn_engine(agent, spec, ev_tx);

        // 1) Idle: send Interrupt. The engine must NOT store the flag.
        handle.send(EngineCommand::Interrupt);
        let mut saw_ignored_notice = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !saw_ignored_notice && std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::Notice(text)) => {
                    assert!(
                        text.contains("no turn running"),
                        "unexpected notice while idle: {text}"
                    );
                    saw_ignored_notice = true;
                }
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
        assert!(saw_ignored_notice, "idle Interrupt acknowledged");
        assert!(
            !interrupt.load(Ordering::SeqCst),
            "engine must not store the interrupt flag while no turn is running"
        );

        // 2) Even a flag poisoned by a NON-engine source (the UI holds the
        // same Arc) must be wiped at the idle→running transition: poison
        // directly, then Submit, and require a clean (not interrupted)
        // outcome. Unauthenticated provider → the Submit arm's early-exit
        // path fires (no run_turn call), which pre-fix would have left the
        // poison flag set for the NEXT credentialed turn; post-fix the
        // arm-top clear wipes it before the early exit can skip it.
        interrupt.store(true, Ordering::SeqCst);
        handle.send(EngineCommand::Submit {
            prompt: "hello".into(),
            active_agent: "default".into(),
            announce: false,
        });
        let mut finished = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while finished.is_none() && std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::TurnFinished { interrupted, .. }) => {
                    finished = Some(interrupted);
                }
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
        let interrupted = finished.expect("turn finished after idle interrupt");
        assert!(
            !interrupted,
            "turn born after an idle-time interrupt must NOT be interrupted"
        );
        assert!(
            !interrupt.load(Ordering::SeqCst),
            "flag is clean after the turn"
        );

        // 3) The engine stays alive and a further turn still completes.
        handle.send(EngineCommand::Submit {
            prompt: "again".into(),
            active_agent: "default".into(),
            announce: false,
        });
        let mut second = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !second && std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::TurnFinished { .. }) => second = true,
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
        assert!(second, "engine survived the stale-interrupt scenario");
    }

    /// Regression (try_recv race fix): a command arriving right as the
    /// previous turn finishes must NOT be swallowed by the post-turn
    /// abandon check — it is re-queued and runs.
    #[tokio::test]
    async fn engine_survives_post_turn_submit() {
        let _env_guard = TestEnvGuard::new();
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
        let _env_guard = TestEnvGuard::new();
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
        let _env_guard = TestEnvGuard::new();
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
        let _env_guard = TestEnvGuard::new();
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
        let _env_guard = TestEnvGuard::new();
        let spec = unauth_spec("enghc1");
        let agent = spec.build_agent().expect("agent builds");
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let (handle, _int) = spawn_engine(agent, spec, ev_tx);
        handle.send(EngineCommand::Hypercode {
            goal: "test the hypercode pipeline".into(),
        });

        let mut saw_planning_progress = false;
        let mut finished_text: Option<String> = None;
        // T027: 60s tripped under heavy ambient load (load avg ~8-10 during
        // full-suite convergence runs, 2026-08-25) — the pipeline's child
        // phases (planner/explorer/implementor) and this test's event loop
        // all slow down together. The deadline is a meta-budget: the
        // assertions below check event content and ordering, not speed.
        // 3x headroom instead of racing ambient load.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
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

#[cfg(test)]
mod subagent_control_tests {
    use super::*;

    /// T024 (US6, FR-017): StopSubagent for an id no candidate manager
    /// knows surfaces as an EngineEvent::Notice carrying the error — the
    /// operator always gets feedback, never a silent drop. Exercises the
    /// full engine-actor path (idle top-level arm → engine_stop_subagent
    /// → candidate resolution → unknown-id error Notice). ChildRegistry is
    /// private cross-crate, so a REAL child can't be seeded here (that
    /// path is covered by joey-orchestration's stop/steer tests); the
    /// unknown-id arm is the reachable CLI-side contract.
    #[tokio::test]
    async fn engine_stop_subagent_unknown_id_yields_error_notice() {
        let _env_guard = actor_tests::TestEnvGuard::new();
        let spec = actor_tests::unauth_spec("engstop1");
        let agent = spec.build_agent().expect("agent builds");
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let (handle, _int) = spawn_engine(agent, spec, ev_tx);

        // No child was ever dispatched: id 999999 can't exist.
        handle.send(EngineCommand::StopSubagent { id: 999_999 });

        let mut notice = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while notice.is_none() && std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::Notice(t)) => notice = Some(t),
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            }
        }
        let notice = notice.expect("engine must ack StopSubagent with a Notice");
        assert!(
            notice.contains("999999"),
            "notice identifies the child id: {notice}"
        );
        assert!(
            notice.contains("cannot stop"),
            "notice is the unknown-id error path: {notice}"
        );
        handle.send(EngineCommand::ForceKill);
    }

    /// T024 (US6): SteerSubagent for an unknown id — same contract, the
    /// steer arm's error Notice.
    #[tokio::test]
    async fn engine_steer_subagent_unknown_id_yields_error_notice() {
        let _env_guard = actor_tests::TestEnvGuard::new();
        let spec = actor_tests::unauth_spec("engsteer1");
        let agent = spec.build_agent().expect("agent builds");
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
        let (handle, _int) = spawn_engine(agent, spec, ev_tx);

        handle.send(EngineCommand::SteerSubagent {
            id: 999_998,
            text: "pivot to tests-only".into(),
        });

        let mut notice = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while notice.is_none() && std::time::Instant::now() < deadline {
            match ev_rx.try_recv() {
                Ok(EngineEvent::Notice(t)) => notice = Some(t),
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            }
        }
        let notice = notice.expect("engine must ack SteerSubagent with a Notice");
        assert!(
            notice.contains("999998"),
            "notice identifies the child id: {notice}"
        );
        assert!(
            notice.contains("cannot steer"),
            "notice is the unknown-id error path: {notice}"
        );
        handle.send(EngineCommand::ForceKill);
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
