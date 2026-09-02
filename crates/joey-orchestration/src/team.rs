//! Agent teams (feature 022): file-backed shared task list + per-member
//! mailboxes for HyperCode team mode. OFF by default
//! (hypercode.team.enabled). In-process registry is the claiming authority;
//! JSON state under <joey-home>/teams/<team>/ is written synchronously on
//! every change (research.md D2).
//!
//! crates/joey-omo/src/team.rs stays untouched (research.md D7).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use joey_tools::registry::{Tool, ToolResult};
use joey_tools::ToolContext;

use crate::manager::SubagentManager;
use crate::types::StopReason;

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TeamState {
    Active,
    WindingDown,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TeamTaskStatus {
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MemberStatus {
    #[default]
    Idle,
    Working,
    Stopped,
}

/// On-disk contract (contracts/team-tools.md §4): config.json members are
/// {name, role, model} — status is live state, never serialized.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamMember {
    pub name: String,
    pub role: String,
    pub model: String,
    #[serde(skip_serializing, default)]
    pub status: MemberStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamTask {
    pub id: String,
    pub title: String,
    pub status: TeamTaskStatus,
    pub claimed_by: Option<String>,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamMessage {
    pub from: String,
    pub to: String,
    pub content: String,
    pub timestamp: String,
}

pub struct Team {
    pub name: String,
    pub objective: String,
    pub lead: String,
    pub created_at: String, // unix seconds string
    pub state: TeamState,
    pub members: Vec<TeamMember>,
}

pub struct TeamRecord {
    pub team: Team,
    pub tasks: Vec<TeamTask>,
    pub inboxes: HashMap<String, Vec<TeamMessage>>,
    /// member name -> child id (in-memory only; never persisted).
    pub member_child_ids: HashMap<String, u64>,
}

// ---------------------------------------------------------------------------
// Directives (contract-pinned wording — do not reword)
// ---------------------------------------------------------------------------

pub const TEAM_LEAD_DIRECTIVE: &str = "You are the LEAD of an agent team. Coordinate the team; you do not do the hands-on work yourself.\n\nWORK FLOW:\n1. Decompose the objective into independent tasks with team_tasks (action=add). Set dependencies only where a task genuinely cannot start before another finishes.\n2. Spawn one teammate per area of work with delegate_task: pass `team` (the team name) and a unique `name`, a focused goal, and role=explorer (read-only investigation) or role=implementor (writes code).\n3. Teammates claim unblocked tasks themselves. Track progress with team_status; coordinate with team_message.\n4. When a teammate fails, return its task to Pending (team_tasks action=release) and re-plan: reassign it or split it.\n5. When every incomplete task is blocked (deadlock), tell the user, then re-plan dependencies or wind down.\n6. When all tasks are Done, synthesize the teammates' results into one coherent final answer, then stop.";

pub const TEAMMATE_DIRECTIVE: &str = "You are a TEAMMATE in an agent team. Work independently in your own context.\n\nWORK FLOW:\n1. Check team_status regularly for messages and task state; do not busy-loop.\n2. Claim the next unassigned task whose dependencies are all Done with team_tasks (action=claim). If none is claimable, say so and stay idle.\n3. Complete your claimed task. Report progress or blockers to the lead with team_message.\n4. On completion, mark the task complete (team_tasks action=complete, success=true|false).\n5. When you have no further runnable task, notify the lead with team_message (include your final answer or error) and finish.";

/// Configured variants: prepend the concurrency cap line (lead) and poll
/// cadence line (teammate) from hypercode.team.* config values.
pub fn team_lead_directive(max_members: usize, max_parallel_members: usize) -> String {
    format!("Concurrency: keep at most {max_parallel_members} teammates running at once (advisory cap; {max_members} is the hard member maximum).\n\n{TEAM_LEAD_DIRECTIVE}")
}

pub fn teammate_directive(poll_interval_ms: u64) -> String {
    format!("{TEAMMATE_DIRECTIVE}\n\nCadence: poll team_status/team_message about every {poll_interval_ms}ms of work.")
}

// ---------------------------------------------------------------------------
// Persistence helpers (all take an explicit home so tests avoid env races)
// ---------------------------------------------------------------------------

/// Map any char not in [A-Za-z0-9._-] to '_'.
pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn team_dir(home: &Path, team: &str) -> PathBuf {
    home.join("teams").join(sanitize_name(team))
}

/// Current unix time in milliseconds, as a string (message timestamps).
fn now_ts() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

/// Current unix time in seconds, as a string (team created_at).
fn now_secs() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

fn persist_config(home: &Path, record: &TeamRecord) -> std::io::Result<()> {
    let dir = team_dir(home, &record.team.name);
    std::fs::create_dir_all(&dir)?;
    let members: Vec<Value> = record
        .team
        .members
        .iter()
        .map(|m| json!({"name": m.name, "role": m.role, "model": m.model}))
        .collect();
    let cfg = json!({
        "name": record.team.name,
        "objective": record.team.objective,
        "lead": record.team.lead,
        "created_at": record.team.created_at,
        "members": members,
    });
    std::fs::write(dir.join("config.json"), serde_json::to_vec_pretty(&cfg)?)
}

fn persist_tasks(home: &Path, record: &TeamRecord) -> std::io::Result<()> {
    let dir = team_dir(home, &record.team.name);
    std::fs::create_dir_all(&dir)?;
    let doc = json!({ "tasks": record.tasks });
    std::fs::write(dir.join("tasks.json"), serde_json::to_vec_pretty(&doc)?)
}

fn persist_inbox(home: &Path, record: &TeamRecord, member: &str) -> std::io::Result<()> {
    let dir = team_dir(home, &record.team.name).join("inboxes");
    std::fs::create_dir_all(&dir)?;
    let msgs = record.inboxes.get(member).cloned().unwrap_or_default();
    let doc = json!({ "messages": msgs });
    std::fs::write(
        dir.join(format!("{}.json", sanitize_name(member))),
        serde_json::to_vec_pretty(&doc)?,
    )
}

/// Remove config.json + inboxes/ dir; if !keep_tasks also remove tasks.json
/// and the team dir. Missing files are ignored.
fn delete_team_files(home: &Path, team: &str, keep_tasks: bool) {
    let dir = team_dir(home, team);
    let _ = std::fs::remove_file(dir.join("config.json"));
    let _ = std::fs::remove_dir_all(dir.join("inboxes"));
    if !keep_tasks {
        let _ = std::fs::remove_file(dir.join("tasks.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Read tasks.json for a team; None if absent/unreadable.
pub fn load_tasks(home: &Path, team: &str) -> Option<Vec<TeamTask>> {
    let path = team_dir(home, team).join("tasks.json");
    let text = std::fs::read_to_string(path).ok()?;
    let doc: Value = serde_json::from_str(&text).ok()?;
    serde_json::from_value(doc.get("tasks")?.clone()).ok()
}

/// Home used by every mutating op's synchronous persistence.
fn default_home() -> PathBuf {
    joey_core::constants::joey_home()
}

/// Persistence is best-effort: on IO error warn and continue — the
/// in-process registry stays the claiming authority (research.md D2).
fn persist_or_warn(result: std::io::Result<()>, what: &str) {
    if let Err(e) = result {
        tracing::warn!(target = what, error = %e, "team persistence failed (registry stays authoritative)");
    }
}

// ---------------------------------------------------------------------------
// Global registry
// ---------------------------------------------------------------------------

const NOTICE_CAP: usize = 64;

pub struct TeamRegistry {
    teams: Mutex<HashMap<String, Arc<Mutex<TeamRecord>>>>,
    notices: Mutex<Vec<String>>,
}

static TEAMS: OnceLock<TeamRegistry> = OnceLock::new();

pub fn global_teams() -> &'static TeamRegistry {
    TEAMS.get_or_init(TeamRegistry::new)
}

impl TeamRegistry {
    fn new() -> Self {
        Self {
            teams: Mutex::new(HashMap::new()),
            notices: Mutex::new(Vec::new()),
        }
    }

    /// Create a team (lazily, on the first spawn for the name). Persists
    /// config and creates an empty inbox for the lead.
    pub fn create(
        &self,
        name: &str,
        objective: &str,
        lead: &str,
    ) -> Result<Arc<Mutex<TeamRecord>>, String> {
        let mut teams = self.teams.lock().unwrap();
        if teams.contains_key(name) {
            return Err(format!("team '{name}' already exists"));
        }
        let mut record = TeamRecord {
            team: Team {
                name: name.to_string(),
                objective: objective.to_string(),
                lead: lead.to_string(),
                created_at: now_secs(),
                state: TeamState::Active,
                members: Vec::new(),
            },
            tasks: Vec::new(),
            inboxes: HashMap::new(),
            member_child_ids: HashMap::new(),
        };
        record.inboxes.insert(lead.to_string(), Vec::new());
        let home = default_home();
        persist_or_warn(persist_config(&home, &record), "config");
        persist_or_warn(persist_inbox(&home, &record, lead), "inbox");
        let arc = Arc::new(Mutex::new(record));
        teams.insert(name.to_string(), arc.clone());
        Ok(arc)
    }

    pub fn get(&self, name: &str) -> Option<Arc<Mutex<TeamRecord>>> {
        self.teams.lock().unwrap().get(name).cloned()
    }

    /// Any team whose state != Closed.
    pub fn active_team(&self) -> Option<(String, Arc<Mutex<TeamRecord>>)> {
        let teams = self.teams.lock().unwrap();
        teams
            .iter()
            .find(|(_, rec)| rec.lock().unwrap().team.state != TeamState::Closed)
            .map(|(name, rec)| (name.clone(), rec.clone()))
    }

    /// Look up (team name, member name) by child id.
    pub fn member_by_child_id(&self, child_id: u64) -> Option<(String, String)> {
        let teams = self.teams.lock().unwrap();
        for (team_name, rec) in teams.iter() {
            let rec = rec.lock().unwrap();
            for (member, id) in rec.member_child_ids.iter() {
                if *id == child_id {
                    return Some((team_name.clone(), member.clone()));
                }
            }
        }
        None
    }

    /// Session end: for every non-Closed record, wind_down + close.
    pub fn close_all(&self, manager: &SubagentManager) {
        let records: Vec<Arc<Mutex<TeamRecord>>> = {
            let teams = self.teams.lock().unwrap();
            teams
                .values()
                .filter(|rec| rec.lock().unwrap().team.state != TeamState::Closed)
                .cloned()
                .collect()
        };
        for rec in records {
            let mut r = rec.lock().unwrap();
            r.wind_down(manager);
            r.close();
        }
    }

    /// Remove <home>/teams/<dir> entries whose mtime is older than
    /// cleanup_days days. Returns how many were removed. Missing teams/
    /// dir = 0. Each removal is logged.
    pub fn purge_expired(&self, home: &Path, cleanup_days: i64) -> usize {
        let teams_root = home.join("teams");
        let entries = match std::fs::read_dir(&teams_root) {
            Ok(e) => e,
            Err(_) => return 0,
        };
        let cutoff =
            SystemTime::now() - Duration::from_secs(cleanup_days.max(0) as u64 * 86_400);
        let mut removed = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let expired = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|mtime| mtime < cutoff)
                .unwrap_or(false);
            if expired && std::fs::remove_dir_all(&path).is_ok() {
                tracing::info!(path = %path.display(), "purged expired team directory");
                removed += 1;
            }
        }
        removed
    }

    /// Append a line to the notice board (cap 64, drop-oldest).
    pub fn push_notice(&self, line: String) {
        let mut notices = self.notices.lock().unwrap();
        notices.push(line);
        let len = notices.len();
        if len > NOTICE_CAP {
            notices.drain(..len - NOTICE_CAP);
        }
    }

    /// Last n notices, oldest first.
    pub fn notice_board_tail(&self, n: usize) -> Vec<String> {
        let notices = self.notices.lock().unwrap();
        if notices.len() <= n {
            notices.clone()
        } else {
            notices[notices.len() - n..].to_vec()
        }
    }

    /// Feature 022 (FR-014/US4): stop every member of a team and close it.
    pub fn stop_team(&self, name: &str, manager: &SubagentManager) -> Result<(), String> {
        let rec = self.get(name).ok_or_else(|| format!("unknown team '{name}'"))?;
        {
            let mut r = rec.lock().unwrap();
            r.wind_down(manager);
            r.close();
        }
        self.push_notice(format!("[TEAM] team '{name}' stopped"));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TeamRecord operations (all called under the record's Arc<Mutex>; each
// mutation persists synchronously via default_home())
// ---------------------------------------------------------------------------

fn team_state_str(s: TeamState) -> &'static str {
    match s {
        TeamState::Active => "active",
        TeamState::WindingDown => "windingdown",
        TeamState::Closed => "closed",
    }
}

fn task_status_str(s: TeamTaskStatus) -> &'static str {
    match s {
        TeamTaskStatus::Pending => "pending",
        TeamTaskStatus::Running => "running",
        TeamTaskStatus::Done => "done",
        TeamTaskStatus::Failed => "failed",
    }
}

fn member_status_str(s: MemberStatus) -> &'static str {
    match s {
        MemberStatus::Idle => "idle",
        MemberStatus::Working => "working",
        MemberStatus::Stopped => "stopped",
    }
}

impl TeamRecord {
    /// Register a member. Creates an empty inbox; persists config.
    pub fn add_member(&mut self, member: TeamMember, child_id: Option<u64>) -> Result<(), String> {
        if self.team.members.iter().any(|m| m.name == member.name) {
            return Err(format!(
                "member name '{}' already exists in team '{}'",
                member.name, self.team.name
            ));
        }
        if let Some(id) = child_id {
            self.member_child_ids.insert(member.name.clone(), id);
        }
        self.team.members.push(member.clone());
        self.inboxes.insert(member.name.clone(), Vec::new());
        let home = default_home();
        persist_or_warn(persist_config(&home, self), "config");
        persist_or_warn(persist_inbox(&home, self, &member.name), "inbox");
        Ok(())
    }

    /// Add a Pending task; persists tasks; returns the new task id.
    pub fn add_task(&mut self, title: &str, dependencies: Vec<String>) -> String {
        let id = format!("task_{}", Uuid::new_v4().simple());
        self.tasks.push(TeamTask {
            id: id.clone(),
            title: title.to_string(),
            status: TeamTaskStatus::Pending,
            claimed_by: None,
            dependencies,
        });
        persist_or_warn(persist_tasks(&default_home(), self), "tasks");
        id
    }

    /// Dependency `dep` is satisfied only when the referenced task is Done.
    /// Unknown dependency ids count as unfinished.
    fn dependency_done(&self, dep: &str) -> bool {
        self.tasks
            .iter()
            .any(|t| t.id == dep && t.status == TeamTaskStatus::Done)
    }

    fn is_claimable(&self, task: &TeamTask) -> bool {
        task.status == TeamTaskStatus::Pending
            && task.dependencies.iter().all(|dep| self.dependency_done(dep))
    }

    /// Claim a Pending task whose dependencies are all Done. Sets it
    /// Running + claimed_by, sets the member Working, persists tasks.
    pub fn claim(&mut self, task_id: &str, member: &str) -> Result<TeamTask, String> {
        let claimable = self
            .tasks
            .iter()
            .find(|t| t.id == task_id)
            .map(|t| self.is_claimable(t));
        match claimable {
            None => Err(format!("unknown task '{task_id}'")),
            Some(false) => Err("task not claimable".to_string()),
            Some(true) => {
                let claimed = {
                    let task = self
                        .tasks
                        .iter_mut()
                        .find(|t| t.id == task_id)
                        .expect("task present (checked above)");
                    task.status = TeamTaskStatus::Running;
                    task.claimed_by = Some(member.to_string());
                    task.clone()
                };
                if let Some(m) = self.team.members.iter_mut().find(|m| m.name == member) {
                    m.status = MemberStatus::Working;
                }
                persist_or_warn(persist_tasks(&default_home(), self), "tasks");
                Ok(claimed)
            }
        }
    }

    /// Complete a Running task (Done/Failed). Sets the member Idle,
    /// persists tasks, returns the final status.
    pub fn complete(&mut self, task_id: &str, success: bool) -> Result<TeamTaskStatus, String> {
        let (final_status, member) = {
            let Some(task) = self.tasks.iter_mut().find(|t| t.id == task_id) else {
                return Err(format!("unknown task '{task_id}'"));
            };
            if task.status != TeamTaskStatus::Running {
                return Err(format!("task '{task_id}' is not Running"));
            }
            task.status = if success {
                TeamTaskStatus::Done
            } else {
                TeamTaskStatus::Failed
            };
            (task.status, task.claimed_by.clone())
        };
        if let Some(name) = member.as_deref() {
            if let Some(m) = self.team.members.iter_mut().find(|m| m.name == name) {
                m.status = MemberStatus::Idle;
            }
        }
        persist_or_warn(persist_tasks(&default_home(), self), "tasks");
        Ok(final_status)
    }

    /// Return a Running task to Pending (claimed_by cleared); persists.
    pub fn release(&mut self, task_id: &str) -> Result<(), String> {
        let Some(task) = self.tasks.iter_mut().find(|t| t.id == task_id) else {
            return Err(format!("unknown task '{task_id}'"));
        };
        if task.status != TeamTaskStatus::Running {
            return Err(format!("task '{task_id}' is not Running"));
        }
        task.status = TeamTaskStatus::Pending;
        task.claimed_by = None;
        persist_or_warn(persist_tasks(&default_home(), self), "tasks");
        Ok(())
    }

    /// Release every Running task claimed by `member` back to Pending,
    /// mark the member Stopped, persist. Returns how many tasks released.
    pub fn release_member_tasks(&mut self, member: &str) -> usize {
        let mut count = 0;
        for task in self.tasks.iter_mut() {
            if task.status == TeamTaskStatus::Running && task.claimed_by.as_deref() == Some(member)
            {
                task.status = TeamTaskStatus::Pending;
                task.claimed_by = None;
                count += 1;
            }
        }
        if let Some(m) = self.team.members.iter_mut().find(|m| m.name == member) {
            m.status = MemberStatus::Stopped;
        }
        persist_or_warn(persist_tasks(&default_home(), self), "tasks");
        count
    }

    pub fn list(&self) -> Vec<TeamTask> {
        self.tasks.clone()
    }

    /// Push a message to a member's inbox. If the inbox exceeds `limit`,
    /// drop the OLDEST until it equals `limit`. Persists the inbox.
    pub fn send(&mut self, from: &str, to: &str, content: &str, limit: usize) -> Result<(), String> {
        if !self.team.members.iter().any(|m| m.name == to) {
            return Err(format!(
                "recipient '{to}' is not a member of team '{}'",
                self.team.name
            ));
        }
        {
            let inbox = self.inboxes.entry(to.to_string()).or_default();
            inbox.push(TeamMessage {
                from: from.to_string(),
                to: to.to_string(),
                content: content.to_string(),
                timestamp: now_ts(),
            });
            if inbox.len() > limit {
                inbox.drain(..inbox.len() - limit);
            }
        }
        persist_or_warn(persist_inbox(&default_home(), self, to), "inbox");
        Ok(())
    }

    /// Drain (remove + persist) a member's inbox.
    pub fn receive(&mut self, member: &str) -> Vec<TeamMessage> {
        match self.inboxes.get_mut(member) {
            Some(inbox) => {
                let drained = std::mem::take(inbox);
                persist_or_warn(persist_inbox(&default_home(), self, member), "inbox");
                drained
            }
            None => Vec::new(),
        }
    }

    /// Non-destructive read of a member's inbox.
    pub fn poll(&self, member: &str) -> Vec<TeamMessage> {
        self.inboxes.get(member).cloned().unwrap_or_default()
    }

    /// Snapshot for team_status: live member state + the shared task list.
    pub fn status_value(&self) -> Value {
        let members: Vec<Value> = self
            .team
            .members
            .iter()
            .map(|m| {
                let current_task = self
                    .tasks
                    .iter()
                    .find(|t| {
                        t.status == TeamTaskStatus::Running
                            && t.claimed_by.as_deref() == Some(&m.name)
                    })
                    .map(|t| t.id.clone());
                let completed = self
                    .tasks
                    .iter()
                    .filter(|t| {
                        t.status == TeamTaskStatus::Done
                            && t.claimed_by.as_deref() == Some(&m.name)
                    })
                    .count();
                let failed = self
                    .tasks
                    .iter()
                    .filter(|t| {
                        t.status == TeamTaskStatus::Failed
                            && t.claimed_by.as_deref() == Some(&m.name)
                    })
                    .count();
                json!({
                    "name": m.name,
                    "role": m.role,
                    "status": member_status_str(m.status),
                    "current_task": current_task,
                    "completed": completed,
                    "failed": failed,
                })
            })
            .collect();
        let tasks: Vec<Value> = self
            .tasks
            .iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "title": t.title,
                    "status": task_status_str(t.status),
                    "claimed_by": t.claimed_by,
                    "dependencies": t.dependencies,
                })
            })
            .collect();
        json!({
            "name": self.team.name,
            "objective": self.team.objective,
            "state": team_state_str(self.team.state),
            "members": members,
            "tasks": tasks,
        })
    }

    /// Some(ids of incomplete tasks) when there ARE incomplete tasks and
    /// NONE is claimable; None when no incomplete tasks or any Pending
    /// task with all deps Done exists.
    pub fn deadlock_check(&self) -> Option<Vec<String>> {
        let incomplete: Vec<&TeamTask> = self
            .tasks
            .iter()
            .filter(|t| t.status != TeamTaskStatus::Done)
            .collect();
        if incomplete.is_empty() {
            return None;
        }
        if incomplete.iter().any(|t| self.is_claimable(t)) {
            return None;
        }
        Some(incomplete.into_iter().map(|t| t.id.clone()).collect())
    }

    /// Wind down: stop every member child, release Running tasks to
    /// Pending, members Working -> Idle. Persists tasks.
    pub fn wind_down(&mut self, manager: &SubagentManager) {
        self.team.state = TeamState::WindingDown;
        for (_, id) in self.member_child_ids.iter() {
            let _ = manager.stop_child(*id, StopReason::OrchestratorRequested);
        }
        for task in self.tasks.iter_mut() {
            if task.status == TeamTaskStatus::Running {
                task.status = TeamTaskStatus::Pending;
                task.claimed_by = None;
            }
        }
        for m in self.team.members.iter_mut() {
            if m.status == MemberStatus::Working {
                m.status = MemberStatus::Idle;
            }
        }
        persist_or_warn(persist_tasks(&default_home(), self), "tasks");
    }

    /// Close: state=Closed; delete config.json + inboxes/ but KEEP
    /// tasks.json (the durable record of the team's work).
    pub fn close(&mut self) {
        self.team.state = TeamState::Closed;
        delete_team_files(&default_home(), &self.team.name, true);
    }
}

