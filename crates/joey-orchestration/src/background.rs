//! Non-blocking background delegation (feature 020, User Story 1).
//!
//! FR-001: `background=true` accepts work and returns a [`WorkHandle`]
//! immediately (SC-001: <2s, in practice <100ms) while the child runs on
//! the SAME manager plumbing a blocking child uses — shared child registry
//! (per-child interrupt/steer handles, T004), child permit pool with
//! grant-back (T005/FR-018/FR-013 two-pool semantics), event tap, and
//! one-way terminal archival (FR-019). Nothing here re-implements dispatch:
//! each child is a `dispatch_single_with_overrides` call on a transient
//! manager sharing the parent's pools/registry.
//!
//! Ownership model (T009): the CALLER never awaits child tasks. Each wave
//! of background children is loaded into a `tokio::task::JoinSet` owned by
//! ONE dedicated watcher task — the only place the JoinSet lives — so every
//! child is tracked and abortable as a set. The watcher is bounded: it
//! drains `join_next()` until the set empties, fires the completion tap
//! (see [`BackgroundCompletionTap`]; wired for notices by
//! [`dispatch_background_with_notices`], T012), and exits. No bare detached
//! child spawns exist on this path.

use std::sync::Arc;

use joey_agent_core::{AgentConfig, AgentEvent};
use joey_core::Config;
use joey_tools::context::ToolContext;
use joey_tools::ToolRegistry;
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};

use crate::manager::SubagentManager;
use crate::types::{
    Budgets, DelegationRequest, DelegationResult, DelegationState, RunningUsage, StopReason,
    TaskSpec, WorkHandle,
};

/// Distilled-summary cap for completion notices (FR-003/FR-004): the summary
/// is bounded to a ~500-token budget. Tokens vary by tokenizer, so we cap on
/// CHARACTERS conservatively: ~4 chars/token ⇒ 2000 chars. SC-006: notice
/// size is bounded regardless of child transcript length — context grows
/// with subagent count, not activity volume.
const NOTICE_SUMMARY_MAX_CHARS: usize = 2000;

/// A background child's terminal outcome, handed to the completion tap.
/// Consumed by the T012 completion-notice path ([`dispatch_background_with_notices`]).
#[derive(Debug, Clone)]
pub(crate) struct BackgroundCompletion {
    pub child_id: u64,
    pub result: DelegationResult,
}

/// Completion tap for background children (FR-003 hook point, wired in T012).
/// The watcher invokes it once per finished child, on the watcher task.
pub(crate) type BackgroundCompletionTap = Arc<dyn Fn(BackgroundCompletion) + Send + Sync>;

/// String form of a [`StopReason`] matching its serde name (snake_case) —
/// mirrors the private `stop_reason_str` in manager.rs, which stays private.
fn stop_reason_str(reason: StopReason) -> &'static str {
    match reason {
        StopReason::OrchestratorRequested => "orchestrator_requested",
        StopReason::OperatorRequested => "operator_requested",
        StopReason::BudgetExceeded => "budget_exceeded",
        StopReason::SessionEnd => "session_end",
    }
}

