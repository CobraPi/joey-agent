//! T025 — automated responsiveness probe (User Story 1, SC-001).
//!
//! Scenario under test (quickstart.md scenario 1): while several agents issue
//! terminal calls concurrently, the runtime must stay responsive — a heartbeat
//! observer's worst tick drift stays under 150ms and it never stalls past 1
//! second.
//!
//! NOTE (tasks.md): "full realism after Phase 4 admission wiring" — the
//! governor admission layer lands in later tasks. This probe therefore drives
//! the CURRENT public terminal-tool API (`ToolRegistry::with_builtins()` →
//! `Arc<dyn Tool>` + `ToolContext` + `execute`) from >= 8 concurrent tokio
//! tasks. It probes RUNTIME responsiveness under concurrent terminal load, not
//! cap enforcement (cap tests live in joey-tools/tests/terminal_governor.rs).
//!
//! Design:
//! - AGENT_COUNT (8) simulated agents, each a spawned tokio task looping over
//!   cheap terminal calls (`echo <marker>` / `sleep 0.05`, alternating) via the
//!   real `terminal` tool execution path.
//! - A heartbeat task runs a 10ms `tokio::time::interval` tick loop and
//!   measures the drift between each tick's SCHEDULED instant and the instant
//!   the task actually observed it (`Instant::now() - tick_instant`). Under a
//!   starved/blocked runtime this drift grows; a healthy multi-thread runtime
//!   keeps it in the micro/millisecond range even with 8 concurrent tool
//!   executions.
//! - Pass criteria (asserted):
//!     1. every agent terminal call returned exit_code 0 and carried its
//!        marker (the real execution path genuinely ran concurrently), and
//!     2. the heartbeat collected >= MIN_SAMPLES observations, and
//!     3. worst observed drift < 150ms (SC-001 responsiveness budget), and
//!     4. no tick ever stalled past 1 second (hard ceiling).
//!
//! Runtime budget: 8 agents x 50 iterations of ~25ms commands ≈ 2s+ of
//! sustained concurrent load; total test wall time stays well under ~30s.
//!
//! Compile/run note: uses real wall-clock time (no `start_paused`) because the
//! terminal tool spawns real child processes.

use joey_tools::{Tool, ToolContext, ToolRegistry};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::MissedTickBehavior;

/// Simulated agents issuing terminal calls concurrently (spec: "at least 8").
const AGENT_COUNT: usize = 8;
/// Terminal calls each agent performs (alternating echo / sleep 0.05).
/// Sized so the sustained-load window comfortably exceeds the heartbeat's
/// MIN_SAMPLES floor (>=100 samples at 10ms ticks → >=1s of load): 8 agents
/// x 50 calls averaging ~25ms of child time each ≈ 2s+ under the governor.
const CALLS_PER_AGENT: usize = 50;
/// Heartbeat sampling period.
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(10);
/// SC-001: worst tolerable heartbeat response latency.
const MAX_WORST_DRIFT: Duration = Duration::from_millis(150);
/// SC-001: a tick must never stall past this hard ceiling.
const STALL_CEILING: Duration = Duration::from_secs(1);
/// The heartbeat must have actually sampled the runtime, not raced to zero.
const MIN_SAMPLES: u64 = 100;

fn terminal_tool() -> Arc<dyn Tool> {
    ToolRegistry::with_builtins()
        .get("terminal")
        .expect("terminal tool registered")
}

fn ctx(session: &str) -> ToolContext {
    ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), session)
}

fn parse_result(content: &str) -> Value {
    serde_json::from_str::<Value>(content).unwrap_or_else(|e| {
        panic!("terminal result is not valid JSON: {e}\n---\n{content}\n---")
    })
}

