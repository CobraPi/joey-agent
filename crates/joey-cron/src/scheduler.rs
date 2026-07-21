//! The cron ticker loop (port of `cron/scheduler.py` core).
//!
//! The scheduler is decoupled from *how* a job runs: the caller supplies a
//! `JobRunner` closure that executes a due job's prompt (typically by spawning
//! an agent) and returns its output text. The scheduler advances `next_run_at`,
//! records status, and persists.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration as StdDuration;

use anyhow::Result;
use chrono::Utc;

use crate::jobs::{compute_next_run, CronStore, Job, TICKER_INTERVAL_SECONDS};

/// Return the jobs due to run at `now` from a job list.
pub fn due_jobs(jobs: &[Job], now: chrono::DateTime<Utc>) -> Vec<Job> {
    jobs.iter().filter(|j| j.is_due(now)).cloned().collect()
}

/// A function that runs one job's prompt and returns its output text.
pub type JobRunner =
    Box<dyn Fn(Job) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> + Send + Sync>;

/// The cron scheduler.
pub struct Scheduler {
    store: CronStore,
    runner: JobRunner,
}

impl Scheduler {
    pub fn new(store: CronStore, runner: JobRunner) -> Self {
        Self { store, runner }
    }

    /// Run a single tick: execute all due jobs, advance schedules, persist.
    /// Returns the number of jobs run this tick.
    pub async fn tick(&self) -> Result<usize> {
        let now = Utc::now();
        let mut jobs = self.store.load()?;
        let due: Vec<usize> = jobs
            .iter()
            .enumerate()
            .filter(|(_, j)| j.is_due(now))
            .map(|(i, _)| i)
            .collect();

        let mut ran = 0;
        for idx in due {
            let job = jobs[idx].clone();
            let result = (self.runner)(job.clone()).await;
            let ts = now.format("%Y%m%dT%H%M%SZ").to_string();

            match result {
                Ok(output) => {
                    let _ = self.store.write_output(&job.id, &ts, &output);
                    jobs[idx].last_status = Some("ok".to_string());
                    jobs[idx].last_error = None;
                }
                Err(e) => {
                    jobs[idx].last_status = Some("error".to_string());
                    jobs[idx].last_error = Some(e.to_string());
                }
            }
            jobs[idx].last_run_at = Some(now);
            jobs[idx].repeat_completed += 1;

            // Advance the schedule (or disable a completed once/bounded job).
            let next = compute_next_run(&jobs[idx].schedule, Some(now), now);
            let exhausted = jobs[idx]
                .repeat_times
                .map(|t| jobs[idx].repeat_completed >= t)
                .unwrap_or(false);
            if next.is_none() || exhausted {
                jobs[idx].enabled = false;
                jobs[idx].state = "completed".to_string();
                jobs[idx].next_run_at = None;
            } else {
                jobs[idx].next_run_at = next;
            }
            ran += 1;
        }

        if ran > 0 {
            self.store.save(&jobs)?;
        }
        Ok(ran)
    }

    /// Run the ticker forever, sleeping `TICKER_INTERVAL_SECONDS` between ticks.
    pub async fn run_forever(&self) -> Result<()> {
        loop {
            if let Err(e) = self.tick().await {
                tracing::warn!("cron tick failed: {}", e);
            }
            tokio::time::sleep(StdDuration::from_secs(TICKER_INTERVAL_SECONDS)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{new_job_id, Schedule};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn job_due_now() -> Job {
        Job {
            id: new_job_id(),
            name: "t".into(),
            prompt: "hi".into(),
            schedule: Schedule::Interval { minutes: 30 },
            schedule_display: "every 30m".into(),
            enabled: true,
            state: "scheduled".into(),
            deliver: "local".into(),
            created_at: String::new(),
            next_run_at: None, // due immediately
            last_run_at: None,
            last_status: None,
            last_error: None,
            repeat_times: None,
            repeat_completed: 0,
        }
    }

    #[tokio::test]
    async fn tick_runs_due_job_and_advances() {
        let dir = tempfile::tempdir().unwrap();
        let store = CronStore::with_dir(dir.path().to_path_buf());
        store.add(job_due_now()).unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        let runner: JobRunner = Box::new(move |job: Job| {
            let calls = calls2.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(format!("ran {}", job.name))
            })
        });

        let sched = Scheduler::new(store, runner);
        let ran = sched.tick().await.unwrap();
        assert_eq!(ran, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // After running, the interval job should have a future next_run_at.
        let store2 = CronStore::with_dir(dir.path().to_path_buf());
        let jobs = store2.load().unwrap();
        assert!(jobs[0].next_run_at.is_some());
        assert_eq!(jobs[0].last_status.as_deref(), Some("ok"));
        // And it should no longer be due right now.
        assert!(!jobs[0].is_due(Utc::now()));
    }
}
