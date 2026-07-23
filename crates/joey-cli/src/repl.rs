//! The interactive chat REPL (port of the `cli.py` REPL, line-based).
//!
//! Reads input with reedline (persistent history, `❯ ` prompt), dispatches
//! slash commands through the upstream registry (prefix expansion, honest
//! not-yet-ported messages), and runs agent turns with live streaming and
//! upstream Ctrl-C semantics (first press interrupts, second within 2s
//! force-exits — cli.py:13640-13727).

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Instant;

use anyhow::Result;
use joey_agent_core::{Agent, AgentConfig, AgentEvent};
use joey_core::{Config, Role, SessionDb};
use joey_providers::Message;
use joey_tools::{ToolContext, ToolRegistry};
use nu_ansi_term::Color;
use reedline::{
    default_emacs_keybindings, EditCommand, Emacs, FileBackedHistory, KeyCode, KeyModifiers,
    Reedline, ReedlineEvent, Signal,
};

use crate::render::{self, RenderOptions};
use crate::slash::{self, Resolution};

/// Interval for automatic checkpoints (in seconds).
const AUTO_CHECKPOINT_INTERVAL_SECS: u64 = 120;

pub struct ChatOptions {
    pub query: Option<String>,
    pub quiet: bool,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub toolsets: Option<String>,
    pub resume: Option<String>,
    /// `Some("")` = most recent session; `Some(name)` = by title.
    pub continue_last: Option<String>,
    pub max_turns: Option<usize>,
    /// Include the `Session ID:` line in the system prompt.
    pub pass_session_id: bool,
    #[allow(dead_code)] // accepted for CLI parity; preloading is not wired yet
    pub skills: Vec<String>,
    /// Launch the animated ratatui TUI instead of the line-based REPL.
    pub tui: bool,
}

/// Session-scoped overrides applied on (re)build of the agent.
#[derive(Clone, Default)]
pub(crate) struct Overrides {
    pub(crate) model: Option<String>,
    pub(crate) provider: Option<String>,
    pub(crate) toolsets: Option<String>,
    pub(crate) max_turns: Option<usize>,
    pub(crate) reasoning: Option<String>,
    pub(crate) pass_session_id: bool,
}

pub(crate) struct ReplState {
    config: Config,
    cwd: PathBuf,
    overrides: Overrides,
    agent: Agent,
    session_id: String,
    /// Separate read handle for /sessions, /history, /usage queries (the
    /// agent owns its own store connection).
    db: Option<SessionDb>,
    ropts: RenderOptions,
    timestamps: bool,
    last_response: String,
    queued: Vec<String>,
    session_start: Instant,
    /// Session-scoped filesystem checkpoint manager.
    checkpoints: Option<joey_tools::vcs::CheckpointManager>,
    /// Last time an automatic checkpoint was taken.
    last_auto_checkpoint: Instant,
}

// ---------------------------------------------------------------------------
// Agent construction
// ---------------------------------------------------------------------------

/// The interactive streaming overlay (cli.py:510): the user-set
/// `display.streaming` wins; unset means ON in interactive chat (the config
/// layer default is false for headless callers).
fn interactive_streaming(config: &Config) -> bool {
    joey_core::config::get_nested(config.user_doc(), "display.streaming")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

pub(crate) fn build_agent_config(config: &Config, ov: &Overrides) -> AgentConfig {
    let mut cfg = AgentConfig::from_config(config);
    if let Some(m) = &ov.model {
        cfg.model = m.clone();
        if ov.provider.is_none() {
            // An explicit model auto-detects its provider (oneshot.py:350-383).
            cfg.provider = "auto".to_string();
        }
    }
    if let Some(p) = &ov.provider {
        cfg.provider = p.clone();
    }
    cfg.enabled_tools = match &ov.toolsets {
        Some(raw) => joey_tools::resolve_toolsets(&crate::commands::normalize_toolsets(raw)),
        None => crate::commands::platform_tools(config, "cli"),
    };
    if let Some(n) = ov.max_turns {
        cfg.max_turns = n;
    }
    if let Some(level) = &ov.reasoning {
        cfg.reasoning = match level.as_str() {
            "none" | "off" => Some(joey_providers::ReasoningEffort::Disabled),
            other => Some(joey_providers::ReasoningEffort::Level(other.to_string())),
        };
    }
    cfg.stream = interactive_streaming(config);
    cfg.pass_session_id = ov.pass_session_id;
    cfg
}

pub(crate) fn build_agent(
    config: &Config,
    cwd: &std::path::Path,
    ov: &Overrides,
    session_id: &str,
    history: Vec<Message>,
) -> Result<Agent> {
    let agent_cfg = build_agent_config(config, ov);
    let ctx = ToolContext::new(cwd.to_path_buf(), config.clone(), session_id.to_string());
    let registry = ToolRegistry::with_builtins();
    let mut agent =
        Agent::new(agent_cfg, registry, ctx).map_err(|e| anyhow::anyhow!("{}", e))?;
    if !history.is_empty() {
        agent.set_history(history);
    }
    if let Ok(db) = SessionDb::open_default() {
        agent.set_session_store(db, session_id.to_string());
    }
    Ok(agent)
}

pub(crate) fn restore_history(db: &SessionDb, session_id: &str) -> Vec<Message> {
    db.messages(session_id)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| match m.role {
            Role::User => Some(Message::user(m.content)),
            Role::Assistant => Some(Message::assistant(m.content)),
            _ => None,
        })
        .collect()
}

