//! Agent mode and tool permission types.
//!
//! Port of OMO's `AgentMode` and tool permission system (data-model.md).

// ── AgentMode ───────────────────────────────────────────────────────

/// Whether an agent is Tab-selectable (Primary) or delegation-invoked
/// (Subagent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentMode {
    /// Tab-selectable: sisyphus, hephaestus, prometheus, atlas.
    Primary,
    /// Delegation-invoked: oracle, librarian, explore, etc.
    Subagent,
}

impl AgentMode {
    pub fn is_primary(self) -> bool {
        matches!(self, Self::Primary)
    }

    pub fn is_subagent(self) -> bool {
        matches!(self, Self::Subagent)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Primary => "Primary",
            Self::Subagent => "Sub",
        }
    }
}

// ── ToolPermissions ─────────────────────────────────────────────────

/// Per-tool allow/deny permissions with deny precedence.
///
/// A tool is allowed if it's in the allow set (or allow is empty = allow all)
/// AND not in the deny set. Deny always wins over allow.
///
/// This models Sisyphus-Junior's permission model: deny `task`/delegate_task,
/// allow `call_omo_agent`, default-deny all other delegation.
#[derive(Debug, Clone, Default)]
pub struct ToolPermissions {
    /// If non-empty, only these tools are allowed (beyond the defaults).
    allow: Vec<String>,
    /// Tools explicitly denied (deny precedence over allow).
    deny: Vec<String>,
}

impl ToolPermissions {
    /// Create with no restrictions (allow all).
    pub fn allow_all() -> Self {
        Self {
            allow: Vec::new(),
            deny: Vec::new(),
        }
    }

    /// Create from explicit allow + deny lists.
    pub fn new(allow: Vec<String>, deny: Vec<String>) -> Self {
        Self { allow, deny }
    }

    /// Add a tool to the allow list.
    pub fn allow(&mut self, tool: &str) {
        self.allow.push(tool.to_string());
    }

    /// Add a tool to the deny list.
    pub fn deny(&mut self, tool: &str) {
        self.deny.push(tool.to_string());
    }

    /// Is this tool permitted? Deny precedence over allow.
    /// If allow is empty, all non-denied tools are permitted.
    pub fn is_allowed(&self, tool: &str) -> bool {
        // Deny precedence: if in deny list, always false.
        if self.deny.iter().any(|t| t == tool) {
            return false;
        }
        // If allow list is empty, allow everything (not denied).
        if self.allow.is_empty() {
            return true;
        }
        // Otherwise, must be explicitly allowed.
        self.allow.iter().any(|t| t == tool)
    }

    /// Is this tool explicitly denied?
    pub fn is_denied(&self, tool: &str) -> bool {
        self.deny.iter().any(|t| t == tool)
    }

    /// Reference the allow list.
    pub fn allowed(&self) -> &[String] {
        &self.allow
    }

    /// Reference the deny list.
    pub fn denied(&self) -> &[String] {
        &self.deny
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_mode_helpers() {
        assert!(AgentMode::Primary.is_primary());
        assert!(!AgentMode::Primary.is_subagent());
        assert_eq!(AgentMode::Primary.label(), "Primary");
        assert_eq!(AgentMode::Subagent.label(), "Sub");
    }

    #[test]
    fn tool_permissions_allow_all_default() {
        let perms = ToolPermissions::allow_all();
        assert!(perms.is_allowed("anything"));
        assert!(perms.is_allowed("task"));
    }

    #[test]
    fn tool_permissions_deny_precedence() {
        let mut perms = ToolPermissions::new(vec!["task".into()], vec![]);
        perms.deny("task");
        // In both allow and deny → deny wins
        assert!(!perms.is_allowed("task"));
        assert!(perms.is_denied("task"));
    }

    #[test]
    fn junior_permissions_blocks_task_allows_call_omo_agent() {
        // T053: Sisyphus-Junior tool_permissions denies `task` and allows
        // `call_omo_agent`
        let perms = ToolPermissions::new(
            vec!["call_omo_agent".to_string()],
            vec!["task".to_string(), "delegate_task".to_string()],
        );
        assert!(!perms.is_allowed("task"), "task should be denied");
        assert!(!perms.is_allowed("delegate_task"), "delegate_task should be denied");
        assert!(perms.is_allowed("call_omo_agent"), "call_omo_agent should be allowed");
    }
}
