//! Feature 018 — terminal governor admission wiring CONTRACT tests (T011).
//!
//! Contract cases (specs/018-please-fully-implement/tasks.md T011):
//! - (a) cap enforcement        → `cap_enforcement_burst_of_50_*`
//! - (b) round-robin fairness   → `round_robin_two_agents_progress_within_one_cycle`
//! - (c) timeout-from-admission → `timeout_runs_from_admission_not_from_queue_wait`
//! - (d) fault isolation        → `fault_isolation_failing_and_timing_out_*`
//! - (e) cancellation           → `cancellation_interrupted_queued_waiter_*`,
//!                                `cancellation_interrupt_kills_running_children_*`
//! - (f) back-compat            → `backcompat_*`
//!
//! Observation mechanism (design decision, wave-8 revision): every call whose
//! execution window must be observed writes NANOSECOND TIMESTAMP FILES as a
//! side effect of the child process itself — `date +%s%N > start_<id>` as the
//! command's first action and `date +%s%N > end_<id>` as its last. The pair
//! brackets that call's post-admission, post-spawn execution window with no
//! dependence on the tool's output-streaming behavior (on macOS the
//! foreground execute() path delivers output_sender chunks only at command
//! COMPLETION, so mid-run output markers are unobservable; the streaming
//! code is upstream-parity-pinned and must not be changed). The probe polls
//! the filesystem for `start_<id>` existence (admission happened) and, after
//! the calls resolve, computes observed peak overlap / admission order from
//! the recorded timestamps. `date +%s%N` works on macOS and Linux. The
//! governor's own introspection (`queued_for`, `active`, `limit`) is used as
//! a secondary signal for queue-parking and slot-lifecycle checks, and
//! process-list inspection is used ONLY for the orphan check in case (e),
//! where it is the only option.
//!
//! Limit expression (design decision): `TERMINAL_MAX_CONCURRENT=2`
//! (contracts/config.md env override) is set once before the process's first
//! terminal admission; T014 resolves it lazily into the process-global
//! singleton. All tests serialize on a static mutex (each integration test
//! file is its own process, but cargo runs tests within it in parallel
//! threads), so env mutation and singleton resolution never race.
//!
//! Known limitation worked around: the singleton has no public reset, so a
//! per-test limit is impossible — every test shares the limit-2 singleton,
//! which is exactly what these cases specify anyway.

use joey_tools::tools::terminal_governor::terminal_governor;
use joey_tools::{Tool, ToolContext, ToolRegistry};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The concurrency cap every test in this suite runs under.
const GOV_LIMIT: usize = 2;

/// Set TERMINAL_MAX_CONCURRENT before the process's first terminal
/// admission (T014 resolves the env once, lazily).
static INIT: OnceLock<()> = OnceLock::new();

/// Serializes all tests in this binary: the governor is a process-global
/// singleton with no public reset, so parallel tests would observe each
/// other's slots/queues. One cargo test binary == one process == one
/// governor at limit 2.
static SERIAL: Mutex<()> = Mutex::new(());

fn init_env() {
    INIT.get_or_init(|| {
        std::env::set_var("TERMINAL_MAX_CONCURRENT", GOV_LIMIT.to_string());
    });
}

fn serialize() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|p| p.into_inner())
}

fn terminal() -> Arc<dyn Tool> {
    ToolRegistry::with_builtins()
        .get("terminal")
        .expect("terminal tool registered")
}

/// Parse the terminal tool's JSON result string.
fn parse_result(content: &str) -> Value {
    serde_json::from_str::<Value>(content).unwrap_or_else(|e| {
        panic!("terminal result is not valid JSON: {e}\n---\n{content}\n---")
    })
}

/// Poll `cond` every 25ms until true; panic with a T012-oriented message
/// after `timeout` so missing admission wiring fails loudly instead of
/// deadlocking.
async fn eventually(desc: &str, timeout: Duration, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "condition not met within {timeout:?}: {desc} \
         (is the governor admission wiring from T012/T014 in place?)"
    );
}

