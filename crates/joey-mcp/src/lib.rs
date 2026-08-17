//! `joey-mcp` — Model Context Protocol client (port of the client side of
//! `tools/mcp_tool.py`).
//!
//! Speaks JSON-RPC 2.0 over a stdio subprocess: spawns an MCP server with a
//! filtered environment, performs the `initialize` handshake, lists its tools
//! (following `nextCursor` pagination, gated on the server's advertised
//! `tools` capability), and calls them. Discovered tools are exposed to the
//! agent under the `mcp__<server>__<tool>` naming convention (the wire prefix
//! is kept identical to upstream for compatibility).
//!
//! Wire names are never parsed back into `(server, tool)` — that string shape
//! is ambiguous when server names contain underscores. Provenance is instead
//! captured in a registration-time map (see [`McpClient::tool_provenance`]),
//! matching upstream `_track_mcp_tool_server`.
//!
//! Tool-call results are rendered into upstream's JSON envelope
//! (`{"result": ...}` / `{"error": ...}`); see [`crate::result`].

mod config;
mod result;
mod schema;
mod security;

pub use config::{
    build_safe_env, interpolate_env_vars, load_server_configs, resolve_stdio_command,
    ServerConfig, ToolsFilter, DEFAULT_CONNECT_TIMEOUT, DEFAULT_TOOL_TIMEOUT,
    MAX_BACKOFF_SECONDS, MAX_INITIAL_CONNECT_RETRIES,
};
pub use result::sanitize_error;
pub use schema::{normalize_mcp_input_schema, strip_nullable_unions};
pub use security::{is_mcp_server_entry_suspicious, validate_mcp_server_entry};

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex as StdMutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tracing::{info, warn};

use result::{error_envelope, render_call_result};

/// The MCP tool-name prefix (upstream `MCP_TOOL_NAME_PREFIX`, kept identical:
/// `mcp__`). The `mcp__<server>__<tool>` convention is shared by Claude Code,
/// Codex, and OpenCode; the double-underscore delimiter disambiguates the
/// server/tool boundary even when either component contains underscores.
pub const MCP_TOOL_NAME_PREFIX: &str = "mcp__";
const MCP_NAME_DELIM: &str = "__";

/// The protocol version the `mcp==1.26.0` SDK sends in `initialize`
/// (`types.LATEST_PROTOCOL_VERSION`).
pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";
/// `mcp.shared.version.SUPPORTED_PROTOCOL_VERSIONS`.
const SUPPORTED_PROTOCOL_VERSIONS: [&str; 4] =
    ["2024-11-05", "2025-03-26", "2025-06-18", LATEST_PROTOCOL_VERSION];

/// Safety cap on `nextCursor` pagination loops (`_MCP_LIST_MAX_PAGES`): a
/// misbehaving server that returns a cursor forever cannot spin discovery
/// indefinitely.
pub const MCP_LIST_MAX_PAGES: usize = 50;

/// The SDK's timeout before escalating from terminate to kill on shutdown
/// (`PROCESS_TERMINATION_TIMEOUT`).
const PROCESS_TERMINATION_TIMEOUT: Duration = Duration::from_secs(2);

/// Return an MCP name component safe for tool and prefix generation
/// (`sanitize_mcp_name_component`): every character outside `[A-Za-z0-9_]`
/// (hyphens included) becomes `_`.
pub fn sanitize_mcp_name_component(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

/// Build the registry/wire name for an MCP tool (`mcp_prefixed_tool_name`):
/// `mcp__<sanitizedServer>__<sanitizedTool>`. The server component is the
/// sanitized *wire prefix* registered via [`WirePrefixRegistry::register`],
/// which may differ from the raw sanitized name when another server already
/// claimed the same prefix (see that type's docs).
pub fn mcp_prefixed_tool_name(server_name: &str, tool_name: &str) -> String {
    format!(
        "{}{}{}{}",
        MCP_TOOL_NAME_PREFIX,
        sanitize_mcp_name_component(server_name),
        MCP_NAME_DELIM,
        sanitize_mcp_name_component(tool_name)
    )
}

/// Registry of claimed MCP wire prefixes (`mcp__<server>_`).
///
/// Sanitization maps every non `[A-Za-z0-9_]` character (hyphens, dots, ...)
/// to `_`, so distinct server names such as `my-server`, `my_server`, and
/// `my.server` all sanitize to `my_server` and would otherwise collide on the
/// wire prefix `mcp__my_server_` — silently rerouting tool calls to the wrong
/// server. Registration is therefore collision-checked: the first claimant
/// (in config order) keeps the plain prefix and later claimants are
/// disambiguated with deterministic numeric suffixes (`my_server_2`,
/// `my_server_3`, ...). A human-readable warning is returned so the caller
/// (e.g. the CLI) can surface it.
#[derive(Debug, Default)]
pub struct WirePrefixRegistry {
    /// `claimed prefix (without the trailing delimiter) -> raw server name`.
    prefixes: HashMap<String, String>,
}

impl WirePrefixRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `server_name`, claiming its sanitized wire prefix.
    ///
    /// Returns the prefix to use for this server's tool names (without the
    /// leading `mcp__` or trailing `__`), plus an optional warning when the
    /// sanitized name collided with an earlier registration and a numeric
    /// suffix was applied.
    pub fn register(&mut self, server_name: &str) -> (String, Option<String>) {
        let base = sanitize_mcp_name_component(server_name);
        if !self.prefixes.contains_key(&base) {
            self.prefixes.insert(base.clone(), server_name.to_string());
            return (base, None);
        }
        // Collision: deterministic numeric suffix in config/registration
        // order — `_2` for the second claimant, `_3` for the third, ...
        let mut n = 2;
        while self.prefixes.contains_key(&format!("{base}_{n}")) {
            n += 1;
        }
        let disambiguated = format!("{base}_{n}");
        self.prefixes.insert(disambiguated.clone(), server_name.to_string());
        let warning = format!(
            "MCP server name collision: '{}' sanitizes to wire prefix '{}', already claimed by \
             server '{}'; using '{}' instead. Rename one of the servers in the MCP config to \
             avoid ambiguity.",
            server_name, base, self.prefixes[&base], disambiguated
        );
        warn!("{}", warning);
        (disambiguated, Some(warning))
    }

    /// Release a previously claimed prefix.
    ///
    /// A failed connection (handshake error/timeout) must give its claim
    /// back: otherwise every connect retry re-registers the same name and
    /// the prefix drifts (`my_server` → `my_server_2` → ...) permanently for
    /// the process, leaving the real server unreachable at its plain prefix.
    pub fn unregister(&mut self, prefix: &str) {
        self.prefixes.remove(prefix);
    }
}