// ---------------------------------------------------------------------------
// Team tools (T005)
// ---------------------------------------------------------------------------

/// Resolve the team record for a tool call: the tool's bound team name
/// (feature 022 T006b: per-child bound instances) when set, else any
/// active team (unbound/global registration).
fn resolve_tool_record(
    bound: Option<&str>,
) -> Result<Arc<Mutex<TeamRecord>>, ToolResult> {
    bound
        .and_then(|t| global_teams().get(t))
        .or_else(|| global_teams().active_team().map(|(_, r)| r))
        .ok_or_else(|| ToolResult::Error("no active team".to_string()))
}

pub struct TeamStatusTool {
    pub team: Option<String>,
    pub member: Option<String>,
}

impl TeamStatusTool {
    pub fn new() -> Self {
        Self {
            team: None,
            member: None,
        }
    }
    pub fn bound(team: String, member: String) -> Self {
        Self {
            team: Some(team),
            member: Some(member),
        }
    }
}

impl Default for TeamStatusTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for TeamStatusTool {
    fn name(&self) -> &str {
        "team_status"
    }
    fn toolset(&self) -> &str {
        "team"
    }
    fn description(&self) -> &str {
        "Snapshot of the active team: members, their statuses, and the shared task list"
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {}, "additionalProperties": false})
    }
    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> ToolResult {
        let rec = match resolve_tool_record(self.team.as_deref()) {
            Ok(r) => r,
            Err(e) => return e,
        };
        let snapshot = rec.lock().unwrap().status_value();
        ToolResult::Text(serde_json::to_string_pretty(&snapshot).unwrap_or_default())
    }
}