/// Find a session by exact title (case-insensitive), newest first.
pub(crate) fn find_by_title(db: &SessionDb, title: &str) -> Option<String> {
    let want = title.trim().to_lowercase();
    db.list_sessions(200)
        .ok()?
        .into_iter()
        .find(|s| s.title.as_deref().map(|t| t.to_lowercase() == want).unwrap_or(false))
        .map(|s| s.id)
}

// ---------------------------------------------------------------------------
// Prompt (`❯ ` — skin default prompt symbol)
// ---------------------------------------------------------------------------

struct JoeyPrompt;

impl reedline::Prompt for JoeyPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed("❯ ")
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_indicator(&self, _mode: reedline::PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("… ")
    }
    fn render_prompt_history_search_indicator(
        &self,
        history_search: reedline::PromptHistorySearch,
    ) -> Cow<'_, str> {
        match history_search.status {
            reedline::PromptHistorySearchStatus::Passing => Cow::Borrowed("(search) "),
            reedline::PromptHistorySearchStatus::Failing => Cow::Borrowed("(failing search) "),
        }
    }
    fn get_prompt_color(&self) -> reedline::Color {
        reedline::Color::DarkGrey
    }
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

pub async fn run_chat(opts: ChatOptions) -> Result<i32> {
    let config = Config::load()?;

    // First-run guard (main.py:2497-2527).
    if let Some(code) = crate::commands::first_run_guard(&config) {
        return Ok(code);
    }
    // The guard may have just written credentials/model — reload.
    let config = Config::load()?;

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let db = SessionDb::open_default().ok();

    // Establish or resume a session (-r by id-or-title; -c by name/most recent).
    let mut resumed = false;
    let session_id = if let Some(target) = &opts.resume {
        match db.as_ref().and_then(|d| {
            d.resolve_session_id(target)
                .ok()
                .flatten()
                .or_else(|| find_by_title(d, target))
        }) {
            Some(id) => {
                resumed = true;
                id
            }
            None => {
                render::error(&format!("No session found matching '{}'", target));
                return Ok(1);
            }
        }
    } else if let Some(name) = &opts.continue_last {
        let found = db.as_ref().and_then(|d| {
            if name.is_empty() {
                d.most_recent_session().ok().flatten()
            } else {
                find_by_title(d, name).or_else(|| d.resolve_session_id(name).ok().flatten())
            }
        });
        match found {
            Some(id) => {
                resumed = true;
                id
            }
            None => {
                if name.is_empty() {
                    render::error("No previous session to continue");
                } else {
                    render::error(&format!("No session found matching '{}'", name));
                }
                return Ok(1);
            }
        }
    } else {
        let model_hint = opts.model.clone().unwrap_or_else(|| config.model());
        db.as_ref()
            .and_then(|d| d.create_session("cli", Some(&model_hint), cwd.to_str()).ok())
            .unwrap_or_else(SessionDb::new_session_id)
    };
    joey_core::logging::set_session_context(Some(&session_id));

    let overrides = Overrides {
        model: opts.model.clone(),
        provider: opts.provider.clone(),
        toolsets: opts.toolsets.clone(),
        max_turns: opts.max_turns,
        reasoning: None,
        pass_session_id: opts.pass_session_id,
    };

    let history = if resumed {
        db.as_ref().map(|d| restore_history(d, &session_id)).unwrap_or_default()
    } else {
        Vec::new()
    };
    let restored_count = history.len();

    let agent = build_agent(&config, &cwd, &overrides, &session_id, history)?;

    let ropts = RenderOptions {
        show_reasoning: config.get_bool("display.show_reasoning", true),
        tool_progress: config.get_str("display.tool_progress", "all"),
        quiet: opts.quiet,
    };

    let mut st = ReplState {
        config,
        cwd: cwd.clone(),
        overrides,
        agent,
        session_id,
        db,
        ropts,
        timestamps: false,
        last_response: String::new(),
        queued: Vec::new(),
        session_start: Instant::now(),
        checkpoints: None,
        last_auto_checkpoint: Instant::now(),
    };

    // Initialize session-scoped filesystem checkpoints (fresh every session).
    {
        let cp = joey_tools::vcs::CheckpointManager::new(&st.session_id, &st.cwd);
        if cp.is_enabled() {
            st.checkpoints = Some(cp);
        }
    }

    if !opts.quiet {
        let enabled = build_agent_config(&st.config, &st.overrides).enabled_tools;
        let ctx_len = st.config.get_i64("model.context_length", 0);
        render::banner(&render::BannerInfo {
            model: &build_agent_config(&st.config, &st.overrides).model,
            context_length: if ctx_len > 0 { Some(ctx_len) } else { None },
            cwd: &cwd.to_string_lossy(),
            session_id: &st.session_id,
            enabled_tools: &enabled,
            yolo: std::env::var("JOEY_YOLO_MODE").map(|v| v == "1").unwrap_or(false),
        });
        if resumed && restored_count > 0 {
            render::info(&format!(
                "Resumed session {} ({} messages).",
                &st.session_id[..8.min(st.session_id.len())],
                restored_count
            ));
        }
        if !st.agent.client().has_credentials() {
            render::info(&format!(
                "No API key found for provider '{}'. Set one with `joey model` or `joey config set <PROVIDER>_API_KEY <key>`.",
                st.agent.client().profile().name
            ));
        }
    }

    // Single-query mode (chat -q).
    if let Some(query) = &opts.query {
        let final_text = run_turn_interactive(&mut st, query).await;
        if opts.quiet {
            if !final_text.is_empty() {
                println!("{}", final_text);
            }
            println!();
            println!("Session: {}", st.session_id);
        }
        end_session(&st, "query_complete");
        return Ok(0);
    }

    // Batch mode: stdin is a pipe — process lines without the line editor.
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            match process_input(&line, &mut st).await {
                LoopOutcome::Quit => break,
                LoopOutcome::Continue => {}
            }
        }
        end_session(&st, "stdin_eof");
        return Ok(0);
    }

    // Line editor: persistent history + Alt+Enter newline insert.
    let history_path = joey_core::joey_home().join(".joey_history");
    let mut keybindings = default_emacs_keybindings();
    keybindings.add_binding(
        KeyModifiers::ALT,
        KeyCode::Enter,
        ReedlineEvent::Edit(vec![EditCommand::InsertNewline]),
    );
    let mut editor = Reedline::create().with_edit_mode(Box::new(Emacs::new(keybindings)));
    if let Ok(hist) = FileBackedHistory::with_file(1000, history_path) {
        editor = editor.with_history(Box::new(hist));
    }
    let prompt = JoeyPrompt;

    let mut last_idle_ctrlc: Option<Instant> = None;
    loop {
        let sig = editor.read_line(&prompt);
        let input = match sig {
            Ok(Signal::Success(line)) => line,
            Ok(Signal::CtrlC) => {
                // At idle, Ctrl-C clears the buffer (reedline already did);
                // a second press within 2s exits (upstream exits immediately
                // when the buffer is already empty — reedline can't observe
                // that, so a double-press stands in for it).
                let now = Instant::now();
                if last_idle_ctrlc.map(|t| now.duration_since(t).as_secs_f64() < 2.0).unwrap_or(false)
                {
                    break;
                }
                last_idle_ctrlc = Some(now);
                render::info("(press Ctrl-C again to exit)");
                continue;
            }
            Ok(Signal::CtrlD) => break,
            Err(e) => {
                render::error(&format!("input error: {}", e));
                break;
            }
        };
        last_idle_ctrlc = None;
        match process_input(&input, &mut st).await {
            LoopOutcome::Quit => break,
            LoopOutcome::Continue => {}
        }
    }

    end_session(&st, "user_exit");

    // Exit outro (cli.py:12690-12727).
    let history = st.agent.history();
    let user_msgs = history.iter().filter(|m| m.role == "user").count();
    let tool_calls =
        history.iter().filter(|m| m.role == "tool" || !m.tool_calls.is_empty()).count();
    let title = st
        .db
        .as_ref()
        .and_then(|d| d.get_session(&st.session_id).ok().flatten())
        .and_then(|s| s.title);
    render::exit_outro(&render::OutroInfo {
        session_id: &st.session_id,
        title,
        message_count: history.len(),
        user_messages: user_msgs,
        tool_calls,
        started: st.session_start,
        profile: crate::active_profile(),
    });
    Ok(0)
}

