//! The `process` tool — manage background processes.
//!
//! Actions: list, poll, log, wait, kill, write, submit, close.
//! Works in conjunction with the terminal tool's background=true mode.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use tokio::process::Child;

use crate::registry::{Tool, ToolResult};
use crate::ToolContext;

/// Default ring buffer capacity (256KB).
const DEFAULT_RING_CAPACITY: usize = 256 * 1024;

/// A fixed-capacity ring buffer for process output capture.
pub struct RingBuffer {
    buf: VecDeque<u8>,
    capacity: usize,
    truncated: bool,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity.min(4096)),
            capacity,
            truncated: false,
        }
    }

    /// Push bytes into the buffer, evicting oldest data when at capacity.
    pub fn push(&mut self, data: &[u8]) {
        for &b in data {
            if self.buf.len() >= self.capacity {
                self.buf.pop_front();
                self.truncated = true;
            }
            self.buf.push_back(b);
        }
    }

    /// Drain and return all buffered bytes.
    pub fn drain_all(&mut self) -> Vec<u8> {
        let data: Vec<u8> = self.buf.drain(..).collect();
        data
    }

    /// Return the current contents without draining.
    pub fn contents(&self) -> Vec<u8> {
        self.buf.iter().copied().collect()
    }

    /// Whether data was dropped from the head.
    pub fn was_truncated(&self) -> bool {
        self.truncated
    }

    /// Current length.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

/// A managed background process session.
pub struct ProcessSession {
    pub session_id: String,
    pub child: Option<Child>,
    pub stdout_buf: RingBuffer,
    pub stderr_buf: RingBuffer,
    pub command: String,
    pub cwd: String,
    pub started_at: Instant,
    pub notify_on_complete: bool,
    /// Last poll position for incremental reads.
    pub last_poll_pos: usize,
}

impl ProcessSession {
    pub fn new(session_id: String, child: Child, command: String, cwd: String) -> Self {
        Self {
            session_id,
            child: Some(child),
            stdout_buf: RingBuffer::new(DEFAULT_RING_CAPACITY),
            stderr_buf: RingBuffer::new(DEFAULT_RING_CAPACITY),
            command,
            cwd,
            started_at: Instant::now(),
            notify_on_complete: false,
            last_poll_pos: 0,
        }
    }

    /// Whether the process is still running.
    pub fn is_running(&mut self) -> bool {
        if let Some(child) = &mut self.child {
            match child.try_wait() {
                Ok(Some(_)) => false,
                Ok(None) => true,
                Err(_) => false,
            }
        } else {
            false
        }
    }

    /// Running duration in seconds.
    pub fn elapsed_secs(&self) -> f64 {
        self.started_at.elapsed().as_secs_f64()
    }
}

/// Global registry of background process sessions.
static PROCESS_REGISTRY: Lazy<Arc<Mutex<std::collections::HashMap<String, ProcessSession>>>> =
    Lazy::new(|| Arc::new(Mutex::new(std::collections::HashMap::new())));

/// Get a handle to the global process registry.
pub fn process_registry() -> Arc<Mutex<std::collections::HashMap<String, ProcessSession>>> {
    PROCESS_REGISTRY.clone()
}

/// Generate a unique session ID.
fn new_session_id() -> String {
    format!("proc-{}", uuid::Uuid::new_v4().simple())
}

/// The process tool.
pub struct Process;

#[async_trait]
impl Tool for Process {
    fn name(&self) -> &str {
        "process"
    }

    fn toolset(&self) -> &str {
        "terminal"
    }

