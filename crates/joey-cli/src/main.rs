//! `joey` — the joey-agent command-line interface.
//!
//! Port of the `hermes` CLI entrypoint (`hermes_cli/_parser.py` +
//! `hermes_cli/main.py`). Bare `joey` starts the interactive REPL; `-z`
//! runs one-shot mode; subcommands cover chat/model/config/tools/doctor/
//! cron/mcp/skills/version. A rewrite of Hermes Agent (Nous Research, MIT).

mod commands;
mod auth_cmd;
mod config_cmd;
mod cron_cmd;
mod doctor_cmd;
mod mcp_cmd;
mod model_catalog;
mod oneshot;
mod render;
mod repl;
mod secret_prompt;
mod setup_wizard;
mod skills_cmd;
mod slash;
mod tools_cmd;

use std::sync::OnceLock;

use clap::{Args, Parser, Subcommand};

/// Active profile name (set by the pre-argparse `-p/--profile` scan; used by
/// the exit outro to build resume hints — cli.py:12706-12712).
static ACTIVE_PROFILE: OnceLock<String> = OnceLock::new();

pub fn active_profile() -> &'static str {
    ACTIVE_PROFILE.get().map(String::as_str).unwrap_or("default")
}

/// Examples epilogue (port of `_parser.py:40-97`, trimmed to the commands
/// this port ships).
const EPILOGUE: &str = "\
Examples:
    joey                          Start interactive chat
    joey chat -q \"Hello\"          Single query mode
    joey -c                       Resume the most recent session
    joey -c \"my project\"          Resume a session by name
    joey --resume <session_id>    Resume a specific session by ID
    joey -z \"summarize FOO.md\"    One-shot mode: print only the final answer
    joey model                    Select default model
    joey config                   View configuration
    joey config edit              Edit config in $EDITOR
    joey config set model.default gpt-4
    joey -s joey-agent-dev,github-auth
    joey cron list                List scheduled jobs
    joey mcp add gh --command npx --args -y @modelcontextprotocol/server-github
    joey doctor                   Check configuration and dependencies

For more help on a command:
    joey <command> --help";

#[derive(Parser, Debug)]
#[command(
    name = "joey",
    about = "Joey Agent - AI assistant with tool-calling capabilities",
    after_help = EPILOGUE,
    disable_help_subcommand = true,
    disable_version_flag = true
)]
pub struct Cli {
    /// Show version and exit
    #[arg(short = 'V', long = "version")]
    version: bool,

    /// One-shot mode: send a single prompt and print ONLY the final response
    /// text to stdout. No banner, no spinner, no tool previews, no session_id
    /// line. Tools and AGENTS.md in the CWD are loaded as normal; approvals
    /// are auto-bypassed. Intended for scripts / pipes.
    #[arg(short = 'z', long = "oneshot", value_name = "PROMPT")]
    oneshot: Option<String>,

    /// One-shot mode only: after the run, write a JSON usage report
    /// (token counts, model, api_calls) to PATH. The report is written even
    /// when the run fails, so pipelines can always account for spend. No
    /// effect outside -z/--oneshot.
    #[arg(long = "usage-file", value_name = "PATH")]
    usage_file: Option<String>,

    /// Model override for this invocation (e.g. anthropic/claude-sonnet-4.6).
    /// Applies to -z/--oneshot and chat. Also settable via
    /// JOEY_INFERENCE_MODEL env var (one-shot mode only).
    #[arg(short = 'm', long = "model")]
    model: Option<String>,

    /// Provider override for this invocation (e.g. openrouter, anthropic).
    /// The persistent provider lives in config.yaml under model.provider —
    /// use `joey model` or edit the file to change it.
    #[arg(long = "provider")]
    provider: Option<String>,

    /// Comma-separated toolsets to enable for this invocation.
    #[arg(short = 't', long = "toolsets")]
    toolsets: Option<String>,

