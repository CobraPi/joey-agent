//! The cron ticker (port of `cron/scheduler.py` + `cron/scheduler_provider.py`
//! core).
//!
//! Tick model (upstream `tick()`):
//! 1. take the non-blocking `.tick.lock` (a concurrent tick is a no-op);
//! 2. `get_due_jobs()` — repairs, claims and fast-forwards under the jobs lock;
//! 3. advance `next_run_at` for all due recurring jobs BEFORE any execution
//!    (at-most-once);
//! 4. dispatch jobs CONCURRENTLY, guarded by an in-flight set so a
//!    still-running job is never re-fired by a later tick;
//! 5. each job's completion does its own load-modify-save (`mark_job_run`)
//!    at event time — never a whole-tick rewrite of stale state.
//!
//! The ticker loop (`run_forever`) starts a tick every 60 seconds regardless
//! of job runtime and records the heartbeat files every iteration.
//!
//! Execution itself is delegated to a caller-supplied [`JobRunner`] (the CLI
//! spawns a headless agent); delivery to chat platforms is not ported.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use anyhow::Result;
use fs2::FileExt;
use once_cell::sync::Lazy;

use crate::jobs::{CronStore, Job, TICKER_INTERVAL_SECONDS};

/// The cron execution hint prepended to every agent-run job prompt
/// (upstream `cron/scheduler.py` `cron_hint`).
pub const CRON_PROMPT_HINT: &str = "[IMPORTANT: You are running as a scheduled cron job. \
DELIVERY: Your final response will be automatically delivered \
to the user — do NOT use send_message or try to deliver \
the output yourself. Just produce your report/output as your \
final response and the system handles the rest. \
SILENT: If there is genuinely nothing new to report, respond \
with exactly \"[SILENT]\" (nothing else) to suppress delivery. \
Never combine [SILENT] with content — either report your \
findings normally, or say [SILENT] and nothing more.]\n\n";

/// Soft-failure message for an empty agent response (upstream #8585).
const EMPTY_RESPONSE_ERROR: &str =
    "Agent completed but produced empty response (model error, timeout, or misconfiguration)";

/// Assemble the prompt a cron job runs with: the cron execution hint
/// prepended to the job's prompt. Runners MUST use this rather than the raw
/// `job.prompt` (the same assembled prompt is embedded in the output doc).
pub fn build_cron_prompt(job: &Job) -> String {
    format!("{}{}", CRON_PROMPT_HINT, job.prompt)
}

/// In-flight job ids for this process (upstream `_running_job_ids`).
static RUNNING_JOB_IDS: Lazy<Mutex<HashSet<String>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

/// Snapshot of cron job ids currently executing in this process.
pub fn get_running_job_ids() -> HashSet<String> {
    RUNNING_JOB_IDS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

pub(crate) fn is_job_running(job_id: &str) -> bool {
    RUNNING_JOB_IDS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(job_id)
}

/// Removes the id from the running set even if the job task panics.
struct RunningGuard(String);

impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING_JOB_IDS
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.0);
    }
}

/// A function that runs one job's (assembled) prompt and returns the agent's
/// final response text.
pub type JobRunner =
    Box<dyn Fn(Job) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> + Send + Sync>;

fn job_display_name(job: &Job) -> String {
    if !job.name.is_empty() {
        job.name.clone()
    } else if !job.prompt.is_empty() {
        job.prompt.clone()
    } else if !job.id.is_empty() {
        job.id.clone()
    } else {
        "cron job".to_string()
    }
}

fn schedule_line(job: &Job) -> String {
    if job.schedule_display.is_empty() {
        "N/A".to_string()
    } else {
        job.schedule_display.clone()
    }
}

/// The per-run output document (upstream `run_job` success shape).
fn success_output_doc(job: &Job, prompt: &str, final_response: &str) -> String {
    let logged_response = if final_response.is_empty() {
        "(No response generated)"
    } else {
        final_response
    };
    format!(
        "# Cron Job: {}\n\n**Job ID:** {}\n**Run Time:** {}\n**Schedule:** {}\n\n## Prompt\n\n{}\n\n## Response\n\n{}\n",
        job_display_name(job),
        job.id,
        crate::jobs::time_now().format("%Y-%m-%d %H:%M:%S"),
        schedule_line(job),
        prompt,
        logged_response
    )
}