pub struct TeamMessageTool {
    pub team: Option<String>,
    pub member: Option<String>,
    pub message_limit: usize,
}

impl TeamMessageTool {
    pub fn new(message_limit: usize) -> Self {
        Self {
            team: None,
            member: None,
            message_limit,
        }
    }
    pub fn bound(team: String, member: String, message_limit: usize) -> Self {
        Self {
            team: Some(team),
            member: Some(member),
            message_limit,
        }
    }
}

#[async_trait]
impl Tool for TeamMessageTool {
    fn name(&self) -> &str {
        "team_message"
    }
    fn toolset(&self) -> &str {
        "team"
    }
    fn description(&self) -> &str {
        "Send a message to a teammate's inbox (delivered directly, no relay)"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "to": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["to", "content"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let Some(to) = args.get("to").and_then(|v| v.as_str()) else {
            return ToolResult::Error("missing required parameter 'to'".to_string());
        };
        let Some(content) = args.get("content").and_then(|v| v.as_str()) else {
            return ToolResult::Error("missing required parameter 'content'".to_string());
        };
        let from = self.member.clone().unwrap_or_else(|| "unknown".to_string());
        let rec = match resolve_tool_record(self.team.as_deref()) {
            Ok(r) => r,
            Err(e) => return e,
        };
        let mut r = rec.lock().unwrap();
        match r.send(&from, to, content, self.message_limit) {
            Ok(()) => ToolResult::Text(format!("delivered to {to}")),
            Err(e) => ToolResult::Error(e),
        }
    }
}

pub struct TeamTasksTool {
    pub team: Option<String>,
    pub member: Option<String>,
}

impl TeamTasksTool {
    pub fn new() -> Self {
        Self {
            team: None,
            member: None,
        }
    }
    pub fn bound(team: String, member: String) -> Self {
        Self {
            team: Some(team),
            member: Some(member),
        }
    }
}

impl Default for TeamTasksTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for TeamTasksTool {
    fn name(&self) -> &str {
        "team_tasks"
    }
    fn toolset(&self) -> &str {
        "team"
    }
    fn description(&self) -> &str {
        "Shared team task list: add, list, claim, complete, or release tasks"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["add", "list", "claim", "complete", "release"]},
                "title": {"type": "string"},
                "task_id": {"type": "string"},
                "success": {"type": "boolean", "default": true},
                "dependencies": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["action"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let Some(action) = args.get("action").and_then(|v| v.as_str()) else {
            return ToolResult::Error("missing required parameter 'action'".to_string());
        };
        let rec = match resolve_tool_record(self.team.as_deref()) {
            Ok(r) => r,
            Err(e) => return e,
        };
        let mut r = rec.lock().unwrap();
        match action {
            "add" => {
                let Some(title) = args.get("title").and_then(|v| v.as_str()) else {
                    return ToolResult::Error(
                        "missing required parameter 'title' for action add".to_string(),
                    );
                };
                let dependencies: Vec<String> = args
                    .get("dependencies")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let id = r.add_task(title, dependencies);
                ToolResult::Text(
                    serde_json::to_string(&json!({"id": id})).unwrap_or_default(),
                )
            }
            "list" => ToolResult::Text(
                serde_json::to_string_pretty(&r.list()).unwrap_or_default(),
            ),
            "claim" => {
                let Some(task_id) = args.get("task_id").and_then(|v| v.as_str()) else {
                    return ToolResult::Error(
                        "missing required parameter 'task_id' for action claim".to_string(),
                    );
                };
                let member = self.member.clone().unwrap_or_else(|| "unknown".to_string());
                match r.claim(task_id, &member) {
                    Ok(t) => ToolResult::Text(
                        serde_json::to_string_pretty(&t).unwrap_or_default(),
                    ),
                    Err(e) => ToolResult::Error(e),
                }
            }
            "complete" => {
                let Some(task_id) = args.get("task_id").and_then(|v| v.as_str()) else {
                    return ToolResult::Error(
                        "missing required parameter 'task_id' for action complete".to_string(),
                    );
                };
                let success = args.get("success").and_then(|v| v.as_bool()).unwrap_or(true);
                match r.complete(task_id, success) {
                    Ok(status) => ToolResult::Text(
                        serde_json::to_string(&json!({
                            "id": task_id,
                            "status": task_status_str(status),
                        }))
                        .unwrap_or_default(),
                    ),
                    Err(e) => ToolResult::Error(e),
                }
            }
            "release" => {
                let Some(task_id) = args.get("task_id").and_then(|v| v.as_str()) else {
                    return ToolResult::Error(
                        "missing required parameter 'task_id' for action release".to_string(),
                    );
                };
                match r.release(task_id) {
                    Ok(()) => ToolResult::Text(format!("task '{task_id}' released to Pending")),
                    Err(e) => ToolResult::Error(e),
                }
            }
            other => ToolResult::Error(format!("unknown action '{other}'")),
        }
    }
}

/// Feature 022 (T006b): the three team tools bound to a specific team +
/// member identity, for injection into a team child's tool registry.
pub fn bound_team_tools(
    team: &str,
    member: &str,
    message_limit: usize,
) -> Vec<std::sync::Arc<dyn joey_tools::Tool>> {
    vec![
        std::sync::Arc::new(TeamStatusTool::bound(
            team.to_string(),
            member.to_string(),
        )),
        std::sync::Arc::new(TeamMessageTool::bound(
            team.to_string(),
            member.to_string(),
            message_limit,
        )),
        std::sync::Arc::new(TeamTasksTool::bound(
            team.to_string(),
            member.to_string(),
        )),
    ]
}

/// Outcome of a gated team spawn (contracts/team-tools.md §1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamSpawn {
    /// Mailbox identity registered for the child.
    pub member: String,
    /// True when this spawn lazily created the team — that child is the LEAD.
    pub is_lead: bool,
}