fn end_session(st: &ReplState, reason: &str) {
    if let Some(db) = st.agent.session_db() {
        let _ = db.end_session(&st.session_id, reason);
    }
    // Clean up the shadow repo on session end.
    if let Some(cp) = &st.checkpoints {
        cp.cleanup();
    }
}

enum LoopOutcome {
    Continue,
    Quit,
}

/// Handle one line of user input: slash command, or a chat turn with any
/// queued prompts prepended (upstream /queue drains into the next turn).
async fn process_input(raw: &str, st: &mut ReplState) -> LoopOutcome {
    let input = raw.trim().to_string();
    if input.is_empty() && st.queued.is_empty() {
        return LoopOutcome::Continue;
    }

    if input.starts_with('/') {
        return match handle_slash(&input, st).await {
            SlashOutcome::Quit => LoopOutcome::Quit,
            SlashOutcome::Continue => LoopOutcome::Continue,
        };
    }

    let mut turn_input = String::new();
    if !st.queued.is_empty() {
        turn_input = st.queued.join("\n");
        st.queued.clear();
    }
    if !input.is_empty() {
        if !turn_input.is_empty() {
            turn_input.push('\n');
        }
        turn_input.push_str(&input);
    }

    if !st.agent.client().has_credentials() {
        render::error(&format!(
            "no API key configured for provider '{}' — set one with `joey model` or `joey config set`.",
            st.agent.client().profile().name
        ));
        return LoopOutcome::Continue;
    }

    run_turn_interactive(st, &turn_input).await;
    println!();
    LoopOutcome::Continue
}

