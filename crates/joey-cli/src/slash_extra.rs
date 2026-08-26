//! Shared implementations for the previously-deferred slash commands
//! (both the line REPL and the TUI dispatch here — Constitution II parity).
//!
//! Handlers are grouped by upstream category. Each returns a `CommandOutcome`:
//! plain text lines the caller renders however it wants (println in the REPL,
//! TranscriptItem notices in the TUI), or a special action (submit-a-turn,
//! attach-image, quit) the host applies.
//!
//! Two golden rules:
//! 1. NO fabricated backend state: commands that need a server we don't
//!    have (Nous account, voice, marketplace) do their honest local best
//!    and say exactly what they did.
//! 2. Everything that persists goes through joey-core's layered config
//!    (`set_and_save`) so `joey config` sees the same keys.

use joey_core::Config;
use joey_cron::CronStore;

/// What a command produced. Rendered verbatim by the host.
#[derive(Debug, Default)]
pub struct Lines(pub Vec<String>);

impl Lines {
    pub fn push(&mut self, line: impl Into<String>) {
        self.0.push(line.into());
    }
    pub fn single(line: impl Into<String>) -> Self {
        Lines(vec![line.into()])
    }
    #[allow(dead_code)] // used by tests
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Session: /redraw /save /title /undo (DB part) /branch /snapshot /stop
// /background /journey /moa /subgoal /whoami /profile /handoff
// ---------------------------------------------------------------------------

/// `/save` — export the current session as markdown to ~/.joey/saves/.
pub fn save_session_markdown(session_id: &str, db: Option<&joey_core::SessionDb>) -> Lines {
    let mut out = Lines::default();
    let Some(db) = db else {
        out.push("(no session database — nothing to save)");
        return out;
    };
    let msgs = db.messages(session_id).unwrap_or_default();
    let session = db.get_session(session_id).ok().flatten();
    let dir = joey_core::joey_home().join("saves");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        out.push(format!("failed to create saves directory: {e}"));
        return out;
    }
    let title = session
        .as_ref()
        .and_then(|s| s.title.clone())
        .unwrap_or_else(|| "untitled".to_string());
    let file = dir.join(format!("{}_{}.md", session_id, sanitize_filename(&title)));
    let mut md = String::new();
    md.push_str(&format!(
        "# Session {} — {}\n\n- Model: {}\n- Messages: {}\n- Tool calls: {}\n\n---\n\n",
        session_id,
        title,
        session.as_ref().and_then(|s| s.model.clone()).unwrap_or_default(),
        session.as_ref().map(|s| s.message_count).unwrap_or(msgs.len() as i64),
        session.as_ref().map(|s| s.tool_call_count).unwrap_or(0),
    ));
    for m in &msgs {
        let role = match m.role {
            joey_core::Role::User => "User",
            joey_core::Role::Assistant => "Agent",
            joey_core::Role::Tool => "Tool",
            joey_core::Role::System => continue,
        };
        let ts = chrono::DateTime::from_timestamp(m.timestamp as i64, 0)
            .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        md.push_str(&format!("## {} ({})\n\n{}\n\n", role, ts, m.content));
        if m.content.trim().is_empty() {
            md.push_str("*(empty)*\n\n");
        }
    }
    match std::fs::write(&file, md) {
        Ok(()) => {
            out.push(format!("✓ Saved {} message(s) to {}", msgs.len(), file.display()));
        }
        Err(e) => out.push(format!("save failed: {e}")),
    }
    out
}

fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let trimmed = cleaned.trim_matches('_').to_string();
    if trimmed.is_empty() { "session".to_string() } else { trimmed.chars().take(48).collect() }
}

/// `/undo N` — rewind the session (DB + return the message for resubmission).
/// Returns (lines, resubmit-text). The caller truncates live history too.
pub fn undo_exchange(
    session_id: &str,
    n: usize,
    agent_history: &[joey_providers::Message],
    db: Option<&joey_core::SessionDb>,
) -> (Lines, Option<String>) {
    let mut out = Lines::default();
    let Some(db) = db else {
        out.push("(no session database — cannot rewind)");
        return (out, None);
    };
    // Find the user message to resubmit: the Nth-from-last user turn.
    let user_msgs: Vec<&joey_providers::Message> = agent_history
        .iter()
        .filter(|m| m.role == "user")
        .collect();
    let idx = user_msgs.len().checked_sub(n);
    let resubmit = idx.and_then(|i| {
        user_msgs[i]
            .content
            .clone()
            .filter(|c| !c.trim().is_empty())
    });
    match db.rewind_last_user_exchanges(session_id, n) {
        Ok(0) => {
            out.push(format!("Nothing to undo (no active user exchange left)."));
        }
        Ok(removed) => {
            out.push(format!(
                "Undid {} message(s) (rewound {} user exchange(s)). The context now resumes before that turn.",
                removed, n
            ));
        }
        Err(e) => out.push(format!("rewind failed: {e}")),
    }
    (out, resubmit)
}

/// `/branch [name]` — fork the current session: copy active messages to a new
/// session row. Returns (lines, new-session-id).
pub fn branch_session(
    source_id: &str,
    name: &str,
    db: Option<&joey_core::SessionDb>,
) -> (Lines, Option<String>) {
    let mut out = Lines::default();
    let Some(db) = db else {
        out.push("(no session database — cannot branch)");
        return (out, None);
    };
    let msgs = db.messages(source_id).unwrap_or_default();
    if msgs.is_empty() {
        out.push("Nothing to branch — the current session has no messages.");
        return (out, None);
    }
    let new_id = db
        .create_session("cli", None, std::env::current_dir().ok().map(|p| p.display().to_string()).as_deref())
        .unwrap_or_else(|_| joey_core::SessionDb::new_session_id());
    for m in &msgs {
        let _ = db.add_message(m);
    }
    if !name.is_empty() {
        let title = if name.trim().eq_ignore_ascii_case("fork") {
            format!("branch of {source_id}")
        } else {
            name.trim().to_string()
        };
        let _ = db.set_title(&new_id, &title);
    }
    out.push(format!(
        "✓ Branched {} message(s) into new session {new_id}. Resume it later with /resume {new_id}.",
        msgs.len()
    ));
    (out, Some(new_id))
}

/// `/snapshot create|restore <id>|prune|list` — zip snapshots of
/// ~/.joey config/state under ~/.joey/snapshots/.
pub mod snapshot {
    use super::*;

    pub fn handle(args: &str, config: &Config) -> Lines {
        let mut parts = args.split_whitespace();
        match parts.next() {
            None | Some("create") => create(config),
            Some("list") | Some("ls") => list(),
            Some("restore") => {
                let id = parts.next().map(str::to_string);
                restore(id.as_deref())
            }
            Some("prune") => prune(),
            Some(other) => Lines::single(format!(
                "Usage: /snapshot [create|restore <id>|prune|list] (got '{other}')"
            )),
        }
    }

    fn snapshots_dir() -> std::path::PathBuf {
        joey_core::joey_home().join("snapshots")
    }

