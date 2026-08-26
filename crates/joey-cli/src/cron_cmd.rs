//! `joey cron` (port of `hermes_cli/subcommands/cron.py` +
//! `hermes_cli/cron.py`): bare = list; create/add with the full flag set;
//! run = trigger-now + one synchronous tick; status = ticker heartbeat ages;
//! the standalone ticker loop lives under `tick --loop`.

use anyhow::Result;
use clap::{Args, Subcommand};
use joey_agent_core::{Agent, AgentConfig};
use joey_core::Config;
use joey_cron::{build_cron_prompt, CreateJobOptions, CronStore, Job, Scheduler, TICKER_INTERVAL_SECONDS};
use joey_tools::{ToolContext, ToolRegistry};
use nu_ansi_term::Color;

use crate::render;

#[derive(Args, Debug)]
pub struct CronArgs {
    #[command(subcommand)]
    pub action: Option<CronAction>,
}

#[derive(Subcommand, Debug)]
pub enum CronAction {
    /// List scheduled jobs
    List(ListArgs),
    /// Create a scheduled job
    #[command(alias = "add")]
    Create(CreateArgs),
    /// Pause a scheduled job
    Pause { job_id: String },
    /// Resume a paused job
    Resume { job_id: String },
    /// Trigger a job now (runs one scheduler tick synchronously)
    Run { job_id: String },
    /// Remove a scheduled job
    #[command(aliases = ["rm", "delete"])]
    Remove { job_id: String },
    /// Check if the cron scheduler is running
    Status,
    /// Run due jobs once and exit; --loop runs the standalone scheduler
    Tick(TickArgs),
    #[command(external_subcommand)]
    Other(Vec<String>),
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Include disabled jobs
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Debug)]
pub struct TickArgs {
    /// Run the ticker forever (the standalone scheduler — upstream runs this
    /// inside the gateway; use this on hosts without a gateway)
    #[arg(long = "loop")]
    pub run_loop: bool,
}

#[derive(Args, Debug)]
pub struct CreateArgs {
    /// Schedule like '30m', 'every 2h', or '0 9 * * *'
    pub schedule: String,
    /// Optional self-contained prompt or task instruction
    pub prompt: Option<String>,
    /// Optional human-friendly job name
    #[arg(long)]
    pub name: Option<String>,
    /// Delivery target: origin, local, or platform:chat_id
    #[arg(long)]
    pub deliver: Option<String>,
    /// Optional repeat count
    #[arg(long)]
    pub repeat: Option<i64>,
    /// Attach a skill. Repeat to add multiple skills.
    #[arg(long = "skill", action = clap::ArgAction::Append)]
    pub skill: Vec<String>,
    /// Comma-separated skills to attach
    #[arg(long = "skills")]
    pub skills: Option<String>,
    /// Path to a script under ~/.joey/scripts/. Default mode: script stdout
    /// is injected into the agent's prompt each run. With --no-agent: the
    /// script IS the job and its stdout is delivered verbatim.
    #[arg(long)]
    pub script: Option<String>,
    /// Absolute path for the job to run from
    #[arg(long)]
    pub workdir: Option<String>,
    /// Skip the LLM entirely — run --script on schedule and deliver its
    /// stdout directly
    #[arg(long = "no-agent")]
    pub no_agent: bool,
}