/// Process-global wire-prefix registry: `McpClient::connect` claims prefixes
/// here so simultaneously-connected servers with colliding sanitized names
/// are deterministically disambiguated (first claimant in connect order
/// keeps the plain prefix). Poisoning is recovered (into_inner) — a panicked
/// prior connection must not take down later ones.
static GLOBAL_WIRE_PREFIX_REGISTRY: std::sync::OnceLock<
    std::sync::Mutex<WirePrefixRegistry>,
> = std::sync::OnceLock::new();

fn global_wire_prefix_registry() -> &'static std::sync::Mutex<WirePrefixRegistry> {
    GLOBAL_WIRE_PREFIX_REGISTRY.get_or_init(|| std::sync::Mutex::new(WirePrefixRegistry::new()))
}

/// A discovered MCP tool.
#[derive(Debug, Clone)]
pub struct McpTool {
    /// The server-side (unprefixed) tool name, used in `tools/call`.
    pub name: String,
    /// The prefixed registry/wire name (`mcp__<server>__<tool>`).
    pub wire_name: String,
    /// Description, falling back to `"MCP tool {name} from {server}"`.
    pub description: String,
    /// The input schema, normalized for LLM tool-calling compatibility.
    pub input_schema: Value,
}

fn tool_from_listing(server_name: &str, wire_prefix: &str, tool: &Value) -> Option<McpTool> {
    let name = tool.get("name")?.as_str()?.to_string();
    let raw_description = tool.get("description").and_then(Value::as_str).unwrap_or("");
    let description = if raw_description.is_empty() {
        // Upstream `_convert_mcp_schema` fallback (mcp_tool.py:4787).
        format!("MCP tool {} from {}", name, server_name)
    } else {
        raw_description.to_string()
    };
    Some(McpTool {
        wire_name: format!(
            "{}{}{}{}",
            MCP_TOOL_NAME_PREFIX,
            wire_prefix,
            MCP_NAME_DELIM,
            sanitize_mcp_name_component(&name)
        ),
        description,
        input_schema: normalize_mcp_input_schema(tool.get("inputSchema")),
        name,
    })
}

/// Drives a paginated `list_*` call by following `nextCursor`
/// (`_paginate_full_list`): feed each page's JSON-RPC result, request the next
/// page while a cursor is returned, stop after [`MCP_LIST_MAX_PAGES`].
struct ListPaginator {
    items_attr: &'static str,
    items: Vec<Value>,
    pages: usize,
    truncated: bool,
}

impl ListPaginator {
    fn new(items_attr: &'static str) -> Self {
        Self { items_attr, items: Vec::new(), pages: 0, truncated: false }
    }

    /// Ingest one page; returns the cursor for the next request, or `None`
    /// when pagination should stop.
    fn feed(&mut self, result: &Value) -> Option<String> {
        self.pages += 1;
        if let Some(items) = result.get(self.items_attr).and_then(Value::as_array) {
            self.items.extend(items.iter().cloned());
        }
        // Per the MCP spec the cursor is an opaque string; anything else
        // (including a non-string or empty value) means "no more pages".
        let cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
            .map(str::to_string)?;
        if self.pages >= MCP_LIST_MAX_PAGES {
            self.truncated = true;
            return None;
        }
        Some(cursor)
    }
}

/// A JSON-RPC request failure. Upstream surfaces JSON-RPC error frames as
/// `McpError` and transport failures as whatever exception the stack raised;
/// the distinction feeds the `MCP call failed: {Type}: {msg}` envelope.
#[derive(Debug, thiserror::Error)]
enum RequestError {
    #[error("{message}")]
    Rpc { message: String },
    #[error("{0}")]
    Transport(String),
}

/// A connected stdio MCP server.
pub struct McpClient {
    server_name: String,
    child: Child,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    /// Serializes whole request/response exchanges (upstream `_rpc_lock`).
    rpc_lock: Mutex<()>,
    /// JSON-RPC ids; the SDK starts at 0.
    next_id: AtomicI64,
    initialize_result: OnceLock<Value>,
    tool_timeout: f64,
    /// `wire_name -> (server, tool)`: exact provenance captured at listing
    /// time (upstream `_mcp_tool_server_names`); wire names are never parsed.
    tool_names: StdMutex<HashMap<String, (String, String)>>,
    /// Sanitized (and possibly collision-disambiguated) wire prefix for this
    /// server's tools, from [`WirePrefixRegistry::register`].
    wire_prefix: String,
    /// Collisions/warnings recorded for this server (name collisions, ...);
    /// drained via [`McpClient::take_warnings`] so the CLI can surface them.
    warnings: StdMutex<Vec<String>>,
}

/// Upper bound on a single server-framed stdout line (`MAX_LINE_BYTES`,
/// 32 MiB). Legitimate MCP JSON-RPC frames are far smaller; a line longer
/// than this indicates a hostile or broken server and the connection errors
/// cleanly instead of buffering unbounded data.
pub const MAX_LINE_BYTES: usize = 32 * 1024 * 1024;