/// Run one agent turn with upstream Ctrl-C semantics: first press interrupts
/// the agent (it finishes the turn gracefully), a second within 2s
/// force-exits (cli.py:13704-13716).
async fn run_turn_interactive(st: &mut ReplState, input: &str) -> String {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let render_handle = tokio::spawn(render::render_turn(rx, st.ropts.clone()));
    let interrupt = st.agent.interrupt_handle();

    let mut last_ctrlc: Option<Instant> = None;
    {
        let turn = st.agent.run_turn(input, tx);
        tokio::pin!(turn);
        loop {
            tokio::select! {
                _res = &mut turn => break,
                sig = tokio::signal::ctrl_c() => {
                    if sig.is_err() { continue; }
                    let now = Instant::now();
                    if last_ctrlc.map(|t| now.duration_since(t).as_secs_f64() < 2.0).unwrap_or(false) {
                        println!("\n⚡ Force exiting...");
                        std::process::exit(0);
                    }
                    last_ctrlc = Some(now);
                    println!("\n⚡ Interrupting agent... (press Ctrl+C again to force exit)");
                    interrupt.store(true, Ordering::SeqCst);
                }
            }
        }
    }
    let final_text = render_handle.await.unwrap_or_default();
    st.last_response = final_text.clone();

    // Auto-checkpoint: take a filesystem snapshot after each agent turn if
    // enough time has passed.
    maybe_auto_checkpoint(st);

    final_text
}

// ---------------------------------------------------------------------------
// Slash commands (dispatch through the registry in slash.rs)
// ---------------------------------------------------------------------------

enum SlashOutcome {
    Continue,
    Quit,
}

async fn handle_slash(input: &str, st: &mut ReplState) -> SlashOutcome {
    let lower_full = input.to_lowercase();
    match slash::resolve(input) {
        Resolution::Unknown => {
            println!("{}", Color::Red.bold().paint(format!("Unknown command: {}", lower_full)));
            println!("{}", Color::DarkGray.paint("Type /help for available commands"));
            SlashOutcome::Continue
        }
        Resolution::Ambiguous(matches) => {
            println!("{}", Color::Cyan.paint(format!("Ambiguous command: {}", lower_full)));
            println!(
                "{}",
                Color::DarkGray.paint(format!("Did you mean: {}?", matches.join(", ")))
            );
            SlashOutcome::Continue
        }
        Resolution::Command { def, rest } => {
            if !def.implemented {
                println!("Command '/{}' is not available in joey-agent yet.", def.name);
                return SlashOutcome::Continue;
            }
            run_slash_command(def.name, rest.trim(), st).await
        }
    }
}

async fn run_slash_command(name: &str, args: &str, st: &mut ReplState) -> SlashOutcome {
    match name {
        "quit" => return SlashOutcome::Quit,
        "help" => print_help(),
        "new" => new_session(st, args, false),
        "clear" => {
            // Clear screen + scrollback, then start a new session.
            print!("\x1b[3J\x1b[2J\x1b[H");
            let _ = std::io::Write::flush(&mut std::io::stdout());
            new_session(st, "", true);
        }
        "queue" => {
            if args.is_empty() {
                if st.queued.is_empty() {
                    render::info("No prompts queued. Usage: /queue <prompt>");
                } else {
                    render::info(&format!("{} prompt(s) queued for the next turn:", st.queued.len()));
                    for q in &st.queued {
                        println!("  · {}", q);
                    }
                }
            } else {
                st.queued.push(args.to_string());
                render::success(&format!(
                    "Queued for the next turn ({} pending).",
                    st.queued.len()
                ));
            }
        }
        "model" => model_slash(st, args),
        "reasoning" => reasoning_slash(st, args),
        "tools" => {
            let cfg = build_agent_config(&st.config, &st.overrides);
            render::info(&format!("Enabled tools ({}):", cfg.enabled_tools.len()));
            println!("  {}", cfg.enabled_tools.join(", "));
            render::info("Manage per-platform tools with `joey tools enable|disable <name> --platform cli`.");
        }
        "toolsets" => {
            for ts in joey_tools::toolsets::names() {
                let desc = joey_tools::toolsets::description(ts).unwrap_or("");
                println!("  {:<14} {}", ts, desc);
            }
        }
        "skills" => {
            let skills = joey_tools::tools::skills_tool::discover();
            if skills.is_empty() {
                render::info("No skills installed.");
            } else {
                for s in skills {
                    println!("  {:<24} {}", s.name, s.description);
                }
            }
        }
        "history" => show_history(st),
        "sessions" => show_sessions(st),
        "resume" => {
            if args.is_empty() {
                render::info("Usage: /resume <session-id-or-title>");
            } else {
                resume_session(st, args);
            }
        }
        "config" => config_slash(st, args),
        "status" => show_status(st),
        "usage" => show_usage(st),
        "verbose" => {
            let next = match st.ropts.tool_progress.as_str() {
                "off" => "new",
                "new" => "all",
                "all" => "verbose",
                _ => "off",
            };
            st.ropts.tool_progress = next.to_string();
            render::info(&format!("Tool progress display: {}", next));
        }
        "timestamps" => {
            match args {
                "on" => st.timestamps = true,
                "off" => st.timestamps = false,
                "" | "status" => {
                    if args.is_empty() {
                        st.timestamps = !st.timestamps;
                    }
                }
                other => {
                    render::info(&format!("Usage: /timestamps [on|off|status] (got '{}')", other));
                    return SlashOutcome::Continue;
                }
            }
            render::info(&format!(
                "Timestamps: {}",
                if st.timestamps { "on" } else { "off" }
            ));
        }
        "version" => crate::commands::print_version_info(),
        "copy" => copy_last(st),
        "compress" => manual_compress(st, args).await,
        "checkpoint" => checkpoint_slash(st, args),
        "revert" | "rollback" => revert_slash(st, args),
        other => {
            // Registry says implemented but no handler — treat as unported.
            println!("Command '/{}' is not available in joey-agent yet.", other);
        }
    }
    SlashOutcome::Continue
}

