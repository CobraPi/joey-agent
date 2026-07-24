//! Slash-command registry + resolution (port of `hermes_cli/commands.py`
//! `COMMAND_REGISTRY` and the prefix-expansion dispatch in cli.py:9326-9364).
//!
//! Every CLI-visible upstream command is REGISTERED here (so `/compress`
//! answers honestly instead of "unknown"), but only the `implemented` subset
//! has handlers in `repl.rs`.

/// One slash command (commands.py `CommandDef`, gateway fields dropped).
pub struct CommandDef {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub category: &'static str,
    pub args_hint: &'static str,
    /// Whether joey-cli has a handler for it.
    pub implemented: bool,
}

macro_rules! cmd {
    ($name:expr, $aliases:expr, $desc:expr, $cat:expr, $hint:expr, $impl_:expr) => {
        CommandDef {
            name: $name,
            aliases: $aliases,
            description: $desc,
            category: $cat,
            args_hint: $hint,
            implemented: $impl_,
        }
    };
}

/// The registry: all non-gateway-only upstream commands (commands.py:64-256),
/// order preserved.
pub static REGISTRY: &[CommandDef] = &[
    // Session
    cmd!("new", &["reset"], "Start a new session (fresh session ID + history)", "Session", "[name]", true),
    cmd!("clear", &[], "Clear screen and start a new session", "Session", "", true),
    cmd!("redraw", &[], "Force a full UI repaint (recovers from terminal drift)", "Session", "", false),
    cmd!("history", &[], "Show conversation history", "Session", "", true),
    cmd!("save", &[], "Save the current conversation", "Session", "", false),
    cmd!("retry", &[], "Retry the last message (resend to agent)", "Session", "", false),
    cmd!("prompt", &["compose"], "Compose your next prompt in $EDITOR (markdown), then send it", "Session", "[initial text]", false),
    cmd!("undo", &[], "Back up N user turns and re-prompt (default 1)", "Session", "[N]", false),
    cmd!("title", &[], "Set a title for the current session", "Session", "[name]", false),
    cmd!("handoff", &[], "Hand off this session to a messaging platform (Telegram, Discord, etc.)", "Session", "<platform>", false),
    cmd!("branch", &["fork"], "Branch the current session (explore a different path)", "Session", "[name]", false),
    cmd!("compress", &["compact"], "Compress conversation context (add 'here [N]' to keep recent N turns; --preview shows what would happen)", "Session", "[here [N] | focus topic | --preview|--dry-run]", true),
    cmd!("rollback", &[], "List or restore filesystem checkpoints", "Session", "[number]", true),
    cmd!("checkpoint", &["snap"], "Create a filesystem checkpoint (snapshot of all tracked files)", "Session", "[message]", true),
    cmd!("snapshot", &[], "Create or restore state snapshots of Joey config/state", "Session", "[create|restore <id>|prune]", false),
    cmd!("stop", &[], "Kill all running background processes", "Session", "", false),
    cmd!("background", &["bg", "btw"], "Run a prompt in the background", "Session", "<prompt>", false),
    cmd!("agents", &["tasks"], "Show active agents, running tasks, and OMO agent registry", "Session", "", true),
    cmd!("journey", &["learning", "memory-graph"], "Open the learning journey timeline", "Session", "[list|delete <id>|edit <id>]", false),
    cmd!("start-work", &[], "Activate Atlas on a plan (reads .omo/plans/{name}.md, auto-resumes if 1 active work)", "Session", "[plan-name]", true),
    cmd!("queue", &["q"], "Queue a prompt for the next turn (doesn't interrupt)", "Session", "<prompt>", true),
    cmd!("steer", &[], "Inject a message after the next tool call without interrupting", "Session", "<prompt>", false),
    cmd!("goal", &[], "Set a standing goal Joey works on across turns until achieved", "Session", "[set <text> | pause | resume | clear | show]", true),
    cmd!("moa", &[], "Run one prompt through the default Mixture of Agents preset", "Session", "<prompt>", false),
    cmd!("subgoal", &[], "Add or manage extra criteria on the active goal", "Session", "[text | remove N | clear]", false),
    cmd!("status", &[], "Show session, model, token, and context info", "Session", "", true),
    cmd!("whoami", &[], "Show your slash command access (admin / user)", "Info", "", false),
    cmd!("profile", &[], "Show active profile name and home directory", "Info", "", false),
    cmd!("resume", &[], "Resume a previously-named session", "Session", "[name]", true),
    cmd!("sessions", &[], "Browse and resume previous sessions", "Session", "", true),
    // Configuration
    cmd!("config", &[], "Show current configuration", "Configuration", "", true),
    cmd!("model", &[], "Switch model (session-scoped; --global to persist)", "Configuration", "[model] [--global]", true),
    cmd!("codex-runtime", &["codex_runtime"], "Toggle codex app-server runtime for OpenAI/Codex models", "Configuration", "[auto|codex_app_server]", false),
    cmd!("personality", &[], "Set a predefined personality", "Configuration", "[name]", false),
    cmd!("statusbar", &["sb"], "Toggle the context/model status bar", "Configuration", "", false),
    cmd!("timestamps", &["ts"], "Toggle [HH:MM] timestamps on messages and /history", "Configuration", "[on|off|status]", true),
    cmd!("verbose", &[], "Cycle tool progress display: off -> new -> all -> verbose", "Configuration", "", true),
    cmd!("footer", &[], "Toggle gateway runtime-metadata footer on final replies", "Configuration", "[on|off|status]", false),
    cmd!("yolo", &[], "Toggle YOLO mode (skip all dangerous command approvals)", "Configuration", "", false),
    cmd!("reasoning", &[], "Manage reasoning effort and display", "Configuration", "[level|show|hide] [--global]", true),
    cmd!("fast", &[], "Toggle fast mode (Normal/Fast)", "Configuration", "[normal|fast|status] [--global]", false),
    cmd!("skin", &[], "Show or change the display skin/theme", "Configuration", "[name]", false),
    cmd!("indicator", &[], "Pick the TUI busy-indicator style", "Configuration", "[kaomoji|emoji|unicode|ascii]", false),
    cmd!("voice", &[], "Toggle voice mode", "Configuration", "[on|off|tts|status]", false),
    cmd!("busy", &[], "Control what Enter does while Joey is working", "Configuration", "[queue|steer|interrupt|status]", false),
    // Tools & Skills
    cmd!("tools", &[], "Manage tools: /tools [list|disable|enable] [name...]", "Tools & Skills", "[list|disable|enable] [name...]", true),
    cmd!("toolsets", &[], "List available toolsets", "Tools & Skills", "", true),
    cmd!("skills", &[], "Search, install, inspect, or manage skills", "Tools & Skills", "", true),
    cmd!("memory", &[], "Review pending memory writes / toggle the approval gate", "Tools & Skills", "[pending|approve|reject|approval] [id|on|off]", false),
    cmd!("bundles", &[], "List skill bundles (aliases /<name> for multiple skills)", "Tools & Skills", "", false),
    cmd!("pet", &[], "Toggle or adopt a petdex mascot", "Tools & Skills", "[toggle|list|scale <n>|<slug>]", false),
    cmd!("hatch", &["generate-pet"], "Generate a new petdex pet from a description", "Tools & Skills", "[description]", false),
    cmd!("learn", &[], "Learn a reusable skill from anything you describe", "Tools & Skills", "<what to learn from>", false),
    cmd!("cron", &[], "Manage scheduled tasks", "Tools & Skills", "[subcommand]", false),
    cmd!("suggestions", &["suggest"], "Review suggested automations (accept/dismiss)", "Tools & Skills", "[accept|dismiss N | catalog]", false),
    cmd!("blueprint", &["bp"], "Set up an automation from a blueprint template", "Tools & Skills", "[name] [slot=value ...]", false),
    cmd!("curator", &[], "Background skill maintenance", "Tools & Skills", "[subcommand]", false),
    cmd!("kanban", &[], "Multi-profile collaboration board (tasks, links, comments)", "Tools & Skills", "[subcommand]", false),
    cmd!("reload", &[], "Reload .env variables into the running session", "Tools & Skills", "", false),
    cmd!("reload-mcp", &["reload_mcp"], "Reload MCP servers from config", "Tools & Skills", "", false),
    cmd!("reload-skills", &["reload_skills"], "Re-scan ~/.joey/skills/ for newly installed or removed skills", "Tools & Skills", "", false),
    cmd!("browser", &[], "Connect browser tools to your live Chromium-family browser via CDP", "Tools & Skills", "[connect|disconnect|status]", false),
    cmd!("plugins", &[], "List installed plugins and their status", "Tools & Skills", "", false),
    // Info
    cmd!("help", &[], "Show available commands", "Info", "", true),
    cmd!("usage", &[], "Show token usage for this session", "Info", "", true),
    cmd!("subscription", &["upgrade"], "View your plan and change it in the browser", "Info", "", false),
    cmd!("topup", &[], "Show your balance and manage billing on the portal", "Info", "", false),
    cmd!("insights", &[], "Show usage insights and analytics", "Info", "[days]", false),
    cmd!("platforms", &["gateway"], "Show gateway/messaging platform status", "Info", "", false),
    cmd!("copy", &[], "Copy the last assistant response to clipboard", "Info", "[number]", true),
    cmd!("paste", &[], "Attach clipboard image from your clipboard", "Info", "", false),
    cmd!("image", &[], "Attach a local image file for your next prompt", "Info", "<path>", false),
    cmd!("update", &[], "Update Joey Agent to the latest version", "Info", "", false),
    cmd!("version", &["v"], "Show Joey Agent version", "Info", "", true),
    cmd!("debug", &[], "Upload debug report (system info + logs)", "Info", "[nous|local]", false),
    // Exit
    cmd!("quit", &["exit"], "Exit the CLI", "Exit", "", true),
];