/// Truncate `text` to at most [`NOTICE_SUMMARY_MAX_CHARS`] chars on a char
/// boundary, marking truncation with an ellipsis (the mark itself sits just
/// past the cap, keeping the budget honest at cap+1 chars).
fn truncate_summary(text: &str) -> String {
    if text.chars().count() <= NOTICE_SUMMARY_MAX_CHARS {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(NOTICE_SUMMARY_MAX_CHARS).collect();
    truncated.push('…');
    truncated
}

// ---------------------------------------------------------------------------
// T020: parent-side budget watcher (FR-011/FR-012, D6/SC-004)
// ---------------------------------------------------------------------------

/// Budget-watcher tick (D6): fast enough that a breach is detected within
/// one short action of occurring, cheap enough to run per child for the
/// child's whole lifetime. The child-side interrupt bridge polls at 50 ms,
/// so detection latency is dominated by this tick.
const BUDGET_TICK_MS: u64 = 50;

/// Whether `usage` breaches `budgets` (D6 breach math).
///
/// Turns: an IterationStart beyond max_turns is a breach. Tokens: cumulative
/// `total_tokens` strictly exceeding max_tokens. Wall: elapsed strictly
/// exceeding max_wall_clock_secs.
fn budget_breached(budgets: &Budgets, usage: &RunningUsage, elapsed_secs: f64) -> bool {
    if let Some(max_turns) = budgets.max_turns {
        if usage.iterations > max_turns as u64 {
            return true;
        }
    }
    if let Some(max_tokens) = budgets.max_tokens {
        if usage.total_tokens > max_tokens {
            return true;
        }
    }
    if let Some(max_wall) = budgets.max_wall_clock_secs {
        if elapsed_secs > max_wall as f64 {
            return true;
        }
    }
    false
}

/// One child's budget-watcher task (T020). Event sources:
/// - a dedicated mpsc channel that REPLACES the child's legacy per-dispatch
///   event channel for budgeted background children (see `spawn_wave`): the
///   child's raw events arrive here, the watcher folds usage and FORWARDS
///   every event to the original channel — IterationStart updates turns,
///   ApiCallEnd accumulates tokens, Done/Failed end the watch;
/// - the registry handle's `started_at` each tick (wall-clock leg).
///
/// Every tick the cumulative [`RunningUsage`] is mirrored into the child's
/// registry record (`record_child_usage`) so overview()/child_status() show
/// live consumption (FR-012).
///
/// On breach: `stop_child(id, BudgetExceeded)` — the manager records
/// `pending_stop` FIRST, then sets the child's per-child interrupt flag; the
/// control bridge forwards it into the child Agent within its 50 ms poll, so
/// the child stops after ≤1 more action post-detection (SC-004: the Agent
/// consults the flag at the loop top and in the tool pre-flight check — a
/// queued tool call is cancelled, never started). The child's own run
/// archives `Stopped{BudgetExceeded}` (FR-019) and emits `SubagentStopped`;
/// the completion tap then reports `outcome=budget_exceeded` (FR-016).
///
/// Cancellability / no leaks: the watcher exits when the child's event
/// channel closes (the child task dropped its sender at return — the
/// authoritative end) or when the registry record is terminal. `stop_child`
/// is idempotent while a stop is pending and errors harmlessly once
/// terminal, so repeated breach ticks racing natural completion can never
/// convert a finished child (FR-019 one-way).
#[allow(clippy::too_many_arguments)]
async fn budget_watcher(
    manager: SubagentManager,
    child_id: u64,
    budgets: Budgets,
    mut events: mpsc::UnboundedReceiver<AgentEvent>,
    forward_to: Option<mpsc::UnboundedSender<AgentEvent>>,
    started_at: std::time::Instant,
) {
    let mut usage = RunningUsage::default();
    loop {
        // Fold one RAW child event (this watcher owns the child's legacy
        // event channel — see spawn_wave) into the cumulative usage.
        fn fold(ev: &AgentEvent, usage: &mut RunningUsage) {
            match ev {
                AgentEvent::IterationStart { iteration, .. } => {
                    usage.iterations = usage.iterations.max(*iteration as u64);
                }
                AgentEvent::ApiCallEnd { usage: u } => {
                    usage.prompt_tokens += u.prompt_tokens;
                    usage.completion_tokens += u.completion_tokens;
                    usage.total_tokens += u.total_tokens;
                }
                _ => {}
            }
        }

        // Drain everything currently queued (coalescing event bursts),
        // forwarding each to the original per-dispatch channel so legacy
        // consumers (parent UI event_tx) keep seeing the child's stream.
        loop {
            match events.try_recv() {
                Ok(ev) => {
                    fold(&ev, &mut usage);
                    if let Some(prev) = forward_to.as_ref() {
                        let _ = prev.send(ev);
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    // Child task finished and dropped its sender: final
                    // mirror below, then exit. (Done/Failed arrived as
                    // events already; the channel close is the authoritative
                    // end — the manager's SubagentComplete/SubagentStopped
                    // lifecycle events precede it.)
                    manager.record_child_usage(child_id, usage);
                    return;
                }
            }
        }

        // Mirror live cumulative usage into the registry record (FR-012).
        manager.record_child_usage(child_id, usage);

        if budget_breached(&budgets, &usage, started_at.elapsed().as_secs_f64()) {
            // Err = child already terminal (raced with natural completion);
            // its own outcome wins — never convert a finished child. The
            // watcher KEEPS RUNNING (forwarding + mirroring) until the
            // channel closes so the child's wind-down events still flow.
            let _ = manager.stop_child(child_id, StopReason::BudgetExceeded);
        }

        if manager
            .child_status(child_id)
            .map_or(true, |s| s.state.is_terminal())
        {
            // Registry says terminal but the channel may still hold the
            // child's final events — drain once more, then exit.
            while let Ok(ev) = events.try_recv() {
                fold(&ev, &mut usage);
                if let Some(prev) = forward_to.as_ref() {
                    let _ = prev.send(ev);
                }
            }
            manager.record_child_usage(child_id, usage);
            return;
        }

        // Wait for the next event OR the tick — whichever comes first.
        enum Next {
            Event(AgentEvent),
            Tick,
            Closed,
        }
        let next = tokio::select! {
            maybe = events.recv() => match maybe {
                Some(ev) => Next::Event(ev),
                None => Next::Closed,
            },
            _ = tokio::time::sleep(std::time::Duration::from_millis(BUDGET_TICK_MS)) => Next::Tick,
        };
        match next {
            Next::Event(ev) => {
                fold(&ev, &mut usage);
                if let Some(prev) = forward_to.as_ref() {
                    let _ = prev.send(ev);
                }
            }
            Next::Tick => {}
            Next::Closed => {
                manager.record_child_usage(child_id, usage);
                return;
            }
        }
    }
}

/// Distill a finished child's terminal state into the completion-notice wire
/// format (contracts/config-and-events.md, character-for-character):
///
/// ```text
/// [SUBAGENT COMPLETE|FAILED|STOPPED] id=<id> goal=<goal> outcome=<...> tokens=<n> duration=<secs>s
/// <summary ≤500 tokens>
/// ```
///
/// COMPLETE for natural success (`outcome=success`), FAILED for failure
/// (`outcome=failure`, body carries the reason — failures are never silently
/// dropped, SC-002), STOPPED for stopped-with-reason (the snake_case stop
/// reason rides `outcome=`; body carries the partial-result summary).
pub(crate) fn format_completion_notice(
    child_id: u64,
    goal: &str,
    stopped_reason: Option<StopReason>,
    result: &DelegationResult,
) -> String {
    let (tag, outcome, body) = if let Some(reason) = stopped_reason {
        // Stopped children keep their partial result as the body (FR-010).
        ("STOPPED", stop_reason_str(reason), result.summary.clone())
    } else if result.success {
        ("COMPLETE", "success", result.summary.clone())
    } else {
        // Failure body prefers the reason over the (usually empty) summary.
        (
            "FAILED",
            "failure",
            result
                .error
                .clone()
                .unwrap_or_else(|| result.summary.clone()),
        )
    };
    format!(
        "[SUBAGENT {tag}] id={child_id} goal={goal} outcome={outcome} tokens={} duration={:.1}s\n{}",
        result.token_usage.total_tokens,
        result.wall_clock.as_secs_f64(),
        truncate_summary(&body),
    )
}

/// Build the T012 completion tap: on each finished child, distill a notice
/// and push it into the orchestrator's session-persistent pending-completions
/// queue on `ToolContext` (cap 64, drop-oldest — the EXISTING queue), so the
/// agent drains it at the next `run_turn` start. Delivery at a turn boundary
/// is exactly the FR-003 contract; the queue survives the launching turn.
fn completion_notice_tap(manager: &SubagentManager, ctx: ToolContext) -> BackgroundCompletionTap {
    // A transient manager sharing the registry: the child archives its
    // terminal record INSIDE its run (before the watcher's join_next fires),
    // so by tap time `child_status` always sees the one-way terminal state.
    let mgr = manager.shared_child_manager();
    Arc::new(move |completion: BackgroundCompletion| {
        let stopped_reason = match mgr.child_status(completion.child_id) {
            Some(crate::types::DelegationOverview {
                state: DelegationState::Stopped { reason },
                ..
            }) => Some(reason),
            // Not archived as Stopped: natural completion or failure. The
            // result's own stop_reason (if a future wave populates it) wins.
            _ => completion.result.stop_reason,
        };
        let notice = format_completion_notice(
            completion.child_id,
            &completion.result.goal,
            stopped_reason,
            &completion.result,
        );
        ctx.push_background_completion(joey_tools::context::BackgroundCompletion {
            // Correlates the notice with the work handle's child_id.
            session_id: format!("subagent-{}", completion.child_id),
            exit_code: if completion.result.success { 0 } else { 1 },
            output_tail: notice,
            elapsed_secs: completion.result.wall_clock.as_secs_f64(),
        });
    })
}

/// Dispatch ONE child in the background (FR-001): allocate the child id,
/// hand the run to a watcher-owned JoinSet, and return the handle NOW.
///
/// The child registers itself in the shared registry at spawn (inside the
/// run), acquires provider permits from the manager's child pool (FR-013:
/// excess work queues, never rejected), and archives its terminal state in
/// the registry exactly as a blocking child does (FR-019).
#[allow(dead_code)] // legacy unbudgeted entry; delegation_tool now routes through the budgeted chain (T021)
pub(crate) fn dispatch_background(
    manager: &SubagentManager,
    req: &DelegationRequest,
    parent_config: &AgentConfig,
    parent_config_tree: &Config,
    base_registry: &ToolRegistry,
    event_tx: Option<&mpsc::UnboundedSender<AgentEvent>>,
) -> WorkHandle {
    dispatch_background_b(
        manager,
        req,
        parent_config,
        parent_config_tree,
        base_registry,
        event_tx,
        None,
    )
}

/// [`dispatch_background`] with optional per-child budgets (T021): when set,
/// a parent-side budget watcher enforces them (T020, FR-011).
#[allow(dead_code)] // legacy unbudgeted entry; delegation_tool now routes through the budgeted chain (T021)
pub(crate) fn dispatch_background_b(
    manager: &SubagentManager,
    req: &DelegationRequest,
    parent_config: &AgentConfig,
    parent_config_tree: &Config,
    base_registry: &ToolRegistry,
    event_tx: Option<&mpsc::UnboundedSender<AgentEvent>>,
    budgets: Option<Budgets>,
) -> WorkHandle {
    dispatch_background_wave_bt(
        manager,
        std::iter::once((req.clone(), budgets)),
        parent_config,
        parent_config_tree,
        base_registry,
        event_tx,
        None,
    )
    .into_iter()
    .next()
    .expect("wave of one returns one handle")
}

/// Dispatch ONE child in the background WITH completion notices (T012,
/// FR-003/FR-004/SC-002/SC-006): like [`dispatch_background`], and the
/// watcher additionally distills each finished child into a bounded notice
/// (`[SUBAGENT COMPLETE|FAILED|STOPPED] …`) pushed via
/// `ToolContext::push_background_completion` onto the EXISTING pending-
/// completions queue (cap 64, drop-oldest), drained at the next `run_turn`
/// start. `ctx` is the ORCHESTRATOR's context — the queue is shared across
/// clones via `Arc`, so the clone the tool received shares it too.
///
/// Failures are never silently dropped at the delegation level: every
/// finished failure yields exactly one notice (US2-2). Under cap pressure
/// the queue's uniform drop-oldest applies (see tests/notices.rs).
pub fn dispatch_background_with_notices(
    manager: &SubagentManager,
    req: &DelegationRequest,
    parent_config: &AgentConfig,
    parent_config_tree: &Config,
    base_registry: &ToolRegistry,
    event_tx: Option<&mpsc::UnboundedSender<AgentEvent>>,
    orchestrator_ctx: &ToolContext,
) -> WorkHandle {
    dispatch_background_with_notices_and_budgets(
        manager,
        req,
        parent_config,
        parent_config_tree,
        base_registry,
        event_tx,
        orchestrator_ctx,
        None,
    )
}

/// [`dispatch_background_with_notices`] + optional per-child budgets (T021):
/// a parent-side budget watcher enforces them while the child runs (T020,
/// FR-011) — a breach stops the child with `BudgetExceeded`, which the
/// completion notice reports as `outcome=budget_exceeded` (FR-016).
pub fn dispatch_background_with_notices_and_budgets(
    manager: &SubagentManager,
    req: &DelegationRequest,
    parent_config: &AgentConfig,
    parent_config_tree: &Config,
    base_registry: &ToolRegistry,
    event_tx: Option<&mpsc::UnboundedSender<AgentEvent>>,
    orchestrator_ctx: &ToolContext,
    budgets: Option<Budgets>,
) -> WorkHandle {
    dispatch_background_wave_bt(
        manager,
        std::iter::once((req.clone(), budgets)),
        parent_config,
        parent_config_tree,
        base_registry,
        event_tx,
        Some(completion_notice_tap(manager, orchestrator_ctx.clone())),
    )
    .into_iter()
    .next()
    .expect("wave of one returns one handle")
}

/// Dispatch a WAVE of children in the background (batch form, FR-001):
/// ids are allocated in request order, all children load into ONE
/// watcher-owned JoinSet, and handles return immediately in the same order.
///
/// FR-013: nothing is rejected here — permits are acquired later, inside
/// each child's provider calls, from the manager's child semaphore; work
/// beyond the concurrency limits queues under the same limits that govern
/// blocking delegation. The handle does not imply a permit is held.
#[allow(dead_code)] // legacy unbudgeted wave entry; delegation_tool now routes through the budgeted chain (T021)
pub(crate) fn dispatch_background_wave<I>(
    manager: &SubagentManager,
    requests: I,
    parent_config: &AgentConfig,
    parent_config_tree: &Config,
    base_registry: &ToolRegistry,
    event_tx: Option<&mpsc::UnboundedSender<AgentEvent>>,
) -> Vec<WorkHandle>
where
    I: IntoIterator<Item = DelegationRequest>,
    I::IntoIter: ExactSizeIterator,
{
    dispatch_background_wave_bt(
        manager,
        requests.into_iter().map(|r| (r, None)),
        parent_config,
        parent_config_tree,
        base_registry,
        event_tx,
        None,
    )
}

/// Dispatch a WAVE of budgeted background children (T021 batch form):
/// each (request, budgets) pair dispatches in the background; children with
/// budgets get a parent-side budget watcher (T020). A top-level budgets
/// applies to every task in the batch (contracts/delegation-tools.md;
/// per-task override is out of scope).
#[allow(clippy::too_many_arguments)]
pub(crate) fn dispatch_background_wave_budgeted(
    manager: &SubagentManager,
    requests: Vec<(DelegationRequest, Option<Budgets>)>,
    parent_config: &AgentConfig,
    parent_config_tree: &Config,
    base_registry: &ToolRegistry,
    event_tx: Option<&mpsc::UnboundedSender<AgentEvent>>,
) -> Vec<WorkHandle> {
    dispatch_background_wave_bt(
        manager,
        requests.into_iter(),
        parent_config,
        parent_config_tree,
        base_registry,
        event_tx,
        None,
    )
}

/// Wave dispatch with a completion tap (T012) and optional per-child budgets
/// (T020/T021): same as [`dispatch_background_wave`], the watcher fires
/// `on_complete` once per finished child, and each child with budgets gets a
/// parent-side budget watcher. `dispatch_background_with_notices` installs
/// the notice-distilling tap here.
fn dispatch_background_wave_bt<I>(
    manager: &SubagentManager,
    requests: I,
    parent_config: &AgentConfig,
    parent_config_tree: &Config,
    base_registry: &ToolRegistry,
    event_tx: Option<&mpsc::UnboundedSender<AgentEvent>>,
    on_complete: Option<BackgroundCompletionTap>,
) -> Vec<WorkHandle>
where
    I: IntoIterator<Item = (DelegationRequest, Option<Budgets>)>,
    I::IntoIter: ExactSizeIterator,
{
    let requests = requests.into_iter();
    // Start the grant-back watcher from the TOP manager so lend/reclaim
    // computes with the real pool sizes (transient children would otherwise
    // seed it with default-config totals). No-op when disabled/spawned.
    manager.ensure_grant_back_watcher_shared();

    let mut handles = Vec::with_capacity(requests.len());
    let mut children: Vec<(u64, DelegationRequest, Option<Budgets>)> =
        Vec::with_capacity(requests.len());
    for (req, budgets) in requests {
        // Stable, process-global child id (T033), minted in dispatch order.
        let id = manager.next_id();
        // PRE-REGISTER at spawn (T009): the handle is backed by a registry
        // record immediately — stop/steer/overview see the child before its
        // task starts. The child's run reuses this entry (never overwrites).
        let spec = TaskSpec {
            goal: req.goal.clone(),
            context: req.context.clone(),
            model: req.model.clone(),
            toolsets: req.toolsets.clone(),
            role: None,
            background: true,
            // T021: per-child budgets ride the registry record so
            // status/overview can show caps vs consumption (FR-012) and the
            // budget watcher (T020) enforces them during the run.
            budgets,
        };
        handles.push(manager.pre_register_child(id, spec));
        children.push((id, req, budgets));
    }

    spawn_wave(
        manager,
        children,
        parent_config.clone(),
        parent_config_tree.clone(),
        base_registry.clone(),
        event_tx.cloned(),
        on_complete,
    );

    handles
}

/// Load `children` into a JoinSet owned by one dedicated watcher task and
/// return the watcher's JoinHandle. The watcher drains completions (firing
/// `on_complete` per child when set — T012 hook) and exits when the set
/// empties: bounded lifetime, no detached child tasks, full set ownership.
///
/// T020: children dispatched WITH budgets get a parent-side budget watcher.
/// Their per-dispatch `event_tx` is intercepted: the child's raw events feed
/// the budget watcher (usage folding), which FORWARDS every event to the
/// original channel so legacy consumers see an identical stream.
fn spawn_wave(
    manager: &SubagentManager,
    children: Vec<(u64, DelegationRequest, Option<Budgets>)>,
    parent_config: AgentConfig,
    parent_config_tree: Config,
    base_registry: ToolRegistry,
    event_tx: Option<mpsc::UnboundedSender<AgentEvent>>,
    on_complete: Option<BackgroundCompletionTap>,
) -> JoinHandle<()> {
    let default_model = manager.config().default_model.clone();
    let max_turns = manager.config().default_max_turns;
    let max_spawn_depth = manager.config().max_spawn_depth;

    let mut join_set: JoinSet<(u64, DelegationResult)> = JoinSet::new();
    for (id, req, budgets) in children {
        // Transient per-child manager sharing the parent's pools, registry,
        // grant-back state, interrupt, and tap — the same plumbing
        // `dispatch_requests` builds for each blocking batch child.
        let mgr = manager.shared_child_manager();
        let parent_cfg = parent_config.clone();
        let config_tree = parent_config_tree.clone();
        let registry = base_registry.clone();
        let dm = default_model.clone();
        let tx = event_tx.clone();

        // T020: budgeted child → intercept its event channel with a budget
        // watcher. The child sends its RAW events to `watch_tx`; the watcher
        // folds usage (turns/tokens), mirrors it into the registry record
        // (FR-012), enforces the budgets (stop_child(BudgetExceeded) on
        // breach), and forwards every event to the ORIGINAL per-dispatch
        // channel — legacy consumers see an identical stream. The watcher
        // ends when the child drops `watch_tx` (run returned) — no leaks.
        let child_event_tx: Option<mpsc::UnboundedSender<AgentEvent>> = if budgets.is_some() {
            let (watch_tx, watch_rx) = mpsc::unbounded_channel::<AgentEvent>();
            let started_at = manager
                .child_status(id)
                .map(|s| {
                    // Reconstruct: the record's elapsed is measured from the
                    // registry handle's spawn Instant.
                    std::time::Instant::now() - s.elapsed
                })
                .unwrap_or_else(std::time::Instant::now);
            let watcher_mgr = manager.shared_child_manager();
            tokio::spawn(budget_watcher(
                watcher_mgr,
                id,
                budgets.unwrap(),
                watch_rx,
                tx.clone(),
                started_at,
            ));
            Some(watch_tx)
        } else {
            None
        };
        let tx_for_child = child_event_tx.or(tx);

        join_set.spawn(async move {
            let result = mgr
                .dispatch_single_with_overrides(
                    &req,
                    &parent_cfg,
                    &config_tree,
                    &registry,
                    tx_for_child.as_ref(),
                    dm.as_deref(),
                    max_turns,
                    max_spawn_depth,
                    id,
                )
                .await;
            (id, result)
        });
    }

    // The watcher: sole owner of the JoinSet. Bounded — exits once every
    // child has joined (children archive themselves inside their run).
    tokio::spawn(async move {
        while let Some(joined) = join_set.join_next().await {
            match joined {
                Ok((child_id, result)) => {
                    tracing::trace!(
                        child_id,
                        goal = %result.goal,
                        success = result.success,
                        "background child finished"
                    );
                    if let Some(tap) = on_complete.as_ref() {
                        tap(BackgroundCompletion { child_id, result });
                    }
                }
                Err(join_err) => {
                    // A panicked/aborted child task: its registry entry (if
                    // registered) is wound down by `shutdown`/session end;
                    // never panic the watcher.
                    tracing::warn!("background child task failed: {join_err}");
                }
            }
        }
    })
}