    /// Resume a previous session by ID or title
    #[arg(short = 'r', long = "resume", value_name = "SESSION")]
    resume: Option<String>,

    /// Resume a session by name, or the most recent if no name given
    #[arg(
        short = 'c',
        long = "continue",
        value_name = "SESSION_NAME",
        num_args = 0..=1,
        default_missing_value = ""
    )]
    continue_last: Option<String>,

    /// Preload one or more skills for the session (repeat flag or
    /// comma-separate)
    #[arg(short = 's', long = "skills", action = clap::ArgAction::Append)]
    skills: Vec<String>,

    /// Maximum tool-calling iterations per conversation turn (default: 90,
    /// or agent.max_turns in config)
    #[arg(long = "max-turns", value_name = "N")]
    max_turns: Option<usize>,

    /// Bypass all dangerous command approval prompts (use at your own risk)
    #[arg(long = "yolo")]
    yolo: bool,

    /// Include the session ID in the agent's system prompt
    #[arg(long = "pass-session-id")]
    pass_session_id: bool,

    /// Ignore ~/.joey/config.yaml and fall back to built-in defaults
    /// (credentials in .env are still loaded)
    #[arg(long = "ignore-user-config")]
    ignore_user_config: bool,

    /// Troubleshooting mode: disable ALL customizations — user config and
    /// MCP servers (implies --ignore-user-config)
    #[arg(long = "safe-mode")]
    safe_mode: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Interactive chat with the agent
    Chat(ChatArgs),
    /// Select default model and provider
    Model(ModelArgs),
    /// Manage provider authentication
    Auth(auth_cmd::AuthArgs),
    /// Configure which tools are enabled per platform
    Tools(tools_cmd::ToolsArgs),
    /// View and edit configuration
    Config(config_cmd::ConfigArgs),
    /// Check configuration and dependencies
    Doctor(doctor_cmd::DoctorArgs),
    /// Show version
    Version,
    /// Cron job management
    Cron(cron_cmd::CronArgs),
    /// Manage MCP servers
    Mcp(mcp_cmd::McpArgs),
    /// Search, install, configure, and manage skills
    Skills(skills_cmd::SkillsArgs),
    /// Print the resolved home directory (joey extension)
    Home,
}

/// `joey model` (main.py `cmd_model`): the provider + model setup wizard.
#[derive(Args, Debug, Default)]
pub struct ModelArgs {
    /// Clear the cached model picker catalogs before showing the picker
    #[arg(long = "refresh")]
    pub refresh: bool,
}

/// `joey chat` — mirrors the top-level flags (with `-q`/`-Q` extras);
/// chat-level values win over top-level ones (`_parser.py:269-448`).
#[derive(Args, Debug, Default)]
pub struct ChatArgs {
    /// Single query (non-interactive mode)
    #[arg(short = 'q', long = "query")]
    pub query: Option<String>,

    /// Model to use (e.g., anthropic/claude-sonnet-4)
    #[arg(short = 'm', long = "model")]
    pub model: Option<String>,

    /// Comma-separated toolsets to enable
    #[arg(short = 't', long = "toolsets")]
    pub toolsets: Option<String>,

    /// Preload one or more skills for the session (repeat flag or
    /// comma-separate)
    #[arg(short = 's', long = "skills", action = clap::ArgAction::Append)]
    pub skills: Vec<String>,

    /// Inference provider (default: auto)
    #[arg(long = "provider")]
    pub provider: Option<String>,

    /// Verbose output
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,

    /// Quiet mode for programmatic use: suppress banner, spinner, and tool
    /// previews. Only output the final response and session info.
    #[arg(short = 'Q', long = "quiet")]
    pub quiet: bool,

    /// Resume a previous session by ID (shown on exit)
    #[arg(short = 'r', long = "resume", value_name = "SESSION_ID")]
    pub resume: Option<String>,