fn print_help() {
    let mut by_cat: indexmap::IndexMap<&str, Vec<&slash::CommandDef>> = indexmap::IndexMap::new();
    for def in slash::REGISTRY.iter().filter(|d| d.implemented) {
        by_cat.entry(def.category).or_default().push(def);
    }
    println!();
    for (cat, defs) in by_cat {
        println!("{}", Color::Cyan.bold().paint(cat));
        for def in defs {
            let aliases = if def.aliases.is_empty() {
                String::new()
            } else {
                format!(
                    " (alias {})",
                    def.aliases.iter().map(|a| format!("/{}", a)).collect::<Vec<_>>().join(", ")
                )
            };
            let usage = if def.args_hint.is_empty() {
                format!("/{}", def.name)
            } else {
                format!("/{} {}", def.name, def.args_hint)
            };
            println!("  {:<28} {}{}", usage, def.description, Color::DarkGray.paint(aliases));
        }
        println!();
    }
    println!(
        "{}",
        Color::DarkGray.paint(
            "Other upstream commands are recognized but not yet ported — they answer with an honest notice."
        )
    );
}

fn new_session(st: &mut ReplState, name: &str, quiet: bool) {
    let model_hint = build_agent_config(&st.config, &st.overrides).model;
    let new_id = st
        .db
        .as_ref()
        .and_then(|d| d.create_session("cli", Some(&model_hint), st.cwd.to_str()).ok())
        .unwrap_or_else(SessionDb::new_session_id);
    if !name.is_empty() {
        if let Some(d) = &st.db {
            let _ = d.set_title(&new_id, name);
        }
    }
    end_session(st, "new_session");

    // Re-initialize checkpoint manager for the new session.
    st.session_id = new_id.clone();
    st.checkpoints = None;
    {
        let cp = joey_tools::vcs::CheckpointManager::new(&new_id, &st.cwd);
        if cp.is_enabled() {
            st.checkpoints = Some(cp);
        }
    }
    st.last_auto_checkpoint = Instant::now();

    match build_agent(&st.config, &st.cwd, &st.overrides, &new_id, Vec::new()) {
        Ok(agent) => {
            st.agent = agent;
            st.session_start = Instant::now();
            st.last_response.clear();
            joey_core::logging::set_session_context(Some(&new_id));
            if !quiet {
                if name.is_empty() {
                    render::success(&format!("Started a new session: {}", new_id));
                } else {
                    render::success(&format!("Started a new session '{}': {}", name, new_id));
                }
            }
        }
        Err(e) => render::error(&format!("failed to start a new session: {}", e)),
    }
}

fn rebuild_agent_preserving_history(st: &mut ReplState) -> Result<()> {
    let history = st.agent.history().to_vec();
    let agent = build_agent(&st.config, &st.cwd, &st.overrides, &st.session_id, history)?;
    st.agent = agent;
    Ok(())
}

fn model_slash(st: &mut ReplState, args: &str) {
    let mut parts: Vec<&str> = args.split_whitespace().collect();
    let global = parts.contains(&"--global");
    parts.retain(|p| *p != "--global" && *p != "--session");
    if parts.is_empty() {
        let cfg = build_agent_config(&st.config, &st.overrides);
        render::info(&format!(
            "Current model: {} (provider: {})",
            if cfg.model.is_empty() { "(not set)" } else { &cfg.model },
            st.agent.client().profile().name
        ));
        render::info("Set one with /model <name> [--global], or run `joey model`.");
        return;
    }
    let model = parts.join(" ");
    st.overrides.model = Some(model.clone());
    match rebuild_agent_preserving_history(st) {
        Ok(()) => {
            if global {
                if let Err(e) = st.config.set_and_save("model.default", &model) {
                    render::error(&format!("failed to persist model.default: {}", e));
                } else {
                    render::success(&format!(
                        "✓ Model set to {} and saved to {}",
                        model,
                        st.config.path().display()
                    ));
                    return;
                }
            }
            render::success(&format!("✓ Model set to {} for this session.", model));
        }
        Err(e) => render::error(&format!("failed to switch model: {}", e)),
    }
}

const REASONING_LEVELS: &[&str] =
    &["none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra", "on", "off"];