/// Build a ToolContext with optional observability/admission plumbing.
fn ctx_with(
    sender: Option<&tokio::sync::mpsc::UnboundedSender<String>>,
    queue_key: Option<Arc<str>>,
    interrupt: Option<Arc<AtomicBool>>,
) -> ToolContext {
    let mut ctx = ToolContext::new(
        std::env::temp_dir(),
        joey_core::Config::defaults(),
        "governor-test",
    );
    if let Some(tx) = sender {
        ctx = ctx.with_output_sender(Some(tx.clone()));
    }
    if let Some(key) = queue_key {
        ctx = ctx.with_queue_key(Some(key));
    }
    if let Some(flag) = interrupt {
        ctx = ctx.with_interrupt_flag(Some(flag));
    }
    ctx
}

/// Spawn one terminal `execute()` call on the current runtime; resolves to
/// the parsed JSON result.
#[allow(clippy::too_many_arguments)]
fn spawn_call(
    tool: Arc<dyn Tool>,
    command: String,
    timeout_secs: Option<u64>,
    sender: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    queue_key: Option<Arc<str>>,
    interrupt: Option<Arc<AtomicBool>>,
) -> tokio::task::JoinHandle<Value> {
    tokio::spawn(async move {
        let mut args = json!({ "command": command });
        if let Some(t) = timeout_secs {
            args["timeout"] = json!(t);
        }
        let ctx = ctx_with(sender.as_ref(), queue_key, interrupt);
        let result = tool.execute(args, &ctx).await;
        parse_result(&result.to_content_string())
    })
}

// ── Concurrency probe: side-effect timestamp files ────────────────────────

/// Per-test directory of `start_<id>` / `end_<id>` nanosecond-timestamp
/// files. Observed commands write them from INSIDE the spawned child, so
/// observation is independent of the tool's output-streaming timing (which
/// on macOS delivers chunks only at completion).
struct Probe {
    dir: PathBuf,
}

impl Probe {
    /// Fresh directory per test (tag unique within this process; stale dirs
    /// from previous runs are removed opportunistically).
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "joey-gov-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create side-effect probe dir");
        Self { dir }
    }

    /// A command snippet that timestamps its own start, runs `body` (which
    /// must exit 0), then timestamps its own end. The window
    /// (start_<id>, end_<id>) brackets the child's actual execution.
    fn ok_cmd(&self, id: usize, body: &str) -> String {
        let d = self.dir.display();
        format!("date +%s%N > '{d}/start_{id}'; {{ {body}; }}; date +%s%N > '{d}/end_{id}'")
    }

    /// A command snippet that only timestamps its own start (used for calls
    /// expected to be killed / fail before completing cleanly).
    fn start_only_cmd(&self, id: usize, body: &str) -> String {
        let d = self.dir.display();
        format!("date +%s%N > '{d}/start_{id}'; {body}")
    }

    fn ts_path(&self, kind: &str, id: usize) -> PathBuf {
        self.dir.join(format!("{kind}_{id}"))
    }

    /// Read one timestamp (ns since epoch) if the file exists and parses.
    fn ts(&self, kind: &str, id: usize) -> Option<u128> {
        std::fs::read_to_string(self.ts_path(kind, id))
            .ok()
            .and_then(|s| s.trim().parse::<u128>().ok())
    }

    /// Await until `start_<id>` exists (the child actually began running —
    /// i.e. the call was admitted and spawned). Async fs polling; never
    /// blocks the runtime thread.
    async fn wait_started(&self, id: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.ts("start", id).is_some() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        false
    }

    /// Peak number of simultaneously-open (start_<id>, end_<id>) windows
    /// among `ids`. An id whose end file is missing (call failed / timed
    /// out / was killed before finishing) stays open through the end of the
    /// sweep, so it keeps counting as running — only pass ids whose calls
    /// are expected to complete, or whose in-flight occupancy must not be
    /// lost, per the per-test contract.
    fn peak_overlap(&self, ids: &[usize]) -> usize {
        let mut deltas: Vec<(u128, i32)> = Vec::new();
        for id in ids {
            let start = self.ts("start", *id).unwrap_or_else(|| {
                panic!("probe: start_{id} missing — call {id} never began executing")
            });
            deltas.push((start, 1));
            if let Some(end) = self.ts("end", *id) {
                deltas.push((end, -1));
            }
        }
        // Sort ends (-1) before starts (+1) at identical timestamps so
        // back-to-back windows on one slot never count as overlapping.
        deltas.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        let (mut cur, mut peak) = (0i32, 0usize);
        for (_, d) in deltas {
            cur += d;
            peak = peak.max(cur.max(0) as usize);
        }
        peak
    }

    /// The tag sequence ordered by each id's `start_<id>` timestamp — i.e.
    /// the admission/execution-start order of the tagged calls.
    fn start_order(&self, tagged: &[(usize, char)]) -> Vec<char> {
        let mut starts: Vec<(u128, char)> = Vec::new();
        for (id, tag) in tagged {
            let ts = self.ts("start", *id).unwrap_or_else(|| {
                panic!("probe: start_{id} missing — call {id} never began executing")
            });
            starts.push((ts, *tag));
        }
        starts.sort_by(|a, b| a.0.cmp(&b.0));
        starts.into_iter().map(|(_, t)| t).collect()
    }
}