    /// Resume a session by name, or the most recent if no name given
    #[arg(
        short = 'c',
        long = "continue",
        value_name = "SESSION_NAME",
        num_args = 0..=1,
        default_missing_value = ""
    )]
    pub continue_last: Option<String>,

    /// Maximum tool-calling iterations per conversation turn (default: 90,
    /// or agent.max_turns in config)
    #[arg(long = "max-turns", value_name = "N")]
    pub max_turns: Option<usize>,

    /// Bypass all dangerous command approval prompts (use at your own risk)
    #[arg(long = "yolo")]
    pub yolo: bool,

    /// Include the session ID in the agent's system prompt
    #[arg(long = "pass-session-id")]
    pub pass_session_id: bool,

    /// Ignore ~/.joey/config.yaml and fall back to built-in defaults
    /// (credentials in .env are still loaded)
    #[arg(long = "ignore-user-config")]
    pub ignore_user_config: bool,

    /// Troubleshooting mode: disable ALL customizations — user config and
    /// MCP servers (implies --ignore-user-config)
    #[arg(long = "safe-mode")]
    pub safe_mode: bool,
}

// ---------------------------------------------------------------------------
// Profile pre-parse (port of main.py:474-546 `_apply_profile_override`)
// ---------------------------------------------------------------------------

/// Flags that consume a value (so `-p` scanning skips their arguments).
const VALUE_FLAGS: &[&str] = &[
    "-z", "--oneshot", "-m", "--model", "--provider", "-t", "--toolsets", "-r", "--resume", "-s",
    "--skills", "--usage-file", "--max-turns", "-q", "--query",
];
const OPTIONAL_VALUE_FLAGS: &[&str] = &["-c", "--continue"];

fn profile_name_valid(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// True once argv reaches `joey mcp add ... --args <command argv>` — flags
/// after that point belong to the child MCP command.
fn inside_mcp_add_args(argv: &[String], index: usize) -> bool {
    let Some(mcp_idx) = argv[..index].iter().position(|a| a == "mcp") else {
        return false;
    };
    argv[mcp_idx + 1..index].iter().any(|a| a == "add")
}

/// Scan argv for `-p NAME` / `--profile NAME` / `--profile=NAME`, returning
/// `(profile_name, strip_range)`.
pub fn scan_profile_flag(argv: &[String]) -> (Option<String>, Option<(usize, usize)>) {
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        if arg == "--" {
            break;
        }
        if arg == "--args" && inside_mcp_add_args(argv, i) {
            break;
        }
        if (arg == "--profile" || arg == "-p") && i + 1 < argv.len() {
            let name = argv[i + 1].clone();
            if !profile_name_valid(&name) {
                return (None, None);
            }
            return (Some(name), Some((i, 2)));
        }
        if let Some(rest) = arg.strip_prefix("--profile=") {
            if !profile_name_valid(rest) {
                return (None, None);
            }
            return (Some(rest.to_string()), Some((i, 1)));
        }
        if !arg.contains('=') && VALUE_FLAGS.contains(&arg.as_str()) && i + 1 < argv.len() {
            i += 2;
        } else if !arg.contains('=')
            && OPTIONAL_VALUE_FLAGS.contains(&arg.as_str())
            && i + 1 < argv.len()
            && !argv[i + 1].starts_with('-')
        {
            i += 2;
        } else {
            i += 1;
        }
    }
    (None, None)
}

