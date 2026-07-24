//! Team mode: parallel multi-agent orchestration via shared mailbox + task list.
//!
//! Port of OMO's team mode. OFF by default (FR-041). When enabled, team members
//! coordinate via a shared mailbox and shared task list.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

// ── TeamModeConfig ──────────────────────────────────────────────────

/// Configuration for team mode (T119). OFF by default.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamModeConfig {
    /// Whether team mode is enabled (default: false — FR-041).
    #[serde(default)]
    pub enabled: bool,
    /// Maximum parallel members (default: 4).
    #[serde(default = "default_max_parallel")]
    pub max_parallel_members: usize,
    /// Maximum total members (default: 8).
    #[serde(default = "default_max_members")]
    pub max_members: usize,
    /// Message limits per poll cycle.
    #[serde(default = "default_message_limit")]
    pub message_limit: usize,
    /// Polling interval in milliseconds.
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,
    /// Whether to use tmux visualization.
    #[serde(default)]
    pub tmux_visualization: bool,
}

fn default_max_parallel() -> usize { 4 }
fn default_max_members() -> usize { 8 }
fn default_message_limit() -> usize { 10 }
fn default_poll_interval() -> u64 { 500 }

impl Default for TeamModeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_parallel_members: default_max_parallel(),
            max_members: default_max_members(),
            message_limit: default_message_limit(),
            poll_interval_ms: default_poll_interval(),
            tmux_visualization: false,
        }
    }
}

// ── TeamMember ──────────────────────────────────────────────────────

/// A team member specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    /// Member name/label.
    pub name: String,
    /// Kind: category-based or subagent_type-based.
    pub kind: TeamMemberKind,
    /// Custom prompt for this member.
    pub prompt: Option<String>,
}

/// How a team member is spawned.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TeamMemberKind {
    /// Spawn via a category name (e.g. "quick").
    Category { category: String },
    /// Spawn via a subagent type name (e.g. "explore").
    SubagentType { subagent_type: String },
}

// ── TeamSpec ────────────────────────────────────────────────────────

/// A complete team specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamSpec {
    pub name: String,
    pub members: Vec<TeamMember>,
}

// ── TeamMailbox ─────────────────────────────────────────────────────

/// A shared in-memory mailbox for inter-member message passing (T121).
#[derive(Debug, Clone, Default)]
pub struct TeamMailbox {
    messages: Arc<Mutex<Vec<TeamMessage>>>,
}

/// A message between team members.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMessage {
    pub from: String,
    pub to: String,
    pub content: String,
    pub timestamp: String,
}

impl TeamMailbox {
    pub fn new() -> Self {
        Self::default()
    }

    /// Send a message to a member.
    pub fn send(&self, from: &str, to: &str, content: &str) {
        let msg = TeamMessage {
            from: from.to_string(),
            to: to.to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        self.messages.lock().unwrap().push(msg);
    }

    /// Receive all messages addressed to a member (and remove them).
    pub fn receive(&self, member: &str) -> Vec<TeamMessage> {
        let mut msgs = self.messages.lock().unwrap();
        let (to_member, rest): (Vec<TeamMessage>, Vec<TeamMessage>) = msgs
            .drain(..)
            .partition(|m| m.to == member);
        *msgs = rest;
        to_member
    }

    /// Poll for messages (non-destructive peek).
    pub fn poll(&self, member: &str) -> Vec<TeamMessage> {
        self.messages
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.to == member)
            .cloned()
            .collect()
    }
}

// ── TeamTaskList ────────────────────────────────────────────────────

/// A shared task list for cross-member coordination (T122).
#[derive(Debug, Clone, Default)]
pub struct TeamTaskList {
    tasks: Arc<Mutex<Vec<TeamTask>>>,
}

/// A task in the shared task list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamTask {
    pub id: String,
    pub title: String,
    pub status: TeamTaskStatus,
    pub claimed_by: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TeamTaskStatus {
    Pending,
    Running,
    Done,
    Failed,
}

