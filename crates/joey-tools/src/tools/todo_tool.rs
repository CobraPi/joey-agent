//! Todo tool: planning & task management — port of `tools/todo_tool.py`
//! (TodoStore semantics, the full behavioral schema, and the JSON result
//! envelope with summary counts). State is per-session.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::context::ToolContext;
use crate::pyjson::dumps;
use crate::registry::{tool_error, Tool, ToolResult};

pub const VALID_STATUSES: &[&str] = &["pending", "in_progress", "completed", "cancelled"];
pub const MAX_TODO_CONTENT_CHARS: usize = 4000;
pub const MAX_TODO_ITEMS: usize = 256;
const TRUNCATION_MARKER: &str = "… [truncated]";

#[derive(Debug, Clone)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: String,
}

/// Global per-session todo store (session_id → items).
fn store() -> &'static Mutex<HashMap<String, Vec<TodoItem>>> {
    static STORE: OnceLock<Mutex<HashMap<String, Vec<TodoItem>>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Read the current todo list for a session (for CLI rendering).
pub fn current(session_id: &str) -> Vec<TodoItem> {
    store().lock().unwrap().get(session_id).cloned().unwrap_or_default()
}

/// Render the list with the upstream status markers
/// (`format_for_injection` markers: `[x]` / `[>]` / `[ ]` / `[~]`).
pub fn render(list: &[TodoItem]) -> String {
    let mut out = String::new();
    for t in list {
        let mark = match t.status.as_str() {
            "completed" => "[x]",
            "in_progress" => "[>]",
            "cancelled" => "[~]",
            _ => "[ ]",
        };
        out.push_str(&format!("{} {}\n", mark, t.content));
    }
    out
}

fn cap_content(content: &str) -> String {
    if content.chars().count() > MAX_TODO_CONTENT_CHARS {
        let keep = MAX_TODO_CONTENT_CHARS - TRUNCATION_MARKER.chars().count();
        let kept: String = content.chars().take(keep).collect();
        format!("{}{}", kept, TRUNCATION_MARKER)
    } else {
        content.to_string()
    }
}

fn validate(item: &Value) -> TodoItem {
    let Some(obj) = item.as_object() else {
        return TodoItem {
            id: "?".to_string(),
            content: "(invalid item)".to_string(),
            status: "pending".to_string(),
        };
    };
    let mut id = value_to_string(obj.get("id")).trim().to_string();
    if id.is_empty() {
        id = "?".to_string();
    }
    let mut content = value_to_string(obj.get("content")).trim().to_string();
    if content.is_empty() {
        content = "(no description)".to_string();
    } else {
        content = cap_content(&content);
    }
    let mut status = value_to_string(obj.get("status")).trim().to_lowercase();
    if status.is_empty() {
        status = "pending".to_string();
    }
    if !VALID_STATUSES.contains(&status.as_str()) {
        status = "pending".to_string();
    }
    TodoItem { id, content, status }
}