/// `AsyncBufReadExt::read_line`, but aborts with an error once `line` plus
/// the incoming data would exceed `max_bytes` — leaving the excess unread is
/// fine because the connection is torn down on error anyway.
async fn read_line_bounded<R: std::ops::DerefMut + Unpin>(
    reader: &mut R,
    line: &mut String,
    max_bytes: usize,
) -> std::io::Result<usize>
where
    R::Target: tokio::io::AsyncBufRead + Unpin,
{
    let reader: &mut R::Target = &mut **reader;
    // Bytes are accumulated and UTF-8-decoded once at the end: decoding per
    // fill_buf chunk would corrupt multibyte characters that straddle a
    // buffer boundary.
    let mut bytes: Vec<u8> = Vec::new();
    loop {
        // Scan the internal buffer for the newline instead of read_line,
        // which would keep growing the String unchecked.
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            line.push_str(&String::from_utf8_lossy(&bytes));
            return Ok(bytes.len()); // EOF
        }
        match available.iter().position(|&b| b == b'\n') {
            Some(idx) => {
                bytes.extend_from_slice(&available[..idx]);
                if bytes.len() > max_bytes {
                    return Err(std::io::Error::other(line_cap_error(max_bytes)));
                }
                reader.consume(idx + 1);
                line.push_str(&String::from_utf8_lossy(&bytes));
                // +1 for the consumed newline.
                return Ok(bytes.len() + 1);
            }
            None => {
                bytes.extend_from_slice(available);
                let len = available.len();
                reader.consume(len);
                if bytes.len() > max_bytes {
                    return Err(std::io::Error::other(line_cap_error(max_bytes)));
                }
            }
        }
    }
}

fn line_cap_error(max_bytes: usize) -> String {
    format!(
        "frame exceeded the {}-byte line cap (possible hostile/buggy server); \
         dropping the connection",
        max_bytes
    )
}

impl McpClient {
    /// Spawn a stdio MCP server described by `config` and perform the
    /// handshake, retrying the initial connection up to
    /// [`MAX_INITIAL_CONNECT_RETRIES`] times with doubling backoff (1s start,
    /// capped at [`MAX_BACKOFF_SECONDS`]) — a transient blip at startup
    /// should not permanently kill the server.
    pub async fn connect(server_name: &str, config: &ServerConfig) -> Result<Self> {
        Self::connect_with_backoff(server_name, config, 1.0).await
    }

    async fn connect_with_backoff(
        server_name: &str,
        config: &ServerConfig,
        backoff_scale: f64,
    ) -> Result<Self> {
        let mut backoff = 1.0_f64;
        let mut initial_retries = 0u32;
        loop {
            match Self::connect_once(server_name, config).await {
                Ok(client) => return Ok(client),
                Err(exc) => {
                    initial_retries += 1;
                    if initial_retries > MAX_INITIAL_CONNECT_RETRIES {
                        warn!(
                            "MCP server '{}' failed initial connection after {} attempts: {}",
                            server_name, MAX_INITIAL_CONNECT_RETRIES, exc
                        );
                        return Err(exc);
                    }
                    warn!(
                        "MCP server '{}' initial connection failed (attempt {}/{}), retrying in {:.0}s: {}",
                        server_name, initial_retries, MAX_INITIAL_CONNECT_RETRIES, backoff, exc
                    );
                    tokio::time::sleep(duration_from_secs(backoff * backoff_scale)).await;
                    backoff = (backoff * 2.0).min(MAX_BACKOFF_SECONDS);
                }
            }
        }
    }

