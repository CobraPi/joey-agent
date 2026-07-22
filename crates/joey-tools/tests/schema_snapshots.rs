//! Schema snapshot tests: every built-in tool's model-visible surface —
//! name, description, and parameters JSON Schema — asserted against literals
//! derived from the upstream Python schemas (rebranded Hermes→Joey only).
//!
//! These are the byte-for-byte contracts of the port; any drift here is a
//! fidelity regression, not a refactor.

use joey_tools::{Tool, ToolRegistry};
use serde_json::{json, Value};

fn tool(name: &str) -> std::sync::Arc<dyn Tool> {
    ToolRegistry::with_builtins().get(name).unwrap_or_else(|| panic!("tool {} missing", name))
}

#[test]
fn read_file_schema() {
    let t = tool("read_file");
    assert_eq!(
        t.description(),
        "Read a text file with line numbers and pagination. Use this instead of cat/head/tail in terminal. Output format: 'LINE_NUM|CONTENT'. Suggests similar filenames if not found. Use offset and limit for large files. Reads exceeding ~100K characters are truncated on a line boundary and return a next_offset; continue with offset to read the rest. Jupyter notebooks (.ipynb), Word documents (.docx), and Excel workbooks (.xlsx) are auto-extracted to readable text. NOTE: Cannot read images or other binary files — use vision_analyze for images."
    );
    assert_eq!(
        t.parameters(),
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to the file to read (absolute, relative, or ~/path)"},
                "offset": {"type": "integer", "description": "Line number to start reading from (1-indexed, default: 1)", "default": 1, "minimum": 1},
                "limit": {"type": "integer", "description": "Maximum number of lines to read (default: 500, max: 2000)", "default": 500, "maximum": 2000}
            },
            "required": ["path"]
        })
    );
}

#[test]
fn write_file_schema() {
    let t = tool("write_file");
    assert_eq!(
        t.description(),
        "Write content to a file, completely replacing existing content. Use this instead of echo/cat heredoc in terminal. Creates parent directories automatically. OVERWRITES the entire file — use 'patch' for targeted edits. Auto-runs syntax checks on .py/.json/.yaml/.toml and other linted languages; only NEW errors introduced by this write are surfaced (pre-existing errors are filtered out)."
    );
    assert_eq!(
        t.parameters(),
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to the file to write (will be created if it doesn't exist, overwritten if it does)"},
                "content": {"type": "string", "description": "Complete content to write to the file"},
                "cross_profile": {
                    "type": "boolean",
                    "description": "Opt out of the cross-profile soft guard. Defaults to false. Set true ONLY after explicit user direction to edit another Joey profile's skills/plugins/cron/memories — by default these writes are blocked with a warning because they affect a different profile than the one this session is running under.",
                    "default": false,
                },
            },
            "required": ["path", "content"]
        })
    );
}

#[test]
fn patch_schema() {
    let t = tool("patch");
    assert_eq!(
        t.description(),
        "Targeted find-and-replace edits in files. Use this instead of sed/awk in terminal. Uses fuzzy matching (9 strategies) so minor whitespace/indentation differences won't break it. Returns a unified diff. Auto-runs syntax checks after editing.\n\nREPLACE MODE (mode='replace', default): find a unique string and replace it. REQUIRED PARAMETERS: mode, path, old_string, new_string.\nPATCH MODE (mode='patch'): apply V4A multi-file patches for bulk changes. REQUIRED PARAMETERS: mode, patch."
    );
    let p = t.parameters();
    assert_eq!(p["required"], json!(["mode"]));
    assert_eq!(
        p["properties"]["mode"],
        json!({
            "type": "string",
            "enum": ["replace", "patch"],
            "description": "Edit mode. 'replace' (default): requires path + old_string + new_string. 'patch': requires patch content only.",
            "default": "replace",
        })
    );
    assert_eq!(
        p["properties"]["old_string"]["description"],
        "REQUIRED when mode='replace'. Exact text to find and replace. Must be unique in the file unless replace_all=true. Include surrounding context lines to ensure uniqueness."
    );
    assert_eq!(
        p["properties"]["new_string"]["description"],
        "REQUIRED when mode='replace'. Replacement text. Pass empty string '' to delete the matched text."
    );
    assert_eq!(
        p["properties"]["replace_all"],
        json!({
            "type": "boolean",
            "description": "Replace all occurrences instead of requiring a unique match (default: false)",
            "default": false,
        })
    );
    assert_eq!(
        p["properties"]["patch"]["description"],
        "REQUIRED when mode='patch'. V4A format patch content. Format:\n*** Begin Patch\n*** Update File: path/to/file\n@@ context hint @@\n context line\n-removed line\n+added line\n*** End Patch"
    );
    assert_eq!(
        p["properties"]["cross_profile"]["description"],
        "Opt out of the cross-profile soft guard. Defaults to false. Set true ONLY after explicit user direction to edit another Joey profile's skills/plugins/cron/memories."
    );
}