/// Apply `-p/--profile` BEFORE clap runs: set `JOEY_HOME` to the profile home
/// (`<root>/profiles/<name>`) and strip the flag from argv. Falls back to the
/// sticky `<root>/active_profile` file.
fn apply_profile_override(argv: &mut Vec<String>) {
    let (mut profile_name, strip) = scan_profile_flag(&argv[..]);

    // Trust an existing JOEY_HOME only when it already points at a profile dir.
    if profile_name.is_none() {
        if let Ok(home) = std::env::var("JOEY_HOME") {
            if !home.trim().is_empty() {
                let p = std::path::PathBuf::from(home.trim());
                if p.parent().and_then(|d| d.file_name()).map(|n| n == "profiles").unwrap_or(false)
                {
                    return;
                }
            }
        }
    }

    // Sticky default from <root>/active_profile.
    if profile_name.is_none() {
        let active_path = joey_core::constants::default_root().join("active_profile");
        if let Ok(name) = std::fs::read_to_string(&active_path) {
            let name = name.trim().to_string();
            if !name.is_empty() && name != "default" {
                profile_name = Some(name);
            }
        }
    }

    let Some(name) = profile_name else { return };
    if name != "default" {
        let dir = joey_core::constants::default_root().join("profiles").join(&name);
        if !dir.is_dir() {
            // Only hard-fail for the explicit flag; a stale sticky file
            // degrades to the default home with a warning.
            if strip.is_some() {
                eprintln!(
                    "Error: Profile '{}' does not exist. Create it with: joey profile create {}",
                    name, name
                );
                std::process::exit(1);
            }
            eprintln!("Warning: profile override failed (profile '{}' missing), using default", name);
            return;
        }
        std::env::set_var("JOEY_HOME", &dir);
    }
    let _ = ACTIVE_PROFILE.set(name);
    if let Some((at, count)) = strip {
        argv.drain(at..at + count);
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Restore default SIGPIPE handling so `joey ... | head` dies quietly like
/// any Unix CLI instead of panicking on EPIPE.
#[cfg(unix)]
fn reset_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}
#[cfg(not(unix))]
fn reset_sigpipe() {}

#[tokio::main]
async fn main() {
    reset_sigpipe();
    let mut argv: Vec<String> = std::env::args().collect();
    // argv[0] is the binary; the scanner operates on the tail.
    let mut tail: Vec<String> = argv.split_off(1);
    apply_profile_override(&mut tail);
    argv.extend(tail);
    let _ = ACTIVE_PROFILE.set("default".to_string());

    let cli = match Cli::try_parse_from(&argv) {
        Ok(c) => c,
        // clap prints help/version at 0 and usage errors at 2.
        Err(e) => e.exit(),
    };

    let verbose = matches!(&cli.command, Some(Command::Chat(a)) if a.verbose);
    let _guard = if verbose {
        joey_core::logging::init_verbose("cli")
    } else {
        joey_core::logging::init("cli")
    };

    let code = match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            render::error(&e.to_string());
            1
        }
    };
    std::process::exit(code);
}

/// Wire the env-var-backed flags BEFORE any Config::load (config.rs reads
/// JOEY_IGNORE_USER_CONFIG at load; joey-tools reads JOEY_YOLO_MODE; joey-mcp
/// reads JOEY_SAFE_MODE).
fn wire_flag_env(yolo: bool, ignore_user_config: bool, safe_mode: bool) {
    if yolo {
        std::env::set_var("JOEY_YOLO_MODE", "1");
    }
    if safe_mode {
        std::env::set_var("JOEY_SAFE_MODE", "1");
        std::env::set_var("JOEY_IGNORE_USER_CONFIG", "1");
    }
    if ignore_user_config {
        std::env::set_var("JOEY_IGNORE_USER_CONFIG", "1");
    }
}