fn reasoning_slash(st: &mut ReplState, args: &str) {
    let mut parts: Vec<&str> = args.split_whitespace().collect();
    let global = parts.contains(&"--global");
    parts.retain(|p| *p != "--global");
    match parts.first().copied() {
        None => {
            let level = st
                .overrides
                .reasoning
                .clone()
                .unwrap_or_else(|| st.config.get_str("agent.reasoning_effort", "(default)"));
            render::info(&format!(
                "Reasoning effort: {} · display: {}",
                level,
                if st.ropts.show_reasoning { "shown" } else { "hidden" }
            ));
            render::info("Usage: /reasoning [level|show|hide] [--global]");
        }
        Some("show") => {
            st.ropts.show_reasoning = true;
            if global {
                let _ = st.config.set_and_save("display.show_reasoning", "true");
            }
            render::info("Reasoning display: shown");
        }
        Some("hide") => {
            st.ropts.show_reasoning = false;
            if global {
                let _ = st.config.set_and_save("display.show_reasoning", "false");
            }
            render::info("Reasoning display: hidden");
        }
        Some(level) if REASONING_LEVELS.contains(&level) => {
            st.overrides.reasoning = Some(level.to_string());
            match rebuild_agent_preserving_history(st) {
                Ok(()) => {
                    if global {
                        if let Err(e) = st.config.set_and_save("agent.reasoning_effort", level) {
                            render::error(&format!("failed to persist: {}", e));
                        }
                    }
                    render::success(&format!(
                        "✓ Reasoning effort set to {}{}",
                        level,
                        if global { " (saved)" } else { " for this session" }
                    ));
                }
                Err(e) => render::error(&format!("failed to apply reasoning level: {}", e)),
            }
        }
        Some(other) => {
            render::error(&format!(
                "Unknown reasoning option '{}'. Levels: {}",
                other,
                REASONING_LEVELS.join(", ")
            ));
        }
    }
}

fn resume_session(st: &mut ReplState, target: &str) {
    let Some(db) = &st.db else {
        render::error("session database unavailable");
        return;
    };
    let resolved =
        db.resolve_session_id(target).ok().flatten().or_else(|| find_by_title(db, target));
    let Some(id) = resolved else {
        render::error(&format!("No session found matching '{}'", target));
        return;
    };
    let history = restore_history(db, &id);
    let count = history.len();
    end_session(st, "switched_session");
    match build_agent(&st.config, &st.cwd, &st.overrides, &id, history) {
        Ok(agent) => {
            st.agent = agent;
            st.session_id = id.clone();
            joey_core::logging::set_session_context(Some(&id));
            render::success(&format!("Resumed session {} ({} messages).", id, count));
        }
        Err(e) => render::error(&format!("failed to resume: {}", e)),
    }
}

fn show_history(st: &ReplState) {
    let Some(db) = &st.db else {
        render::info("(no session database)");
        return;
    };
    let msgs = db.messages(&st.session_id).unwrap_or_default();
    if msgs.is_empty() {
        render::info("No messages in this session yet.");
        return;
    }
    let start = msgs.len().saturating_sub(30);
    for m in &msgs[start..] {
        let ts = if st.timestamps {
            let dt = chrono::DateTime::from_timestamp(m.timestamp as i64, 0)
                .map(|d| d.with_timezone(&chrono::Local).format("[%H:%M] ").to_string())
                .unwrap_or_default();
            dt
        } else {
            String::new()
        };
        let role = m.role.as_str();
        let mut content = m.content.replace('\n', " ");
        if content.chars().count() > 100 {
            content = format!("{}…", content.chars().take(100).collect::<String>());
        }
        let colored_role = match m.role {
            Role::User => Color::Cyan.paint(role).to_string(),
            Role::Assistant => Color::Green.paint(role).to_string(),
            _ => Color::DarkGray.paint(role).to_string(),
        };
        println!("  {}{:<12} {}", Color::DarkGray.paint(ts), colored_role, content);
    }
}

fn show_sessions(st: &ReplState) {
    let Some(db) = &st.db else {
        render::info("(no session database)");
        return;
    };
    let sessions = db.list_sessions(10).unwrap_or_default();
    if sessions.is_empty() {
        render::info("No sessions recorded.");
        return;
    }
    println!("Recent sessions (resume with /resume <id-or-title>):");
    for s in sessions {
        let when = chrono::DateTime::from_timestamp(s.started_at as i64, 0)
            .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        let title = s.title.clone().unwrap_or_default();
        let marker = if s.id == st.session_id { "*" } else { " " };
        println!(
            " {} {}  {}  {:>4} msgs  {}",
            marker,
            s.id,
            when,
            s.message_count,
            if title.is_empty() { Color::DarkGray.paint("(untitled)").to_string() } else { title }
        );
    }
}

fn config_slash(st: &mut ReplState, args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    match parts.split_first() {
        None => {
            let _ = crate::config_cmd::show_config();
        }
        Some((&"get", rest)) if !rest.is_empty() => {
            match crate::config_cmd::get_value_string(&st.config, rest[0]) {
                Some(v) => println!("{}", v),
                None => println!("Config key not set: {}", rest[0]),
            }
        }
        Some((&"set", rest)) if rest.len() >= 2 => {
            let value = rest[1..].join(" ");
            match st.config.set_and_save(rest[0], &value) {
                Ok(()) => render::success(&format!("✓ Set {} = {}", rest[0], value)),
                Err(e) => render::error(&e.to_string()),
            }
        }
        Some((&"path", _)) => println!("{}", st.config.path().display()),
        _ => render::info("Usage: /config [get <key> | set <key> <value> | path]"),
    }
}

