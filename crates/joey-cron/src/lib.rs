//! `joey-cron` — the built-in cron scheduler for joey-agent.
//!
//! Port of `cron/`. Self-contained: no crontab binary and no external cron
//! crate — schedule matching is a croniter-compatible matcher. Jobs are
//! stored hermes-compatibly under `~/.joey/cron/jobs.json` (a
//! `{"jobs": [...], "updated_at": ...}` envelope), scheduled via
//! duration/interval/cron expressions, and run by a 60-second ticker.
//! Delivery of job output back to platforms is handled by the gateway layer.

pub mod croniter;
pub mod jobs;
pub mod scheduler;

pub use croniter::CronExpr;
pub use jobs::{
    compute_next_run, ensure_aware_str, fmt_isoformat, new_job_id, now_isoformat, parse_duration,
    parse_isoformat, parse_schedule, CreateJobOptions, CronStore, IsoStamp, Job, Repeat, Schedule,
    ONESHOT_GRACE_SECONDS, TICKER_INTERVAL_SECONDS,
};
pub use scheduler::{build_cron_prompt, get_running_job_ids, JobRunner, Scheduler, CRON_PROMPT_HINT};