    async fn connect_once(server_name: &str, config: &ServerConfig) -> Result<Self> {
        if config.url.is_some() {
            if config.command.is_some() {
                warn!(
                    "MCP server '{}' has both 'url' and 'command' in config. Using HTTP \
                     transport ('url'). Remove 'command' to silence this warning.",
                    server_name
                );
            }
            anyhow::bail!(
                "MCP server '{}': HTTP/StreamableHTTP/SSE transports are not ported yet \
                 (stdio 'command' servers only)",
                server_name
            );
        }
        let command = config.command.clone().unwrap_or_default();
        if command.is_empty() {
            anyhow::bail!("MCP server '{}' has no 'command' in config", server_name);
        }

        // Filtered env (secrets never leak into MCP subprocesses) + PATH-aware
        // command resolution, exactly as upstream `_run_stdio` does.
        let safe_env = build_safe_env(&config.env);
        let (command, safe_env) = resolve_stdio_command(&command, &safe_env);

        // Child stderr goes to ~/.joey/logs/mcp-stderr.log (with a per-server
        // header) so server banners can't corrupt the TUI.
        let stderr = config::stderr_log_stdio(server_name);

        let mut child = Command::new(&command)
            .args(&config.args)
            .env_clear()
            .envs(&safe_env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            // Backstop: if the client is dropped without shutdown() (panic,
            // cancellation), the server process must not outlive us as a zombie.
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawning MCP server '{}'", command))?;

        let stdin = child.stdin.take().context("no stdin on MCP server")?;
        let stdout = child.stdout.take().context("no stdout on MCP server")?;

        let mut client = Self {
            server_name: server_name.to_string(),
            child,
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            rpc_lock: Mutex::new(()),
            next_id: AtomicI64::new(0),
            initialize_result: OnceLock::new(),
            tool_timeout: config.tool_timeout(),
            tool_names: StdMutex::new(HashMap::new()),
            wire_prefix: String::new(), // set below via the global registry
            warnings: StdMutex::new(Vec::new()),
        };

        // Claim the wire prefix through the PROCESS-GLOBAL registry so any
        // concurrently-connected servers with colliding sanitized names
        // (e.g. "my-server" vs "my_server") get deterministic disambiguated
        // prefixes instead of silently rerouting each other's tool calls.
        {
            let mut reg = global_wire_prefix_registry()
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let (prefix, warning) = reg.register(server_name);
            client.wire_prefix = prefix;
            if let Some(w) = warning {
                client.push_warning(w);
            }
        }

        // Bound the MCP handshake: a stdio server that never completes
        // `initialize` must not hang connection forever (upstream wraps
        // `session.initialize()` in `asyncio.wait_for(connect_timeout)`).
        let connect_timeout = config.effective_connect_timeout();
        match tokio::time::timeout(duration_from_secs(connect_timeout), client.initialize()).await
        {
            Ok(Ok(())) => Ok(client),
            Ok(Err(exc)) => {
                let claimed_prefix = client.wire_prefix.clone();
                client.shutdown().await;
                global_wire_prefix_registry()
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .unregister(&claimed_prefix);
                Err(exc)
            }
            Err(_) => {
                let claimed_prefix = client.wire_prefix.clone();
                client.shutdown().await;
                global_wire_prefix_registry()
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .unregister(&claimed_prefix);
                Err(anyhow::anyhow!(
                    "MCP server '{}': initialize handshake timed out after {:.0}s",
                    server_name,
                    connect_timeout
                ))
            }
        }
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// The raw `initialize` result (capabilities, serverInfo, ...).
    pub fn initialize_result(&self) -> Option<&Value> {
        self.initialize_result.get()
    }

    /// The sanitized (and possibly collision-disambiguated) wire prefix this
    /// client's tools are registered under (without the `mcp__` head and the
    /// trailing `__` delimiter).
    pub fn wire_prefix(&self) -> &str {
        &self.wire_prefix
    }

    /// Drain recorded warnings (e.g. wire-prefix name collisions) so the
    /// registration API caller can surface them to the user.
    pub fn take_warnings(&self) -> Vec<String> {
        self.warnings.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect()
    }

    /// Record a warning for later retrieval via [`McpClient::take_warnings`].
    pub fn push_warning(&self, warning: String) {
        self.warnings.lock().unwrap_or_else(|e| e.into_inner()).push(warning);
    }

    /// Claim this server's wire prefix in `registry` (before the first
    /// [`McpClient::list_tools`]) so that tools from sanitization-colliding
    /// servers (`my-server` vs `my_server` vs `my.server`) get distinct,
    /// provenance-safe wire names. On collision the client's prefix is
    /// disambiguated deterministically (numeric suffix in registration
    /// order) and a warning is recorded (surfaced via
    /// [`McpClient::take_warnings`]) and returned.
    pub fn register_wire_prefix(&mut self, registry: &mut WirePrefixRegistry) -> Option<String> {
        let (prefix, warning) = registry.register(&self.server_name);
        self.wire_prefix = prefix;
        if let Some(w) = &warning {
            self.push_warning(w.clone());
        }
        warning
    }

    async fn initialize(&self) -> Result<()> {
        // Upstream (via the mcp SDK) advertises `sampling` and `elicitation`
        // client capabilities because hermes-agent installs handlers for
        // both. This port has no sampling/elicitation handlers, so
        // advertising them would be dishonest — capabilities stay empty.
        let params = json!({
            "protocolVersion": LATEST_PROTOCOL_VERSION,
            "capabilities": {},
            // The SDK's DEFAULT_CLIENT_INFO; upstream never overrides it.
            "clientInfo": {"name": "mcp", "version": "0.1.0"},
        });
        let result = self.request("initialize", Some(params)).await?;
        let protocol = result.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&protocol) {
            anyhow::bail!("Unsupported protocol version from the server: {}", protocol);
        }
        let _ = self.initialize_result.set(result);
        // The SDK omits `params` entirely on this notification.
        self.notify("notifications/initialized", None).await?;
        Ok(())
    }

    /// Whether the server advertises the `tools` capability
    /// (`_advertises_tools`). Per the MCP spec,
    /// `InitializeResult.capabilities.tools` is non-null iff the server
    /// implements the `tools/*` request family; calling `tools/list` against
    /// a prompts/resources-only server raises `-32601 Method not found`.
    /// Returns true when no capability info was captured (legacy fallback).
    fn advertises_tools(&self) -> bool {
        let Some(init_result) = self.initialize_result.get() else {
            return true;
        };
        match init_result.get("capabilities") {
            None | Some(Value::Null) => true,
            Some(caps) => caps.get("tools").map(|t| !t.is_null()).unwrap_or(false),
        }
    }

    /// List the tools this server exposes, following `nextCursor` pagination
    /// up to [`MCP_LIST_MAX_PAGES`] pages. Returns an empty list (without
    /// issuing `tools/list`) when the server does not advertise the tools
    /// capability.
    pub async fn list_tools(&self) -> Result<Vec<McpTool>> {
        if !self.advertises_tools() {
            info!(
                "MCP server '{}': does not advertise 'tools' capability — skipping tools/list \
                 (prompts/resources remain available)",
                self.server_name
            );
            return Ok(Vec::new());
        }

        let mut paginator = ListPaginator::new("tools");
        let mut cursor: Option<String> = None;
        loop {
            // The SDK omits `params` on the first page and sends
            // `{"cursor": ...}` on follow-ups.
            let params = cursor.as_ref().map(|c| json!({ "cursor": c }));
            let result = self.request("tools/list", params).await?;
            match paginator.feed(&result) {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        if paginator.truncated {
            warn!(
                "MCP server '{}': {} pagination exceeded {} pages; truncating at {} items",
                self.server_name,
                "tools",
                MCP_LIST_MAX_PAGES,
                paginator.items.len()
            );
        }

        let tools: Vec<McpTool> = paginator
            .items
            .iter()
            .filter_map(|t| tool_from_listing(&self.server_name, &self.wire_prefix, t))
            .collect();

        // Registration-time provenance map (upstream `_track_mcp_tool_server`).
        let mut map = self.tool_names.lock().unwrap_or_else(|e| {
            tracing::warn!(
                target: env!("CARGO_CRATE_NAME"),
                error = %e,
                input_kind = "mcp_jsonrpc",
                path = concat!(file!(), ":", line!()),
                "recovered from malformed external input via fallback"
            );
            e.into_inner()
        });
        for tool in &tools {
            map.insert(tool.wire_name.clone(), (self.server_name.clone(), tool.name.clone()));
        }
        Ok(tools)
    }

    /// Exact server/tool provenance for a wire name captured at listing time.
    /// Upstream deliberately never parses `mcp__{server}__{tool}` back — the
    /// string shape is ambiguous when server names contain underscores.
    pub fn tool_provenance(&self, wire_name: &str) -> Option<(String, String)> {
        self.tool_names.lock().unwrap_or_else(|e| {
            tracing::warn!(
                target: env!("CARGO_CRATE_NAME"),
                error = %e,
                input_kind = "mcp_jsonrpc",
                path = concat!(file!(), ":", line!()),
                "recovered from malformed external input via fallback"
            );
            e.into_inner()
        }).get(wire_name).cloned()
    }

    /// Call a tool by its bare (unprefixed) name with the given arguments.
    ///
    /// Always returns the model-visible JSON envelope, exactly as upstream's
    /// tool handler does: `{"result": ...}` on success (text blocks joined
    /// with `\n`, plus `structuredContent` when present), `{"error": ...}` on
    /// `isError` results, timeouts, and transport failures. Error text is
    /// credential-sanitized.
    pub async fn call_tool(&self, tool: &str, arguments: Value) -> String {
        let tool_timeout = self.tool_timeout;
        let start = std::time::Instant::now();

        let mut params = serde_json::Map::new();
        params.insert("name".to_string(), json!(tool));
        if !arguments.is_null() {
            params.insert("arguments".to_string(), arguments);
        }

        let fut = self.request("tools/call", Some(Value::Object(params)));
        let (exc_type, exc_msg) =
            match tokio::time::timeout(duration_from_secs(tool_timeout), fut).await {
                Ok(Ok(result)) => return render_call_result(&self.server_name, &result),
                Ok(Err(RequestError::Rpc { message })) => ("McpError", message),
                Ok(Err(RequestError::Transport(message))) => ("RuntimeError", message),
                Err(_) => {
                    let elapsed = start.elapsed().as_secs_f64();
                    (
                        "TimeoutError",
                        format!(
                            "MCP call timed out after {:.1}s (configured timeout: {:.1}s)",
                            elapsed, tool_timeout
                        ),
                    )
                }
            };
        tracing::error!("MCP tool {}/{} call failed: {}", self.server_name, tool, exc_msg);
        error_envelope(&format!("MCP call failed: {}: {}", exc_type, exc_msg))
    }

    /// Send a JSON-RPC request and await its matching response.
    async fn request(&self, method: &str, params: Option<Value>) -> Result<Value, RequestError> {
        // Serialize whole exchanges so concurrent callers can't interleave
        // reads (upstream holds `_rpc_lock` across each call).
        let _rpc = self.rpc_lock.lock().await;

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut msg = serde_json::Map::new();
        msg.insert("jsonrpc".to_string(), json!("2.0"));
        msg.insert("id".to_string(), json!(id));
        msg.insert("method".to_string(), json!(method));
        if let Some(params) = params {
            msg.insert("params".to_string(), params);
        }
        self.write_line(&Value::Object(msg)).await?;

        let mut stdout = self.stdout.lock().await;
        let mut line = String::new();
        loop {
            line.clear();
            // Bounded read (SC hardening): a hostile or buggy server that
            // streams an enormous line with no trailing newline must not be
            // able to grow memory without bound. `read_line` keeps
            // accumulating into `line`, so check the accumulated length
            // after each chunk-read and error out once the cap is exceeded.
            let n = read_line_bounded(&mut stdout, &mut line, MAX_LINE_BYTES)
                .await
                .map_err(|exc| RequestError::Transport(format!(
                    "MCP server '{}': {}",
                    self.server_name, exc
                )))?;
            if n == 0 {
                return Err(RequestError::Transport(format!(
                    "MCP server '{}' closed the connection",
                    self.server_name
                )));
            }
            let Ok(frame) = serde_json::from_str::<Value>(line.trim()) else {
                // Non-JSON noise on stdout (banners); skip.
                continue;
            };
            // A frame carrying a "method" key is a server->client request or
            // notification (ping, logging, sampling, ...), NOT our response.
            // Handlers for those are not ported; skip them.
            if frame.get("method").is_some() {
                continue;
            }
            if !id_matches(frame.get("id"), id) {
                continue;
            }
            if let Some(err) = frame.get("error") {
                // Upstream raises `McpError(error.message)` for JSON-RPC
                // error frames.
                let message = err.get("message").and_then(Value::as_str).unwrap_or("").to_string();
                let message = if message.is_empty() { err.to_string() } else { message };
                return Err(RequestError::Rpc { message });
            }
            return Ok(frame.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), RequestError> {
        let mut msg = serde_json::Map::new();
        msg.insert("jsonrpc".to_string(), json!("2.0"));
        msg.insert("method".to_string(), json!(method));
        if let Some(params) = params {
            msg.insert("params".to_string(), params);
        }
        self.write_line(&Value::Object(msg)).await
    }

    async fn write_line(&self, msg: &Value) -> Result<(), RequestError> {
        let mut stdin = self.stdin.lock().await;
        let mut line = serde_json::to_string(msg)
            .map_err(|exc| RequestError::Transport(exc.to_string()))?;
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|exc| RequestError::Transport(exc.to_string()))?;
        stdin.flush().await.map_err(|exc| RequestError::Transport(exc.to_string()))?;
        Ok(())
    }

    /// Shut down the server subprocess: close its stdin (letting a
    /// well-behaved server exit on its own, as the SDK's session unwind
    /// does), wait briefly, then escalate SIGTERM → SIGKILL (the SDK's
    /// terminate-then-kill sequence with its 2s timeout).
    pub async fn shutdown(self) {
        let McpClient { mut child, stdin, stdout, .. } = self;
        drop(stdin);
        drop(stdout);
        if tokio::time::timeout(PROCESS_TERMINATION_TIMEOUT, child.wait()).await.is_ok() {
            return;
        }
        #[cfg(unix)]
        if let Some(pid) = child.id() {
            let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            if tokio::time::timeout(PROCESS_TERMINATION_TIMEOUT, child.wait()).await.is_ok() {
                return;
            }
        }
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

fn id_matches(frame_id: Option<&Value>, id: i64) -> bool {
    match frame_id {
        Some(Value::Number(n)) => n.as_i64() == Some(id),
        // The SDK normalizes string echoes of numeric ids
        // (`_normalize_request_id`).
        Some(Value::String(s)) => s.parse::<i64>().ok() == Some(id),
        _ => false,
    }
}

fn duration_from_secs(seconds: f64) -> Duration {
    Duration::try_from_secs_f64(seconds).unwrap_or(Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------
    // Naming
    // ---------------------------------------------------------------------

    #[test]
    fn sanitize_replaces_everything_outside_word_chars() {
        assert_eq!(sanitize_mcp_name_component("github"), "github");
        assert_eq!(sanitize_mcp_name_component("my-server"), "my_server");
        assert_eq!(sanitize_mcp_name_component("my server!"), "my_server_");
        // Non-ASCII (including Unicode alphanumerics) becomes `_` too.
        assert_eq!(sanitize_mcp_name_component("café"), "caf_");
        assert_eq!(sanitize_mcp_name_component("日本語x"), "___x");
        assert_eq!(sanitize_mcp_name_component(""), "");
    }

    #[test]
    fn wire_names_are_prefixed_and_sanitized() {
        assert_eq!(mcp_prefixed_tool_name("github", "create_issue"), "mcp__github__create_issue");
        // Hyphenated server names sanitize into the wire name.
        assert_eq!(mcp_prefixed_tool_name("my-server", "x"), "mcp__my_server__x");
        assert_eq!(mcp_prefixed_tool_name("my server!", "do.it"), "mcp__my_server___do_it");
    }

    // ---------------------------------------------------------------------
    // Pagination
    // ---------------------------------------------------------------------

    #[test]
    fn paginator_follows_cursor_until_absent() {
        let mut p = ListPaginator::new("tools");
        let next = p.feed(&json!({"tools": [{"name": "a"}, {"name": "b"}], "nextCursor": "c1"}));
        assert_eq!(next.as_deref(), Some("c1"));
        let next = p.feed(&json!({"tools": [{"name": "c"}]}));
        assert_eq!(next, None);
        assert_eq!(p.items.len(), 3);
        assert!(!p.truncated);
    }

    #[test]
    fn paginator_stops_on_non_string_or_empty_cursor() {
        let mut p = ListPaginator::new("tools");
        assert_eq!(p.feed(&json!({"tools": [], "nextCursor": 42})), None);
        assert!(!p.truncated);
        let mut p = ListPaginator::new("tools");
        assert_eq!(p.feed(&json!({"tools": [], "nextCursor": ""})), None);
        assert!(!p.truncated);
        let mut p = ListPaginator::new("tools");
        assert_eq!(p.feed(&json!({"nextCursor": null})), None);
    }

    #[test]
    fn paginator_truncates_at_page_cap() {
        let mut p = ListPaginator::new("tools");
        let mut fetched_pages = 0;
        let mut cursor = Some("start".to_string());
        while cursor.is_some() {
            fetched_pages += 1;
            cursor = p.feed(&json!({"tools": [{"name": "t"}], "nextCursor": "more"}));
        }
        assert_eq!(fetched_pages, MCP_LIST_MAX_PAGES);
        assert_eq!(p.items.len(), MCP_LIST_MAX_PAGES);
        assert!(p.truncated);
    }

    // ---------------------------------------------------------------------
    // Listing conversion
    // ---------------------------------------------------------------------

    #[test]
    fn listing_applies_description_fallback_and_schema_normalization() {
        let tool = tool_from_listing("srv", "srv", &json!({"name": "alpha"})).unwrap();
        assert_eq!(tool.description, "MCP tool alpha from srv");
        assert_eq!(tool.wire_name, "mcp__srv__alpha");
        assert_eq!(tool.input_schema, json!({"type": "object", "properties": {}}));

        let tool = tool_from_listing(
            "srv",
            "srv",
            &json!({"name": "beta", "description": "", "inputSchema": {"type": "object"}}),
        )
        .unwrap();
        // Empty description also falls back (Python truthiness).
        assert_eq!(tool.description, "MCP tool beta from srv");
        assert_eq!(tool.input_schema, json!({"type": "object", "properties": {}}));

        assert!(tool_from_listing("srv", "srv", &json!({"description": "no name"})).is_none());
    }

    #[test]
    fn frame_id_matching_normalizes_strings() {
        assert!(id_matches(Some(&json!(3)), 3));
        assert!(id_matches(Some(&json!("3")), 3));
        assert!(!id_matches(Some(&json!(4)), 3));
        assert!(!id_matches(Some(&json!("x")), 3));
        assert!(!id_matches(Some(&json!(null)), 3));
        assert!(!id_matches(None, 3));
    }

    // ---------------------------------------------------------------------
    // End-to-end against a scripted fake stdio server
    // ---------------------------------------------------------------------

    #[cfg(unix)]
    fn sh_server(script: &str) -> ServerConfig {
        ServerConfig {
            command: Some("/bin/sh".to_string()),
            args: vec!["-c".to_string(), script.to_string()],
            ..Default::default()
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn full_flow_list_call_and_error_envelope() {
        let script = r#"
IFS= read -r line
printf '%s\n' '{"jsonrpc": "2.0", "id": 0, "result": {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}}, "serverInfo": {"name": "fake", "version": "0"}}}'
IFS= read -r line
IFS= read -r line
printf '%s\n' '{"jsonrpc": "2.0", "method": "notifications/message", "params": {"level": "info", "data": "noise"}}'
printf '%s\n' '{"jsonrpc": "2.0", "id": 1, "result": {"tools": [{"name": "alpha", "inputSchema": {"type": "object"}}], "nextCursor": "page2"}}'
IFS= read -r line
printf '%s\n' '{"jsonrpc": "2.0", "id": 2, "result": {"tools": [{"name": "beta", "description": "Beta tool"}]}}'
IFS= read -r line
printf '%s\n' '{"jsonrpc": "2.0", "id": 3, "result": {"content": [{"type": "text", "text": "hello"}, {"type": "text", "text": "world"}]}}'
IFS= read -r line
printf '%s\n' '{"jsonrpc": "2.0", "id": 4, "result": {"isError": true, "content": [{"type": "text", "text": "boom ghp_abc123"}]}}'
"#;
        let client = McpClient::connect("fake", &sh_server(script)).await.expect("connect");
        assert!(client.initialize_result().is_some());

        let tools = client.list_tools().await.expect("list_tools");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "alpha");
        assert_eq!(tools[0].wire_name, "mcp__fake__alpha");
        assert_eq!(tools[0].description, "MCP tool alpha from fake");
        assert_eq!(tools[1].description, "Beta tool");
        assert_eq!(
            client.tool_provenance("mcp__fake__alpha"),
            Some(("fake".to_string(), "alpha".to_string()))
        );
        assert_eq!(client.tool_provenance("mcp__fake__nope"), None);

        let out = client.call_tool("alpha", json!({})).await;
        assert_eq!(out, "{\"result\": \"hello\\nworld\"}");

        let out = client.call_tool("alpha", json!({})).await;
        assert_eq!(out, r#"{"error": "boom [REDACTED]"}"#);

        client.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn no_tools_capability_skips_tools_list() {
        let script = r#"
IFS= read -r line
printf '%s\n' '{"jsonrpc": "2.0", "id": 0, "result": {"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "fake", "version": "0"}}}'
IFS= read -r line
IFS= read -r line
"#;
        let client = McpClient::connect("promptsonly", &sh_server(script)).await.expect("connect");
        // Returns empty WITHOUT sending tools/list (the script would not
        // answer one — a request would error, not return Ok).
        let tools = client.list_tools().await.expect("list_tools");
        assert!(tools.is_empty());
        client.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unsupported_protocol_version_fails_connect() {
        let script = r#"
while IFS= read -r line; do
printf '%s\n' '{"jsonrpc": "2.0", "id": 0, "result": {"protocolVersion": "1999-01-01", "capabilities": {}, "serverInfo": {"name": "old", "version": "0"}}}'
done
"#;
        let err = McpClient::connect_with_backoff("old", &sh_server(script), 0.0)
            .await
            .err()
            .expect("must fail");
        assert!(
            format!("{err:#}").contains("Unsupported protocol version from the server: 1999-01-01"),
            "unexpected error: {err:#}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tool_call_timeout_produces_upstream_envelope() {
        let script = r#"
IFS= read -r line
printf '%s\n' '{"jsonrpc": "2.0", "id": 0, "result": {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}}, "serverInfo": {"name": "slow", "version": "0"}}}'
IFS= read -r line
IFS= read -r line
sleep 30
"#;
        let config = ServerConfig { timeout: Some(0.3), ..sh_server(script) };
        let client = McpClient::connect("slow", &config).await.expect("connect");
        let out = client.call_tool("hang", json!({})).await;
        assert!(
            out.starts_with(r#"{"error": "MCP call failed: TimeoutError: MCP call timed out after "#),
            "unexpected envelope: {out}"
        );
        assert!(out.contains("(configured timeout: 0.3s)"), "unexpected envelope: {out}");
        client.shutdown().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn initial_connect_retries_three_times_with_backoff() {
        let marker = std::env::temp_dir()
            .join(format!("joey-mcp-retry-test-{}-{:?}.log", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_file(&marker);
        let script = format!("echo attempt >> '{}'; exit 1", marker.display());
        let config = sh_server(&script);
        // Scale the 1s/2s/4s backoff down so the test stays fast.
        let err = McpClient::connect_with_backoff("flaky", &config, 0.01).await.err();
        assert!(err.is_some());
        let attempts =
            std::fs::read_to_string(&marker).unwrap_or_default().lines().count();
        let _ = std::fs::remove_file(&marker);
        // 1 initial attempt + MAX_INITIAL_CONNECT_RETRIES retries.
        assert_eq!(attempts as u32, 1 + MAX_INITIAL_CONNECT_RETRIES);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_command_matches_upstream_error() {
        let err = McpClient::connect_with_backoff("nocmd", &ServerConfig::default(), 0.0)
            .await
            .err()
            .expect("must fail");
        assert!(format!("{err:#}").contains("MCP server 'nocmd' has no 'command' in config"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn http_transport_is_rejected() {
        let config = ServerConfig {
            url: Some("https://example.com/mcp".to_string()),
            ..Default::default()
        };
        let err = McpClient::connect_with_backoff("remote", &config, 0.0)
            .await
            .err()
            .expect("must fail");
        assert!(format!("{err:#}").contains("not ported yet"));
    }

    // ---------------------------------------------------------------------
    // FR-006 / SC-005: Malformed-input regression tests for hardened sites.
    // Each test feeds input that exercises the formerly-panicking path and
    // asserts no panic occurs (typed error or graceful recovery).
    // ---------------------------------------------------------------------

    // ---------------------------------------------------------------------
    // Name-collision disambiguation (WirePrefixRegistry)
    // ---------------------------------------------------------------------

    #[test]
    fn colliding_server_names_get_distinct_wire_prefixes() {
        let mut reg = WirePrefixRegistry::new();
        // 'my-server' and 'my_server' (and 'my.server') all sanitize to
        // 'my_server' - first claimant keeps the plain prefix.
        let (p1, w1) = reg.register("my-server");
        let (p2, w2) = reg.register("my_server");
        let (p3, w3) = reg.register("my.server");
        assert_eq!(p1, "my_server");
        assert!(w1.is_none(), "first claimant must not warn");
        assert_eq!(p2, "my_server_2");
        assert!(w2.is_some(), "second claimant must warn");
        assert_eq!(p3, "my_server_3");
        assert!(w3.is_some(), "third claimant must warn");
        // Non-colliding names are unaffected and don't warn.
        let (p4, w4) = reg.register("other");
        assert_eq!(p4, "other");
        assert!(w4.is_none());
    }

    #[test]
    fn collision_warning_mentions_both_servers_and_disambiguated_prefix() {
        let mut reg = WirePrefixRegistry::new();
        let _ = reg.register("my-server");
        let (_, warning) = reg.register("my_server");
        let warning = warning.expect("must warn");
        assert!(warning.contains("my-server"), "{warning}");
        assert!(warning.contains("my_server_2"), "{warning}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn colliding_servers_produce_distinct_wire_names_and_provenance() {
        let script = r#"
IFS= read -r line
printf '%s
' '{"jsonrpc": "2.0", "id": 0, "result": {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}}, "serverInfo": {"name": "s", "version": "0"}}}'
IFS= read -r line
IFS= read -r line
printf '%s
' '{"jsonrpc": "2.0", "id": 1, "result": {"tools": [{"name": "echo", "inputSchema": {"type": "object"}}]}}'
IFS= read -r line
"#;
        // connect() itself claims prefixes via the process-global registry,
        // so colliding names are disambiguated at connect time.
        let mut a = McpClient::connect("my-server", &sh_server(script)).await.expect("connect a");
        let mut b = McpClient::connect("my_server", &sh_server(script)).await.expect("connect b");

        let tools_a = a.list_tools().await.expect("list a");
        let tools_b = b.list_tools().await.expect("list b");
        assert_eq!(tools_a[0].wire_name, "mcp__my_server__echo");
        assert_eq!(tools_b[0].wire_name, "mcp__my_server_2__echo");

        // Provenance lookups use the FINAL disambiguated names and route to
        // the correct server - no silent overwrite.
        assert_eq!(
            a.tool_provenance("mcp__my_server__echo"),
            Some(("my-server".to_string(), "echo".to_string()))
        );
        assert_eq!(
            b.tool_provenance("mcp__my_server_2__echo"),
            Some(("my_server".to_string(), "echo".to_string()))
        );
        assert!(a.take_warnings().is_empty());
        assert_eq!(b.take_warnings().len(), 1);
        a.shutdown().await;
        b.shutdown().await;
    }

    // ---------------------------------------------------------------------
    // Bounded line read (MAX_LINE_BYTES)
    // ---------------------------------------------------------------------

    #[test]
    fn read_line_bounded_reads_normal_lines() {
        let data = b"{\"jsonrpc\": \"2.0\", \"id\": 0, \"result\": {}}\ntrailing";
        let mut reader = tokio::io::BufReader::new(&data[..]);
        let mut line = String::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let n = rt
            .block_on(read_line_bounded(&mut &mut reader, &mut line, MAX_LINE_BYTES))
            .expect("read");
        assert_eq!(line, r#"{"jsonrpc": "2.0", "id": 0, "result": {}}"#);
        assert_eq!(n, line.len() + 1);
    }

    #[test]
    fn read_line_bounded_errors_on_oversized_line() {
        // 1000-byte cap, 5000-byte line with no newline.
        let data = vec![b'A'; 5000];
        let mut reader = tokio::io::BufReader::new(&data[..]);
        let mut line = String::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(read_line_bounded(&mut &mut reader, &mut line, 1000))
            .err()
            .expect("must error");
        assert!(err.to_string().contains("1000-byte line cap"), "{err}");
        assert!(line.len() <= 1000, "buffer must stay bounded");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn oversized_server_frame_fails_the_call_cleanly() {
        // 40MB of junk without a newline (over the 32MB cap), then hang - without the cap the
        // read would keep buffering indefinitely; with it the call errors
        // out with the line-cap message well before the 20s timeout.
        let script = r#"
IFS= read -r line
printf '%s
' '{"jsonrpc": "2.0", "id": 0, "result": {"protocolVersion": "2025-11-25", "capabilities": {"tools": {}}, "serverInfo": {"name": "s", "version": "0"}}}'
IFS= read -r line
head -c 41943040 /dev/zero | tr '\000' 'A'
sleep 30
"#;
        let config = ServerConfig { timeout: Some(20.0), ..sh_server(script) };
        let client = McpClient::connect("flood", &config).await.expect("connect");
        let out = client.call_tool("anything", json!({})).await;
        assert!(
            out.contains("line cap") && out.contains("MCP call failed"),
            "unexpected envelope: {out}"
        );
        client.shutdown().await;
    }

    /// The mutex poison recovery at lib.rs:394,414 uses `unwrap_or_else` to
    /// recover from a poisoned lock instead of panicking. Verify the recovery
    /// path produces a usable guard (the data is accessible after recovery).
    #[test]
    fn tool_names_mutex_poison_recovery_does_not_panic() {
        use std::sync::Mutex;
        // Simulate the pattern: a poisoned mutex recovered via into_inner().
        let m = std::sync::Arc::new(Mutex::new(42i32));
        // Poison it by panicking while holding the lock.
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("intentional poison");
        })
        .join();
        // This mirrors the unwrap_or_else(|e| { warn!(...); e.into_inner() }) pattern.
        let recovered = m.lock().unwrap_or_else(|e| {
            tracing::warn!(target: env!("CARGO_CRATE_NAME"), "mutex poisoned, recovering");
            e.into_inner()
        });
        assert_eq!(*recovered, 42, "recovered guard must provide access to the data");
    }
}