/// The per-run output document for a FAILED run.
fn failure_output_doc(job: &Job, prompt: &str, error_msg: &str) -> String {
    format!(
        "# Cron Job: {} (FAILED)\n\n**Job ID:** {}\n**Run Time:** {}\n**Schedule:** {}\n\n## Prompt\n\n{}\n\n## Error\n\n```\n{}\n```\n",
        job_display_name(job),
        job.id,
        crate::jobs::time_now().format("%Y-%m-%d %H:%M:%S"),
        schedule_line(job),
        prompt,
        error_msg
    )
}

/// Run ONE due job end-to-end: claim → execute → save output → mark
/// (upstream `run_one_job`, minus platform delivery). Returns true when the
/// job was processed (even if the job itself failed), false when processing
/// itself broke.
async fn run_one_job(store: CronStore, runner: Arc<JobRunner>, job: Job) -> bool {
    // Pre-run dispatch claim (#38758): commit a finite one-shot's dispatch
    // BEFORE its side effect runs, so a crash mid-execution can't re-fire it.
    match store.claim_dispatch(&job.id) {
        Ok(true) => {}
        Ok(false) => {
            tracing::info!(
                "Job '{}': one-shot dispatch limit reached — skipping",
                job_display_name(&job)
            );
            return true; // not an error — already handled/removed
        }
        Err(e) => {
            tracing::error!("Error processing job {}: {}", job.id, e);
            let _ = store.mark_job_run(&job.id, false, Some(&e.to_string()), None);
            return false;
        }
    }

    let prompt = build_cron_prompt(&job);
    let (mut success, output, final_response, mut error): (bool, String, String, Option<String>) =
        match (runner)(job.clone()).await {
            Ok(text) => {
                tracing::info!("Job '{}' completed successfully", job_display_name(&job));
                (true, success_output_doc(&job, &prompt, &text), text, None)
            }
            Err(e) => {
                let error_msg = format!("{:#}", e);
                tracing::error!("Job '{}' failed: {}", job_display_name(&job), error_msg);
                (
                    false,
                    failure_output_doc(&job, &prompt, &error_msg),
                    String::new(),
                    Some(error_msg),
                )
            }
        };

    // Output is written for EVERY run, including failures.
    match store.save_job_output(&job.id, &output) {
        Ok(path) => tracing::debug!("Output saved to: {}", path.display()),
        Err(e) => {
            tracing::error!("Error processing job {}: {}", job.id, e);
            let _ = store.mark_job_run(&job.id, false, Some(&e.to_string()), None);
            return false;
        }
    }

    // Treat an empty final response as a soft failure so last_status is not
    // "ok" — the agent ran but produced nothing useful (#8585).
    if success && final_response.trim().is_empty() {
        success = false;
        error = Some(EMPTY_RESPONSE_ERROR.to_string());
    }

    if let Err(e) = store.mark_job_run(&job.id, success, error.as_deref(), None) {
        tracing::error!("Error processing job {}: {}", job.id, e);
        return false;
    }
    true
}

/// The cron scheduler: pairs a [`CronStore`] with a [`JobRunner`].
pub struct Scheduler {
    store: CronStore,
    runner: Arc<JobRunner>,
}

impl Scheduler {
    pub fn new(store: CronStore, runner: JobRunner) -> Self {
        Self {
            store,
            runner: Arc::new(runner),
        }
    }

    /// Run a single tick and WAIT for every dispatched job to finish
    /// (upstream `tick(sync=True)` — manual `joey cron tick` / tests).
    /// Returns the number of jobs processed.
    pub async fn tick(&self) -> Result<usize> {
        self.tick_inner(true).await
    }

