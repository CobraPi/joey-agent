//! `joey` — the joey-agent command-line interface.
//!
//! Port of the `hermes` CLI entrypoint (`hermes_cli/main.py`). Bare `joey`
//! starts the interactive REPL; subcommands cover model/config/tools/doctor/
//! cron/mcp/version. A rewrite of Hermes Agent (Nous Research, MIT).

mod commands;
mod cron_cmd;
mod render;
mod repl;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use joey_core::Config;

#[derive(Parser)]
#[command(
    name = "joey",
    version,
    about = "joey-agent — the self-improving AI agent (Rust). A port of Hermes Agent.",
    disable_help_subcommand = true
)]
struct Cli {
    /// One-shot query: run a single prompt, print only the final answer, exit.
    #[arg(short = 'q', long = "query")]
    query: Option<String>,

    /// Override the model for this invocation.
    #[arg(short = 'm', long = "model")]
    model: Option<String>,

    /// Resume a session by id prefix.
    #[arg(short = 'r', long = "resume")]
    resume: Option<String>,

    /// Working directory (default: current directory).
    #[arg(long = "cwd")]
    cwd: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show or resolve the current model + provider.
    Model,
    /// List toolsets and built-in tools.
    Tools,
    /// Manage configuration.
    Config(ConfigArgs),
    /// Diagnose the environment.
    Doctor,
    /// Show version.
    Version,
    /// Manage scheduled cron jobs.
    Cron(CronArgs),
    /// Manage MCP servers.
    Mcp(McpArgs),
    /// List installed skills.
    Skills,
    /// Print the resolved home directory.
    Home,
}

#[derive(Args)]
struct ConfigArgs {
    /// get | set | show | path
    action: String,
    key: Option<String>,
    value: Option<String>,
}

#[derive(Args)]
struct CronArgs {
    #[command(subcommand)]
    action: CronAction,
}

#[derive(Subcommand)]
enum CronAction {
    /// List all cron jobs.
    List,
    /// Create a job: <schedule> <prompt>.
    Create {
        schedule: String,
        prompt: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        deliver: Option<String>,
    },
    /// Remove a job by id.
    Remove { id: String },
    /// Pause a job.
    Pause { id: String },
    /// Resume a job.
    Resume { id: String },
    /// Run all due jobs once.
    Tick,
    /// Run the scheduler forever.
    Run,
}

#[derive(Args)]
struct McpArgs {
    #[command(subcommand)]
    action: McpAction,
}

#[derive(Subcommand)]
enum McpAction {
    /// Connect to a stdio MCP server and list its tools.
    List {
        /// The server command (e.g. `npx`).
        command: String,
        /// Arguments to the server command.
        args: Vec<String>,
    },
}

#[tokio::main]
async fn main() {
    let _guard = joey_core::logging::init("cli");
    if let Err(e) = run().await {
        render::error(&e.to_string());
        std::process::exit(1);
    }
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Ensure the home dir exists for any command that touches state.
    let _ = joey_core::ensure_home();

    let cwd = cli
        .cwd
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));

    // Load config, applying a per-invocation model override.
    let mut config = Config::load()?;
    if let Some(model) = &cli.model {
        config.set_model_override(model);
    }

    // Subcommand dispatch.
    if let Some(cmd) = cli.command {
        return dispatch(cmd, config, cwd).await;
    }

    // One-shot query mode.
    if let Some(query) = cli.query {
        return commands::oneshot(config, cwd, &query).await;
    }

    // Default: interactive REPL.
    repl::run(config, cwd, cli.resume).await
}

async fn dispatch(cmd: Command, _config: Config, _cwd: PathBuf) -> anyhow::Result<()> {
    match cmd {
        Command::Model => commands::model_cmd(),
        Command::Tools => commands::tools_cmd(),
        Command::Config(a) => commands::config_cmd(&a.action, a.key.as_deref(), a.value.as_deref()),
        Command::Doctor => commands::doctor(),
        Command::Version => {
            commands::version();
            Ok(())
        }
        Command::Home => {
            println!("{}", joey_core::joey_home().display());
            Ok(())
        }
        Command::Skills => {
            let skills = joey_tools::tools::skills_tool::discover();
            if skills.is_empty() {
                render::info("No skills installed.");
            } else {
                for s in skills {
                    println!("{:<28} {}", s.name, s.description);
                }
            }
            Ok(())
        }
        Command::Cron(a) => match a.action {
            CronAction::List => cron_cmd::list(),
            CronAction::Create { schedule, prompt, name, deliver } => {
                cron_cmd::create(&schedule, &prompt, name.as_deref(), deliver.as_deref())
            }
            CronAction::Remove { id } => cron_cmd::remove(&id),
            CronAction::Pause { id } => cron_cmd::set_enabled(&id, false),
            CronAction::Resume { id } => cron_cmd::set_enabled(&id, true),
            CronAction::Tick => cron_cmd::tick().await,
            CronAction::Run => cron_cmd::run_forever().await,
        },
        Command::Mcp(a) => match a.action {
            McpAction::List { command, args } => {
                let client = joey_mcp::McpClient::connect("adhoc", &command, &args).await?;
                let tools = client.list_tools().await?;
                println!("MCP server exposes {} tool(s):", tools.len());
                for t in tools {
                    println!("  {:<28} {}", t.name, t.description);
                }
                client.shutdown().await;
                Ok(())
            }
        },
    }
}