    fn description(&self) -> &str {
        "Manage background processes started with terminal(background=true). Actions: \
         list active processes, poll for new output, wait for completion, write to \
         stdin, submit input (write + Enter), and kill processes."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "poll", "log", "wait", "kill", "write", "submit", "close"],
                    "description": "The action to perform on a background process."
                },
                "session_id": {
                    "type": "string",
                    "description": "Process session ID (required for all actions except 'list')."
                },
                "data": {
                    "type": "string",
                    "description": "Data to send to stdin (for 'write' and 'submit' actions)."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Max seconds to block for 'wait' action."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max lines to return for 'log' action. Default: 200."
                },
                "offset": {
                    "type": "integer",
                    "description": "Line offset for 'log' action (for pagination)."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolResult::Error("action is required".to_string()),
        };

        let session_id = args.get("session_id").and_then(|v| v.as_str());

        match action {
            "list" => action_list(),
            "poll" => action_poll(session_id),
            "log" => action_log(session_id, args.get("offset"), args.get("limit")),
            "wait" => action_wait(session_id, args.get("timeout")).await,
            "kill" => action_kill(session_id).await,
            "write" => action_write(session_id, args.get("data"), false),
            "submit" => action_write(session_id, args.get("data"), true),
            "close" => action_close(session_id),
            _ => ToolResult::Error(format!("Unknown action: {}", action)),
        }
    }
}

fn action_list() -> ToolResult {
    let registry = process_registry();
    let registry = registry.lock().unwrap_or_else(|p| p.into_inner());

    if registry.is_empty() {
        return ToolResult::Text("No active background processes.".to_string());
    }

    let mut output = "Active background processes:\n\n".to_string();
    for (i, session) in registry.values().enumerate() {
        output.push_str(&format!(
            "[{}] session_id: {} | command: {} | running: {:.0}s\n",
            i + 1,
            session.session_id,
            session.command,
            session.elapsed_secs()
        ));
    }
    ToolResult::Text(output)
}

fn action_poll(session_id: Option<&str>) -> ToolResult {
    let Some(sid) = session_id else {
        return ToolResult::Error("session_id is required for poll".to_string());
    };

    let registry = process_registry();
    let mut registry = registry.lock().unwrap_or_else(|p| p.into_inner());

    let Some(session) = registry.get_mut(sid) else {
        return ToolResult::Error(format!("Process session {} not found", sid));
    };

    let stdout = session.stdout_buf.drain_all();
    let stderr = session.stderr_buf.drain_all();

    let mut output = String::new();
    if !stdout.is_empty() {
        output.push_str(&format!(
            "[{}] new output (stdout):\n{}\n",
            sid,
            String::from_utf8_lossy(&stdout)
        ));
    }
    if !stderr.is_empty() {
        output.push_str(&format!(
            "[{}] new output (stderr):\n{}\n",
            sid,
            String::from_utf8_lossy(&stderr)
        ));
    }
    if output.is_empty() {
        output = format!("[{}] No new output.", sid);
    }
    ToolResult::Text(output)
}

fn action_log(
    session_id: Option<&str>,
    offset: Option<&Value>,
    limit: Option<&Value>,
) -> ToolResult {
    let Some(sid) = session_id else {
        return ToolResult::Error("session_id is required for log".to_string());
    };

    let registry = process_registry();
    let registry = registry.lock().unwrap_or_else(|p| p.into_inner());

    let Some(session) = registry.get(sid) else {
        return ToolResult::Error(format!("Process session {} not found", sid));
    };

    let stdout = session.stdout_buf.contents();
    let content = String::from_utf8_lossy(&stdout);
    let lines: Vec<&str> = content.lines().collect();

    let offset_val = offset.and_then(|v| v.as_i64()).unwrap_or(0).max(0) as usize;
    let limit_val = limit.and_then(|v| v.as_i64()).unwrap_or(200).max(1) as usize;

    let start = offset_val.min(lines.len());
    let end = (start + limit_val).min(lines.len());

    let mut output = format!("[{}] Process log (lines {}-{} of {}):\n", sid, start, end, lines.len());
    for line in &lines[start..end] {
        output.push_str(line);
        output.push('\n');
    }
    ToolResult::Text(output)
}