/// Feature 022: gate + lazily create/register a team spawn
/// (contracts/team-tools.md §1).
pub fn register_spawn(
    tree: &joey_core::Config,
    team: &str,
    name: Option<&str>,
    objective: &str,
    role: &str,
) -> Result<TeamSpawn, String> {
    if !tree.get_bool("hypercode.team.enabled", false) {
        return Err("team mode is disabled".to_string());
    }
    let reg = global_teams();
    if let Some(rec) = reg.get(team) {
        let mut r = rec.lock().unwrap();
        let name = match name {
            Some(n) => n.to_string(),
            None => {
                return Err(
                    "team spawns on an existing team require a unique 'name'".to_string(),
                )
            }
        };
        if r.team.members.iter().any(|m| m.name == name) {
            return Err(format!("member name '{name}' already exists in team '{team}'"));
        }
        let max = tree.get_i64("hypercode.team.max_members", 8).max(1) as usize;
        if r.team.members.len() >= max {
            return Err(format!("team '{team}' is at its member cap ({max})"));
        }
        let member_role = role;
        let _ = r.add_member(
            TeamMember {
                name: name.clone(),
                role: member_role.to_string(),
                model: String::new(),
                status: MemberStatus::Idle,
            },
            None,
        );
        Ok(TeamSpawn {
            member: name,
            is_lead: false,
        })
    } else {
        if let Some((active, _)) = reg.active_team() {
            return Err(format!(
                "cannot start team '{team}': team '{active}' is already active in this session (one active team per session); use subagent mode for this task"
            ));
        }
        let member = name.unwrap_or("lead").to_string();
        // create() makes the record + the lead's inbox but does NOT insert
        // the lead as a member row — add it explicitly.
        reg.create(team, objective, &member)?;
        if let Some(rec) = reg.get(team) {
            let mut r = rec.lock().unwrap();
            if !r.team.members.iter().any(|m| m.name == member) {
                let _ = r.add_member(
                    TeamMember {
                        name: member.clone(),
                        role: "lead".to_string(),
                        model: String::new(),
                        status: MemberStatus::Idle,
                    },
                    None,
                );
            }
        }
        Ok(TeamSpawn {
            member,
            is_lead: true,
        })
    }
}