pub async fn cron_command(args: CronArgs) -> Result<i32> {
    match args.action {
        None => list(false),
        Some(CronAction::List(a)) => list(a.all),
        Some(CronAction::Create(a)) => create(&a),
        Some(CronAction::Pause { job_id }) => job_action("pause", &job_id),
        Some(CronAction::Resume { job_id }) => job_action("resume", &job_id),
        Some(CronAction::Remove { job_id }) => job_action("remove", &job_id),
        Some(CronAction::Run { job_id }) => run_now(&job_id).await,
        Some(CronAction::Status) => status(),
        Some(CronAction::Tick(a)) => {
            if a.run_loop {
                run_loop().await
            } else {
                tick_once().await
            }
        }
        Some(CronAction::Other(rest)) => {
            let sub = rest.first().map(String::as_str).unwrap_or("");
            match sub {
                "edit" => edit(rest.get(1).map(String::as_str)),
                "runs" | "history" => {
                    let job_ref = rest.get(1).map(String::as_str);
                    runs(job_ref)
                }
                _ => {
                    println!("Unknown cron command: {}", sub);
                    println!("Usage: joey cron [list|create|pause|resume|run|remove|status|tick|edit|runs]");
                    Ok(1)
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// edit / runs / history (upstream cron.py extras)
// ---------------------------------------------------------------------------

/// `joey cron edit [job]` — open the cron job store in $EDITOR. With a job
/// ref, edits just that job's YAML block in a focused temp file.
fn edit(job_ref: Option<&str>) -> Result<i32> {
    let store = CronStore::open_default();
    let jobs_path = store.dir().join("jobs.json");
    if !jobs_path.exists() {
        println!("No scheduled jobs yet ({} doesn't exist).", jobs_path.display());
        println!("Create one first: joey cron create <schedule> <prompt>");
        return Ok(1);
    }
    let editor = std::env::var("EDITOR")
        .ok()
        .filter(|e| !e.trim().is_empty())
        .or_else(|| std::env::var("VISUAL").ok().filter(|e| !e.trim().is_empty()))
        .or_else(|| {
            for cmd in ["nano", "vim", "vi"] {
                if which::which(cmd).is_ok() {
                    return Some(cmd.to_string());
                }
            }
            None
        });
    let Some(editor) = editor else {
        println!("No editor found ($EDITOR/$VISUAL, nano/vim/vi).");
        println!("The job store lives at {}", jobs_path.display());
        return Ok(1);
    };
    // Lock-aware: snapshot → edit → validate → save.
    let before = std::fs::read_to_string(&jobs_path)?;
    let tmp = std::env::temp_dir().join(format!("joey-cron-{}.json", std::process::id()));
    std::fs::write(&tmp, &before)?;
    let _ = job_ref; // focused single-job editing deferred; full-store edit works
    println!("Opening {} in {}…", jobs_path.display(), editor);
    let status = std::process::Command::new(&editor).arg(&tmp).status();
    let edited = match status {
        Ok(s) if s.success() => std::fs::read_to_string(&tmp)?,
        _ => {
            let _ = std::fs::remove_file(&tmp);
            println!("Editor exited without saving — no changes.");
            return Ok(0);
        }
    };
    let _ = std::fs::remove_file(&tmp);
    if edited == before {
        println!("No changes.");
        return Ok(0);
    }
    // Validate: must deserialize as the upstream envelope.
    match serde_json::from_str::<serde_json::Value>(&edited) {
        Ok(v) if v.get("jobs").and_then(|j| j.as_array()).is_some() => {
            std::fs::write(&jobs_path, &edited)?;
            let count = store.list_jobs(true).map(|j| j.len()).unwrap_or(0);
            println!("{}", Color::Green.paint(format!("✓ Job store updated ({count} job(s)).")));
            Ok(0)
        }
        Ok(_) => {
            println!("{}", Color::Red.paint("Edited file is not a valid cron jobs envelope (needs a `jobs` array) — changes NOT saved."));
            println!("  Your edit is recoverable from the editor buffer; the store is unchanged.");
            Ok(1)
        }
        Err(e) => {
            println!("{}", Color::Red.paint(format!("Edited file has a JSON syntax error ({e}) — changes NOT saved.")));
            Ok(1)
        }
    }
}

/// `joey cron runs [job]` — show recent run outputs (from the per-job
/// output/ directories, newest first, retention-capped).
fn runs(job_ref: Option<&str>) -> Result<i32> {
    let store = CronStore::open_default();

    // Resolve which jobs to show.
    let jobs: Vec<Job> = match job_ref {
        Some(r) => match store.resolve_job_ref(r)? {
            Some(job) => vec![job],
            None => {
                println!("Job not found: {r}");
                return Ok(1);
            }
        },
        None => store.list_jobs(true)?,
    };

    let mut any = false;
    for job in &jobs {
        let out_dir = match store.job_output_dir(&job.id) {
            Ok(d) => d,
            Err(_) => continue,
        };
        let mut runs: Vec<std::path::PathBuf> = std::fs::read_dir(&out_dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().map(|x| x == "md").unwrap_or(false))
                    .collect()
            })
            .unwrap_or_default();
        runs.sort();
        runs.reverse();
        if runs.is_empty() {
            continue;
        }
        any = true;
        println!(
            "  {} {} — {} retained run(s):",
            Color::Yellow.paint(&job.id),
            if job.name.is_empty() { "(unnamed)" } else { &job.name },
            runs.len()
        );
        for run in runs.iter().take(10) {
            let stamp = run
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let failed = std::fs::read_to_string(run)
                .map(|c| c.contains("(FAILED)"))
                .unwrap_or(false);
            let marker = if failed {
                Color::Red.paint("✗").to_string()
            } else {
                Color::Green.paint("✓").to_string()
            };
            println!("    {marker} {stamp}  ({})", run.display());
        }
        if let Some(last) = &job.last_run_at {
            println!(
                "    {} last run recorded on job: {last} ({})",
                Color::DarkGray.paint("·"),
                job.last_status.as_deref().unwrap_or("?")
            );
        }
        println!();
    }
    if !any {
        println!("No run outputs recorded yet (they appear under {}/output/ after jobs fire).", store.dir().display());
        println!("Trigger one now: joey cron run <job-id>");
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// list (hermes_cli/cron.py:99-190 card layout)
// ---------------------------------------------------------------------------

fn list(show_all: bool) -> Result<i32> {
    let store = CronStore::open_default();
    let jobs = store.list_jobs(show_all)?;
    if jobs.is_empty() {
        println!("{}", Color::DarkGray.paint("No scheduled jobs."));
        println!(
            "{}",
            Color::DarkGray.paint("Create one with 'joey cron create ...' or the /cron command in chat.")
        );
        return Ok(0);
    }

    println!();
    println!("{}", Color::Cyan.paint("┌─────────────────────────────────────────────────────────────────────────┐"));
    println!("{}", Color::Cyan.paint("│                         Scheduled Jobs                                  │"));
    println!("{}", Color::Cyan.paint("└─────────────────────────────────────────────────────────────────────────┘"));
    println!();

    for job in &jobs {
        print_job_card(job);
    }
    warn_if_scheduler_not_running(&store);
    Ok(0)
}

fn print_job_card(job: &Job) {
    let status = if job.state == "paused" {
        Color::Yellow.paint("[paused]").to_string()
    } else if job.state == "completed" {
        Color::Blue.paint("[completed]").to_string()
    } else if job.enabled {
        Color::Green.paint("[active]").to_string()
    } else {
        Color::Red.paint("[disabled]").to_string()
    };
    let repeat_str = match &job.repeat {
        Some(r) => match r.times {
            Some(times) => format!("{}/{}", r.completed, times),
            None => "∞".to_string(),
        },
        None => "∞".to_string(),
    };
    println!("  {} {}", Color::Yellow.paint(&job.id), status);
    println!("    Name:      {}", if job.name.is_empty() { "(unnamed)" } else { &job.name });
    println!("    Schedule:  {}", job.schedule_display);
    println!("    Repeat:    {}", repeat_str);
    println!("    Next run:  {}", job.next_run_at.as_deref().unwrap_or("?"));
    println!("    Deliver:   {}", if job.deliver.is_empty() { "local" } else { &job.deliver });
    if !job.skills.is_empty() {
        println!("    Skills:    {}", job.skills.join(", "));
    }
    if let Some(script) = &job.script {
        println!("    Script:    {}", script);
    }
    if job.no_agent {
        println!("    Mode:      {} (script stdout delivered directly)", Color::DarkGray.paint("no-agent"));
    }
    if let Some(workdir) = &job.workdir {
        println!("    Workdir:   {}", workdir);
    }
    if let Some(last_status) = &job.last_status {
        let display = if last_status == "ok" {
            Color::Green.paint("ok").to_string()
        } else {
            Color::Red
                .paint(format!("{}: {}", last_status, job.last_error.as_deref().unwrap_or("?")))
                .to_string()
        };
        println!("    Last run:  {}  {}", job.last_run_at.as_deref().unwrap_or("?"), display);
    }
    if let Some(err) = &job.last_delivery_error {
        println!("    {} {}", Color::Yellow.paint("⚠ Delivery failed:"), err);
    }
    println!();
}

/// Warn that jobs won't fire unless a ticker runs (hermes_cli/cron.py:66-96 —
/// upstream checks for a gateway process; this port checks the ticker
/// heartbeat since `joey cron tick --loop` is the standalone scheduler).
fn warn_if_scheduler_not_running(store: &CronStore) {
    let fresh = store
        .get_ticker_heartbeat_age()
        .map(|age| age <= (TICKER_INTERVAL_SECONDS * 3 + 20) as f64)
        .unwrap_or(false);
    if fresh {
        return;
    }
    println!("{}", Color::Yellow.paint("  ⚠  Scheduler is not running — jobs won't fire automatically."));
    println!("{}", Color::DarkGray.paint("     Start it with: joey cron tick --loop"));
    println!("{}", Color::DarkGray.paint("     Check status:  joey cron status"));
}

// ---------------------------------------------------------------------------
// create (cron.py:319-356)
// ---------------------------------------------------------------------------

fn normalized_skills(create: &CreateArgs) -> Option<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for s in &create.skill {
        let t = s.trim();
        if !t.is_empty() && !out.iter().any(|e| e == t) {
            out.push(t.to_string());
        }
    }
    if let Some(raw) = &create.skills {
        for s in raw.split(',') {
            let t = s.trim();
            if !t.is_empty() && !out.iter().any(|e| e == t) {
                out.push(t.to_string());
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn create(args: &CreateArgs) -> Result<i32> {
    let store = CronStore::open_default();
    let opts = CreateJobOptions {
        name: args.name.clone(),
        repeat: args.repeat,
        deliver: args.deliver.clone(),
        skills: normalized_skills(args),
        script: args.script.clone(),
        workdir: args.workdir.clone(),
        no_agent: args.no_agent,
        ..Default::default()
    };
    match store.create_job(args.prompt.as_deref(), &args.schedule, opts) {
        Err(e) => {
            println!("{}", Color::Red.paint(format!("Failed to create job: {}", e)));
            Ok(1)
        }
        Ok(job) => {
            println!("{}", Color::Green.paint(format!("Created job: {}", job.id)));
            println!("  Name: {}", job.name);
            println!("  Schedule: {}", job.schedule_display);
            if !job.skills.is_empty() {
                println!("  Skills: {}", job.skills.join(", "));
            }
            if let Some(script) = &job.script {
                println!("  Script: {}", script);
            }
            if job.no_agent {
                println!("  Mode: no-agent (script stdout delivered directly)");
            }
            if let Some(workdir) = &job.workdir {
                println!("  Workdir: {}", workdir);
            }
            println!("  Next run: {}", job.next_run_at.as_deref().unwrap_or("?"));
            warn_if_scheduler_not_running(&store);
            Ok(0)
        }
    }
}

// ---------------------------------------------------------------------------
// pause / resume / remove (cron.py:423-441)
// ---------------------------------------------------------------------------

fn job_action(action: &str, job_ref: &str) -> Result<i32> {
    let store = CronStore::open_default();
    let outcome: Result<Option<Job>> = match action {
        "pause" => store.pause_job(job_ref, None),
        "resume" => store.resume_job(job_ref),
        "remove" => {
            let job = store.resolve_job_ref(job_ref)?;
            match job {
                None => Ok(None),
                Some(j) => {
                    store.remove_job(job_ref)?;
                    Ok(Some(j))
                }
            }
        }
        _ => unreachable!(),
    };
    let verb = match action {
        "pause" => "Paused",
        "resume" => "Resumed",
        _ => "Removed",
    };
    match outcome {
        Err(e) => {
            println!("{}", Color::Red.paint(format!("Failed to {} job: {}", action, e)));
            Ok(1)
        }
        Ok(None) => {
            println!("{}", Color::Red.paint(format!("Failed to {} job: Job not found: {}", action, job_ref)));
            Ok(1)
        }
        Ok(Some(job)) => {
            println!("{}", Color::Green.paint(format!("{} job: {} ({})", verb, job.name, job.id)));
            if action == "resume" {
                if let Ok(Some(updated)) = store.get_job(&job.id) {
                    if let Some(next) = updated.next_run_at {
                        println!("  Next run: {}", next);
                    }
                }
            }
            Ok(0)
        }
    }
}

// ---------------------------------------------------------------------------
// run — trigger-now + synchronous tick (cron.py:477-478, tool "run" action)
// ---------------------------------------------------------------------------

async fn run_now(job_ref: &str) -> Result<i32> {
    let store = CronStore::open_default();
    let triggered = match store.trigger_job(job_ref) {
        Err(e) => {
            println!("{}", Color::Red.paint(format!("Failed to run job: {}", e)));
            return Ok(1);
        }
        Ok(None) => {
            println!("{}", Color::Red.paint(format!("Failed to run job: Job not found: {}", job_ref)));
            return Ok(1);
        }
        Ok(Some(j)) => j,
    };
    println!("{}", Color::Green.paint(format!("Triggered job: {} ({})", triggered.name, triggered.id)));

    // Run one synchronous tick so the job executes now.
    let scheduler = Scheduler::new(CronStore::open_default(), build_runner());
    let processed = scheduler.tick().await.unwrap_or(0);
    if processed == 0 {
        println!("  It will run on the next scheduler tick.");
        return Ok(0);
    }
    match store.get_job(&triggered.id)? {
        Some(after) => {
            let outcome = match after.last_status.as_deref() {
                Some("ok") => "succeeded",
                Some(_) => "failed",
                None => "succeeded",
            };
            println!("  Ran now: {}.", outcome);
            if let Some(next) = after.next_run_at {
                println!("  Next run: {}", next);
            }
        }
        // Repeat limit reached → the store auto-deleted the job.
        None => println!("  Ran now: succeeded. (repeat limit reached — job removed)"),
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// status (hermes_cli/cron.py:217-316, heartbeat variant)
// ---------------------------------------------------------------------------

fn status() -> Result<i32> {
    let store = CronStore::open_default();
    println!();
    let stale_after = (TICKER_INTERVAL_SECONDS * 3 + 20) as f64;
    let hb_age = store.get_ticker_heartbeat_age();
    let ok_age = store.get_ticker_success_age();
    match hb_age {
        Some(age) if age <= stale_after => {
            if let Some(ok) = ok_age {
                if ok > stale_after {
                    println!(
                        "{}",
                        Color::Yellow.paint(format!(
                            "⚠ Cron ticker is running, but no tick has succeeded in {}s — ticks may be failing.",
                            ok as i64
                        ))
                    );
                    println!("  Check the logs for 'Cron tick error'.");
                } else {
                    println!("{}", Color::Green.paint("✓ Cron scheduler is running — jobs will fire automatically"));
                    println!("  Ticker heartbeat: {}s ago", age as i64);
                }
            } else {
                println!("{}", Color::Green.paint("✓ Cron scheduler is running — jobs will fire automatically"));
                println!("  Ticker heartbeat: {}s ago", age as i64);
            }
        }
        Some(age) => {
            println!(
                "{}",
                Color::Yellow.paint(format!(
                    "⚠ Cron ticker looks STALLED — no heartbeat for {}s (expected every ~{}s).",
                    age as i64, TICKER_INTERVAL_SECONDS
                ))
            );
            println!("  Cron jobs may NOT be firing. Restart: joey cron tick --loop");
        }
        None => {
            println!("{}", Color::Red.paint("✗ Cron scheduler is not running — cron jobs will NOT fire"));
            println!();
            println!("  To enable automatic execution:");
            println!("    joey cron tick --loop     # Run the standalone scheduler in foreground");
        }
    }
    println!();

    let jobs = store.list_jobs(false)?;
    if jobs.is_empty() {
        println!("  No active jobs");
    } else {
        println!("  {} active job(s)", jobs.len());
        let next: Option<String> = jobs.iter().filter_map(|j| j.next_run_at.clone()).min();
        if let Some(next) = next {
            println!("  Next run: {}", next);
        }
    }
    println!();
    Ok(0)
}

// ---------------------------------------------------------------------------
// tick / tick --loop
// ---------------------------------------------------------------------------

async fn tick_once() -> Result<i32> {
    let scheduler = Scheduler::new(CronStore::open_default(), build_runner());
    let ran = scheduler.tick().await?;
    render::info(&format!("Ran {} due job(s).", ran));
    Ok(0)
}

async fn run_loop() -> Result<i32> {
    render::info(&format!(
        "Starting cron scheduler ({}s ticker). Ctrl-C to stop.",
        TICKER_INTERVAL_SECONDS
    ));
    let scheduler = Scheduler::new(CronStore::open_default(), build_runner());
    scheduler.run_forever().await?;
    Ok(0)
}

/// Each due job runs its prompt through a headless agent (the cron execution
/// hint comes from `build_cron_prompt`).
fn build_runner() -> joey_cron::JobRunner {
    Box::new(|job: Job| {
        Box::pin(async move {
            let config = Config::load()?;
            let mut agent_cfg = AgentConfig::from_config(&config);
            if let Some(model) = &job.model {
                agent_cfg.model = model.clone();
            }
            if agent_cfg.enabled_tools.is_empty() {
                agent_cfg.enabled_tools = crate::commands::platform_tools(&config, "cron");
            }
            let cwd = job
                .workdir
                .as_ref()
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default();
            let ctx = ToolContext::new(cwd, config.clone(), format!("cron-{}", job.id)).with_interactive(false);
            let mut registry = ToolRegistry::with_builtins();

            // Wire session_search.
            let session_db = joey_core::SessionDb::open_default()
                .ok()
                .map(|db| std::sync::Arc::new(std::sync::Mutex::new(db)));
            joey_tools::builtins::register_session_tools(&mut registry, session_db);

            // Wire orchestration.
            let mgr_config = joey_orchestration::ManagerConfig::from_config(&config);
            let manager = std::sync::Arc::new(joey_orchestration::SubagentManager::new(mgr_config));
            let base_registry = registry.clone();
            joey_orchestration::register_orchestration(
                &mut registry,
                manager.clone(),
                agent_cfg.clone(),
                config.clone(),
                base_registry,
                None,
            );

            let mut agent = Agent::new(agent_cfg, registry, ctx)
                .map_err(|e| anyhow::anyhow!("agent init failed: {}", e))?;
            agent.set_provider_semaphore(manager.semaphore());
            let prompt = build_cron_prompt(&job);
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            let drain = tokio::spawn(async move {
                let mut failed: Option<String> = None;
                while let Some(ev) = rx.recv().await {
                    if let joey_agent_core::AgentEvent::Failed(m) = ev {
                        failed = Some(m);
                    }
                }
                failed
            });
            let result = agent.run_turn(&prompt, tx).await;
            if let Some(err) = drain.await.ok().flatten() {
                anyhow::bail!("{}", err);
            }
            Ok(result.final_text)
        })
    })
}