    /// Run a single tick without waiting for job completion (upstream
    /// gateway-ticker `tick(sync=False)`). Returns the number dispatched.
    pub async fn tick_detached(&self) -> Result<usize> {
        self.tick_inner(false).await
    }

    async fn tick_inner(&self, sync: bool) -> Result<usize> {
        // Whole-tick lock: only one tick runs at a time, across processes.
        std::fs::create_dir_all(self.store.dir())?;
        let lock_file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.store.tick_lock_path())?;
        if lock_file.try_lock_exclusive().is_err() {
            tracing::debug!("Tick skipped — another instance holds the lock");
            return Ok(0);
        }
        // The flock releases when `lock_file` drops at the end of this call.

        let due_jobs = self.store.get_due_jobs()?;
        if due_jobs.is_empty() {
            tracing::debug!(
                "{} - No jobs due",
                crate::jobs::time_now().format("%H:%M:%S")
            );
            return Ok(0);
        }
        tracing::info!(
            "{} - {} job(s) due",
            crate::jobs::time_now().format("%H:%M:%S"),
            due_jobs.len()
        );

        // Advance next_run_at for all recurring due jobs FIRST, before any
        // execution begins — at-most-once semantics. mark_job_run overwrites
        // next_run_at on completion.
        for job in &due_jobs {
            if let Err(e) = self.store.advance_next_run(&job.id) {
                tracing::warn!("advance_next_run failed for job {}: {}", job.id, e);
            }
        }

        let mut handles = Vec::new();
        let mut dispatched = 0usize;
        for job in due_jobs {
            // In-flight guard: a job still running from a previous tick is
            // skipped, not re-fired.
            {
                let mut running = RUNNING_JOB_IDS.lock().unwrap_or_else(|p| p.into_inner());
                if running.contains(&job.id) {
                    tracing::info!(
                        "Job '{}' already running — skipping",
                        job_display_name(&job)
                    );
                    continue;
                }
                running.insert(job.id.clone());
            }
            let guard = RunningGuard(job.id.clone());
            let store = self.store.clone();
            let runner = self.runner.clone();
            handles.push(tokio::spawn(async move {
                let _guard = guard;
                run_one_job(store, runner, job).await
            }));
            dispatched += 1;
        }