// ── (a) Cap enforcement ───────────────────────────────────────────────────

/// SC-002: with TERMINAL_MAX_CONCURRENT=2, a burst of >= 50 cheap sleeper
/// calls never exceeds 2 concurrently-executing commands, every call still
/// succeeds, and the singleton's limit reflects the env override (T014).
#[tokio::test]
async fn cap_enforcement_burst_of_50_observed_peak_concurrency_is_2() {
    init_env();
    let _serialized = serialize();
    let tool = terminal();
    let probe = Probe::new("burst");
    let n = 50usize;

    let mut handles = Vec::new();
    for i in 0..n {
        handles.push(spawn_call(
            tool.clone(),
            probe.ok_cmd(i, "sleep 0.25"),
            None,
            None,
            None,
            None,
        ));
    }

    for (i, h) in handles.into_iter().enumerate() {
        let v = tokio::time::timeout(Duration::from_secs(90), h)
            .await
            .expect("burst call did not finish within 90s (governor wedged?)")
            .unwrap_or_else(|e| panic!("burst call {i} panicked: {e}"));
        assert_eq!(v["exit_code"], json!(0), "burst call {i} must succeed: {v}");
    }

    let peak = probe.peak_overlap(&(0..n).collect::<Vec<_>>());
    assert!(
        peak <= GOV_LIMIT,
        "observed peak concurrency {peak} exceeded the cap of {GOV_LIMIT} (SC-002)"
    );
    assert!(
        peak >= 2,
        "probe sanity: expected to observe both slots busy at once, got peak {peak}"
    );

    // T014 contract: the env override must be resolved into the singleton.
    assert_eq!(
        terminal_governor().limit(),
        GOV_LIMIT,
        "TERMINAL_MAX_CONCURRENT=2 must set the process-global governor limit"
    );
}

// ── (b) Round-robin fairness across agents ────────────────────────────────

/// FR-004 / spec Q4: agent A's queued burst must not starve agent B's. Both
/// slots are held under key A; then A queues 4 and B queues 4 (B queued
/// LAST — a pure global FIFO would admit all four A calls first).
/// Round-robin must admit B within one cycle: B's first execution starts no
/// later than the 3rd queued start. All 8 queued calls must complete.
#[tokio::test]
async fn round_robin_two_agents_progress_within_one_cycle() {
    init_env();
    let _serialized = serialize();
    let tool = terminal();
    let probe = Probe::new("rr");
    let key_a: Arc<str> = Arc::from("gov-agent-a");
    let key_b: Arc<str> = Arc::from("gov-agent-b");

    // Holders (key A) occupy both slots for 3s.
    let mut holders = Vec::new();
    for id in [100usize, 101] {
        holders.push(spawn_call(
            tool.clone(),
            probe.ok_cmd(id, "sleep 3"),
            None,
            None,
            Some(key_a.clone()),
            None,
        ));
    }
    assert!(
        probe.wait_started(100, Duration::from_secs(5)).await
            && probe.wait_started(101, Duration::from_secs(5)).await,
        "both slot holders must start before the burst is queued"
    );

    // A queues 4, then B queues 4 — B is strictly last.
    let mut queued = Vec::new();
    for k in 0..4usize {
        let id = 200 + k;
        queued.push(spawn_call(
            tool.clone(),
            probe.ok_cmd(id, "sleep 0.2"),
            None,
            None,
            Some(key_a.clone()),
            None,
        ));
    }
    for k in 0..4usize {
        let id = 300 + k;
        queued.push(spawn_call(
            tool.clone(),
            probe.ok_cmd(id, "sleep 0.2"),
            None,
            None,
            Some(key_b.clone()),
            None,
        ));
    }

    // Both bursts must be parked in their per-key queues while holders run.
    eventually(
        "agent A burst queued (4 waiters)",
        Duration::from_secs(5),
        || terminal_governor().queued_for("gov-agent-a") == 4,
    )
    .await;
    eventually(
        "agent B burst queued (4 waiters)",
        Duration::from_secs(5),
        || terminal_governor().queued_for("gov-agent-b") == 4,
    )
    .await;

    // Everything must complete.
    for h in holders.into_iter().chain(queued.into_iter()) {
        let v = tokio::time::timeout(Duration::from_secs(30), h)
            .await
            .expect("round-robin test call did not finish within 30s")
            .expect("round-robin test call panicked");
        assert_eq!(v["exit_code"], json!(0), "queued call must succeed: {v}");
    }

    let order = probe.start_order(&[
        (200, 'a'),
        (201, 'a'),
        (202, 'a'),
        (203, 'a'),
        (300, 'b'),
        (301, 'b'),
        (302, 'b'),
        (303, 'b'),
    ]);
    assert_eq!(order.len(), 8, "all 8 queued calls must have started");
    let first_b = order
        .iter()
        .position(|t| *t == 'b')
        .expect("agent B must start at least once");
    assert!(
        first_b <= 2,
        "agent B (queued last) must make progress within one round-robin \
         cycle; execution-start order was {order:?}"
    );
}