/// Heartbeat observer: ticks every HEARTBEAT_INTERVAL and records the worst
/// drift between the scheduled tick instant and when it actually ran, plus the
/// total sample count. Stops when `stop` is set.
async fn heartbeat(
    stop: Arc<AtomicBool>,
    worst_drift_ms: Arc<AtomicU64>,
    samples: Arc<AtomicU64>,
) {
    let mut ticker = tokio::time::interval(HEARTBEAT_INTERVAL);
    // After a stall, resume from the NEXT deadline instead of firing a burst
    // of catch-up ticks (which would mask the drift we want to observe).
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // The first tick completes immediately (interval creation); consume it so
    // it does not pollute the drift measurement.
    ticker.tick().await;

    while !stop.load(Ordering::SeqCst) {
        let scheduled = ticker.tick().await;
        let drift = tokio::time::Instant::now().saturating_duration_since(scheduled);
        let drift_ms = drift.as_millis() as u64;
        samples.fetch_add(1, Ordering::SeqCst);
        worst_drift_ms.fetch_max(drift_ms, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn heartbeat_stays_responsive_under_8_concurrent_terminal_agents() {
    let tool = terminal_tool();

    // Shared heartbeat bookkeeping + stop flag.
    let stop = Arc::new(AtomicBool::new(false));
    let worst_drift_ms = Arc::new(AtomicU64::new(0));
    let samples = Arc::new(AtomicU64::new(0));

    let heartbeat_handle = {
        let (stop, worst, count) =
            (stop.clone(), worst_drift_ms.clone(), samples.clone());
        tokio::spawn(async move { heartbeat(stop, worst, count).await })
    };

    // Spawn AGENT_COUNT simulated agents. Each drives the REAL terminal tool
    // (`execute` on the registered builtin) from its own tokio task with its
    // own ToolContext, alternating a no-op echo with a 50ms sleep so there is
    // a sustained mix of quick and (mildly) blocking child processes.
    let mut agents = Vec::with_capacity(AGENT_COUNT);
    for agent_id in 0..AGENT_COUNT {
        let tool = tool.clone();
        agents.push(tokio::spawn(async move {
            let session = format!("responsiveness-probe-agent-{agent_id}");
            let ctx = ctx(&session);
            for call in 0..CALLS_PER_AGENT {
                let marker = format!("probe-a{agent_id}-c{call}");
                let command = if call % 2 == 0 {
                    format!("echo {marker}")
                } else {
                    format!("sleep 0.05; echo {marker}")
                };
                let result = tool.execute(json!({ "command": command }), &ctx).await;
                assert!(
                    !result.is_error(),
                    "agent {agent_id} call {call} errored: {}",
                    result.to_content_string()
                );
                let v = parse_result(&result.to_content_string());
                assert_eq!(
                    v["exit_code"], json!(0),
                    "agent {agent_id} call {call} nonzero exit: {v}"
                );
                assert!(
                    v["output"].as_str().unwrap().contains(&marker),
                    "agent {agent_id} call {call} output missing marker {marker}: {v}"
                );
            }
            agent_id
        }));
    }

    // Wait for every agent to finish its terminal calls, then stop the
    // heartbeat and collect its observations. Bound the wait so a wedged agent
    // fails the test instead of hanging.
    //
    // T027: the finish budget was 20s, which assumes ~400 child-process
    // spawns run near nominal speed; under heavy ambient load (load avg
    // ~8-10 during full-suite runs, 2026-08-25) spawn+sched latency
    // inflates several-fold and the join blew the budget while every
    // agent was still progressing normally. This is a meta-deadline (a
    // wedged-runtime tripwire), NOT an SC-001 budget — the 150ms
    // worst-drift and 1s no-stall ceilings below stay strict. 60s is
    // several-fold the nominal ~2-3s of child time yet still trips a
    // genuine runtime stall.
    let joined = tokio::time::timeout(Duration::from_secs(60), futures::future::join_all(agents)).await;
    let joined = joined.expect("agents did not finish within 60s (runtime stalled?)");
    for handle in joined {
        handle.expect("agent task panicked");
    }

    stop.store(true, Ordering::SeqCst);
    heartbeat_handle
        .await
        .expect("heartbeat task panicked");

    let worst = worst_drift_ms.load(Ordering::SeqCst);
    let sample_count = samples.load(Ordering::SeqCst);
    eprintln!(
        "responsiveness_probe: samples={sample_count} worst_drift={worst}ms \
         (budgets: drift<{MAX_WORST_DRIFT:?}, stall<{STALL_CEILING:?})",
    );

    // The probe must have actually sampled the runtime for a meaningful
    // window (~2s of sustained load at 10ms ticks → hundreds of samples).
    assert!(
        sample_count >= MIN_SAMPLES,
        "heartbeat collected only {sample_count} samples (expected >= {MIN_SAMPLES}); \
         the load window was too short to probe responsiveness"
    );

    // SC-001: worst observed response latency under 150ms.
    assert!(
        Duration::from_millis(worst) < MAX_WORST_DRIFT,
        "heartbeat worst drift {worst}ms exceeds {}ms budget with \
         {AGENT_COUNT} concurrent terminal agents",
        MAX_WORST_DRIFT.as_millis()
    );

    // SC-001 hard ceiling: never stalled past 1 second.
    assert!(
        Duration::from_millis(worst) < STALL_CEILING,
        "heartbeat stalled past the 1s ceiling (worst drift {worst}ms)"
    );
}