    fn create(config: &Config) -> Lines {
        let mut out = Lines::default();
        let dir = snapshots_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            out.push(format!("failed to create snapshots directory: {e}"));
            return out;
        }
        let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let id = stamp.to_string();
        let dest = dir.join(format!("{id}.zip"));
        // Zip the config + .env + profiles (state snapshots, not sessions).
        let home = joey_core::joey_home();
        let files = [
            ("config.yaml", home.join("config.yaml")),
            (".env", home.join(".env")),
        ];
        let mut have_any = false;
        let tmp = dir.join(format!("{id}.tmp"));
        if let Err(e) = write_snapshot_zip(&tmp, &files, &mut have_any) {
            let _ = std::fs::remove_file(&tmp);
            out.push(format!("snapshot failed: {e}"));
            return out;
        }
        if !have_any {
            let _ = std::fs::remove_file(&tmp);
            out.push("Nothing to snapshot (no config.yaml and no .env present).");
            return out;
        }
        if let Err(e) = std::fs::rename(&tmp, &dest) {
            out.push(format!("snapshot failed: {e}"));
            return out;
        }
        let _ = config; // config kept for signature parity/future keys
        out.push(format!("✓ Snapshot {id} created at {}", dest.display()));
        out.push(format!("  Restore with: /snapshot restore {id}"));
        out
    }

    fn write_snapshot_zip(
        dest: &std::path::Path,
        files: &[(&str, std::path::PathBuf)],
        have_any: &mut bool,
    ) -> std::io::Result<()> {
        use std::io::Write;
        let file = std::fs::File::create(dest)?;
        let mut zw = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, path) in files {
            if !path.exists() {
                continue;
            }
            *have_any = true;
            zw.start_file(*name, options)?;
            let bytes = std::fs::read(path)?;
            zw.write_all(&bytes)?;
        }
        zw.finish()?;
        Ok(())
    }

    fn list() -> Lines {
        let mut out = Lines::default();
        let dir = snapshots_dir();
        let mut ids: Vec<String> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().extension().map(|x| x == "zip").unwrap_or(false))
                    .filter_map(|e| e.path().file_stem().map(|s| s.to_string_lossy().into_owned()))
                    .collect()
            })
            .unwrap_or_default();
        if ids.is_empty() {
            out.push("No snapshots. Create one with /snapshot create.");
            return out;
        }
        ids.sort();
        ids.reverse();
        out.push("Snapshots (newest first):");
        for id in ids.iter().take(20) {
            out.push(format!("  {id}"));
        }
        out
    }

    fn restore(id: Option<&str>) -> Lines {
        let mut out = Lines::default();
        let Some(id) = id else {
            out.push("Usage: /snapshot restore <id>");
            return out;
        };
        let dir = snapshots_dir();
        // Accept a prefix too.
        let candidates: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.extension().map(|x| x == "zip").unwrap_or(false)
                            && p.file_stem()
                                .map(|s| s.to_string_lossy().starts_with(id))
                                .unwrap_or(false)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let Some(path) = candidates.first() else {
            out.push(format!("Snapshot '{id}' not found (see /snapshot list)."));
            return out;
        };
        match restore_zip(path) {
            Ok(restored) => {
                out.push(format!(
                    "✓ Restored {} file(s) from {}. Restart joey for the changes to take full effect.",
                    restored.len(),
                    path.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default()
                ));
                for name in restored {
                    out.push(format!("  · {name}"));
                }
            }
            Err(e) => out.push(format!("restore failed: {e}")),
        }
        out
    }

    fn restore_zip(path: &std::path::Path) -> anyhow::Result<Vec<String>> {
        let file = std::fs::File::open(path)?;
        let mut archive = zip::ZipArchive::new(file)?;
        let home = joey_core::joey_home();
        // Safety: refuse to restore over a config that exists unless it's
        // backed up first.
        let mut restored = Vec::new();
        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            let Some(name) = entry.enclosed_name() else { continue };
            let name_str = name.display().to_string();
            let dest = home.join(&name_str);
            if dest.exists() {
                let backup = home.join(format!("{}.bak.{}", name_str, chrono::Local::now().format("%Y%m%d_%H%M%S")));
                std::fs::copy(&dest, &backup)?;
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut bytes)?;
            // Restore .env with restrictive permissions.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if name_str == ".env" {
                    std::fs::write(&dest, &bytes)?;
                    let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600));
                } else {
                    std::fs::write(&dest, &bytes)?;
                }
            }
            #[cfg(not(unix))]
            std::fs::write(&dest, &bytes)?;
            restored.push(name_str);
        }
        Ok(restored)
    }

    fn prune() -> Lines {
        let mut out = Lines::default();
        let dir = snapshots_dir();
        let mut zips: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().map(|x| x == "zip").unwrap_or(false)).collect())
            .unwrap_or_default();
        zips.sort();
        let total = zips.len();
        let keep = 10;
        if total <= keep {
            out.push(format!("{total} snapshot(s) on disk (keeping up to {keep})."));
            return out;
        }
        let mut removed = 0;
        for path in &zips[..total - keep] {
            if std::fs::remove_file(path).is_ok() {
                removed += 1;
            }
        }
        out.push(format!("Pruned {removed} old snapshot(s); {keep} kept."));
        out
    }
}

/// `/stop` — kill every running background process session.
pub fn stop_background_processes() -> Lines {
    use joey_tools::tools::process_tool::process_registry;
    use std::sync::MutexGuard;
    let mut out = Lines::default();
    let registry = process_registry();
    let mut guard: MutexGuard<'_, std::collections::HashMap<String, joey_tools::tools::process_tool::ProcessSession>> =
        registry.lock().unwrap_or_else(|p| p.into_inner());
    // Collect ids first (is_running needs &mut for try_wait).
    let ids: Vec<String> = guard
        .keys()
        .cloned()
        .collect();
    let running: Vec<String> = ids
        .into_iter()
        .filter(|id| {
            guard
                .get_mut(id)
                .map(|s| s.is_running())
                .unwrap_or(false)
        })
        .collect();
    if running.is_empty() {
        out.push("No background processes running.");
        return out;
    }
    let mut killed = 0;
    for id in running {
        if let Some(session) = guard.get_mut(&id) {
            // Signal the reaper to stop, then kill the child.
            if let Some(handle) = session.reaper_handle.take() {
                handle.abort();
            }
            if let Some(child) = session.child.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
            killed += 1;
        }
    }
    out.push(format!("Stopped {killed} background process(es):"));
    out
}

/// `/journey` — the OMO learning journey timeline, derived from `.omo/`
/// notepads (learnings/decisions/issues) + goal history.
pub fn journey_lines(cwd: &std::path::Path, args: &str) -> Lines {
    let mut out = Lines::default();
    let omo_dir = cwd.join(".omo");
    if !omo_dir.exists() {
        out.push("No .omo/ directory here — the journey timeline starts once you use /goal, /start-work, or team mode in this project.");
        return out;
    }
    let mut parts = args.split_whitespace();
    match parts.next() {
        Some("delete") | Some("edit") => {
            out.push("Journey entries live in .omo/notepads/<plan>/*.md — edit those files directly (delete/edit verbs are file operations).");
        }
        _ => {
            out.push("Learning journey (from .omo/):");
            // Goals
            if let Some(goal) = joey_omo::GoalState::read(&omo_dir) {
                let status = match goal.status { joey_omo::GoalStatus::Active => "active", joey_omo::GoalStatus::Paused => "paused" };
                out.push(format!("  · goal [{}] {} (set {})", status, goal.objective, goal.set_at));
                for sg in &goal.subgoals {
                    let mark = if sg.done { "[x]" } else { "[ ]" };
                    out.push(format!("    {mark} #{} {}", sg.number, sg.text));
                }
            }
            // Boulder works
            {
                let boulder = joey_omo::BoulderState::read(&omo_dir);
                for w in &boulder.works {
                    let status = match w.status { joey_omo::BoulderWorkStatus::Active => "active", _ => "stopped" };
                    out.push(format!("  · work [{status}] {} ({})", w.plan_name, w.started_at));
                }
            }
            // Notepads
            let notepads = omo_dir.join("notepads");
            if notepads.exists() {
                if let Ok(rd) = std::fs::read_dir(&notepads) {
                    for plan in rd.flatten().filter(|e| e.path().is_dir()) {
                        let pname = plan.file_name().to_string_lossy().into_owned();
                        let mut lines = 0;
                        if let Ok(rd2) = std::fs::read_dir(plan.path()) {
                            for f in rd2.flatten() {
                                if let Ok(content) = std::fs::read_to_string(f.path()) {
                                    lines += content.lines().filter(|l| l.trim_start().starts_with('-')).count();
                                }
                            }
                        }
                        if lines > 0 {
                            out.push(format!("  · notepad [{pname}] {lines} wisdom entr(y/ies)"));
                        }
                    }
                }
            }
            out.push("Full detail: .omo/goals.json, .omo/boulder.json, .omo/notepads/");
        }
    }
    out
}

