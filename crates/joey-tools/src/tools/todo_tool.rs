//! Todo tool: task planning/tracking for multi-step work
//! (port of `tools/todo_tool.py`). State is per-session, held in the shared
//! store so both the tool and the CLI can render it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::context::ToolContext;
use crate::registry::{tool_error, Tool, ToolResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String {
    "pending".to_string()
}

/// Global per-session todo store (session_id → items).
fn store() -> &'static Mutex<HashMap<String, Vec<TodoItem>>> {
    static STORE: OnceLock<Mutex<HashMap<String, Vec<TodoItem>>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Read the current todo list for a session (for CLI rendering).
pub fn current(session_id: &str) -> Vec<TodoItem> {
    store()
        .lock()
        .unwrap()
        .get(session_id)
        .cloned()
        .unwrap_or_default()
}

pub struct Todo;

#[async_trait]
impl Tool for Todo {
    fn name(&self) -> &str {
        "todo"
    }
    fn toolset(&self) -> &str {
        "todo"
    }
    fn description(&self) -> &str {
        "Track a plan for multi-step work. Pass the full list of todos each time \
         (or set merge=true to update by id). Statuses: pending, in_progress, completed, cancelled."
    }
    fn emoji(&self) -> &str {
        "📋"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string"},
                            "content": {"type": "string"},
                            "status": {"type": "string", "enum": ["pending", "in_progress", "completed", "cancelled"]}
                        },
                        "required": ["id", "content", "status"]
                    }
                },
                "merge": {"type": "boolean", "default": false}
            },
            "required": ["todos"]
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let Some(todos_val) = args.get("todos").and_then(|v| v.as_array()) else {
            return tool_error("missing required parameter: todos");
        };
        let merge = args.get("merge").and_then(|v| v.as_bool()).unwrap_or(false);
        let incoming: Vec<TodoItem> = match serde_json::from_value(Value::Array(todos_val.clone())) {
            Ok(v) => v,
            Err(e) => return tool_error(format!("invalid todos: {}", e)),
        };

        let mut guard = store().lock().unwrap();
        let list = guard.entry(ctx.session_id().to_string()).or_default();
        if merge {
            for item in incoming {
                if let Some(existing) = list.iter_mut().find(|t| t.id == item.id) {
                    *existing = item;
                } else {
                    list.push(item);
                }
            }
        } else {
            *list = incoming;
        }

        let rendered = render(list);
        ToolResult::Text(rendered)
    }
}

fn render(list: &[TodoItem]) -> String {
    if list.is_empty() {
        return "Todo list cleared.".to_string();
    }
    let mut out = String::from("Todo list updated:\n");
    for t in list {
        let mark = match t.status.as_str() {
            "completed" => "[x]",
            "in_progress" => "[~]",
            "cancelled" => "[-]",
            _ => "[ ]",
        };
        out.push_str(&format!("{} {}\n", mark, t.content));
    }
    out
}