fn show_status(st: &ReplState) {
    let cfg = build_agent_config(&st.config, &st.overrides);
    let session = st.db.as_ref().and_then(|d| d.get_session(&st.session_id).ok().flatten());
    println!("Session:   {}", st.session_id);
    if let Some(title) = session.as_ref().and_then(|s| s.title.clone()) {
        println!("Title:     {}", title);
    }
    println!(
        "Model:     {} (provider: {})",
        if cfg.model.is_empty() { "(not set)" } else { &cfg.model },
        st.agent.client().profile().name
    );
    println!("Messages:  {}", st.agent.history().len());
    if let Some(s) = &session {
        println!("Recorded:  {} messages, {} tool calls", s.message_count, s.tool_call_count);
    }
    println!("Cwd:       {}", st.cwd.display());
    // Context usage from the live compressor (upstream status surfaces read
    // context_compressor.last_prompt_tokens/context_length/compression_count;
    // the -1 "awaiting real usage" sentinel clamps to 0).
    let comp = st.agent.compressor();
    let last_prompt = comp.last_prompt_tokens.max(0);
    let ctx_len = comp.context_length;
    let pct = if ctx_len > 0 {
        (last_prompt as f64 / ctx_len as f64 * 100.0).min(100.0)
    } else {
        0.0
    };
    println!(
        "Context:   {} / {} tokens ({:.0}%), threshold {} · compressions: {}",
        commafy_i64(last_prompt),
        commafy_i64(ctx_len),
        pct,
        commafy_i64(comp.threshold_tokens),
        comp.compression_count
    );
    let elapsed = st.session_start.elapsed().as_secs();
    println!("Uptime:    {}m {}s", elapsed / 60, elapsed % 60);
    println!(
        "Display:   streaming={} reasoning={} tool_progress={}",
        interactive_streaming(&st.config),
        if st.ropts.show_reasoning { "shown" } else { "hidden" },
        st.ropts.tool_progress
    );
}

fn show_usage(st: &ReplState) {
    let Some(db) = &st.db else {
        render::info("(no session database)");
        return;
    };
    let msgs = db.messages(&st.session_id).unwrap_or_default();
    let mut total: i64 = 0;
    let mut assistant_tokens: i64 = 0;
    let mut counted = 0usize;
    for m in &msgs {
        if let Some(t) = m.token_count {
            total += t;
            counted += 1;
            if matches!(m.role, Role::Assistant) {
                assistant_tokens += t;
            }
        }
    }
    println!("Session token usage (from the session store):");
    println!("  Messages recorded:  {}", msgs.len());
    println!("  With token counts:  {}", counted);
    println!("  Total tokens:       {}", total);
    println!("  Assistant tokens:   {}", assistant_tokens);
    // Upstream `/usage` context block (cli.py `_show_usage`).
    let comp = st.agent.compressor();
    let last_prompt = comp.last_prompt_tokens.max(0);
    let ctx_len = comp.context_length;
    let pct = if ctx_len > 0 {
        (last_prompt as f64 / ctx_len as f64 * 100.0).min(100.0)
    } else {
        0.0
    };
    println!("  {}", "─".repeat(40));
    println!(
        "  Current context:  {} / {} ({:.0}%)",
        commafy_i64(last_prompt),
        commafy_i64(ctx_len),
        pct
    );
    println!("  Messages:         {}", st.agent.history().len());
    println!("  Compressions:     {}", comp.compression_count);
}

/// Manual context compression (`/compress` / `/compact` — cli.py
/// `_manual_compress`). `force=true` bypasses the summary-failure cooldown;
/// a non-flag argument tail is the focus topic (Claude Code's `/compact
/// <focus>` shape). `here [N]` / `--preview` / `--aggressive` are recognized
/// but not ported.
async fn manual_compress(st: &mut ReplState, args: &str) {
    if st.agent.history().len() < 4 {
        println!("(._.) Not enough conversation to compress (need at least 4 messages).");
        return;
    }
    if !st.agent.compression_enabled() {
        println!("(._.) Compression is disabled in config.");
        return;
    }

    let mut raw_args = args.trim().to_string();
    // Strip the flag forms first, then parse positionals (partial_compress
    // `extract_compress_flags` shape).
    let mut preview = false;
    let mut aggressive = false;
    let mut kept: Vec<&str> = Vec::new();
    for tok in raw_args.split_whitespace() {
        match tok {
            "--preview" | "--dry-run" => preview = true,
            "--aggressive" => aggressive = true,
            other => kept.push(other),
        }
    }
    raw_args = kept.join(" ");

    if aggressive {
        println!(
            "(._.) --aggressive is not supported; use '/compress here [N]' to keep only recent \
             exchanges, or /undo to drop turns."
        );
        if !preview {
            return;
        }
    }
    if preview {
        println!("Compression preview (--preview) is not available in joey-agent yet.");
        return;
    }
    let mut words = raw_args.split_whitespace();
    if words.next().map(|w| w.eq_ignore_ascii_case("here")).unwrap_or(false) {
        println!("'/compress here [N]' is not available in joey-agent yet.");
        return;
    }
    let focus_topic = if raw_args.is_empty() { None } else { Some(raw_args.as_str()) };

    let original_count = st.agent.history().len();
    let approx_tokens = st.agent.request_tokens_estimate();
    if let Some(topic) = focus_topic {
        println!(
            "🗜️  Compressing {} messages (~{} tokens), focus: \"{}\"...",
            original_count,
            commafy_i64(approx_tokens),
            topic
        );
    } else {
        println!(
            "🗜️  Compressing {} messages (~{} tokens)...",
            original_count,
            commafy_i64(approx_tokens)
        );
    }

    // Drain compression notices (compaction status, warnings) to the console.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let summary = st.agent.manual_compress(focus_topic, Some(&tx)).await;
    drop(tx);
    while let Ok(ev) = rx.try_recv() {
        if let AgentEvent::Notice(text) = ev {
            println!("  {}", text);
        }
    }

    let icon = if summary.aborted || summary.fallback_used {
        "⚠️"
    } else if summary.noop {
        "🗜️"
    } else {
        "✅"
    };
    println!("  {} {}", icon, summary.headline);
    println!("     {}", summary.token_line);
    if let Some(note) = &summary.note {
        println!("     {}", note);
    }
}