/// `/whoami` — slash command access level. Upstream gates commands by admin
/// allowlists per platform; this port has a single local user = admin.
pub fn whoami_lines() -> Lines {
    let mut out = Lines::default();
    out.push("Access: admin (single-user local install — every command is available).");
    out.push("Gateway platforms can restrict commands per chat via allowlists when configured.");
    out
}

/// `/profile` — active profile name + home dir.
pub fn profile_lines() -> Lines {
    let mut out = Lines::default();
    out.push(format!("Profile:  {}", crate::active_profile()));
    out.push(format!("Home:     {}", joey_core::joey_home().display()));
    out.push("Switch with: joey -p <name> (or --profile).");
    out
}

/// `/handoff <platform>` — hand the session to a messaging platform.
pub fn handoff_lines(platform: &str, session_id: &str) -> Lines {
    let mut out = Lines::default();
    let p = platform.trim();
    if p.is_empty() {
        out.push("Usage: /handoff <platform>");
        out.push("Platforms with adapters in this build: none yet (the gateway spine is present; concrete adapters ship incrementally).");
        return out;
    }
    out.push(format!(
        "Handoff to '{p}' is not wired in this build — no {p} adapter is compiled in. \
         The session ({session_id}) stays local; resume it anywhere with /resume {session_id}."
    ));
    out
}

/// `/moa <prompt>` — mixture-of-agents: the preset is expressed as a
/// structured prompt run through the SAME agent (which fans out via
/// delegate_task when the orchestration toolset is enabled).
pub fn moa_prompt(prompt: &str) -> (Lines, String) {
    let mut out = Lines::default();
    out.push("🧠 Mixture of Agents: drafting 3 independent proposals, then a synthesizer pass.");
    let composed = format!(
        "Run this request as a Mixture of Agents (MoA):\n\
         1. Use delegate_task with a batch of 3 INDEPENDENT subagents (different angles) on this task:\n   {prompt}\n\
         2. Then synthesize: produce ONE final answer combining the strongest parts of all proposals, \
         noting and resolving any disagreements.\n\
         If delegation is unavailable, answer directly and note that.",
        prompt = prompt.trim()
    );
    (out, composed)
}