fn value_to_string(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// Collapse duplicate ids, keeping the last occurrence in its position.
fn dedupe_by_id(todos: &[Value]) -> Vec<Value> {
    let mut last_index: indexmap::IndexMap<String, usize> = indexmap::IndexMap::new();
    for (i, item) in todos.iter().enumerate() {
        let key = match item.as_object() {
            Some(obj) => {
                let id = value_to_string(obj.get("id")).trim().to_string();
                if id.is_empty() {
                    "?".to_string()
                } else {
                    id
                }
            }
            None => format!("__invalid_{}", i),
        };
        last_index.insert(key, i);
    }
    let mut indices: Vec<usize> = last_index.values().copied().collect();
    indices.sort_unstable();
    indices.into_iter().map(|i| todos[i].clone()).collect()
}

fn write_items(list: &mut Vec<TodoItem>, todos: &[Value], merge: bool) {
    if !merge {
        *list = dedupe_by_id(todos).iter().map(validate).collect();
    } else {
        for t in dedupe_by_id(todos) {
            let Some(obj) = t.as_object() else { continue };
            let item_id = value_to_string(obj.get("id")).trim().to_string();
            if item_id.is_empty() {
                continue; // Can't merge without an id
            }
            if let Some(existing) = list.iter_mut().find(|i| i.id == item_id) {
                // Update only the fields the LLM actually provided.
                if let Some(c) = obj.get("content") {
                    let cs = value_to_string(Some(c));
                    if !cs.is_empty() {
                        existing.content = cap_content(cs.trim());
                    }
                }
                if let Some(s) = obj.get("status") {
                    let ss = value_to_string(Some(s)).trim().to_lowercase();
                    if VALID_STATUSES.contains(&ss.as_str()) {
                        existing.status = ss;
                    }
                }
            } else {
                list.push(validate(&t));
            }
        }
    }
    if list.len() > MAX_TODO_ITEMS {
        list.truncate(MAX_TODO_ITEMS);
    }
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
        "Manage your task list for the current session. Use for complex tasks with 3+ steps or when the user provides multiple tasks. Call with no parameters to read the current list.\n\nWriting:\n- Provide 'todos' array to create/update items\n- merge=false (default): replace the entire list with a fresh plan\n- merge=true: update existing items by id, add any new ones\n\nEach item: {id: string, content: string, status: pending|in_progress|completed|cancelled}\nList order is priority. Only ONE item in_progress at a time.\nMark items completed immediately when done. If something fails, cancel it and add a revised item.\n\nAlways returns the full current list."
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
                    "description": "Task items to write. Omit to read current list.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Unique item identifier"
                            },
                            "content": {
                                "type": "string",
                                "description": "Task description"
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed", "cancelled"],
                                "description": "Current status"
                            }
                        },
                        "required": ["id", "content", "status"]
                    }
                },
                "merge": {
                    "type": "boolean",
                    "description": "true: update existing items by id, add new ones. false (default): replace the entire list.",
                    "default": false
                }
            },
            "required": []
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let merge = args.get("merge").and_then(|v| v.as_bool()).unwrap_or(false);
        let todos_val = args.get("todos").cloned();

        let items: Vec<TodoItem> = {
            let mut guard = store().lock().unwrap();
            let list = guard.entry(ctx.session_id().to_string()).or_default();
            match todos_val {
                None | Some(Value::Null) => list.clone(),
                Some(v) => {
                    // Guard: LLM sometimes sends todos as a JSON string.
                    let parsed: Value = match v {
                        Value::String(s) => match serde_json::from_str(&s) {
                            Ok(p) => p,
                            Err(_) => {
                                return tool_error(
                                    "todos must be a list of objects, got unparseable string",
                                )
                            }
                        },
                        other => other,
                    };
                    let Value::Array(arr) = parsed else {
                        return tool_error(format!(
                            "todos must be a list, got {}",
                            match parsed {
                                Value::Object(_) => "dict",
                                Value::String(_) => "str",
                                Value::Number(_) => "int",
                                Value::Bool(_) => "bool",
                                Value::Null => "NoneType",
                                Value::Array(_) => "list",
                            }
                        ));
                    };
                    write_items(list, &arr, merge);
                    list.clone()
                }
            }
        };

        let pending = items.iter().filter(|i| i.status == "pending").count();
        let in_progress = items.iter().filter(|i| i.status == "in_progress").count();
        let completed = items.iter().filter(|i| i.status == "completed").count();
        let cancelled = items.iter().filter(|i| i.status == "cancelled").count();

        let todos_json: Vec<Value> = items
            .iter()
            .map(|i| {
                let mut m = Map::new();
                m.insert("id".into(), json!(i.id));
                m.insert("content".into(), json!(i.content));
                m.insert("status".into(), json!(i.status));
                Value::Object(m)
            })
            .collect();
        ToolResult::Text(dumps(&json!({
            "todos": todos_json,
            "summary": {
                "total": items.len(),
                "pending": pending,
                "in_progress": in_progress,
                "completed": completed,
                "cancelled": cancelled,
            },
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use joey_core::Config;

    fn ctx(id: &str) -> ToolContext {
        ToolContext::new(std::env::temp_dir(), Config::defaults(), id)
    }

    fn parse(r: &ToolResult) -> Value {
        serde_json::from_str(&r.to_content_string()).unwrap()
    }

    #[tokio::test]
    async fn write_and_read_envelope() {
        let c = ctx("todo-t1");
        let v = parse(
            &Todo
                .execute(
                    json!({"todos": [
                        {"id": "1", "content": "first", "status": "in_progress"},
                        {"id": "2", "content": "second", "status": "bogus"}
                    ]}),
                    &c,
                )
                .await,
        );
        assert_eq!(v["summary"]["total"], 2);
        assert_eq!(v["summary"]["in_progress"], 1);
        // Invalid status normalizes to pending.
        assert_eq!(v["todos"][1]["status"], "pending");
        assert_eq!(v["summary"]["pending"], 1);

        // No-param call reads the current list.
        let read = parse(&Todo.execute(json!({}), &c).await);
        assert_eq!(read["summary"]["total"], 2);
        assert_eq!(read["todos"][0]["content"], "first");
    }

    #[tokio::test]
    async fn merge_updates_only_provided_fields() {
        let c = ctx("todo-t2");
        Todo.execute(
            json!({"todos": [{"id": "a", "content": "orig", "status": "pending"}]}),
            &c,
        )
        .await;
        let v = parse(
            &Todo
                .execute(json!({"todos": [{"id": "a", "status": "completed"}], "merge": true}), &c)
                .await,
        );
        assert_eq!(v["todos"][0]["content"], "orig");
        assert_eq!(v["todos"][0]["status"], "completed");
    }

    #[tokio::test]
    async fn dedupe_keeps_last_and_caps() {
        let c = ctx("todo-t3");
        let v = parse(
            &Todo
                .execute(
                    json!({"todos": [
                        {"id": "x", "content": "old", "status": "pending"},
                        {"id": "x", "content": "new", "status": "pending"}
                    ]}),
                    &c,
                )
                .await,
        );
        assert_eq!(v["summary"]["total"], 1);
        assert_eq!(v["todos"][0]["content"], "new");

        // Content cap at 4000 chars.
        let long = "z".repeat(5000);
        let v2 = parse(
            &Todo
                .execute(json!({"todos": [{"id": "big", "content": long, "status": "pending"}]}), &c)
                .await,
        );
        let content = v2["todos"][0]["content"].as_str().unwrap();
        assert_eq!(content.chars().count(), MAX_TODO_CONTENT_CHARS);
        assert!(content.ends_with(TRUNCATION_MARKER));
    }

    #[tokio::test]
    async fn tolerates_json_string_todos() {
        let c = ctx("todo-t4");
        let v = parse(
            &Todo
                .execute(
                    json!({"todos": "[{\"id\": \"s\", \"content\": \"from string\", \"status\": \"pending\"}]"}),
                    &c,
                )
                .await,
        );
        assert_eq!(v["todos"][0]["content"], "from string");
        let err = parse(&Todo.execute(json!({"todos": "not json"}), &c).await);
        assert_eq!(err["error"], "todos must be a list of objects, got unparseable string");
    }

    #[test]
    fn render_markers() {
        let list = vec![
            TodoItem { id: "1".into(), content: "a".into(), status: "in_progress".into() },
            TodoItem { id: "2".into(), content: "b".into(), status: "cancelled".into() },
            TodoItem { id: "3".into(), content: "c".into(), status: "completed".into() },
            TodoItem { id: "4".into(), content: "d".into(), status: "pending".into() },
        ];
        let r = render(&list);
        assert!(r.contains("[>] a"));
        assert!(r.contains("[~] b"));
        assert!(r.contains("[x] c"));
        assert!(r.contains("[ ] d"));
    }
}