#[test]
fn search_files_schema() {
    let t = tool("search_files");
    assert_eq!(
        t.description(),
        "Search file contents or find files by name. Use this instead of grep/rg/find/ls in terminal. Ripgrep-backed, faster than shell equivalents.\n\nContent search (target='content'): Regex search inside files. Output modes: full matches with line numbers, file paths only, or match counts.\n\nFile search (target='files'): Find files by glob pattern (e.g., '*.py', '*config*'). Also use this instead of ls — results sorted by modification time."
    );
    assert_eq!(
        t.parameters(),
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Regex pattern for content search, or glob pattern (e.g., '*.py') for file search"},
                "target": {"type": "string", "enum": ["content", "files"], "description": "'content' searches inside file contents, 'files' searches for files by name", "default": "content"},
                "path": {"type": "string", "description": "Directory or file to search in (default: current working directory)", "default": "."},
                "file_glob": {"type": "string", "description": "Filter files by pattern in grep mode (e.g., '*.py' to only search Python files)"},
                "limit": {"type": "integer", "description": "Maximum number of results to return (default: 50)", "default": 50},
                "offset": {"type": "integer", "description": "Skip first N results for pagination (default: 0)", "default": 0},
                "output_mode": {"type": "string", "enum": ["content", "files_only", "count"], "description": "Output format for grep mode: 'content' shows matching lines with line numbers, 'files_only' lists file paths, 'count' shows match counts per file", "default": "content"},
                "context": {"type": "integer", "description": "Number of context lines before and after each match (grep mode only)", "default": 0}
            },
            "required": ["pattern"]
        })
    );
}

#[test]
fn terminal_schema() {
    let t = tool("terminal");
    let d = t.description();
    // Full-text spot checks on the rebranded TERMINAL_TOOL_DESCRIPTION.
    assert!(d.starts_with("Execute shell commands on a Linux environment. Filesystem, current working directory, and exported environment variables persist between calls.\n"));
    assert!(d.contains("Do NOT use cat/head/tail to read files — use read_file instead.\n"));
    assert!(d.contains("Use background=true so Joey can track lifecycle and output.\n"));
    assert!(d.contains("PTY mode: Set pty=true for interactive CLI tools (Codex, Claude Code, Python REPL).\n"));
    let p = t.parameters();
    assert_eq!(p["required"], json!(["command"]));
    assert_eq!(p["properties"]["command"]["description"], "The command to execute on the VM");
    assert_eq!(p["properties"]["timeout"]["minimum"], 1);
    assert_eq!(
        p["properties"]["timeout"]["description"],
        "Max seconds to wait (default: 180, foreground max: 600). Returns INSTANTLY when command finishes — set high for long tasks, you won't wait unnecessarily. Foreground timeout above 600s is rejected; use background=true for longer commands."
    );
    assert_eq!(p["properties"]["background"]["default"], false);
    assert_eq!(p["properties"]["pty"]["default"], false);
    assert_eq!(p["properties"]["notify_on_complete"]["default"], false);
    assert_eq!(p["properties"]["watch_patterns"]["items"], json!({"type": "string"}));
    assert_eq!(
        p["properties"]["workdir"]["description"],
        "Working directory for this command (absolute path). Defaults to the session working directory."
    );
    for key in ["command", "background", "timeout", "workdir", "pty", "notify_on_complete", "watch_patterns"] {
        assert!(p["properties"].get(key).is_some(), "terminal param {} missing", key);
    }
}