        if sync {
            let mut processed = 0usize;
            for handle in handles {
                match handle.await {
                    Ok(true) => processed += 1,
                    Ok(false) => {}
                    Err(e) => tracing::error!("Cron job task failed: {}", e),
                }
            }
            Ok(processed)
        } else {
            Ok(dispatched)
        }
    }

    /// Run the ticker forever: a tick starts every `TICKER_INTERVAL_SECONDS`
    /// regardless of job runtime, with heartbeat files written every loop
    /// (port of `InProcessCronScheduler.start`).
    pub async fn run_forever(&self) -> Result<()> {
        tracing::info!(
            "In-process cron scheduler started (interval={}s)",
            TICKER_INTERVAL_SECONDS
        );
        // Heartbeat once before the first sleep so status sees a live ticker
        // immediately after startup.
        self.store.record_ticker_heartbeat(false);
        loop {
            let ok = match self.tick_detached().await {
                Ok(_) => true,
                Err(e) => {
                    tracing::error!("Cron tick error: {}", e);
                    false
                }
            };
            self.store.record_ticker_heartbeat(ok);
            tokio::time::sleep(StdDuration::from_secs(TICKER_INTERVAL_SECONDS)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::CreateJobOptions;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn store() -> (tempfile::TempDir, CronStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CronStore::with_dir(dir.path().join("cron"));
        (dir, store)
    }

    fn ok_runner(calls: Arc<AtomicUsize>, response: &'static str) -> JobRunner {
        Box::new(move |_job: Job| {
            let calls = calls.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(response.to_string())
            })
        })
    }

    fn read_only_output(store: &CronStore, job_id: &str) -> String {
        let dir = store.job_output_dir(job_id).unwrap();
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(entries.len(), 1, "expected exactly one output file");
        std::fs::read_to_string(entries.pop().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn tick_runs_due_recurring_job_and_writes_output_doc() {
        let (_tmp, store) = store();
        let job = store
            .create_job(Some("say hi"), "every 30m", CreateJobOptions::default())
            .unwrap();
        store.trigger_job(&job.id).unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let sched = Scheduler::new(store.clone(), ok_runner(calls.clone(), "hello there"));
        assert_eq!(sched.tick().await.unwrap(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Output document (success shape) embeds the ASSEMBLED prompt.
        let doc = read_only_output(&store, &job.id);
        assert!(doc.starts_with(&format!("# Cron Job: {}\n\n**Job ID:** {}\n", job.name, job.id)));
        assert!(doc.contains("**Schedule:** every 30m\n"));
        assert!(doc.contains("## Prompt\n\n[IMPORTANT: You are running as a scheduled cron job."));
        assert!(doc.contains("say hi\n\n## Response\n\nhello there\n"));

        // Store state advanced by mark_job_run (load-modify-save at event time).
        let stored = store.get_job(&job.id).unwrap().unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("ok"));
        assert!(stored.last_error.is_none());
        assert!(stored.last_run_at.is_some());
        let next = crate::jobs::ensure_aware_str(stored.next_run_at.as_deref().unwrap()).unwrap();
        assert!(next > crate::jobs::time_now());
        // No longer due.
        assert_eq!(sched.tick().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn tick_failure_writes_failed_doc_and_marks_error() {
        let (_tmp, store) = store();
        let job = store
            .create_job(Some("boom"), "every 30m", CreateJobOptions::default())
            .unwrap();
        store.trigger_job(&job.id).unwrap();

        let runner: JobRunner =
            Box::new(|_job| Box::pin(async { Err(anyhow::anyhow!("kaput")) }));
        let sched = Scheduler::new(store.clone(), runner);
        // Failed jobs still count as processed and still write output.
        assert_eq!(sched.tick().await.unwrap(), 1);
        let doc = read_only_output(&store, &job.id);
        assert!(doc.contains("(FAILED)\n"), "{doc}");
        assert!(doc.contains("## Error\n\n```\nkaput\n```\n"), "{doc}");
        let stored = store.get_job(&job.id).unwrap().unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("error"));
        assert_eq!(stored.last_error.as_deref(), Some("kaput"));
        assert!(stored.enabled, "a failing recurring job stays enabled");
    }

    #[tokio::test]
    async fn empty_response_is_soft_failure() {
        let (_tmp, store) = store();
        let job = store
            .create_job(Some("quiet"), "every 30m", CreateJobOptions::default())
            .unwrap();
        store.trigger_job(&job.id).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let sched = Scheduler::new(store.clone(), ok_runner(calls, "   \n"));
        assert_eq!(sched.tick().await.unwrap(), 1);
        let stored = store.get_job(&job.id).unwrap().unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("error"));
        assert_eq!(
            stored.last_error.as_deref(),
            Some("Agent completed but produced empty response (model error, timeout, or misconfiguration)")
        );
        let doc = read_only_output(&store, &job.id);
        assert!(doc.contains("## Response\n\n   \n"), "raw response is logged: {doc:?}");
    }

    #[tokio::test]
    async fn oneshot_tick_removes_job_after_run() {
        let (_tmp, store) = store();
        let job = store
            .create_job(Some("one time"), "30m", CreateJobOptions::default())
            .unwrap();
        store.trigger_job(&job.id).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let sched = Scheduler::new(store.clone(), ok_runner(calls.clone(), "done"));
        assert_eq!(sched.tick().await.unwrap(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Repeat limit reached → the job was DELETED from the store.
        assert!(store.load().unwrap().is_empty());
        // Its output document remains.
        assert!(read_only_output(&store, &job.id).contains("## Response\n\ndone\n"));
        assert_eq!(sched.tick().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn recurring_next_run_advances_before_execution() {
        // At-most-once: by the time the runner executes, next_run_at is
        // already in the future (advance_next_run ran before dispatch).
        let (_tmp, store) = store();
        let job = store
            .create_job(Some("check"), "every 30m", CreateJobOptions::default())
            .unwrap();
        store.trigger_job(&job.id).unwrap();
        let observed = Arc::new(Mutex::new(None::<String>));
        let observed2 = observed.clone();
        let store2 = store.clone();
        let runner: JobRunner = Box::new(move |job: Job| {
            let observed = observed2.clone();
            let store = store2.clone();
            Box::pin(async move {
                let stored = store.get_job(&job.id).unwrap().unwrap();
                *observed.lock().unwrap() = stored.next_run_at.clone();
                Ok("ok".into())
            })
        });
        let sched = Scheduler::new(store.clone(), runner);
        assert_eq!(sched.tick().await.unwrap(), 1);
        let seen = observed.lock().unwrap().clone().expect("next_run_at observed");
        let seen_dt = crate::jobs::ensure_aware_str(&seen).unwrap();
        assert!(seen_dt > crate::jobs::time_now() - chrono::Duration::seconds(5));
        assert!(
            seen_dt > crate::jobs::time_now() + chrono::Duration::minutes(25),
            "advanced a full interval before execution: {seen}"
        );
    }

    #[tokio::test]
    async fn in_flight_guard_skips_still_running_job() {
        let (_tmp, store) = store();
        let job = store
            .create_job(Some("slow"), "every 30m", CreateJobOptions::default())
            .unwrap();
        store.trigger_job(&job.id).unwrap();

        let gate = Arc::new(tokio::sync::Notify::new());
        let gate2 = gate.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let runner: JobRunner = Box::new(move |_job| {
            let gate = gate2.clone();
            let calls = calls2.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                gate.notified().await;
                Ok("finally".into())
            })
        });
        let sched = Arc::new(Scheduler::new(store.clone(), runner));

        // Detached tick dispatches the job and returns while it runs.
        assert_eq!(sched.tick_detached().await.unwrap(), 1);
        // Wait until the job is actually executing.
        for _ in 0..100 {
            if calls.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(get_running_job_ids().contains(&job.id));

        // Make it due again; the in-flight guard must skip it.
        store.trigger_job(&job.id).unwrap();
        assert_eq!(sched.tick().await.unwrap(), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no re-fire while running");

        // Release and wait for completion (mark_job_run lands).
        gate.notify_waiters();
        for _ in 0..200 {
            if store
                .get_job(&job.id)
                .unwrap()
                .and_then(|j| j.last_run_at)
                .is_some()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let stored = store.get_job(&job.id).unwrap().unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("ok"));
        assert!(!get_running_job_ids().contains(&job.id));
    }

    #[tokio::test]
    async fn concurrent_tick_is_noop_under_tick_lock() {
        let (_tmp, store) = store();
        let job = store
            .create_job(Some("p"), "every 30m", CreateJobOptions::default())
            .unwrap();
        store.trigger_job(&job.id).unwrap();
        // Hold the tick lock as "another process" would.
        let lock_file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(store.tick_lock_path())
            .unwrap();
        FileExt::try_lock_exclusive(&lock_file).unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let sched = Scheduler::new(store.clone(), ok_runner(calls.clone(), "x"));
        assert_eq!(sched.tick().await.unwrap(), 0, "tick skipped under lock");
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        FileExt::unlock(&lock_file).unwrap();
        assert_eq!(sched.tick().await.unwrap(), 1);
    }

    #[test]
    fn cron_prompt_hint_contract() {
        let (_tmp, store) = store();
        let job = store
            .create_job(Some("my prompt"), "every 5m", CreateJobOptions::default())
            .unwrap();
        let assembled = build_cron_prompt(&job);
        assert!(assembled.starts_with("[IMPORTANT: You are running as a scheduled cron job. DELIVERY:"));
        assert!(assembled.ends_with("]\n\nmy prompt"));
        assert!(CRON_PROMPT_HINT.contains(
            "respond with exactly \"[SILENT]\" (nothing else) to suppress delivery."
        ));
        assert!(CRON_PROMPT_HINT.contains("do NOT use send_message"));
    }
}