/// Exact name/alias lookup (no slash).
pub fn lookup(name: &str) -> Option<&'static CommandDef> {
    let name = name.to_lowercase();
    REGISTRY
        .iter()
        .find(|c| c.name == name || c.aliases.contains(&name.as_str()))
}

/// All "/name" strings, including aliases (upstream `COMMANDS` keys).
pub fn all_slash_names() -> Vec<String> {
    let mut out = Vec::new();
    for c in REGISTRY {
        out.push(format!("/{}", c.name));
        for a in c.aliases {
            out.push(format!("/{}", a));
        }
    }
    out
}

/// The outcome of resolving typed input against the registry.
pub enum Resolution {
    /// A command matched (exactly or via unique prefix). `rest` preserves the
    /// argument tail exactly as typed.
    Command { def: &'static CommandDef, rest: String },
    /// `Ambiguous command: … / Did you mean: …` (sorted slash-names).
    Ambiguous(Vec<String>),
    /// `Unknown command: … / Type /help for available commands`.
    Unknown,
}

/// Resolve `/cmd args…` with upstream prefix expansion (cli.py:9326-9364):
/// exact name/alias first; then unique prefix; then unique-shortest prefix;
/// ambiguous otherwise.
pub fn resolve(input: &str) -> Resolution {
    let trimmed = input.trim();
    let lower = trimmed.to_lowercase();
    let typed_base = lower.split_whitespace().next().unwrap_or("");
    let Some(base_name) = typed_base.strip_prefix('/') else {
        return Resolution::Unknown;
    };
    let rest = trimmed
        .split_whitespace()
        .next()
        .map(|first| trimmed[first.len()..].to_string())
        .unwrap_or_default();

    if let Some(def) = lookup(base_name) {
        return Resolution::Command { def, rest };
    }

    let all = all_slash_names();
    let mut matches: Vec<String> = all.into_iter().filter(|c| c.starts_with(typed_base)).collect();
    if matches.len() > 1 {
        // Prefer the unique shortest match (/qui → /quit over /quintuple).
        let min_len = matches.iter().map(|c| c.len()).min().unwrap_or(0);
        let shortest: Vec<String> = matches.iter().filter(|c| c.len() == min_len).cloned().collect();
        if shortest.len() == 1 {
            matches = shortest;
        }
    }
    match matches.len() {
        1 => {
            let full = matches[0].trim_start_matches('/');
            match lookup(full) {
                Some(def) => Resolution::Command { def, rest },
                None => Resolution::Unknown,
            }
        }
        0 => Resolution::Unknown,
        _ => {
            matches.sort();
            matches.dedup();
            Resolution::Ambiguous(matches)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q_is_queue_not_quit() {
        // commands.py:110-111 — /q is the alias of /queue.
        let def = lookup("q").unwrap();
        assert_eq!(def.name, "queue");
        // /quit aliases /exit only.
        assert_eq!(lookup("exit").unwrap().name, "quit");
    }

    #[test]
    fn reset_is_alias_of_new() {
        // commands.py:68-69 — "new" with alias ("reset",).
        assert_eq!(lookup("reset").unwrap().name, "new");
    }

    #[test]
    fn exact_match_wins() {
        match resolve("/help") {
            Resolution::Command { def, .. } => assert_eq!(def.name, "help"),
            _ => panic!("expected /help to resolve"),
        }
    }

    #[test]
    fn unique_prefix_expands_and_preserves_args() {
        match resolve("/hel me now") {
            Resolution::Command { def, rest } => {
                assert_eq!(def.name, "help");
                assert_eq!(rest, " me now");
            }
            _ => panic!("expected /hel to expand to /help"),
        }
    }

    #[test]
    fn qui_prefers_unique_shortest() {
        // /qui matches only /quit (queue is /queue but /qui isn't its prefix).
        match resolve("/qui") {
            Resolution::Command { def, .. } => assert_eq!(def.name, "quit"),
            _ => panic!("expected /qui → /quit"),
        }
    }

    #[test]
    fn ambiguous_prefix_reports_candidates() {
        match resolve("/re") {
            Resolution::Ambiguous(matches) => {
                assert!(matches.contains(&"/reset".to_string()));
                assert!(matches.contains(&"/reasoning".to_string()));
            }
            _ => panic!("expected /re to be ambiguous"),
        }
    }

    #[test]
    fn unknown_command() {
        assert!(matches!(resolve("/zzzz"), Resolution::Unknown));
    }

    #[test]
    fn unimplemented_commands_are_recognized() {
        let def = lookup("handoff").unwrap();
        assert!(!def.implemented);
        // /compress is implemented and /compact aliases it (commands.py:91-92).
        let def = lookup("compact").unwrap();
        assert_eq!(def.name, "compress");
        assert!(def.implemented);
    }
}