// ── (c) Timeout measured from admission, not queue wait ───────────────────

/// FR-005: the per-call deadline is computed AFTER admission succeeds.
///
/// Discriminator design: holders hold the cap for 6s; the victim (key
/// gov-tfa) is `sleep 3; echo LATE` with timeout 8s, queued behind them.
/// - CORRECT wiring: admitted at ~6s, deadline ~14s, LATE printed at ~9s →
///   exit 0 with LATE present, resolving no earlier than ~9s, and the
///   victim's own start timestamp (written by the child itself) is >= the
///   holders' start + 5s, proving it executed after admission.
/// - WRONG wiring (deadline stamped at queue entry, ~0s): deadline fires at
///   8s, before LATE (9s) → exit 124, no LATE.
/// - NO wiring at all: victim runs immediately, LATE at ~3s → elapsed floor
///   fails (and the queued-wait check before it already failed).
#[tokio::test]
async fn timeout_runs_from_admission_not_from_queue_wait() {
    init_env();
    let _serialized = serialize();
    let tool = terminal();
    let probe = Probe::new("tfa");
    let key: Arc<str> = Arc::from("gov-tfa");

    let mut holders = Vec::new();
    for id in [400usize, 401] {
        holders.push(spawn_call(
            tool.clone(),
            probe.ok_cmd(id, "sleep 6"),
            None,
            None,
            Some(key.clone()),
            None,
        ));
    }
    assert!(
        probe.wait_started(400, Duration::from_secs(5)).await
            && probe.wait_started(401, Duration::from_secs(5)).await,
        "both cap holders must start before the victim is queued"
    );

    let started = Instant::now();
    let victim = spawn_call(
        tool.clone(),
        probe.ok_cmd(450, "sleep 3; echo LATE"),
        Some(8),
        None,
        Some(key.clone()),
        None,
    );

    eventually(
        "victim parked in the gov-tfa queue",
        Duration::from_secs(5),
        || terminal_governor().queued_for("gov-tfa") == 1,
    )
    .await;

    let v = tokio::time::timeout(Duration::from_secs(25), victim)
        .await
        .expect("victim call did not resolve within 25s")
        .expect("victim task panicked");
    let elapsed = started.elapsed();

    assert!(
        elapsed >= Duration::from_secs(7),
        "victim resolved after only {elapsed:?} — it appears to have run without \
         queueing behind the 6s cap holders (min expected ≈ 9s): {v}"
    );
    // The child's own start timestamp proves post-admission execution: the
    // victim cannot begin before a slot frees (~6s after the holders did).
    let victim_start = probe.ts("start", 450).expect("victim start file");
    let holder_start = probe.ts("start", 400).expect("holder start file");
    let queue_delay_ns = victim_start.saturating_sub(holder_start);
    assert!(
        queue_delay_ns >= 5_000_000_000,
        "victim began execution only {queue_delay_ns}ns after the holders did — \
         it must queue behind their 6s occupancy (expected ≈ 6s)"
    );
    assert!(
        v.get("output")
            .and_then(|o| o.as_str())
            .unwrap_or_default()
            .contains("LATE"),
        "victim must COMPLETE (deadline must start at its admission ~6s, expire \
         ~14s > LATE at ~9s); a queue-wait-based deadline would have fired at 8s \
         first — result: {v}"
    );
    assert_eq!(v["exit_code"], json!(0), "victim must succeed: {v}");

    // Holders must finish cleanly.
    for h in holders {
        let hv = tokio::time::timeout(Duration::from_secs(15), h)
            .await
            .expect("holder did not finish")
            .expect("holder task panicked");
        assert_eq!(hv["exit_code"], json!(0));
    }
}