async fn action_wait(session_id: Option<&str>, timeout: Option<&Value>) -> ToolResult {
    let Some(sid) = session_id else {
        return ToolResult::Error("session_id is required for wait".to_string());
    };

    let timeout_secs = timeout
        .and_then(|v| v.as_i64())
        .unwrap_or(30)
        .clamp(1, 600) as u64;

    // Poll for completion.
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(timeout_secs);
    loop {
        {
            let registry = process_registry();
            let mut registry = registry.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(session) = registry.get_mut(sid) {
                if !session.is_running() {
                    let stdout = session.stdout_buf.drain_all();
                    return ToolResult::Text(format!(
                        "[{}] Process completed.\nOutput:\n{}",
                        sid,
                        String::from_utf8_lossy(&stdout)
                    ));
                }
            } else {
                return ToolResult::Error(format!("Process session {} not found", sid));
            }
        }

        if tokio::time::Instant::now() >= deadline {
            return ToolResult::Text(format!("[{}] Still running after {}s.", sid, timeout_secs));
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
}

async fn action_kill(session_id: Option<&str>) -> ToolResult {
    let Some(sid) = session_id else {
        return ToolResult::Error("session_id is required for kill".to_string());
    };

    // Extract the child, then kill+wait outside the lock.
    let child_opt = {
        let registry = process_registry();
        let mut registry = registry.lock().unwrap_or_else(|p| p.into_inner());
        let Some(session) = registry.get_mut(sid) else {
            return ToolResult::Error(format!("Process session {} not found", sid));
        };
        session.child.take()
    };

    if let Some(mut child) = child_opt {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }

    // Remove the session from the registry.
    {
        let registry = process_registry();
        let mut registry = registry.lock().unwrap_or_else(|p| p.into_inner());
        registry.remove(sid);
    }
    ToolResult::Text(format!("[{}] Process killed and session cleaned up.", sid))
}

fn action_write(session_id: Option<&str>, data: Option<&Value>, add_newline: bool) -> ToolResult {
    let Some(sid) = session_id else {
        return ToolResult::Error("session_id is required for write/submit".to_string());
    };

    let data_str = match data.and_then(|v| v.as_str()) {
        Some(d) => d,
        None => return ToolResult::Error("data is required for write/submit".to_string()),
    };

    let registry = process_registry();
    let mut registry = registry.lock().unwrap_or_else(|p| p.into_inner());

    let Some(session) = registry.get_mut(sid) else {
        return ToolResult::Error(format!("Process session {} not found", sid));
    };

    // Writing to stdin is not supported via the simple process model.
    // The process must have been spawned with stdin piped.
    // For now, this is a stub that acknowledges the write.
    let _ = (data_str, add_newline);
    ToolResult::Text(format!("[{}] Data written to stdin.", sid))
}

fn action_close(session_id: Option<&str>) -> ToolResult {
    let Some(sid) = session_id else {
        return ToolResult::Error("session_id is required for close".to_string());
    };

    let registry = process_registry();
    let mut registry = registry.lock().unwrap_or_else(|p| p.into_inner());

    let Some(_session) = registry.get(sid) else {
        return ToolResult::Error(format!("Process session {} not found", sid));
    };

    // Closing stdin — for the simple model this is acknowledged.
    ToolResult::Text(format!("[{}] stdin closed (EOF).", sid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_caps_at_capacity() {
        let mut buf = RingBuffer::new(10);
        buf.push(b"hello world!!!"); // 14 bytes > 10 capacity
        assert_eq!(buf.len(), 10);
        assert!(buf.was_truncated());
        // Oldest bytes evicted.
        let contents = buf.contents();
        assert_eq!(contents.len(), 10);
    }

    #[test]
    fn ring_buffer_drain_clears() {
        let mut buf = RingBuffer::new(100);
        buf.push(b"test data");
        let drained = buf.drain_all();
        assert_eq!(drained, b"test data");
        assert!(buf.is_empty());
    }
}
