use joey_tools::registry::{Tool, ToolRegistry, ToolResult};
use joey_tools::context::ToolContext;
use serde_json::Value as JsonValue;
use std::sync::Arc;

/// Shared per-server connection state. The client is created lazily on the
/// agent's runtime (first tool call) and reused for the session lifetime.
/// Dropping the last reference drops the client, which kills the child
/// process (kill_on_drop).
struct McpSession {
    server_name: String,
    server_cfg: joey_mcp::ServerConfig,
    state: tokio::sync::Mutex<Option<Arc<joey_mcp::McpClient>>>,
}

impl McpSession {
    async fn client(&self) -> Result<Arc<joey_mcp::McpClient>, String> {
        let mut guard = self.state.lock().await;
        if let Some(c) = guard.as_ref() {
            return Ok(c.clone());
        }
        let client = joey_mcp::McpClient::connect(&self.server_name, &self.server_cfg)
            .await
            .map_err(|e| {
                format!(
                    "MCP server '{}' connection failed: {}",
                    self.server_name, e
                )
            })?;
        let client = Arc::new(client);
        *guard = Some(client.clone());
        Ok(client)
    }
}

/// One agent-callable proxy tool forwarding to a remote MCP tool.
struct McpProxyTool {
    session: Arc<McpSession>,
    original_name: String,
    wire_name: String,
    tool_description: String,
    input_schema: JsonValue,
}

#[async_trait::async_trait]
impl Tool for McpProxyTool {
    fn name(&self) -> &str {
        &self.wire_name
    }
    fn toolset(&self) -> &str {
        "mcp"
    }
    fn description(&self) -> &str {
        &self.tool_description
    }
    fn parameters(&self) -> JsonValue {
        self.input_schema.clone()
    }
    async fn execute(&self, args: JsonValue, _ctx: &ToolContext) -> ToolResult {
        let client = match self.session.client().await {
            Ok(c) => c,
            Err(e) => return ToolResult::Error(e),
        };
        ToolResult::Text(client.call_tool(&self.original_name, args).await)
    }
}

/// Discover the tool catalogs of all enabled MCP servers (connect → list →
/// shutdown on a dedicated thread with its own runtime, safe from sync and
/// async contexts alike) and register proxy tools. Returns the wire names
/// registered, for the caller to append to the session's enabled-tools list.
pub fn register_mcp_tools(registry: &mut ToolRegistry, config: &joey_core::Config) -> Vec<String> {
    let (enabled, _disabled) = crate::oneshot::mcp_server_names(config);
    if enabled.is_empty() {
        return Vec::new();
    }

    // Handshake pass: this map's configs are consumed by the worker threads.
    let handshake_configs = joey_mcp::load_server_configs(config);
    let mut catalogs: Vec<(String, Vec<joey_mcp::McpTool>)> = Vec::new();
    for (name, server_cfg) in handshake_configs {
        if !enabled.contains(&name) {
            continue;
        }
        let thread_name = name.clone();
        let handle = std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => return Err((thread_name.clone(), e.to_string())),
            };
            let discover = async {
                let client = joey_mcp::McpClient::connect(&thread_name, &server_cfg).await?;
                let tools = client.list_tools().await?;
                client.shutdown().await;
                Ok::<_, anyhow::Error>(tools)
            };
            match rt.block_on(discover) {
                Ok(tools) => Ok((thread_name, tools)),
                Err(e) => Err((thread_name, e.to_string())),
            }
        });
        match handle.join() {
            Ok(Ok((name, tools))) => catalogs.push((name, tools)),
            Ok(Err((name, err))) => {
                tracing::warn!("MCP server '{}' unavailable, skipping: {}", name, err)
            }
            Err(_) => tracing::warn!("MCP server discovery thread panicked"),
        }
    }

    // Session pass: fresh configs (consumed by the lazy sessions).
    let mut session_configs = joey_mcp::load_server_configs(config);
    let mut wire_names = Vec::new();
    for (server_name, tools) in catalogs {
        let Some(server_cfg) = session_configs.swap_remove(&server_name) else {
            continue;
        };
        let session = Arc::new(McpSession {
            server_name: server_name.clone(),
            server_cfg,
            state: tokio::sync::Mutex::new(None),
        });
        for tool in tools {
            let wire_name = tool.wire_name.clone();
            wire_names.push(wire_name.clone());
            registry.register(Arc::new(McpProxyTool {
                session: session.clone(),
                original_name: tool.name.clone(),
                wire_name,
                tool_description: format!("[mcp:{}] {}", server_name, tool.description),
                input_schema: tool.input_schema.clone(),
            }));
        }
        tracing::info!(
            "MCP server '{}' connected for sessions: {} tools registered",
            server_name,
            wire_names.len()
        );
    }
    wire_names
}