// ── (d) Fault isolation ───────────────────────────────────────────────────

/// FR-006: a failing call (`exit 7`) and a timing-out call sharing the
/// governor with healthy calls must not disturb the healthy calls' results,
/// and the cap still holds while the healthy calls execute.
#[tokio::test]
async fn fault_isolation_failing_and_timing_out_calls_leave_others_intact() {
    init_env();
    let _serialized = serialize();
    let tool = terminal();
    let probe = Probe::new("fault");

    // The failing call timestamps its start then exits 7 (no end file —
    // same shape as the original marker encoding).
    let bad = spawn_call(
        tool.clone(),
        probe.start_only_cmd(500, "exit 7"),
        None,
        None,
        None,
        None,
    );
    // `start; sleep 20` with timeout 1 never writes an end file — it is
    // excluded from the peak sweep (see peak_overlap doc) but still
    // occupies a governor slot for its whole (timed-out) life.
    let slow = spawn_call(
        tool.clone(),
        probe.start_only_cmd(501, "sleep 20"),
        Some(1),
        None,
        None,
        None,
    );
    let mut healthy = Vec::new();
    for k in 0..4usize {
        let id = 502 + k;
        healthy.push(spawn_call(
            tool.clone(),
            probe.ok_cmd(id, &format!("echo ok-{k}; sleep 0.15")),
            None,
            None,
            None,
            None,
        ));
    }

    let bad_v = tokio::time::timeout(Duration::from_secs(30), bad)
        .await
        .expect("failing call must resolve")
        .expect("failing call panicked");
    assert_eq!(bad_v["exit_code"], json!(7), "failing call: {bad_v}");

    let slow_v = tokio::time::timeout(Duration::from_secs(30), slow)
        .await
        .expect("timing-out call must resolve")
        .expect("timing-out call panicked");
    assert_eq!(slow_v["exit_code"], json!(124), "timing-out call: {slow_v}");
    assert!(
        slow_v
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or_default()
            .contains("timed out"),
        "timing-out call error must name the timeout: {slow_v}"
    );

    for (k, h) in healthy.into_iter().enumerate() {
        let v = tokio::time::timeout(Duration::from_secs(30), h)
            .await
            .expect("healthy call must resolve")
            .expect("healthy call panicked");
        assert_eq!(v["exit_code"], json!(0), "healthy call {k} disturbed: {v}");
        assert!(
            v.get("output")
                .and_then(|o| o.as_str())
                .unwrap_or_default()
                .contains(&format!("ok-{k}")),
            "healthy call {k} output corrupted: {v}"
        );
    }

    // Cap holds while the healthy calls execute (bad/slow never write end
    // files so they are excluded from the sweep by design).
    let peak = probe.peak_overlap(&[502, 503, 504, 505]);
    assert!(
        peak <= GOV_LIMIT,
        "cap must hold across failing/timing-out calls too; peak was {peak}"
    );
}

// ── (e) Cancellation ──────────────────────────────────────────────────────

