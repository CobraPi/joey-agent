//! `joey cron ...` — manage and run scheduled jobs (port of `hermes cron`).

use anyhow::Result;
use chrono::Utc;
use joey_agent_core::{Agent, AgentConfig};
use joey_core::Config;
use joey_cron::{compute_next_run, new_job_id, parse_schedule, CronStore, Job, Scheduler};
use joey_tools::{ToolContext, ToolRegistry};

use crate::render;

/// `joey cron list`
pub fn list() -> Result<()> {
    let store = CronStore::open_default();
    let jobs = store.load()?;
    if jobs.is_empty() {
        render::info("No cron jobs.");
        return Ok(());
    }
    for j in jobs {
        let status = if !j.enabled {
            "paused"
        } else {
            j.state.as_str()
        };
        println!(
            "  {}  {:<20} [{}]  {}  next: {}",
            &j.id,
            j.name,
            status,
            j.schedule_display,
            j.next_run_at
                .map(|d| d.to_rfc3339())
                .unwrap_or_else(|| "-".to_string())
        );
        if let Some(err) = &j.last_error {
            render::error(&format!("    last error: {}", err));
        }
    }
    Ok(())
}

/// `joey cron create <schedule> <prompt> [--name ..] [--deliver ..]`
pub fn create(schedule: &str, prompt: &str, name: Option<&str>, deliver: Option<&str>) -> Result<()> {
    let now = Utc::now();
    let sched = parse_schedule(schedule, now)?;
    let next = compute_next_run(&sched, None, now);
    let job = Job {
        id: new_job_id(),
        name: name.unwrap_or("job").to_string(),
        prompt: prompt.to_string(),
        schedule: sched,
        schedule_display: schedule.to_string(),
        enabled: true,
        state: "scheduled".to_string(),
        deliver: deliver.unwrap_or("local").to_string(),
        created_at: now.to_rfc3339(),
        next_run_at: next,
        last_run_at: None,
        last_status: None,
        last_error: None,
        repeat_times: None,
        repeat_completed: 0,
    };
    let id = job.id.clone();
    CronStore::open_default().add(job)?;
    render::success(&format!("Created cron job {} (next run: {})", id, next.map(|d| d.to_rfc3339()).unwrap_or_default()));
    Ok(())
}

/// `joey cron remove <id>`
pub fn remove(id: &str) -> Result<()> {
    if CronStore::open_default().remove(id)? {
        render::success(&format!("Removed job {}", id));
    } else {
        render::error(&format!("no job with id {}", id));
    }
    Ok(())
}

/// `joey cron pause|resume <id>`
pub fn set_enabled(id: &str, enabled: bool) -> Result<()> {
    if CronStore::open_default().set_enabled(id, enabled)? {
        render::success(&format!("{} job {}", if enabled { "Resumed" } else { "Paused" }, id));
    } else {
        render::error(&format!("no job with id {}", id));
    }
    Ok(())
}

/// `joey cron tick` — run all due jobs once (each job spawns a headless agent).
pub async fn tick() -> Result<()> {
    let store = CronStore::open_default();
    let runner = build_runner();
    let scheduler = Scheduler::new(store, runner);
    let ran = scheduler.tick().await?;
    render::info(&format!("Ran {} due job(s).", ran));
    Ok(())
}

/// `joey cron run` — run the ticker forever (blocking).
pub async fn run_forever() -> Result<()> {
    render::info("Starting cron scheduler (60s ticker). Ctrl-C to stop.");
    let store = CronStore::open_default();
    let scheduler = Scheduler::new(store, build_runner());
    scheduler.run_forever().await
}

/// Build the job runner: each due job runs its prompt through a headless agent.
fn build_runner() -> joey_cron::scheduler::JobRunner {
    Box::new(|job: Job| {
        Box::pin(async move {
            let config = Config::load()?;
            let agent_cfg = AgentConfig::from_config(&config);
            let cwd = std::env::current_dir().unwrap_or_default();
            let ctx = ToolContext::new(cwd, config, format!("cron-{}", job.id))
                .with_interactive(false);
            let registry = ToolRegistry::with_builtins();
            let mut agent = Agent::new(agent_cfg, registry, ctx)
                .map_err(|e| anyhow::anyhow!("agent init failed: {}", e))?;
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
            let result = agent.run_turn(&job.prompt, tx).await;
            let _ = drain.await;
            Ok(result.final_text)
        })
    })
}