/// `{:,}` thousands separators.
fn commafy_i64(n: i64) -> String {
    let neg = n < 0;
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if neg { format!("-{}", out) } else { out }
}

fn copy_last(st: &ReplState) {
    if st.last_response.is_empty() {
        render::info("Nothing to copy yet.");
        return;
    }
    let candidates: &[(&str, &[&str])] = &[
        ("pbcopy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("wl-copy", &[]),
    ];
    for (cmd, args) in candidates {
        if which::which(cmd).is_err() {
            continue;
        }
        let mut child = match std::process::Command::new(cmd)
            .args(*args)
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Some(stdin) = child.stdin.as_mut() {
            use std::io::Write;
            let _ = stdin.write_all(st.last_response.as_bytes());
        }
        let _ = child.wait();
        render::success("✓ Copied last response to clipboard.");
        return;
    }
    render::error("no clipboard command found (pbcopy/xclip/wl-copy)");
}

// ---------------------------------------------------------------------------
// Filesystem checkpoint commands (/checkpoint, /revert, /rollback)
// ---------------------------------------------------------------------------

/// Take an automatic checkpoint if the interval has elapsed.
fn maybe_auto_checkpoint(st: &mut ReplState) {
    let elapsed = st.last_auto_checkpoint.elapsed().as_secs();
    if elapsed < AUTO_CHECKPOINT_INTERVAL_SECS {
        return;
    }
    if let Some(cp) = &mut st.checkpoints {
        if let Some(num) = cp.checkpoint("Auto-checkpoint") {
            render::checkpoint_created(num, "Auto-checkpoint (periodic)");
            st.last_auto_checkpoint = Instant::now();
        }
    }
}

/// `/checkpoint [message]` — create a named filesystem checkpoint.
fn checkpoint_slash(st: &mut ReplState, args: &str) {
    let Some(cp) = &mut st.checkpoints else {
        render::info("Filesystem checkpoints are not available (git not found or init failed).");
        return;
    };
    let message = if args.trim().is_empty() {
        "Manual checkpoint"
    } else {
        args.trim()
    };
    if let Some(num) = cp.checkpoint(message) {
        render::checkpoint_created(num, message);
    } else {
        render::info("No filesystem changes since the last checkpoint.");
    }
}

/// `/revert <number>` or `/rollback [number]` — revert filesystem to checkpoint.
fn revert_slash(st: &mut ReplState, args: &str) {
    let Some(cp) = &st.checkpoints else {
        render::info("Filesystem checkpoints are not available.");
        return;
    };

    let checkpoints = match cp.list() {
        Ok(list) => list,
        Err(e) => {
            render::error(&format!("failed to list checkpoints: {}", e));
            return;
        }
    };

    // No argument → list all checkpoints.
    if args.trim().is_empty() {
        render::checkpoint_list(&checkpoints);
        return;
    }

    // Parse the checkpoint number.
    let target_num: usize = match args.trim().parse() {
        Ok(n) => n,
        Err(_) => {
            render::error("Usage: /revert <number> (use /revert with no args to list)");
            return;
        }
    };

    // Find the checkpoint.
    if !checkpoints.iter().any(|c| c.number == target_num) {
        render::error(&format!("Checkpoint #{} not found", target_num));
        return;
    }

    // Confirm the revert.
    let target = checkpoints.iter().find(|c| c.number == target_num).unwrap();
    render::info(&format!(
        "Reverting to checkpoint #{}: \"{}\" ({} files)",
        target_num, target.message, target.files_changed
    ));

    match cp.revert(target_num) {
        Ok(()) => {
            render::checkpoint_reverted(target_num);
            // Update auto-checkpoint timer so we don't immediately snapshot
            // the reverted state.
            st.last_auto_checkpoint = Instant::now();
        }
        Err(e) => {
            render::error(&format!("revert failed: {}", e));
        }
    }
}