#[test]
fn todo_schema() {
    let t = tool("todo");
    assert_eq!(
        t.description(),
        "Manage your task list for the current session. Use for complex tasks with 3+ steps or when the user provides multiple tasks. Call with no parameters to read the current list.\n\nWriting:\n- Provide 'todos' array to create/update items\n- merge=false (default): replace the entire list with a fresh plan\n- merge=true: update existing items by id, add any new ones\n\nEach item: {id: string, content: string, status: pending|in_progress|completed|cancelled}\nList order is priority. Only ONE item in_progress at a time.\nMark items completed immediately when done. If something fails, cancel it and add a revised item.\n\nAlways returns the full current list."
    );
    assert_eq!(
        t.parameters(),
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "Task items to write. Omit to read current list.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string", "description": "Unique item identifier"},
                            "content": {"type": "string", "description": "Task description"},
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
    );
}

#[test]
fn memory_schema() {
    let t = tool("memory");
    let d = t.description();
    assert!(d.starts_with("Save durable facts to persistent memory that survive across sessions."));
    assert!(d.contains("HOW: make ALL your changes in ONE call via an 'operations' array"));
    assert!(d.contains("WHEN: save proactively when the user states a preference"));
    assert!(d.contains("IF FULL: an add is rejected with the current entries shown."));
    assert!(d.contains("TARGETS: 'user' = who the user is (name, role, preferences, style)."));
    assert!(d.contains("SKIP: trivial/obvious info"));
    let p = t.parameters();
    assert_eq!(p["required"], json!(["target"]));
    // action has NO default key.
    assert!(p["properties"]["action"].get("default").is_none());
    assert_eq!(p["properties"]["action"]["enum"], json!(["add", "replace", "remove"]));
    assert_eq!(
        p["properties"]["action"]["description"],
        "The action to perform (single-op shape). Omit when using 'operations'."
    );
    assert_eq!(
        p["properties"]["target"],
        json!({
            "type": "string",
            "enum": ["memory", "user"],
            "description": "Which memory store: 'memory' for personal notes, 'user' for user profile."
        })
    );
    assert_eq!(
        p["properties"]["old_text"]["description"],
        "REQUIRED for 'replace' and 'remove' (single-op shape): a short unique substring identifying the existing entry to modify. Omit only for 'add'."
    );
    assert_eq!(p["properties"]["operations"]["items"]["required"], json!(["action"]));
}

#[test]
fn web_search_schema() {
    let t = tool("web_search");
    assert_eq!(
        t.description(),
        "Search the web for information. Returns up to 5 results by default with titles, URLs, and descriptions. The query is passed through to the configured backend, so operators such as site:domain, filetype:pdf, intitle:word, -term, and \"exact phrase\" may work when the backend supports them."
    );
    assert_eq!(
        t.parameters(),
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query to look up on the web. You may include backend-supported operators such as site:example.com, filetype:pdf, intitle:word, -term, or \"exact phrase\"."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return. Defaults to 5.",
                    "minimum": 1,
                    "maximum": 100,
                    "default": 5
                }
            },
            "required": ["query"]
        })
    );
}