impl TeamTaskList {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a task.
    pub fn add(&self, title: &str) -> String {
        let id = format!("task_{}", uuid::Uuid::new_v4().simple());
        self.tasks.lock().unwrap().push(TeamTask {
            id: id.clone(),
            title: title.to_string(),
            status: TeamTaskStatus::Pending,
            claimed_by: None,
        });
        id
    }

    /// Claim a task (atomic).
    pub fn claim(&self, task_id: &str, member: &str) -> bool {
        let mut tasks = self.tasks.lock().unwrap();
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id && t.status == TeamTaskStatus::Pending) {
            task.status = TeamTaskStatus::Running;
            task.claimed_by = Some(member.to_string());
            true
        } else {
            false
        }
    }

    /// Complete a task.
    pub fn complete(&self, task_id: &str, success: bool) {
        let mut tasks = self.tasks.lock().unwrap();
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            task.status = if success { TeamTaskStatus::Done } else { TeamTaskStatus::Failed };
        }
    }

    /// List all tasks.
    pub fn list(&self) -> Vec<TeamTask> {
        self.tasks.lock().unwrap().clone()
    }
}

// ── Eligibility Validation (T120) ───────────────────────────────────

/// Check if an agent is eligible for team membership (FR-042, T120).
///
/// - Eligible: sisyphus, atlas, sisyphus-junior
/// - Conditional: hephaestus (with permission)
/// - Hard-reject: oracle, librarian, explore, multimodal-looker, metis, momus, prometheus
pub fn validate_team_eligibility(agent_name: &str) -> TeamEligibility {
    match agent_name {
        "sisyphus" | "atlas" | "sisyphus-junior" => TeamEligibility::Eligible,
        "hephaestus" => TeamEligibility::Conditional,
        "oracle" | "librarian" | "explore" | "multimodal-looker" | "metis" | "momus"
        | "prometheus" => TeamEligibility::Rejected,
        _ => TeamEligibility::Rejected,
    }
}

/// Result of team eligibility validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamEligibility {
    /// Fully eligible.
    Eligible,
    /// Eligible with conditions (e.g. requires permission).
    Conditional,
    /// Hard reject.
    Rejected,
}

impl TeamEligibility {
    pub fn is_eligible(self) -> bool {
        matches!(self, Self::Eligible | Self::Conditional)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T124: eligibility validation.
    #[test]
    fn team_eligibility_validation() {
        assert_eq!(validate_team_eligibility("sisyphus"), TeamEligibility::Eligible);
        assert_eq!(validate_team_eligibility("atlas"), TeamEligibility::Eligible);
        assert_eq!(validate_team_eligibility("sisyphus-junior"), TeamEligibility::Eligible);
        assert_eq!(validate_team_eligibility("hephaestus"), TeamEligibility::Conditional);
        assert_eq!(validate_team_eligibility("oracle"), TeamEligibility::Rejected);
        assert_eq!(validate_team_eligibility("librarian"), TeamEligibility::Rejected);
        assert_eq!(validate_team_eligibility("prometheus"), TeamEligibility::Rejected);
    }

    /// T125: team mode disabled by default.
    #[test]
    fn team_mode_disabled_by_default() {
        let config = TeamModeConfig::default();
        assert!(!config.enabled, "team mode must be OFF by default (FR-041)");
        assert_eq!(config.max_parallel_members, 4);
        assert_eq!(config.max_members, 8);
    }

    #[test]
    fn team_mailbox_send_receive() {
        let mailbox = TeamMailbox::new();
        mailbox.send("lead", "worker1", "do task A");
        mailbox.send("lead", "worker2", "do task B");

        let worker1_msgs = mailbox.receive("worker1");
        assert_eq!(worker1_msgs.len(), 1);
        assert_eq!(worker1_msgs[0].content, "do task A");

        // Second receive is empty (already consumed)
        assert!(mailbox.receive("worker1").is_empty());
    }

    #[test]
    fn team_task_list_claim_complete() {
        let tasks = TeamTaskList::new();
        let id = tasks.add("Implement feature");
        assert!(tasks.claim(&id, "worker1"));
        assert!(!tasks.claim(&id, "worker2")); // Already claimed
        tasks.complete(&id, true);
        let all = tasks.list();
        assert_eq!(all[0].status, TeamTaskStatus::Done);
    }
}