async fn run(cli: Cli) -> anyhow::Result<i32> {
    // `joey -V` and `joey version` print identically (main.py:4578-4625).
    if cli.version {
        commands::print_version_info();
        return Ok(0);
    }

    wire_flag_env(cli.yolo, cli.ignore_user_config, cli.safe_mode);
    let _ = joey_core::ensure_home();

    // One-shot mode bypasses everything else (oneshot.py).
    if let Some(prompt) = &cli.oneshot {
        let opts = oneshot::OneshotOptions {
            prompt: prompt.clone(),
            model: cli.model.clone(),
            provider: cli.provider.clone(),
            toolsets: cli.toolsets.clone(),
            usage_file: cli.usage_file.clone(),
            max_turns: cli.max_turns,
        };
        return oneshot::run_oneshot(opts).await;
    }

    match cli.command {
        Some(Command::Version) => {
            commands::print_version_info();
            Ok(0)
        }
        Some(Command::Model(args)) => commands::model_command(args.refresh),
        Some(Command::Auth(args)) => auth_cmd::auth_command(args),
        Some(Command::Config(args)) => config_cmd::config_command(&args),
        Some(Command::Doctor(args)) => doctor_cmd::doctor_command(&args),
        Some(Command::Tools(args)) => tools_cmd::tools_command(&args),
        Some(Command::Skills(args)) => skills_cmd::skills_command(&args),
        Some(Command::Cron(args)) => cron_cmd::cron_command(args).await,
        Some(Command::Mcp(args)) => mcp_cmd::mcp_command(args).await,
        Some(Command::Home) => {
            println!("{}", joey_core::joey_home().display());
            Ok(0)
        }
        Some(Command::Chat(chat)) => {
            wire_flag_env(chat.yolo, chat.ignore_user_config, chat.safe_mode);
            let opts = repl::ChatOptions {
                query: chat.query.clone(),
                quiet: chat.quiet,
                model: chat.model.clone().or(cli.model),
                provider: chat.provider.clone().or(cli.provider),
                toolsets: chat.toolsets.clone().or(cli.toolsets),
                resume: chat.resume.clone().or(cli.resume),
                continue_last: chat.continue_last.clone().or(cli.continue_last),
                max_turns: chat.max_turns.or(cli.max_turns),
                pass_session_id: chat.pass_session_id || cli.pass_session_id,
                skills: if chat.skills.is_empty() { cli.skills } else { chat.skills },
            };
            repl::run_chat(opts).await
        }
        None => {
            let opts = repl::ChatOptions {
                query: None,
                quiet: false,
                model: cli.model,
                provider: cli.provider,
                toolsets: cli.toolsets,
                resume: cli.resume,
                continue_last: cli.continue_last,
                max_turns: cli.max_turns,
                pass_session_id: cli.pass_session_id,
                skills: cli.skills,
            };
            repl::run_chat(opts).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        std::iter::once("joey".to_string())
            .chain(args.iter().map(|s| s.to_string()))
            .collect()
    }

    #[test]
    fn parses_oneshot_flag() {
        let cli = Cli::try_parse_from(argv(&["-z", "hello", "--usage-file", "/tmp/u.json"])).unwrap();
        assert_eq!(cli.oneshot.as_deref(), Some("hello"));
        assert_eq!(cli.usage_file.as_deref(), Some("/tmp/u.json"));
    }

    #[test]
    fn no_top_level_query_flag() {
        // The port-only top-level `-q` is gone; `-q` belongs to `chat`.
        assert!(Cli::try_parse_from(argv(&["-q", "hi"])).is_err());
        let cli = Cli::try_parse_from(argv(&["chat", "-q", "hi", "-Q"])).unwrap();
        match cli.command {
            Some(Command::Chat(c)) => {
                assert_eq!(c.query.as_deref(), Some("hi"));
                assert!(c.quiet);
            }
            other => panic!("expected chat, got {:?}", other),
        }
    }

    #[test]
    fn no_invented_cwd_flag() {
        assert!(Cli::try_parse_from(argv(&["--cwd", "/tmp"])).is_err());
    }

    #[test]
    fn continue_flag_takes_optional_value() {
        let cli = Cli::try_parse_from(argv(&["-c"])).unwrap();
        assert_eq!(cli.continue_last.as_deref(), Some(""));
        let cli = Cli::try_parse_from(argv(&["-c", "my project"])).unwrap();
        assert_eq!(cli.continue_last.as_deref(), Some("my project"));
    }

    #[test]
    fn top_level_flags_parse() {
        let cli = Cli::try_parse_from(argv(&[
            "-m", "anthropic/claude-sonnet-4.6",
            "--provider", "anthropic",
            "-t", "web,file",
            "--max-turns", "5",
            "-s", "a",
            "-s", "b,c",
        ]))
        .unwrap();
        assert_eq!(cli.model.as_deref(), Some("anthropic/claude-sonnet-4.6"));
        assert_eq!(cli.provider.as_deref(), Some("anthropic"));
        assert_eq!(cli.toolsets.as_deref(), Some("web,file"));
        assert_eq!(cli.max_turns, Some(5));
        assert_eq!(cli.skills, vec!["a", "b,c"]);
    }

    #[test]
    fn chat_flags_win_over_top_level() {
        let cli = Cli::try_parse_from(argv(&["-m", "top", "chat", "-m", "sub"])).unwrap();
        match cli.command {
            Some(Command::Chat(c)) => {
                assert_eq!(c.model.as_deref(), Some("sub"));
                assert_eq!(cli.model.as_deref(), Some("top"));
            }
            other => panic!("expected chat, got {:?}", other),
        }
    }

    #[test]
    fn scan_profile_finds_flag_anywhere() {
        let args: Vec<String> = ["chat", "-p", "coder", "-q", "hi"].iter().map(|s| s.to_string()).collect();
        let (name, strip) = scan_profile_flag(&args);
        assert_eq!(name.as_deref(), Some("coder"));
        assert_eq!(strip, Some((1, 2)));
    }

    #[test]
    fn scan_profile_skips_value_flags_and_mcp_args() {
        // `-m -p` value is a model, not a profile flag position.
        let args: Vec<String> = ["-m", "-p", "-p", "coder"].iter().map(|s| s.to_string()).collect();
        let (name, _) = scan_profile_flag(&args);
        assert_eq!(name.as_deref(), Some("coder"));
        // Inside `mcp add --args`, a child `--profile` is NOT joey's.
        let args: Vec<String> = ["mcp", "add", "x", "--args", "--profile", "dockerprof"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (name, _) = scan_profile_flag(&args);
        assert_eq!(name, None);
    }

    #[test]
    fn scan_profile_rejects_invalid_names() {
        let args: Vec<String> = ["-p", "no:xdist"].iter().map(|s| s.to_string()).collect();
        let (name, strip) = scan_profile_flag(&args);
        assert_eq!(name, None);
        assert_eq!(strip, None);
    }

    #[test]
    fn version_flag_parses() {
        let cli = Cli::try_parse_from(argv(&["-V"])).unwrap();
        assert!(cli.version);
    }

    #[test]
    fn mcp_add_parses_remainder_args() {
        let cli = Cli::try_parse_from(argv(&[
            "mcp", "add", "gh", "--command", "npx", "--env", "A=1", "--args", "-y", "server",
        ]))
        .unwrap();
        match cli.command {
            Some(Command::Mcp(m)) => match m.action {
                Some(mcp_cmd::McpAction::Add(a)) => {
                    assert_eq!(a.name, "gh");
                    assert_eq!(a.command.as_deref(), Some("npx"));
                    assert_eq!(a.env, vec!["A=1"]);
                    assert_eq!(a.args, vec!["-y", "server"]);
                }
                other => panic!("expected add, got {:?}", other),
            },
            other => panic!("expected mcp, got {:?}", other),
        }
    }

    #[test]
    fn cron_aliases_parse() {
        for alias in ["create", "add"] {
            let cli = Cli::try_parse_from(argv(&["cron", alias, "30m", "do it"])).unwrap();
            assert!(matches!(
                cli.command,
                Some(Command::Cron(cron_cmd::CronArgs { action: Some(cron_cmd::CronAction::Create(_)) }))
            ));
        }
        for alias in ["remove", "rm", "delete"] {
            let cli = Cli::try_parse_from(argv(&["cron", alias, "someid"])).unwrap();
            assert!(matches!(
                cli.command,
                Some(Command::Cron(cron_cmd::CronArgs {
                    action: Some(cron_cmd::CronAction::Remove { .. })
                }))
            ));
        }
    }
}
