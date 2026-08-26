//! Process-global delegation event tap (parallel-subagent feature).
//!
//! The `delegate_task` tool is registered once per agent build with
//! `event_tx: None` (events flow through per-turn channels at runtime — but
//! only for the MAIN agent's events; delegation events had no route to any
//! UI). Hosts that want live subagent events (the TUI's per-subagent panes)
//! install a global tap: every `SubagentManager` in this process — including
//! managers built later by engine restarts — mirrors its orchestration +
//! wrapped child events to it.
//!
//! Process-scoped by design: each joey process (interactive session, cron
//! worker, oneshot) has its own global, so sessions never cross-talk. A
//! manager-local tap (see [`crate::manager::SubagentManager::set_event_tap`])
//! takes precedence when set; the global is the fallback.

use joey_agent_core::AgentEvent;
use std::sync::Mutex;
use tokio::sync::mpsc;

static GLOBAL_TAP: Mutex<Option<mpsc::UnboundedSender<AgentEvent>>> = Mutex::new(None);

/// Install (or remove, with `None`) the process-global delegation event tap.
pub fn set_global_tap(tap: Option<mpsc::UnboundedSender<AgentEvent>>) {
    *GLOBAL_TAP.lock().unwrap_or_else(|p| p.into_inner()) = tap;
}

/// The process-global tap sender, if installed.
pub fn global_tap() -> Option<mpsc::UnboundedSender<AgentEvent>> {
    GLOBAL_TAP
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_clear_roundtrip() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        set_global_tap(Some(tx));
        let tap = global_tap().expect("tap installed");
        tap.send(AgentEvent::Notice("hello".into())).unwrap();
        assert!(matches!(rx.try_recv(), Ok(AgentEvent::Notice(_))));
        set_global_tap(None);
        assert!(global_tap().is_none());
    }
}