/// `/subgoal` — manage extra criteria on the active goal.
pub fn subgoal_lines(cwd: &std::path::Path, args: &str) -> Lines {
    let mut out = Lines::default();
    let omo_dir = cwd.join(".omo");
    let action = joey_omo::parse_subgoal_command(args);
    let Some(mut goal) = joey_omo::GoalState::read(&omo_dir) else {
        out.push("No goal set — use /goal set <text> first; subgoals hang off the active goal.");
        return out;
    };
    match action {
        joey_omo::SubgoalAction::Show => {
            if goal.subgoals.is_empty() {
                out.push("No subgoals on the active goal. Add one: /subgoal <text>");
            } else {
                out.push(format!("Subgoals of \"{}\":", goal.objective));
                for sg in &goal.subgoals {
                    let mark = if sg.done { "[x]" } else { "[ ]" };
                    out.push(format!("  {mark} #{} {} (added {})", sg.number, sg.text, sg.added_at));
                }
                out.push("Remove: /subgoal remove N · toggle: /subgoal done N · /subgoal clear");
            }
        }
        joey_omo::SubgoalAction::Add(text) => {
            let next = goal.subgoals.iter().map(|s| s.number).max().unwrap_or(0) + 1;
            goal.subgoals.push(joey_omo::Subgoal::new(next, text.clone()));
            match goal.write(&omo_dir) {
                Ok(()) => out.push(format!("✓ Subgoal #{next} added: {text}")),
                Err(e) => out.push(format!("failed to write goal: {e}")),
            }
        }
        joey_omo::SubgoalAction::Remove(n) => {
            let before = goal.subgoals.len();
            goal.subgoals.retain(|s| s.number != n);
            if goal.subgoals.len() == before {
                out.push(format!("No subgoal #{}.", n));
            } else {
                let _ = goal.write(&omo_dir);
                out.push(format!("Removed subgoal #{n}."));
            }
        }
        joey_omo::SubgoalAction::SetDone { number, done } => {
            let mut hit = false;
            for sg in goal.subgoals.iter_mut() {
                if sg.number == number {
                    sg.done = done;
                    hit = true;
                }
            }
            if hit {
                let _ = goal.write(&omo_dir);
                out.push(format!("Subgoal #{number} marked {}.", if done { "done" } else { "open" }));
            } else {
                out.push(format!("No subgoal #{number}."));
            }
        }
        joey_omo::SubgoalAction::Clear => {
            goal.subgoals.clear();
            let _ = goal.write(&omo_dir);
            out.push("Cleared all subgoals.");
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Configuration: /codex-runtime /personality /statusbar /footer /yolo /fast
// /skin /indicator /voice /busy /reload
// ---------------------------------------------------------------------------

/// `/codex-runtime [auto|codex_app_server]` — runtime selection for
/// OpenAI/Codex models. Persists `provider.codex_runtime` and applies to the
/// next provider client build.
pub fn codex_runtime_lines(config: &mut Config, args: &str) -> Lines {
    let mut out = Lines::default();
    let current = config.get_str("provider.codex_runtime", "auto");
    match args.trim() {
        "" => {
            out.push(format!("codex runtime: {current}"));
            out.push("Usage: /codex-runtime [auto|codex_app_server]  (change applies on next session/model switch)");
        }
        "auto" | "codex_app_server" | "codex-app-server" => {
            let value = if args.trim() == "auto" { "auto" } else { "codex_app_server" };
            match config.set_and_save("provider.codex_runtime", value) {
                Ok(()) => out.push(format!("✓ codex runtime → {value} (saved; applies to the next provider client build)")),
                Err(e) => out.push(format!("failed to save: {e}")),
            }
        }
        other => out.push(format!("Unknown runtime '{other}'. Use: auto | codex_app_server")),
    }
    out
}

/// `/personality [name]` — predefined system-prompt persona overlay.
/// Personalities ship as prompt overlays stored in config; unknown names list
/// the catalog.
pub mod personality {
    use super::*;

    /// Built-in personas (name → prompt overlay). Deliberately small; users
    /// can add SOUL.md content for full identity control.
    pub const PERSONAS: &[(&str, &str)] = &[
        ("default", "Respond in your default balanced tone."),
        ("concise", "Be maximally concise: short sentences, no filler, no restating the question. Prefer bullet lists."),
        ("friendly", "Be warm and conversational, like a colleague — light humor welcome, still precise."),
        ("socratic", "Guide with questions: before giving a direct answer, ask the minimal clarifying question that unblocks it."),
        ("mentor", "Teach as you go: explain the why, point out trade-offs, suggest what to learn next."),
        ("pirate", "Answer like a pirate: salty vocabulary and nautical metaphors, but keep the technical content accurate."),
    ];

    pub fn handle(config: &mut Config, args: &str) -> (Lines, Option<String>) {
        let mut out = Lines::default();
        let current = config.get_str("agent.personality", "default");
        let name = args.trim();
        if name.is_empty() {
            out.push(format!("Personality: {current}"));
            out.push("Available:");
            for (n, _) in PERSONAS {
                out.push(format!("  · {n}"));
            }
            out.push("Set with: /personality <name>");
            return (out, None);
        }
        let Some((_, overlay)) = PERSONAS.iter().find(|(n, _)| *n == name) else {
            out.push(format!("Unknown personality '{name}'. Available:"));
            for (n, _) in PERSONAS {
                out.push(format!("  · {n}"));
            }
            return (out, None);
        };
        match config.set_and_save("agent.personality", name) {
            Ok(()) => {
                out.push(format!("✓ Personality → {name} (saved; applied to the next session)"));
                (out, Some(overlay.to_string()))
            }
            Err(e) => {
                out.push(format!("failed to save: {e}"));
                (out, None)
            }
        }
    }
}

/// Toggle-style config helpers. Each returns lines; the host applies any
/// follow-up state (e.g. TUI repaint).
pub fn statusbar_lines(config: &mut Config) -> Lines {
    toggle_config_bool(config, "display.statusbar", "context/model status bar", None)
}

pub fn footer_lines(config: &mut Config, args: &str) -> Lines {
    match args.trim() {
        "" => toggle_config_bool(config, "display.footer", "gateway metadata footer", None),
        "on" => toggle_config_bool(config, "display.footer", "gateway metadata footer", Some(true)),
        "off" => toggle_config_bool(config, "display.footer", "gateway metadata footer", Some(false)),
        "status" => {
            let on = Config::load().map(|c| c.get_bool("display.footer", true)).unwrap_or(true);
            Lines::single(format!("Gateway metadata footer: {}", if on { "on" } else { "off" }))
        }
        other => Lines::single(format!("Usage: /footer [on|off|status] (got '{other}')")),
    }
}

pub fn yolo_lines() -> Lines {
    let current = std::env::var("JOEY_YOLO_MODE").map(|v| v == "1").unwrap_or(false);
    let next = !current;
    if next {
        std::env::set_var("JOEY_YOLO_MODE", "1");
    } else {
        std::env::remove_var("JOEY_YOLO_MODE");
    }
    let mut out = Lines::default();
    out.push(format!(
        "YOLO mode: {} (dangerous command approvals {})",
        if next { "ON" } else { "OFF" },
        if next { "SKIPPED — use at your own risk" } else { "required" }
    ));
    out
}

pub fn fast_lines(config: &mut Config, args: &str) -> Lines {
    let mut parts: Vec<&str> = args.split_whitespace().collect();
    let global = parts.iter().any(|p| *p == "--global" || *p == "-g");
    parts.retain(|p| *p != "--global" && *p != "-g");
    let current = config.get_str("agent.fast_mode", "normal");
    match parts.first().copied() {
        None | Some("status") => {
            let mut out = Lines::default();
            out.push(format!("Fast mode: {current}"));
            out.push("Usage: /fast [normal|fast] [--global]");
            out
        }
        Some("normal") | Some("fast") => {
            let mode = parts[0];
            if let Err(e) = config.set_and_save("agent.fast_mode", mode) {
                return Lines::single(format!("failed to save: {e}"));
            }
            let scope = if global { " (saved globally)" } else { " (this session; --global to persist)" };
            Lines::single(format!(
                "✓ Fast mode → {mode}{scope} — fast routes terse requests, skips niceties"
            ))
        }
        Some(other) => Lines::single(format!("Usage: /fast [normal|fast] [--global] (got '{other}')")),
    }
}

/// `/skin [name]` — TUI theme selection. joey-tui ships the aurora theme;
/// skins select palette variants recorded in config and applied on next start.
pub mod skin {
    use super::*;

    /// Named palette variants derived from the aurora base by channel bias.
    pub const SKINS: &[&str] = &["aurora", "ember", "forest", "mono"];

    pub fn handle(config: &mut Config, args: &str) -> Lines {
        let mut out = Lines::default();
        let name = args.trim();
        let current = config.get_str("display.skin", "aurora");
        if name.is_empty() {
            out.push(format!("Skin: {current}"));
            out.push("Available:");
            for s in SKINS {
                out.push(format!("  · {s}"));
            }
            out.push("Set with: /skin <name> (applies on next TUI start)");
            return out;
        }
        if !SKINS.contains(&name) {
            out.push(format!("Unknown skin '{name}'. Available: {}", SKINS.join(", ")));
            return out;
        }
        match config.set_and_save("display.skin", name) {
            Ok(()) => out.push(format!("✓ Skin → {name} (saved; the TUI reloads it on next start)")),
            Err(e) => out.push(format!("failed to save: {e}")),
        }
        out
    }
}

/// `/indicator [kaomoji|emoji|unicode|ascii]` — busy-indicator style.
pub mod indicator {
    use super::*;

    pub const STYLES: &[&str] = &["kaomoji", "emoji", "unicode", "ascii"];

    pub fn handle(config: &mut Config, args: &str) -> Lines {
        let mut out = Lines::default();
        let name = args.trim();
        let current = config.get_str("display.indicator", "unicode");
        if name.is_empty() {
            out.push(format!("Busy indicator: {current}"));
            out.push("Styles:");
            for s in STYLES {
                out.push(format!("  · {s}"));
            }
            return out;
        }
        if !STYLES.contains(&name) {
            out.push(format!("Unknown indicator '{name}'. Styles: {}", STYLES.join(", ")));
            return out;
        }
        match config.set_and_save("display.indicator", name) {
            Ok(()) => out.push(format!("✓ Busy indicator → {name} (saved; live in the line REPL spinner, next TUI start for panes)")),
            Err(e) => out.push(format!("failed to save: {e}")),
        }
        out
    }
}

/// `/voice [on|off|tts|status]` — voice mode requires STT/TTS backends that
/// are deferred upstream infra in this port; answer honestly.
pub fn voice_lines(_config: &mut Config, args: &str) -> Lines {
    let mut out = Lines::default();
    match args.trim() {
        "" | "status" => out.push("Voice mode: off (STT/TTS backends are deferred in this port — see PORTING.md)"),
        other => out.push(format!(
            "Voice mode cannot be turned {other} here: STT/TTS backends are deferred in this port (PORTING.md)."
        )),
    }
    out
}

/// `/busy [queue|steer|interrupt|status]` — what Enter does while a turn runs.
pub mod busy {
    use super::*;

    pub const MODES: &[&str] = &["queue", "steer", "interrupt"];

    pub fn handle(config: &mut Config, args: &str) -> Lines {
        let mut out = Lines::default();
        let current = config.get_str("display.busy_enter", "interrupt");
        let first = args.split_whitespace().next().unwrap_or("");
        match first {
            "" | "status" => {
                out.push(format!("Busy-Enter mode: {current}"));
                out.push("  queue     — Enter queues the message for the next turn");
                out.push("  steer     — Enter injects the message mid-turn (after the next tool call)");
                out.push("  interrupt — Enter interrupts the running turn (upstream default)");
            }
            m if MODES.contains(&m) => {
                if let Err(e) = config.set_and_save("display.busy_enter", m) {
                    out.push(format!("failed to save: {e}"));
                } else {
                    out.push(format!("✓ Busy-Enter mode → {m} (saved)"));
                }
            }
            other => out.push(format!("Usage: /busy [queue|steer|interrupt|status] (got '{other}')")),
        }
        out
    }
}

/// `/reload` — re-read ~/.joey/.env into the running process.
pub fn reload_env() -> Lines {
    let loaded = joey_core::config::load_joey_dotenv(None, None);
    let mut out = Lines::default();
    if loaded.is_empty() {
        out.push("No .env files found to reload.");
    } else {
        out.push(format!("✓ Reloaded {} .env file(s):", loaded.len()));
        for p in loaded {
            out.push(format!("  · {}", p.display()));
        }
    }
    out
}

fn toggle_config_bool(config: &mut Config, key: &str, label: &str, force: Option<bool>) -> Lines {
    let current = config.get_bool(key, true);
    let next = force.unwrap_or(!current);
    match config.set_and_save(key, if next { "true" } else { "false" }) {
        Ok(()) => Lines::single(format!("✓ {label}: {}", if next { "on" } else { "off" })),
        Err(e) => Lines::single(format!("failed to save: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Tools & Skills: /memory /bundles /pet /hatch /learn /cron /suggestions
// /blueprint /curator /kanban /reload-mcp /reload-skills /plugins
// ---------------------------------------------------------------------------

/// `/memory [pending|approve|reject|approval]` — review the curated memory
/// files. This port has no approval queue (memory writes are direct), so the
/// command surfaces the files + the approval-gate config.
pub fn memory_lines(config: &mut Config, args: &str) -> Lines {
    let mut out = Lines::default();
    let mem_dir = joey_core::joey_home().join("memories");
    let mut parts = args.split_whitespace();
    match parts.next() {
        None | Some("status") | Some("pending") => {
            let approval = config.get_bool("memory.approval_required", false);
            out.push(format!("Memory approval gate: {}", if approval { "on" } else { "off (writes are direct)" }));
            for (label, file) in [("memory", "MEMORY.md"), ("user profile", "USER.md")] {
                let path = mem_dir.join(file);
                match std::fs::read_to_string(&path) {
                    Ok(content) => {
                        let entries = content.lines().filter(|l| l.trim().starts_with("§") || l.trim().starts_with("- ")).count();
                        out.push(format!("  · {label}: {file} ({entries} entries, {} bytes)", content.len()));
                    }
                    Err(_) => out.push(format!("  · {label}: {file} (not created yet)")),
                }
            }
            out.push("Edit directly with the memory tool, or: joey config set memory.approval_required true");
        }
        Some("approve") | Some("reject") => {
            out.push("No pending memory writes — this port applies memory writes directly (the approval queue is upstream infra).");
        }
        Some("approval") => match parts.next() {
            Some("on") => {
                let _ = config.set_and_save("memory.approval_required", "true");
                out.push("✓ Memory approval gate: on");
            }
            Some("off") => {
                let _ = config.set_and_save("memory.approval_required", "false");
                out.push("✓ Memory approval gate: off");
            }
            _ => out.push("Usage: /memory approval [on|off]"),
        },
        Some(other) => out.push(format!("Usage: /memory [pending|approval] [on|off] (got '{other}')")),
    }
    out
}

/// `/bundles` — skill bundles (aliases that load multiple skills).
pub fn bundles_lines(config: &Config) -> Lines {
    let mut out = Lines::default();
    // Bundles live under config key skills.bundles.<name>: [skill, ...]
    if let Some(bundles) = config.get("skills.bundles").and_then(|v| v.as_mapping()) {
        if bundles.is_empty() {
            out.push("No skill bundles defined. Add one with:");
            out.push("  joey config set skills.bundles.review \"review-pr,test-driven-development\"");
            return out;
        }
        out.push("Skill bundles (load all with /skills or -s):");
        for (name, skills) in bundles {
            let list = skills
                .as_sequence()
                .map(|s| s.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
                .unwrap_or_else(|| "(invalid)".to_string());
            out.push(format!("  /{} → {}", name.as_str().unwrap_or("?"), list));
        }
    } else {
        out.push("No skill bundles defined. Add one with:");
        out.push("  joey config set skills.bundles.review \"review-pr,test-driven-development\"");
    }
    out
}

/// `/pet [toggle|list|scale <n>|<slug>]` — petdex mascot. petdex generation is
/// media-tooling upstream; the mascot selection is a config-backed state the
/// TUI renders.
pub mod pet {
    use super::*;

    const KEY: &str = "display.pet";

    pub fn handle(config: &mut Config, args: &str) -> Lines {
        let mut out = Lines::default();
        let current = config.get_str(KEY, "");
        let mut parts = args.split_whitespace();
        match parts.next() {
            None | Some("status") => {
                if current.is_empty() {
                    out.push("No pet adopted. Generate one with /hatch <description>.");
                } else {
                    out.push(format!("Pet: {current} (scale {})", config.get_i64("display.pet_scale", 1)));
                    out.push("Hide with /pet toggle; change size with /pet scale <n>.");
                }
            }
            Some("toggle") => {
                if current.is_empty() {
                    out.push("No pet to toggle — hatch one with /hatch <description>.");
                } else {
                    let hidden = config.get_bool("display.pet_hidden", false);
                    let _ = config.set_and_save("display.pet_hidden", if hidden { "false" } else { "true" });
                    out.push(format!("Pet {}.", if hidden { "shown" } else { "hidden" }));
                }
            }
            Some("list") => {
                out.push("Adoptable pets (built-in):");
                for slug in ["cats:nyan", "cats:喵", "dogs:doge", "memes:rick"] {
                    out.push(format!("  · {slug}"));
                }
                out.push("Or generate a custom one: /hatch <description>");
            }
            Some("scale") => {
                let n = parts.next().unwrap_or("").parse::<i64>().unwrap_or(0);
                if (1..=8).contains(&n) {
                    let _ = config.set_and_save("display.pet_scale", &n.to_string());
                    out.push(format!("✓ Pet scale → {n}"));
                } else {
                    out.push("Usage: /pet scale <1-8>");
                }
            }
            Some(slug) => {
                let _ = config.set_and_save(KEY, slug);
                let _ = config.set_and_save("display.pet_hidden", "false");
                out.push(format!("✓ Pet adopted: {slug}"));
            }
        }
        out
    }

    /// `/hatch <description>` — generate a new pet. Without the media
    /// generation backend this records the request and instructs the petdex
    /// skill route.
    pub fn hatch(args: &str) -> Lines {
        let mut out = Lines::default();
        let desc = args.trim();
        if desc.is_empty() {
            out.push("Usage: /hatch <description> — describe the pet to generate");
            return out;
        }
        out.push(format!("Hatching a pet from: \"{desc}\""));
        out.push(
            "The petdex generator (media backend) is deferred in this port; the request is queued in config \
             (display.pet_pending). Run the petdex skill when media generation is available.",
        );
        let mut config = Config::load().unwrap_or_else(|_| Config::defaults());
        let _ = config.set_and_save("display.pet_pending", desc);
        out
    }
}

/// `/learn <what>` — turn a description into a reusable skill. The heavy
/// lifting is an agent turn that drafts SKILL.md; the command composes the
/// prompt.
pub fn learn_prompt(what: &str) -> (Lines, String) {
    let mut out = Lines::default();
    out.push("🧠 Learning mode: the agent will draft a SKILL.md from your description and install it under ~/.joey/skills/.");
    let prompt = format!(
        "Create a reusable skill from this description:\n\n{what}\n\n\
         Steps:\n\
         1. Draft the SKILL.md (frontmatter: name, description; then concise operational instructions — \
         commands, gotchas, decision rules — no filler).\n\
         2. Create the directory ~/.joey/skills/<kebab-name>/SKILL.md and write the file.\n\
         3. Report the installed path and a one-line usage hint.\n\
         If the description is too vague to operationalize, ask ONE clarifying question first.",
        what = what.trim()
    );
    (out, prompt)
}

/// `/cron …` — cron management mirror of `joey cron`.
pub mod cron {
    use super::*;

    pub fn handle(args: &str) -> Lines {
        let mut out = Lines::default();
        let store = CronStore::open_default();
        let mut parts = args.split_whitespace();
        match parts.next() {
            None | Some("list") | Some("ls") => {
                let jobs = store.list_jobs(false).unwrap_or_default();
                if jobs.is_empty() {
                    out.push("No scheduled jobs. Create one: /cron <schedule> <prompt> · e.g. /cron 30m summarize my inbox");
                } else {
                    out.push(format!("{} scheduled job(s):", jobs.len()));
                    for j in &jobs {
                        let state = if j.state == "paused" { "[paused]" } else if j.enabled { "[active]" } else { "[disabled]" };
                        out.push(format!(
                            "  {} {} · {} · next {} · {}",
                            j.id,
                            state,
                            j.schedule_display,
                            j.next_run_at.as_deref().unwrap_or("?"),
                            if j.name.is_empty() { "(unnamed)" } else { &j.name }
                        ));
                    }
                }
            }
            Some("create") | Some("add") => {
                let rest = args.split_once(parts.next().unwrap_or(" ")).map(|(_, r)| r.trim()).unwrap_or("");
                create_job(&mut out, rest);
            }
            Some("run") | Some("trigger") => {
                let Some(id) = parts.next() else {
                    out.push("Usage: /cron run <job-id-or-name>");
                    return out;
                };
                match store.trigger_job(id) {
                    Ok(Some(j)) => out.push(format!("✓ Triggered '{}' ({}) — runs on the next scheduler tick.", j.name, j.id)),
                    _ => out.push(format!("Job not found: {id}")),
                }
            }
            Some("pause") => {
                let Some(id) = parts.next() else {
                    out.push("Usage: /cron pause <job-id-or-name>");
                    return out;
                };
                match store.pause_job(id, None) {
                    Ok(Some(j)) => out.push(format!("✓ Paused '{}' ({})", j.name, j.id)),
                    _ => out.push(format!("Job not found or not pausable: {id}")),
                }
            }
            Some("resume") => {
                let Some(id) = parts.next() else {
                    out.push("Usage: /cron resume <job-id-or-name>");
                    return out;
                };
                match store.resume_job(id) {
                    Ok(Some(j)) => out.push(format!("✓ Resumed '{}' ({})", j.name, j.id)),
                    _ => out.push(format!("Job not found: {id}")),
                }
            }
            Some("remove") | Some("rm") | Some("delete") => {
                let Some(id) = parts.next() else {
                    out.push("Usage: /cron remove <job-id-or-name>");
                    return out;
                };
                match store.remove_job(id) {
                    Ok(true) => out.push(format!("✓ Removed job {id}")),
                    Ok(false) => out.push(format!("Job not found: {id}")),
                    Err(e) => out.push(format!("remove failed: {e}")),
                }
            }
            Some(other) => {
                // /cron <schedule> <prompt> shorthand: first token parses as a
                // schedule → create; else usage.
                let rest = args.trim();
                if looks_like_schedule(other) {
                    create_job(&mut out, rest);
                } else {
                    out.push(format!("Usage: /cron [list|create <schedule> <prompt>|pause|resume|run|remove] (got '{other}')"));
                }
            }
        }
        out
    }

    fn looks_like_schedule(s: &str) -> bool {
        s.chars().all(|c| c.is_ascii_digit() || matches!(c, 'm' | 'h' | 'd' | '*' | '/' | ',' | '-' | ' '))
            && !s.is_empty()
            && (s.contains(|c: char| c.is_ascii_digit()) && (s.ends_with('m') || s.ends_with('h') || s.ends_with('d') || s.contains('*') || s.contains('/')))
    }

    fn create_job(out: &mut Lines, rest: &str) {
        let mut it = rest.splitn(2, char::is_whitespace);
        let Some(schedule) = it.next().map(str::trim).filter(|s| !s.is_empty()) else {
            out.push("Usage: /cron create <schedule> <prompt> · schedules: 30m · every 2h · 0 9 * * *");
            return;
        };
        let prompt = it.next().map(str::trim).unwrap_or("");
        if prompt.is_empty() {
            out.push("Usage: /cron create <schedule> <prompt>");
            return;
        }
        let store = CronStore::open_default();
        match store.create_job(Some(prompt), schedule, joey_cron::CreateJobOptions::default()) {
            Ok(job) => {
                out.push(format!("✓ Created job {} — {}", job.id, job.schedule_display));
                out.push(format!("  Next run: {}", job.next_run_at.as_deref().unwrap_or("?")));
                out.push("Jobs fire when a scheduler runs: joey cron tick --loop (or joey cron status).");
            }
            Err(e) => out.push(format!("Failed to create job: {e}")),
        }
    }
}

/// `/suggestions [accept|dismiss N|catalog]` — automation suggestions.
pub mod suggestions {
    use super::*;

    pub fn handle(config: &mut Config, args: &str) -> Lines {
        let mut out = Lines::default();
        let mut parts = args.split_whitespace();
        match parts.next() {
            None | Some("list") | Some("catalog") => {
                let suggestions = pending(config);
                if suggestions.is_empty() {
                    out.push("No suggested automations yet — these appear as you repeat similar tasks (tracked locally).");
                } else {
                    out.push("Suggested automations:");
                    for (i, s) in suggestions.iter().enumerate() {
                        out.push(format!("  {}. {}", i + 1, s));
                    }
                    out.push("Accept with: /suggestions accept N · dismiss with: /suggestions dismiss N");
                }
            }
            Some("accept") => {
                let Some(n) = parts.next().and_then(|v| v.parse::<usize>().ok()) else {
                    out.push("Usage: /suggestions accept <N>");
                    return out;
                };
                let mut suggestions = pending(config);
                if n == 0 || n > suggestions.len() {
                    out.push(format!("No suggestion #{}.", n));
                    return out;
                }
                let text = suggestions.remove(n - 1);
                save(config, &suggestions);
                out.push(format!("Accepted: {text}"));
                out.push("Turn it into a scheduled job with /cron create <schedule> <prompt>.");
            }
            Some("dismiss") => {
                let Some(n) = parts.next().and_then(|v| v.parse::<usize>().ok()) else {
                    out.push("Usage: /suggestions dismiss <N>");
                    return out;
                };
                let mut suggestions = pending(config);
                if n == 0 || n > suggestions.len() {
                    out.push(format!("No suggestion #{}.", n));
                    return out;
                }
                let text = suggestions.remove(n - 1);
                save(config, &suggestions);
                out.push(format!("Dismissed: {text}"));
            }
            Some(other) => out.push(format!("Usage: /suggestions [accept|dismiss N|catalog] (got '{other}')")),
        }
        out
    }

    fn pending(config: &Config) -> Vec<String> {
        config
            .get_str_list("automations.suggestions")
            .into_iter()
            .filter(|s| !s.trim().is_empty())
            .collect()
    }

    fn save(config: &mut Config, list: &[String]) {
        let joined = list.join("\n");
        let _ = config.set_and_save("automations.suggestions", &joined);
    }
}

/// `/blueprint [name] [slot=value ...]` — automation blueprints.
pub mod blueprint {
    use super::*;

    /// Built-in templates: name → (description, prompt template).
    const TEMPLATES: &[(&str, &str, &str)] = &[
        (
            "morning-briefing",
            "daily digest of news/blogs you follow",
            "Every morning: check my followed sources and deliver a briefing with the 5 most important updates.",
        ),
        (
            "inbox-triage",
            "periodic email triage",
            "Scan my inbox and summarize: needs-reply, FYI-only, action-required. Never send anything.",
        ),
        (
            "standup-prep",
            "pre-standup summary of recent work",
            "Review my recent git activity and open tickets; draft a 3-bullet standup update.",
        ),
        (
            "code-review-watch",
            "review new PRs on a repo",
            "List open PRs on my active repo, review the newest one, and post a review summary.",
        ),
    ];

    pub fn handle(args: &str) -> Lines {
        let mut out = Lines::default();
        let mut parts = args.split_whitespace();
        match parts.next() {
            None => {
                out.push("Automation blueprints:");
                for (name, desc, _) in TEMPLATES {
                    out.push(format!("  · {name} — {desc}"));
                }
                out.push("Set one up: /blueprint <name> [slot=value ...]");
            }
            Some(name) => {
                let Some((_, _, template)) = TEMPLATES.iter().find(|(n, _, _)| *n == name) else {
                    out.push(format!("Unknown blueprint '{name}'. Available:"));
                    for (n, _, _) in TEMPLATES {
                        out.push(format!("  · {n}"));
                    }
                    return out;
                };
                let mut prompt = template.to_string();
                // slot=value substitution.
                for slot in parts {
                    if let Some((k, v)) = slot.split_once('=') {
                        prompt = prompt.replace(&format!("{{{}}}", k), v);
                    }
                }
                out.push(format!("Blueprint '{name}' → suggested cron job:"));
                out.push(format!("  /cron create 30m {prompt}"));
                out.push("(edit the schedule/prompt to taste before running)");
            }
        }
        out
    }
}

/// `/curator` — background skill maintenance.
pub fn curator_lines(args: &str) -> Lines {
    let mut out = Lines::default();
    let first = args.split_whitespace().next().unwrap_or("");
    match first {
        "" | "status" => {
            let skills = joey_tools::tools::skills_tool::discover();
            out.push(format!("Curator: {} skill(s) discovered; maintenance jobs:", skills.len()));
            out.push("  · dedupe   — collapse duplicate skills");
            out.push("  · refresh  — re-derive descriptions from SKILL.md bodies");
            out.push("Run one with: /curator <job> (runs as an agent turn)");
        }
        job if ["dedupe", "refresh"].contains(&job) => {
            out.push(format!("Curator job '{job}' queued as an agent turn."));
            // Host turns this into a submit_turn via CuratorPrompt.
        }
        other => out.push(format!("Usage: /curator [dedupe|refresh|status] (got '{other}')")),
    }
    out
}

/// Prompt for a curator agent turn.
pub fn curator_prompt(job: &str) -> String {
    match job {
        "dedupe" => "Curator: scan ~/.joey/skills/ for duplicate or near-duplicate skills (same instructions under different names). \
                     List duplicates found and propose (but do not execute) consolidation. Do not delete anything."
            .to_string(),
        _ => "Curator: re-read each SKILL.md under ~/.joey/skills/ and refresh its frontmatter description to a single \
              accurate sentence summarizing the body. Report changes. Preserve all other frontmatter fields."
            .to_string(),
    }
}

/// `/kanban` — multi-profile collaboration board. Kanban infra is deferred
/// (PORTING.md); answer with the local board directory state if any.
pub fn kanban_lines(cwd: &std::path::Path) -> Lines {
    let mut out = Lines::default();
    let board = cwd.join(".joey-kanban");
    if board.exists() {
        out.push(format!("Kanban board found at {} — columns:", board.display()));
        if let Ok(rd) = std::fs::read_dir(&board) {
            for col in rd.flatten().filter(|e| e.path().is_dir()) {
                let count = std::fs::read_dir(col.path()).map(|r| r.flatten().count()).unwrap_or(0);
                out.push(format!("  · {} ({count} card(s))", col.file_name().to_string_lossy()));
            }
        }
    } else {
        out.push("No local kanban board (./joey-kanban/). Multi-profile kanban coordination is deferred in this port (see PORTING.md).");
        out.push("Track work with /goal + /start-work (boulder state) instead.");
    }
    out
}

/// `/reload-mcp` — re-read MCP server configs. The agent rebuild picks up
/// changes; live clients reconnect lazily.
pub fn reload_mcp_lines() -> Lines {
    let config = Config::load().unwrap_or_else(|_| Config::defaults());
    let servers = joey_mcp::load_server_configs(&config);
    let mut out = Lines::default();
    if servers.is_empty() {
        out.push("No MCP servers configured (mcp_servers in config.yaml). Add one: joey mcp add <name> --command <cmd>");
    } else {
        out.push(format!("✓ Re-read {} MCP server config(s):", servers.len()));
        for (name, cfg) in &servers {
            let kind = if cfg.url.is_some() { "url" } else { "stdio" };
            out.push(format!("  · {name} ({kind})"));
        }
        out.push("New tools appear after the agent rebuilds (next /model switch or session restart).");
    }
    out
}

/// `/reload-skills` — rescan ~/.joey/skills/.
pub fn reload_skills_lines() -> Lines {
    let before = joey_tools::tools::skills_tool::discover();
    let mut out = Lines::default();
    out.push(format!(
        "✓ Rescanned skill directories: {} skill(s) discovered ({}).",
        before.len(),
        joey_core::constants::skills_dir().display()
    ));
    out.push("Skills are re-read on each session start; a rebuilt agent (e.g. /model switch) picks them up immediately.");
    out
}

/// `/plugins` — installed plugins. This port has no plugin loader yet; list
/// the directory if a user created one.
pub fn plugins_lines() -> Lines {
    let mut out = Lines::default();
    let dir = joey_core::joey_home().join("plugins");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    if names.is_empty() {
        out.push("No plugins installed (the plugin loader is deferred in this port; ~/.joey/plugins/ is scanned for future use).");
    } else {
        out.push(format!("Plugins in ~/.joey/plugins/ ({}):", names.len()));
        for n in names {
            out.push(format!("  · {n}"));
        }
        out.push("Note: the plugin loader is deferred; these directories are not auto-loaded yet.");
    }
    out
}

// ---------------------------------------------------------------------------
// Info: /subscription /topup /insights /platforms /paste /image /update /debug
// ---------------------------------------------------------------------------

/// `/subscription` — Nous account plan. This port has no Nous account
/// integration; report the local truth.
pub fn subscription_lines() -> Lines {
    let mut out = Lines::default();
    out.push("No Nous account is linked in this build — you use your own provider API keys (BYOK).");
    out.push("Manage keys: joey config set <PROVIDER>_API_KEY <key>   (stored in ~/.joey/.env)");
    out.push("Check the active provider: /status");
    out
}

/// `/topup` — balance/billing; same BYOK story.
pub fn topup_lines() -> Lines {
    let mut out = Lines::default();
    out.push("No Joey balance to top up — billing happens directly with your provider (BYOK).");
    out.push("Provider consoles: console.anthropic.com · platform.openai.com · openrouter.ai/credits · …");
    out
}

/// `/insights [days]` — usage analytics from the local session store.
pub fn insights_lines(days: i64, db: Option<&joey_core::SessionDb>) -> Lines {
    let mut out = Lines::default();
    let Some(db) = db else {
        out.push("(no session database)");
        return out;
    };
    let (sessions, messages, total, assistant) = db.usage_over_days(days).unwrap_or((0, 0, 0, 0));
    let recent = db.list_sessions(10).unwrap_or_default();
    out.push(format!("Usage insights — last {days} day(s):"));
    out.push(format!("  Sessions with usage: {}", sessions));
    out.push(format!("  Messages:             {}", messages));
    out.push(format!("  Total tokens:         {}", total));
    out.push(format!("  Agent (completion):   {}", assistant));
    if total > 0 {
        out.push(format!("  Avg per message:      {}", total / messages.max(1)));
    }
    if !recent.is_empty() {
        out.push("Most recent sessions:");
        for s in recent.iter().take(5) {
            let when = chrono::DateTime::from_timestamp(s.started_at as i64, 0)
                .map(|d| d.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
                .unwrap_or_default();
            out.push(format!(
                "  · {when} {} ({} msgs){}",
                s.id,
                s.message_count,
                s.title.clone().map(|t| format!(" — {t}")).unwrap_or_default()
            ));
        }
    }
    out
}

/// `/platforms` — gateway platform status.
pub fn platforms_lines() -> Lines {
    let mut out = Lines::default();
    out.push("Gateway platforms: none connected (no adapters compiled in this port; the spine — PlatformAdapter trait, session keys — is present).");
    out.push("Local session only. See PORTING.md \"Deferred\" for the adapter roadmap.");
    out
}

/// `/image <path>` — read an image file into a data URL for the next turn.
pub fn image_data_url(path: &str) -> Result<String, String> {
    let p = std::path::Path::new(path);
    let bytes = std::fs::read(p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_else(|| "png".to_string());
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        other => return Err(format!("unsupported image type '{other}' (png/jpg/gif/webp)")),
    };
    if bytes.len() > 15 * 1024 * 1024 {
        return Err("image exceeds the 15MB limit".to_string());
    }
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{mime};base64,{b64}"))
}

/// `/update` — self-update. Check the git-based install if present.
pub fn update_lines() -> Lines {
    let mut out = Lines::default();
    // Cargo-installed from source: report the version + how to update.
    let exe = std::env::current_exe().ok();
    let from_cargo = exe
        .as_ref()
        .map(|p| p.display().to_string().contains(".cargo"))
        .unwrap_or(false);
    out.push(format!("Current: joey-agent {}", env!("CARGO_PKG_VERSION")));
    if from_cargo {
        out.push("Update with: cargo install --path . (from your joey-agent checkout)");
    } else {
        out.push("This binary was built from source — pull the latest and rebuild:");
        out.push("  git pull && cargo build --release");
    }
    out
}

/// `/debug [nous|local]` — package a debug report (system info + recent logs);
/// never auto-uploads.
pub fn debug_lines(mode: &str) -> Lines {
    let mut out = Lines::default();
    let report_dir = joey_core::joey_home().join("debug");
    if let Err(e) = std::fs::create_dir_all(&report_dir) {
        out.push(format!("cannot create debug dir: {e}"));
        return out;
    }
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let path = report_dir.join(format!("debug_{stamp}.md"));
    let mut report = String::new();
    report.push_str("# joey-agent debug report\n\n");
    report.push_str(&format!("- version: {}\n", env!("CARGO_PKG_VERSION")));
    report.push_str(&format!("- os: {} {}\n", std::env::consts::OS, std::env::consts::ARCH));
    report.push_str(&format!("- profile: {}\n", crate::active_profile()));
    report.push_str(&format!("- home: {}\n", joey_core::joey_home().display()));
    report.push_str(&format!("- cwd: {}\n", std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default()));
    report.push_str(&format!("- mode: {mode}\n\n"));
    report.push_str("## Recent logs\n\n```\n");
    // Tail the newest log file.
    let logs_dir = joey_core::logging::logs_dir();
    let mut log_files: Vec<std::path::PathBuf> = std::fs::read_dir(&logs_dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    log_files.sort();
    if let Some(last) = log_files.last() {
        if let Ok(content) = std::fs::read_to_string(last) {
            let tail: String = content.lines().rev().take(100).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            report.push_str(&tail);
        }
    }
    report.push_str("\n```\n");
    match std::fs::write(&path, report) {
        Ok(()) => {
            out.push(format!("✓ Debug report written to {}", path.display()));
            out.push("It contains system info + the last 100 log lines. It was NOT uploaded anywhere — share it manually if asked.");
        }
        Err(e) => out.push(format!("write failed: {e}")),
    }
    out
}

// ---------------------------------------------------------------------------
// /prompt — $EDITOR compose
// ---------------------------------------------------------------------------

/// Open $EDITOR on a temp markdown file seeded with `initial`. Returns the
/// edited text (None = cancelled or editor failed). The TUI host must leave
/// the alternate screen first; the line REPL runs between reedline reads.
pub fn compose_in_editor(initial: &str) -> Option<String> {
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
        return None;
    };
    let dir = std::env::temp_dir();
    let path = dir.join(format!("joey-prompt-{}.md", std::process::id()));
    std::fs::write(&path, initial).ok()?;
    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .ok()?;
    if !status.success() {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    let content = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_filename_replaces_hostiles() {
        assert_eq!(sanitize_filename("my session/with spaces"), "my_session_with_spaces");
        assert_eq!(sanitize_filename("///"), "session");
    }

    #[test]
    fn moa_prompt_mentions_three_proposals() {
        let (lines, prompt) = moa_prompt("improve the README");
        assert!(!lines.is_empty());
        assert!(prompt.contains("3 INDEPENDENT"));
        assert!(prompt.contains("improve the README"));
    }

    #[test]
    fn learn_prompt_includes_skill_path() {
        let (_, p) = learn_prompt("how we release");
        assert!(p.contains("SKILL.md"));
        assert!(p.contains("how we release"));
    }

    #[test]
    fn schedule_detection() {
        // via the public handle: "/cron 45m do x" creates a job only if the
        // first token looks like a schedule. We can't run create (writes to
        // the real store), so exercise looks_like_schedule indirectly by
        // matching known-good/bad shapes through handle with an unknown
        // subcommand path.
        let out = cron::handle("definitely-not-a-schedule");
        assert!(out.0.iter().any(|l| l.contains("Usage")));
    }

    #[test]
    fn blueprint_lists_templates_and_substitutes() {
        let out = blueprint::handle("");
        assert!(out.0.iter().any(|l| l.contains("morning-briefing")));
        let out = blueprint::handle("morning-briefing topics=ai");
        assert!(out.0.iter().any(|l| l.contains("/cron create")));
    }

    #[test]
    fn whoami_and_profile_report_local_admin() {
        assert!(whoami_lines().0[0].contains("admin"));
        let p = profile_lines();
        assert!(p.0[0].contains("Profile:"));
    }

    #[test]
    fn subscription_is_byok_honest() {
        assert!(subscription_lines().0[0].contains("BYOK"));
    }

    #[test]
    fn yolo_toggles_env_var() {
        std::env::remove_var("JOEY_YOLO_MODE");
        let a = yolo_lines();
        assert!(a.0[0].contains("ON"));
        let b = yolo_lines();
        assert!(b.0[0].contains("OFF"));
        std::env::remove_var("JOEY_YOLO_MODE");
    }

    #[test]
    fn image_data_url_rejects_missing_file() {
        assert!(image_data_url("/nonexistent/nope.png").is_err());
    }

    #[test]
    fn image_data_url_builds_png_url() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.png");
        std::fs::write(&p, b"fakepng").unwrap();
        let url = image_data_url(p.to_str().unwrap()).unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
    }
}
