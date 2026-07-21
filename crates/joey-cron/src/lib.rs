//! `joey-cron` — the built-in cron scheduler for joey-agent.
//!
//! Port of `cron/`. Self-contained: no crontab binary. Jobs are stored as JSON
//! under `~/.joey/cron/jobs.json`, scheduled via duration/interval/cron
//! expressions, and run by a 60-second ticker. Delivery of job output back to
//! platforms is handled by the gateway layer.

pub mod jobs;
pub mod scheduler;

pub use jobs::{
    compute_next_run, new_job_id, parse_schedule, CronStore, Job, Schedule, TICKER_INTERVAL_SECONDS,
};
pub use scheduler::{due_jobs, Scheduler};