/// SC-003 (queued side): an interrupt fired while a call is PARKED IN THE
/// QUEUE must deregister the waiter (no slot leak, queue drains), return
/// within ~2s, and never report clean success — while the running holders
/// keep their slots and finish normally.
#[tokio::test]
async fn cancellation_interrupted_queued_waiter_is_removed_and_slot_not_leaked() {
    init_env();
    let _serialized = serialize();
    let tool = terminal();
    let probe = Probe::new("cxl");
    let key: Arc<str> = Arc::from("gov-cxl");

    // Holders occupy both slots for 6s and complete normally (no kill here).
    let mut holders = Vec::new();
    for id in [600usize, 601] {
        holders.push(spawn_call(
            tool.clone(),
            probe.ok_cmd(id, "sleep 6"),
            None,
            None,
            Some(key.clone()),
            None,
        ));
    }
    assert!(
        probe.wait_started(600, Duration::from_secs(5)).await
            && probe.wait_started(601, Duration::from_secs(5)).await,
        "both holders must start before the waiter is queued"
    );

    let flag = Arc::new(AtomicBool::new(false));
    let waiter = spawn_call(
        tool.clone(),
        "sleep 30".to_string(),
        None,
        None,
        Some(key.clone()),
        Some(flag.clone()),
    );

    eventually(
        "waiter parked in gov-cxl queue",
        Duration::from_secs(5),
        || terminal_governor().queued_for("gov-cxl") == 1,
    )
    .await;

    // Interrupt mid-queue; the waiter must return within ~2s (SC-003).
    flag.store(true, Ordering::SeqCst);
    let v = tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .expect("queued waiter must return within 2s of the interrupt")
        .expect("waiter task panicked");

    // Clean success = exit 0 AND no error reported (error field either
    // absent or JSON null — `get` distinguishes nothing here, both mean
    // "no error" for this check).
    let clean_success = v["exit_code"] == json!(0)
        && v.get("error").map(|e| e.is_null()).unwrap_or(true);
    assert!(
        !clean_success,
        "an interrupted queued waiter must not report clean success: {v}"
    );

    eventually(
        "waiter deregistered from the queue",
        Duration::from_secs(2),
        || terminal_governor().queued_for("gov-cxl") == 0,
    )
    .await;
    // Holders are untouched — no slot double-count or theft.
    assert_eq!(
        terminal_governor().active(),
        2,
        "holders must still hold both slots after the waiter's cancellation"
    );

    for h in holders {
        let hv = tokio::time::timeout(Duration::from_secs(15), h)
            .await
            .expect("holder did not finish")
            .expect("holder panicked");
        assert_eq!(hv["exit_code"], json!(0));
    }
    eventually(
        "all slots released",
        Duration::from_secs(2),
        || terminal_governor().active() == 0,
    )
    .await;
}