#[test]
fn web_extract_schema() {
    let t = tool("web_extract");
    assert_eq!(
        t.description(),
        "Extract content from web page URLs. Returns clean page content in markdown/text (no LLM summarization — fast). Also works with PDF URLs (arxiv papers, documents) — pass the PDF link directly. Pages within the char budget (default 15000) return whole; larger pages return a head+tail window with a footer telling you the full text's saved file path and the read_file call to page through the omitted middle. Inline images appear as [IMAGE: alt] placeholders; real image URLs are kept as links. If a URL fails or times out, use the browser tool instead."
    );
    let p = t.parameters();
    assert_eq!(p["properties"]["urls"]["maxItems"], 5);
    assert_eq!(
        p["properties"]["urls"]["description"],
        "List of URLs to extract content from (max 5 URLs per call)"
    );
    // char_limit has minimum 2000 and NO default key.
    assert_eq!(p["properties"]["char_limit"]["minimum"], 2000);
    assert!(p["properties"]["char_limit"].get("default").is_none());
    assert_eq!(p["required"], json!(["urls"]));
}

#[test]
fn skills_schemas() {
    let list = tool("skills_list");
    assert_eq!(
        list.description(),
        "List available skills (name + description). Use skill_view(name) to load full content."
    );
    assert_eq!(
        list.parameters(),
        json!({
            "type": "object",
            "properties": {
                "category": {"type": "string", "description": "Optional category filter to narrow results"}
            },
            "required": []
        })
    );

    let view = tool("skill_view");
    assert_eq!(
        view.description(),
        "Skills allow for loading information about specific tasks and workflows, as well as scripts and templates. Load a skill's full content or access its linked files (references, templates, scripts). First call returns SKILL.md content plus a 'linked_files' dict showing available references/templates/scripts. To access those, call again with file_path parameter."
    );
    assert_eq!(
        view.parameters(),
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The skill name (use skills_list to see available skills). For plugin-provided skills, use the qualified form 'plugin:skill' (e.g. 'superpowers:writing-plans')."
                },
                "file_path": {
                    "type": "string",
                    "description": "OPTIONAL: Path to a linked file within the skill (e.g., 'references/api.md', 'templates/config.yaml', 'scripts/validate.py'). Omit to get the main SKILL.md content."
                }
            },
            "required": ["name"]
        })
    );
}

#[test]
fn emojis_and_result_size() {
    let reg = ToolRegistry::with_builtins();
    for (name, emoji) in [
        ("read_file", "📖"),
        ("write_file", "✍\u{fe0f}"),
        ("patch", "🔧"),
        ("search_files", "🔎"),
        ("terminal", "💻"),
        ("todo", "📋"),
        ("memory", "🧠"),
        ("web_search", "🔍"),
        ("web_extract", "📄"),
        ("skills_list", "📚"),
        ("skill_view", "📚"),
    ] {
        assert_eq!(reg.get_emoji(name), emoji, "emoji for {}", name);
    }
    // Per-tool 100K thresholds (registry entries) and the read_file pin.
    assert_eq!(reg.get_max_result_size("terminal"), Some(100_000));
    assert_eq!(reg.get_max_result_size("web_extract"), Some(100_000));
    assert_eq!(reg.get_max_result_size("read_file"), None, "read_file is pinned to infinity");
}

#[test]
fn definitions_shape_is_openai_format() {
    let reg = ToolRegistry::with_builtins();
    let ctx = joey_tools::ToolContext::new(
        std::env::temp_dir(),
        joey_core::Config::defaults(),
        "snapshot",
    );
    let defs = reg.definitions(&["read_file".to_string(), "todo".to_string()], &ctx);
    assert_eq!(defs.len(), 2);
    for def in &defs {
        assert_eq!(def["type"], "function");
        assert!(def["function"]["name"].is_string());
        assert!(def["function"]["description"].is_string());
        assert_eq!(def["function"]["parameters"]["type"], "object");
    }
    // Sanitizer removes the empty required list from todo (upstream
    // schema_sanitizer behavior on `"required": []`).
    let todo_def: &Value =
        defs.iter().find(|d| d["function"]["name"] == "todo").unwrap();
    assert!(todo_def["function"]["parameters"].get("required").is_none());
}
