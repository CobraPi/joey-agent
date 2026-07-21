//! `joey-mcp` — Model Context Protocol client (port of the client side of
//! `tools/mcp_tool.py`).
//!
//! Speaks JSON-RPC 2.0 over a stdio subprocess: spawns an MCP server, performs
//! the `initialize` handshake, lists its tools, and calls them. Discovered
//! tools are exposed to the agent under the `mcp__<server>__<tool>` naming
//! convention (the wire prefix is kept identical to upstream for compatibility).

use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

/// The MCP tool-name prefix (kept identical to upstream: `mcp__`).
pub const MCP_PREFIX: &str = "mcp__";

/// Build the wire tool name `mcp__<server>__<tool>`.
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    format!("{}{}__{}", MCP_PREFIX, sanitize(server), sanitize(tool))
}

/// Parse a wire tool name back into (server, tool).
pub fn parse_mcp_tool_name(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix(MCP_PREFIX)?;
    let (server, tool) = rest.split_once("__")?;
    Some((server.to_string(), tool.to_string()))
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

/// A discovered MCP tool.
#[derive(Debug, Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// A connected stdio MCP server.
pub struct McpClient {
    server_name: String,
    child: Child,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    next_id: AtomicI64,
}

impl McpClient {
    /// Spawn a stdio MCP server (`command` + `args`) and perform the handshake.
    pub async fn connect(server_name: &str, command: &str, args: &[String]) -> Result<Self> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("spawning MCP server '{}'", command))?;

        let stdin = child.stdin.take().context("no stdin on MCP server")?;
        let stdout = child.stdout.take().context("no stdout on MCP server")?;

        let client = Self {
            server_name: server_name.to_string(),
            child,
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            next_id: AtomicI64::new(1),
        };

        client.initialize().await?;
        Ok(client)
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    async fn initialize(&self) -> Result<()> {
        let params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": joey_core::branding::CLI_NAME, "version": joey_core::branding::VERSION}
        });
        let _ = self.request("initialize", params).await?;
        // Notify initialized (no response expected).
        self.notify("notifications/initialized", json!({})).await?;
        Ok(())
    }

    /// List the tools this server exposes.
    pub async fn list_tools(&self) -> Result<Vec<McpTool>> {
        let result = self.request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(tools
            .into_iter()
            .filter_map(|t| {
                Some(McpTool {
                    name: t.get("name")?.as_str()?.to_string(),
                    description: t
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string(),
                    input_schema: t.get("inputSchema").cloned().unwrap_or(json!({"type": "object"})),
                })
            })
            .collect())
    }

    /// Call a tool by its bare (unprefixed) name with the given arguments.
    pub async fn call_tool(&self, tool: &str, arguments: Value) -> Result<String> {
        let params = json!({"name": tool, "arguments": arguments});
        let result = self.request("tools/call", params).await?;
        // Flatten the content array into text.
        let mut out = String::new();
        if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
            for part in content {
                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                    out.push_str(text);
                    out.push('\n');
                }
            }
        }
        if out.is_empty() {
            out = result.to_string();
        }
        Ok(out)
    }

    /// Send a JSON-RPC request and await its response.
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.write_line(&msg).await?;

        // Read lines until we get the matching id (skip notifications).
        let mut stdout = self.stdout.lock().await;
        let mut line = String::new();
        loop {
            line.clear();
            let n = stdout.read_line(&mut line).await?;
            if n == 0 {
                anyhow::bail!("MCP server '{}' closed the connection", self.server_name);
            }
            let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if v.get("id").and_then(|i| i.as_i64()) == Some(id) {
                if let Some(err) = v.get("error") {
                    anyhow::bail!("MCP error: {}", err);
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.write_line(&msg).await
    }

    async fn write_line(&self, msg: &Value) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        let mut line = serde_json::to_string(msg)?;
        line.push('\n');
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    /// Shut down the server subprocess.
    pub async fn shutdown(mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_name_roundtrip() {
        let name = mcp_tool_name("github", "create_issue");
        assert_eq!(name, "mcp__github__create_issue");
        assert_eq!(
            parse_mcp_tool_name(&name),
            Some(("github".to_string(), "create_issue".to_string()))
        );
    }

    #[test]
    fn sanitizes_server_name() {
        assert_eq!(mcp_tool_name("my server!", "do.it"), "mcp__my_server___do_it");
    }
}