/// SC-003 (running side): interrupting RUNNING calls must kill the children
/// (start_kill path), free the governor slots within ~2s, leave a slot
/// immediately reusable, and leave ZERO orphan processes.
///
/// Orphan encoding: each runner is a bash loop of `sleep 0.1` steps whose
/// full command line carries a run-unique tag. The wrapper bash child (the
/// process start_kill targets) bears the tag in its argv; killing it leaves
/// at most one transient 0.1s step orphan. A pgrep sweep (with retries for
/// the transient) then asserts no tagged process remains — i.e. the child
/// really died. The bracket trick (`TAG[12]`) keeps the probe's own command
/// line from matching its own pattern. Each runner also timestamps its own
/// start into the probe dir so "both runners are executing" is observed
/// without relying on output streaming.
#[tokio::test]
async fn cancellation_interrupt_kills_running_children_and_leaves_no_orphans() {
    init_env();
    let _serialized = serialize();
    let tool = terminal();
    let probe = Probe::new("kill");
    let key: Arc<str> = Arc::from("gov-kl");

    // Run-unique orphan tag; runner commands embed `<tag>1` / `<tag>2`.
    let tag = format!("GOVORPHAN{}Z", std::process::id());
    let flag = Arc::new(AtomicBool::new(false));

    let mut runners = Vec::new();
    for (id, n) in [(700usize, '1'), (701, '2')] {
        runners.push(spawn_call(
            tool.clone(),
            probe.start_only_cmd(
                id,
                &format!("for ((i=0;i<300;i++)); do sleep 0.1; done # {tag}{n}"),
            ),
            None,
            None,
            Some(key.clone()),
            Some(flag.clone()),
        ));
    }
    assert!(
        probe.wait_started(700, Duration::from_secs(5)).await
            && probe.wait_started(701, Duration::from_secs(5)).await,
        "both runners must start before the interrupt"
    );

    flag.store(true, Ordering::SeqCst);
    for (id, h) in runners.into_iter().enumerate() {
        let v = tokio::time::timeout(Duration::from_secs(3), h)
            .await
            .unwrap_or_else(|_| {
                panic!("running call {id} must be killed within ~2s of the interrupt")
            })
            .expect("runner task panicked");
        assert!(
            v.get("output")
                .and_then(|o| o.as_str())
                .unwrap_or_default()
                .contains("[Command interrupted by user]"),
            "runner {id} must report the interruption: {v}"
        );
    }

    eventually(
        "slots freed after kill",
        Duration::from_secs(2),
        || terminal_governor().active() == 0,
    )
    .await;

    // The freed slot must be immediately reusable.
    let canary = spawn_call(tool.clone(), "echo free".to_string(), None, None, None, None);
    let cv = tokio::time::timeout(Duration::from_secs(5), canary)
        .await
        .expect("canary must run once slots are freed")
        .expect("canary panicked");
    assert_eq!(cv["exit_code"], json!(0), "canary: {cv}");

    // Zero orphans: no tagged process may outlive the cancelled calls.
    // (pgrep is BSD/Linux standard; NOPGREP skips gracefully where absent.)
    let orphan_regex = format!("{tag}[12]");
    let probe_cmd = format!(
        "if command -v pgrep >/dev/null 2>&1; then \
             if pgrep -f '{orphan_regex}' >/dev/null 2>&1; then echo FOUND_ORPHANS; \
             else echo CLEAN; fi; \
         else echo NOPGREP; fi"
    );
    let mut clean = false;
    for _ in 0..4 {
        let h = spawn_call(tool.clone(), probe_cmd.clone(), None, None, None, None);
        let v = tokio::time::timeout(Duration::from_secs(5), h)
            .await
            .expect("orphan probe must resolve")
            .expect("orphan probe panicked");
        let out = v.get("output").and_then(|o| o.as_str()).unwrap_or_default();
        if out.contains("CLEAN") || out.contains("NOPGREP") {
            clean = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(
        clean,
        "orphan processes matching /{orphan_regex}/ outlived the cancelled calls"
    );
}

// ── (f) Backward compatibility ────────────────────────────────────────────

/// Calls with NO queue_key, NO senders, and NO interrupt flag must behave
/// exactly as before the governor existed: same schema, same values
/// (modeled on terminal_streaming.rs T009 cases).
#[tokio::test]
async fn backcompat_plain_context_no_queue_key_no_senders_unchanged() {
    init_env();
    let _serialized = serialize();
    let tool = terminal();

    let v = parse_result(
        &tool
            .execute(json!({ "command": "echo hello" }), &ctx_with(None, None, None))
            .await
            .to_content_string(),
    );
    // Field-PRESENCE asserts must use `get().is_some()` — `v["k"].is_null()`
    // is true for both a JSON null and a MISSING key (serde_json Index
    // returns Null for absent fields), so it cannot prove the key exists.
    assert!(v.get("output").is_some(), "output field present: {v}");
    assert!(v.get("exit_code").is_some(), "exit_code field present: {v}");
    assert!(v.get("error").is_some(), "error field present: {v}");
    assert_eq!(v["exit_code"], json!(0));
    assert!(v.get("output").and_then(|o| o.as_str()).unwrap().contains("hello"));

    let v = parse_result(
        &tool
            .execute(json!({ "command": "exit 3" }), &ctx_with(None, None, None))
            .await
            .to_content_string(),
    );
    assert_eq!(v["exit_code"], json!(3));
}

/// A small concurrent burst of plain calls (default shared key) all succeed
/// — the governor must be invisible to identity-less callers (SC-004: lone
/// or light callers never observe failures).
#[tokio::test]
async fn backcompat_concurrent_plain_calls_all_succeed() {
    init_env();
    let _serialized = serialize();
    let tool = terminal();

    let mut handles = Vec::new();
    for i in 0..5 {
        handles.push(spawn_call(
            tool.clone(),
            format!("echo plain-{i}"),
            None,
            None,
            None,
            None,
        ));
    }
    for (i, h) in handles.into_iter().enumerate() {
        let v = tokio::time::timeout(Duration::from_secs(30), h)
            .await
            .expect("plain call must resolve")
            .expect("plain call panicked");
        assert_eq!(v["exit_code"], json!(0), "plain call {i}: {v}");
        assert!(
            v.get("output")
                .and_then(|o| o.as_str())
                .unwrap_or_default()
                .contains(&format!("plain-{i}")),
            "plain call {i} output corrupted: {v}"
        );
    }
}