/// Feature 022: record which child id backs a team member (in-memory only).
/// No-op when the team doesn't exist (e.g. already closed).
pub fn record_member_child_id(team: &str, member: &str, child_id: u64) {
    if let Some(rec) = global_teams().get(team) {
        let mut r = rec.lock().unwrap();
        r.member_child_ids.insert(member.to_string(), child_id);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Shared lock serializing env-guarded tests across this crate's lib test
/// binary (team.rs AND control_tool.rs tests mutate JOEY_HOME).
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_name(prefix: &str) -> String {
        format!("{}-{}", prefix, Uuid::new_v4().simple())
    }

    fn test_record(name: &str) -> TeamRecord {
        TeamRecord {
            team: Team {
                name: name.to_string(),
                objective: "test objective".to_string(),
                lead: "lead".to_string(),
                created_at: "0".to_string(),
                state: TeamState::Active,
                members: Vec::new(),
            },
            tasks: Vec::new(),
            inboxes: HashMap::new(),
            member_child_ids: HashMap::new(),
        }
    }

    fn member(name: &str) -> TeamMember {
        TeamMember {
            name: name.to_string(),
            role: "implementor".to_string(),
            model: "test-model".to_string(),
            status: MemberStatus::Idle,
        }
    }

    #[test]
    fn tasks_file_round_trip() {
        let home = tempfile::tempdir().unwrap();
        let name = unique_name("rt");
        let mut rec = test_record(&name);
        rec.tasks.push(TeamTask {
            id: "task_a".into(),
            title: "A".into(),
            status: TeamTaskStatus::Done,
            claimed_by: Some("m1".into()),
            dependencies: vec![],
        });
        rec.tasks.push(TeamTask {
            id: "task_b".into(),
            title: "B".into(),
            status: TeamTaskStatus::Pending,
            claimed_by: None,
            dependencies: vec!["task_a".into()],
        });
        persist_tasks(home.path(), &rec).unwrap();
        assert_eq!(load_tasks(home.path(), &name), Some(rec.tasks.clone()));
        let raw =
            std::fs::read_to_string(team_dir(home.path(), &name).join("tasks.json")).unwrap();
        assert!(raw.contains("\"pending\""), "raw: {raw}");
        assert!(!raw.contains("\"Pending\""), "raw: {raw}");
    }

    #[test]
    fn member_name_uniqueness() {
        let mut rec = test_record(&unique_name("mn"));
        rec.add_member(member("alice"), None).unwrap();
        let err = rec.add_member(member("alice"), None).unwrap_err();
        assert!(err.contains("already exists"), "err: {err}");
    }

    #[test]
    fn dependency_blocking() {
        let mut rec = test_record(&unique_name("db"));
        rec.add_member(member("m1"), None).unwrap();
        let a = rec.add_task("A", vec![]);
        let b = rec.add_task("B", vec![a.clone()]);
        let err = rec.claim(&b, "m1").unwrap_err();
        assert_eq!(err, "task not claimable");
        rec.claim(&a, "m1").unwrap();
        rec.complete(&a, true).unwrap();
        assert!(rec.claim(&b, "m1").is_ok());
    }

    #[test]
    fn claim_race_single_winner() {
        let mut r = test_record(&unique_name("cr"));
        for i in 0..8 {
            r.add_member(member(&format!("m{i}")), None).unwrap();
        }
        let task_id = r.add_task("only", vec![]);
        let rec = Arc::new(Mutex::new(r));
        let mut handles = Vec::new();
        for i in 0..8 {
            let rec = Arc::clone(&rec);
            let tid = task_id.clone();
            handles.push(std::thread::spawn(move || {
                rec.lock().unwrap().claim(&tid, &format!("m{i}"))
            }));
        }
        let mut ok = 0;
        let mut errs = 0;
        for h in handles {
            match h.join().unwrap() {
                Ok(_) => ok += 1,
                Err(e) => {
                    assert_eq!(e, "task not claimable");
                    errs += 1;
                }
            }
        }
        assert_eq!(ok, 1);
        assert_eq!(errs, 7);
    }

    #[test]
    fn release_semantics() {
        let mut rec = test_record(&unique_name("rs"));
        rec.add_member(member("m1"), None).unwrap();
        let id = rec.add_task("t", vec![]);
        rec.claim(&id, "m1").unwrap();
        rec.release(&id).unwrap();
        let t = &rec.list()[0];
        assert_eq!(t.status, TeamTaskStatus::Pending);
        assert!(t.claimed_by.is_none());
        let err = rec.release(&id).unwrap_err();
        assert!(err.contains("is not Running"), "err: {err}");
    }

    #[test]
    fn mailbox_delivery_cap_and_unknown_recipient() {
        let mut rec = test_record(&unique_name("mb"));
        rec.add_member(member("alice"), None).unwrap();
        rec.add_member(member("bob"), None).unwrap();
        for i in 0..12 {
            rec.send("alice", "bob", &format!("msg-{i}"), 10).unwrap();
        }
        let got = rec.poll("bob");
        assert_eq!(got.len(), 10);
        let contents: Vec<&str> = got.iter().map(|m| m.content.as_str()).collect();
        // Oldest 2 dropped, newest kept.
        assert!(!contents.contains(&"msg-0"));
        assert!(!contents.contains(&"msg-1"));
        assert!(contents.contains(&"msg-11"));
        let err = rec.send("alice", "carol", "hi", 10).unwrap_err();
        assert!(err.contains("is not a member"), "err: {err}");
    }

    #[test]
    fn receive_drains_and_poll_keeps() {
        let mut rec = test_record(&unique_name("dr"));
        rec.add_member(member("alice"), None).unwrap();
        rec.add_member(member("bob"), None).unwrap();
        rec.send("alice", "bob", "one", 10).unwrap();
        rec.send("alice", "bob", "two", 10).unwrap();
        assert_eq!(rec.receive("bob").len(), 2);
        assert!(rec.receive("bob").is_empty());
        rec.send("alice", "bob", "three", 10).unwrap();
        assert_eq!(rec.poll("bob").len(), 1);
        assert_eq!(rec.poll("bob").len(), 1);
        assert_eq!(rec.receive("bob").len(), 1);
    }

    #[test]
    fn status_value_shape() {
        let mut rec = test_record(&unique_name("sv"));
        rec.add_member(member("m1"), None).unwrap();
        rec.add_member(member("m2"), None).unwrap();
        let a = rec.add_task("A", vec![]);
        let b = rec.add_task("B", vec![]);
        let c = rec.add_task("C", vec![]);
        rec.claim(&a, "m1").unwrap();
        rec.complete(&a, true).unwrap();
        rec.claim(&b, "m1").unwrap();
        rec.claim(&c, "m2").unwrap();
        rec.complete(&c, false).unwrap();
        let v = rec.status_value();
        assert_eq!(v["state"], "active");
        let members = v["members"].as_array().unwrap().clone();
        let m1 = members.iter().find(|m| m["name"] == "m1").unwrap();
        assert_eq!(m1["current_task"], json!(b));
        assert_eq!(m1["completed"], json!(1));
        assert_eq!(m1["failed"], json!(0));
        assert_eq!(m1["status"], "working");
        let m2 = members.iter().find(|m| m["name"] == "m2").unwrap();
        assert_eq!(m2["completed"], json!(0));
        assert_eq!(m2["failed"], json!(1));
        let statuses: Vec<&str> = v["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["status"].as_str().unwrap())
            .collect();
        assert!(statuses.contains(&"done"));
        assert!(statuses.contains(&"running"));
        assert!(statuses.contains(&"failed"));
        assert!(statuses.iter().all(|s| !s.chars().any(|c| c.is_uppercase())));
    }

    #[test]
    fn add_task_id_format() {
        let mut rec = test_record(&unique_name("id"));
        let id = rec.add_task("t", vec![]);
        assert!(id.starts_with("task_"));
        assert!(id.len() > 10);
    }

    // Env-guarded (JOEY_HOME) — close + purge assertions share ONE test fn
    // to avoid parallel env races. wind_down/close persist via
    // default_home(), hence the env var.
    #[test]
    fn lifecycle_close_and_purge() {
        let _env = crate::team::TEST_ENV_LOCK.lock().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("JOEY_HOME", home.path());
        let name = unique_name("lc");
        let rec = global_teams().create(&name, "objective", "lead").unwrap();
        {
            let mut r = rec.lock().unwrap();
            r.add_member(member("m1"), None).unwrap();
            let a = r.add_task("A", vec![]);
            let b = r.add_task("B", vec![]);
            r.claim(&a, "m1").unwrap();
            r.complete(&a, true).unwrap();
            r.claim(&b, "m1").unwrap();
            let mgr = SubagentManager::new(crate::manager::ManagerConfig::default());
            r.wind_down(&mgr);
            assert_eq!(r.team.state, TeamState::WindingDown);
            let bt = r.tasks.iter().find(|t| t.id == b).unwrap();
            assert_eq!(bt.status, TeamTaskStatus::Pending);
            assert!(bt.claimed_by.is_none());
            let dir = team_dir(home.path(), &name);
            assert!(dir.join("tasks.json").exists());
            r.close();
            assert_eq!(r.team.state, TeamState::Closed);
            assert!(!dir.join("config.json").exists());
            assert!(!dir.join("inboxes").exists());
            assert!(dir.join("tasks.json").exists());
            assert!(load_tasks(home.path(), &name).is_some());
        }
        // purge_expired: 0-day cutoff removes anything strictly older than
        // now; a fresh dir survives a 7-day cutoff.
        let stale = team_dir(home.path(), &unique_name("stale"));
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("tasks.json"), "{\"tasks\":[]}").unwrap();
        std::thread::sleep(Duration::from_millis(1200));
        let removed = global_teams().purge_expired(home.path(), 0);
        assert!(removed >= 1, "removed: {removed}");
        assert!(!stale.exists());
        let fresh = team_dir(home.path(), &unique_name("fresh"));
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::write(fresh.join("tasks.json"), "{\"tasks\":[]}").unwrap();
        let removed_fresh = global_teams().purge_expired(home.path(), 7);
        assert_eq!(removed_fresh, 0);
        assert!(fresh.exists());
        std::env::remove_var("JOEY_HOME");
    }

    #[test]
    fn deadlock_check() {
        // A Done, B Pending dep A -> B claimable -> None.
        let mut rec = test_record(&unique_name("dl"));
        rec.add_member(member("m1"), None).unwrap();
        let a = rec.add_task("A", vec![]);
        rec.add_task("B", vec![a.clone()]);
        rec.claim(&a, "m1").unwrap();
        rec.complete(&a, true).unwrap();
        assert!(rec.deadlock_check().is_none());
        // A Failed, B Pending dep A -> nothing claimable -> Some.
        let mut rec2 = test_record(&unique_name("dl"));
        rec2.add_member(member("m1"), None).unwrap();
        let a2 = rec2.add_task("A", vec![]);
        let b2 = rec2.add_task("B", vec![a2.clone()]);
        rec2.claim(&a2, "m1").unwrap();
        rec2.complete(&a2, false).unwrap();
        let ids = rec2.deadlock_check().unwrap();
        assert!(ids.contains(&a2));
        assert!(ids.contains(&b2));
        // All Done -> None.
        let mut rec3 = test_record(&unique_name("dl"));
        rec3.add_member(member("m1"), None).unwrap();
        let a3 = rec3.add_task("A", vec![]);
        rec3.claim(&a3, "m1").unwrap();
        rec3.complete(&a3, true).unwrap();
        assert!(rec3.deadlock_check().is_none());
    }

    #[test]
    fn release_member_tasks() {
        let mut rec = test_record(&unique_name("rm"));
        rec.add_member(member("m"), None).unwrap();
        let t1 = rec.add_task("T1", vec![]);
        let t2 = rec.add_task("T2", vec![]);
        rec.claim(&t1, "m").unwrap();
        rec.claim(&t2, "m").unwrap();
        assert_eq!(rec.release_member_tasks("m"), 2);
        assert!(rec
            .tasks
            .iter()
            .all(|t| t.status == TeamTaskStatus::Pending && t.claimed_by.is_none()));
        assert_eq!(rec.team.members[0].status, MemberStatus::Stopped);
    }

    #[test]
    fn lead_and_teammate_directive_helpers() {
        let lead = team_lead_directive(8, 4);
        assert!(lead.contains("at most 4 teammates"));
        assert!(lead.contains("hard member maximum"));
        let mate = teammate_directive(500);
        assert!(mate.contains("every 500ms"));
    }

    #[test]
    fn notice_board_cap() {
        let reg = global_teams();
        for i in 0..70 {
            reg.push_notice(format!("notice-{i}"));
        }
        let tail = reg.notice_board_tail(100);
        assert_eq!(tail.len(), 64);
        assert!(!tail.contains(&"notice-0".to_string()));
        assert!(tail.contains(&"notice-69".to_string()));
        let last5 = reg.notice_board_tail(5);
        assert_eq!(last5.len(), 5);
        assert_eq!(last5[4], "notice-69");
    }

    // ------------------------------------------------------------------
    // Feature 022: register_spawn gating + record_member_child_id.
    // Env-guarded (JOEY_HOME) — register_spawn persists via
    // default_home(), so these serialize against each other AND
    // lifecycle_close_and_purge via ENV_LOCK (no parallel env races).
    // ------------------------------------------------------------------

    // Env-guarded tests serialize on the crate-shared lock
    // (crate::team::TEST_ENV_LOCK — see team.rs top of tests section).

    fn tree_with(yaml: &str) -> joey_core::Config {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), yaml).unwrap();
        joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap()
    }

    #[test]
    fn register_spawn_disabled_errors() {
        let _env = crate::team::TEST_ENV_LOCK.lock().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("JOEY_HOME", home.path());
        let tree_off = tree_with("hypercode:\n  team:\n    enabled: false\n");
        let err = register_spawn(&tree_off, "t-disabled", None, "obj", "explorer").unwrap_err();
        assert_eq!(err, "team mode is disabled");
        let tree_absent = tree_with("");
        let err = register_spawn(&tree_absent, "t-disabled", None, "obj", "explorer").unwrap_err();
        assert_eq!(err, "team mode is disabled");
        std::env::remove_var("JOEY_HOME");
    }

    #[test]
    fn register_spawn_lazy_creates_lead() {
        let _env = crate::team::TEST_ENV_LOCK.lock().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("JOEY_HOME", home.path());
        let tree = tree_with("hypercode:\n  team:\n    enabled: true\n");
        let team = unique_name("t-lazy");
        let spawn = register_spawn(&tree, &team, None, "obj", "explorer").unwrap();
        assert_eq!(spawn.member, "lead");
        assert!(spawn.is_lead);
        let rec = global_teams().get(&team).expect("team created");
        {
            let mut r = rec.lock().unwrap();
            assert_eq!(r.team.members.len(), 1);
            assert_eq!(r.team.members[0].name, "lead");
            assert_eq!(r.team.members[0].role, "lead");
            assert_eq!(r.team.lead, "lead");
            // Global registry: close so later tests can create their own
            // teams (one active team per session).
            r.close();
        }
        std::env::remove_var("JOEY_HOME");
    }

    #[test]
    fn register_spawn_second_member_and_dup() {
        let _env = crate::team::TEST_ENV_LOCK.lock().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("JOEY_HOME", home.path());
        let tree = tree_with("hypercode:\n  team:\n    enabled: true\n");
        let team = unique_name("t-second");
        register_spawn(&tree, &team, None, "obj", "explorer").unwrap();
        // Second member with a unique name: member "bob", not the lead.
        let spawn = register_spawn(&tree, &team, Some("bob"), "obj", "explorer").unwrap();
        assert_eq!(spawn.member, "bob");
        assert!(!spawn.is_lead);
        // Duplicate name errors "already exists".
        let err = register_spawn(&tree, &team, Some("bob"), "obj", "explorer").unwrap_err();
        assert!(err.contains("already exists"), "err: {err}");
        // No name on an existing team errors.
        let err = register_spawn(&tree, &team, None, "obj", "explorer").unwrap_err();
        assert!(err.contains("require a unique 'name'"), "err: {err}");
        if let Some(rec) = global_teams().get(&team) {
            rec.lock().unwrap().close();
        }
        std::env::remove_var("JOEY_HOME");
    }

    #[test]
    fn register_spawn_cap() {
        let _env = crate::team::TEST_ENV_LOCK.lock().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("JOEY_HOME", home.path());
        let tree = tree_with(
            "hypercode:\n  team:\n    enabled: true\n    max_members: 2\n",
        );
        let team = unique_name("t-cap");
        register_spawn(&tree, &team, None, "obj", "explorer").unwrap();
        register_spawn(&tree, &team, Some("bob"), "obj", "explorer").unwrap();
        let err = register_spawn(&tree, &team, Some("carol"), "obj", "explorer").unwrap_err();
        assert!(err.contains("member cap"), "err: {err}");
        assert!(err.contains(&team), "err: {err}");
        if let Some(rec) = global_teams().get(&team) {
            rec.lock().unwrap().close();
        }
        std::env::remove_var("JOEY_HOME");
    }

    #[test]
    fn register_spawn_second_team_refused() {
        let _env = crate::team::TEST_ENV_LOCK.lock().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("JOEY_HOME", home.path());
        let tree = tree_with("hypercode:\n  team:\n    enabled: true\n");
        let first = unique_name("t-first");
        register_spawn(&tree, &first, None, "obj", "explorer").unwrap();
        let second = unique_name("t-refused");
        let err = register_spawn(&tree, &second, None, "obj", "explorer").unwrap_err();
        assert!(err.contains("one active team per session"), "err: {err}");
        assert!(err.contains("subagent mode"), "err: {err}");
        if let Some(rec) = global_teams().get(&first) {
            rec.lock().unwrap().close();
        }
        std::env::remove_var("JOEY_HOME");
    }

    #[test]
    fn record_member_child_id_roundtrip() {
        let _env = crate::team::TEST_ENV_LOCK.lock().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("JOEY_HOME", home.path());
        let tree = tree_with("hypercode:\n  team:\n    enabled: true\n");
        let team = unique_name("t-child-id");
        register_spawn(&tree, &team, None, "obj", "explorer").unwrap();
        record_member_child_id(&team, "lead", 7);
        assert_eq!(
            global_teams().member_by_child_id(7),
            Some((team.clone(), "lead".to_string()))
        );
        assert_eq!(global_teams().member_by_child_id(4242), None);
        if let Some(rec) = global_teams().get(&team) {
            rec.lock().unwrap().close();
        }
        std::env::remove_var("JOEY_HOME");
    }

    #[test]
    fn stop_team_winds_down_and_closes() {
        let _env = crate::team::TEST_ENV_LOCK.lock().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("JOEY_HOME", home.path());
        let tree = tree_with("hypercode:\n  team:\n    enabled: true\n");
        let team = unique_name("t-stop");
        register_spawn(&tree, &team, None, "obj", "explorer").unwrap();
        let rec = global_teams().get(&team).expect("team created");
        let running_task = {
            let mut r = rec.lock().unwrap();
            let id = r.add_task("only", vec![]);
            r.claim(&id, "lead").unwrap();
            id
        };
        let mgr = SubagentManager::new(crate::manager::ManagerConfig::default());
        global_teams().stop_team(&team, &mgr).unwrap();
        {
            let r = rec.lock().unwrap();
            assert_eq!(r.team.state, TeamState::Closed);
            let t = r.tasks.iter().find(|t| t.id == running_task).unwrap();
            assert_eq!(t.status, TeamTaskStatus::Pending);
        }
        let dir = home.path().join("teams").join(sanitize_name(&team));
        assert!(!dir.join("config.json").exists());
        assert!(dir.join("tasks.json").exists());
        std::env::remove_var("JOEY_HOME");
    }
}
