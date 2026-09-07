//! LSP (Language Server Protocol) integration for joey-agent.
//!
//! Port of crush's `internal/lsp/` — provides:
//!   - Lazy LSP server management (starts servers on demand per file type)
//!   - Diagnostics collection (publishes to the agent as a tool)
//!   - Go-to-definition, references, document symbols, rename
//!
//! LSP servers are configured in `~/.joey/config.yaml` — any language with
//! a language server works; the examples below cover the languages joey
//! parses with dedicated tree-sitter grammars:
//!
//! ```yaml
//! lsp:
//!   rust:
//!     command: "rust-analyzer"
//!     file_types: ["rs"]
//!   python:
//!     command: "pylsp"
//!     file_types: ["py"]
//!   typescript:
//!     command: "typescript-language-server"
//!     args: ["--stdio"]
//!     file_types: ["ts", "tsx", "js", "jsx"]
//!   go:
//!     command: "gopls"
//!     file_types: ["go"]
//!   ruby:
//!     command: "solargraph"
//!     args: ["stdio"]
//!     file_types: ["rb"]
//!   php:
//!     command: "intelephense"
//!     args: ["--stdio"]
//!     file_types: ["php"]
//!   csharp:
//!     command: "OmniSharp"
//!     args: ["-lsp"]
//!     file_types: ["cs"]
//!   c:
//!     command: "clangd"
//!     file_types: ["c", "h"]
//!   cpp:
//!     command: "clangd"
//!     file_types: ["cpp", "cc", "cxx", "hpp"]
//!   haskell:
//!     command: "haskell-language-server"
//!     args: ["--lsp"]
//!     file_types: ["hs"]
//!   bash:
//!     command: "bash-language-server"
//!     args: ["start"]
//!     file_types: ["sh", "bash"]
//! ```
//!
//! The agent accesses LSP features via tools: `lsp_diagnostics`,
//! `lsp_definition`, `lsp_references`, `lsp_symbols`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Maximum accepted JSON-RPC message size (16MB). The Content-Length
/// header is server-controlled; without a cap a malicious/buggy server
/// could make us attempt an unbounded allocation.
const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

/// LSP server configuration from config.yaml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub file_types: Vec<String>,
    #[serde(default)]
    pub root_markers: Vec<String>,
    #[serde(default)]
    pub init_options: Option<Value>,
}

/// A diagnostic from the language server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub line: usize,      // 0-indexed
    pub character: usize, // 0-indexed
    pub severity: String, // "error", "warning", "info", "hint"
    pub message: String,
    pub source: Option<String>,
}

/// Diagnostic counts by severity.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiagnosticCounts {
    pub errors: usize,
    pub warnings: usize,
    pub info: usize,
    pub hints: usize,
}

impl std::fmt::Display for DiagnosticCounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if self.errors > 0 {
            parts.push(format!("{} error(s)", self.errors));
        }
        if self.warnings > 0 {
            parts.push(format!("{} warning(s)", self.warnings));
        }
        if self.info > 0 {
            parts.push(format!("{} info", self.info));
        }
        if parts.is_empty() {
            write!(f, "no diagnostics")
        } else {
            write!(f, "{}", parts.join(", "))
        }
    }
}

/// A location reference (go-to-def, references result).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    pub file: String,
    pub line: usize,      // 0-indexed
    pub character: usize, // 0-indexed
    pub end_line: Option<usize>,
    pub end_character: Option<usize>,
}

/// A document symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSymbol {
    pub name: String,
    pub kind: String,
    pub line: usize,
    pub character: usize,
    pub detail: Option<String>,
}

/// The LSP manager: owns running LSP server processes, routes requests.
pub struct LspManager {
    /// config name → server config
    configs: HashMap<String, LspServerConfig>,
    /// file extension (without dot) → config name
    extension_map: HashMap<String, String>,
    /// config name → running client
    clients: HashMap<String, LspClient>,
    /// workspace root
    root: PathBuf,
}

/// A connected LSP client (one per server process).
struct LspClient {
    process: Child,
    stdin: Box<dyn Write + Send>,
    stdout: BufReader<std::process::ChildStdout>,
    /// path → diagnostics cache
    diagnostics: HashMap<String, Vec<Diagnostic>>,
    next_id: i64,
    initialized: bool,
}

impl LspManager {
    /// Create from config.
    pub fn new(root: impl Into<PathBuf>, configs: HashMap<String, LspServerConfig>) -> Self {
        let mut extension_map = HashMap::new();
        for (name, cfg) in &configs {
            for ft in &cfg.file_types {
                extension_map.insert(ft.clone(), name.clone());
            }
        }
        Self {
            configs,
            extension_map,
            clients: HashMap::new(),
            root: root.into(),
        }
    }

    /// Parse LSP configs from the joey config.
    pub fn from_joey_config(config: &joey_core::Config, root: impl Into<PathBuf>) -> Self {
        let mut configs = HashMap::new();
        if let Some(lsp) = config.get("lsp") {
            if let Some(mapping) = lsp.as_mapping() {
                for (key, val) in mapping {
                    if let (Some(name), Ok(cfg)) = (key.as_str(), serde_yaml::from_value::<LspServerConfig>(val.clone())) {
                        if cfg.command.is_empty() {
                            continue;
                        }
                        configs.insert(name.to_string(), cfg);
                    }
                }
            }
        }
        Self::new(root, configs)
    }

    /// Whether any LSP servers are configured.
    pub fn has_servers(&self) -> bool {
        !self.configs.is_empty()
    }

    /// Get the config name for a file path based on its extension.
    fn config_for_path(&self, path: &str) -> Option<(&str, &LspServerConfig)> {
        let ext = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())?;
        let name = self.extension_map.get(&ext)?;
        self.configs.get(name).map(|c| (name.as_str(), c))
    }

    /// Ensure the LSP server for this file type is running (lazy start).
    /// Returns the config name on success.
    fn ensure_client(&mut self, config_name: &str) -> Result<&mut LspClient, LspError> {
        // Finding #12 (review tools.md): if the entry exists but its
        // process is dead (request watchdog SIGKILLed a hung server, or
        // the server exited on its own), drop the dead entry so a fresh
        // server starts below. Previously the dead client stayed in the
        // map forever and every later request failed writing to its stdin.
        let dead = match self.clients.get_mut(config_name) {
            Some(client) => matches!(client.process.try_wait(), Ok(Some(_)) | Err(_)),
            None => false,
        };
        if dead {
            if let Some(mut client) = self.clients.remove(config_name) {
                let _ = client.process.kill(); // reap any straggler
            }
        }
        if !self.clients.contains_key(config_name) {
            let cfg = self
                .configs
                .get(config_name)
                .ok_or(LspError::NoServerForType)?;
            let client = LspClient::start(cfg, &self.root)?;
            self.clients.insert(config_name.to_string(), client);
        }
        // SAFETY: `config_name` was inserted into `self.clients` just
        // above (or already existed); `get_mut` is guaranteed Some.
        Ok(self.clients.get_mut(config_name).unwrap())
    }

    /// Finding #12 (review tools.md): a request that failed with an I/O
    /// error means the server process is gone — a write to its dead stdin
    /// hit EPIPE, or the read side saw EOF after the request watchdog
    /// SIGKILLed a hung server. Evict the dead client from the map so the
    /// NEXT `ensure_client` respawns the server, exactly as `request`'s
    /// doc comment always promised; previously the dead entry stayed in
    /// the map forever and every later LSP call failed. The kill is
    /// best-effort (the process is usually already dead; this just reaps
    /// any straggler so it can't be orphaned).
    fn evict_if_io_error(&mut self, config_name: &str, err: &LspError) {
        if matches!(err, LspError::Io(_)) {
            if let Some(mut client) = self.clients.remove(config_name) {
                let _ = client.process.kill();
            }
        }
    }

    /// Get diagnostics for a file. Starts the server if needed.
    pub fn diagnostics(&mut self, path: &str) -> Result<Vec<Diagnostic>, LspError> {
        let abs = self.resolve_path(path);
        let config_name = self
            .config_for_path(path)
            .ok_or(LspError::NoServerForType)?
            .0
            .to_string();
        let (sync, diags) = {
            let client = self.ensure_client(&config_name)?;
            // Synchronize: fire a cheap request. Servers publish diagnostics
            // after didOpen asynchronously; by the time they answer ANY
            // subsequent request, the publishDiagnostics notification is already
            // in the pipe — `request()`'s read loop captures it on the way past
            // (the old code slept 500ms then drained nothing, so diagnostics
            // were only ever captured incidentally). A failure here is
            // deliberately swallowed (diagnostics are best-effort) but the
            // dead-client eviction below still fires so the next call
            // respawns (#12).
            let sync = client.open_document(&abs).and_then(|_| {
                client.request(
                    "textDocument/documentSymbol",
                    json!({ "textDocument": { "uri": path_to_uri(&abs) } }),
                )
            });
            let diags = client.diagnostics.get(&abs).cloned().unwrap_or_default();
            (sync, diags)
        };
        if let Err(e) = &sync {
            self.evict_if_io_error(&config_name, e);
        }
        Ok(diags)
    }

    /// Go to definition.
    pub fn definition(
        &mut self,
        path: &str,
        line: usize,
        character: usize,
    ) -> Result<Vec<Location>, LspError> {
        let abs = self.resolve_path(path);
        let config_name = self
            .config_for_path(path)
            .ok_or(LspError::NoServerForType)?
            .0
            .to_string();
        let result = {
            let client = self.ensure_client(&config_name)?;
            client.open_document(&abs).and_then(|_| {
                client.request(
                    "textDocument/definition",
                    json!({
                        "textDocument": { "uri": path_to_uri(&abs) },
                        "position": { "line": line, "character": character }
                    }),
                )
            })
        };
        if let Err(e) = &result {
            self.evict_if_io_error(&config_name, e);
        }
        Ok(parse_locations(result?))
    }

    /// Find references.
    pub fn references(
        &mut self,
        path: &str,
        line: usize,
        character: usize,
    ) -> Result<Vec<Location>, LspError> {
        let abs = self.resolve_path(path);
        let config_name = self
            .config_for_path(path)
            .ok_or(LspError::NoServerForType)?
            .0
            .to_string();
        let result = {
            let client = self.ensure_client(&config_name)?;
            client.open_document(&abs).and_then(|_| {
                client.request(
                    "textDocument/references",
                    json!({
                        "textDocument": { "uri": path_to_uri(&abs) },
                        "position": { "line": line, "character": character },
                        "context": { "includeDeclaration": true }
                    }),
                )
            })
        };
        if let Err(e) = &result {
            self.evict_if_io_error(&config_name, e);
        }
        Ok(parse_locations(result?))
    }

    /// Document symbols.
    pub fn document_symbols(&mut self, path: &str) -> Result<Vec<DocumentSymbol>, LspError> {
        let abs = self.resolve_path(path);
        let config_name = self
            .config_for_path(path)
            .ok_or(LspError::NoServerForType)?
            .0
            .to_string();
        let result = {
            let client = self.ensure_client(&config_name)?;
            client.open_document(&abs).and_then(|_| {
                client.request(
                    "textDocument/documentSymbol",
                    json!({ "textDocument": { "uri": path_to_uri(&abs) } }),
                )
            })
        };
        if let Err(e) = &result {
            self.evict_if_io_error(&config_name, e);
        }
        Ok(parse_symbols(result?))
    }

    /// Rename a symbol.
    pub fn rename(
        &mut self,
        path: &str,
        line: usize,
        character: usize,
        new_name: &str,
    ) -> Result<HashMap<String, Vec<TextEdit>>, LspError> {
        let abs = self.resolve_path(path);
        let config_name = self
            .config_for_path(path)
            .ok_or(LspError::NoServerForType)?
            .0
            .to_string();
        let result = {
            let client = self.ensure_client(&config_name)?;
            client.open_document(&abs).and_then(|_| {
                client.request(
                    "textDocument/rename",
                    json!({
                        "textDocument": { "uri": path_to_uri(&abs) },
                        "position": { "line": line, "character": character },
                        "newName": new_name
                    }),
                )
            })
        };
        if let Err(e) = &result {
            self.evict_if_io_error(&config_name, e);
        }
        Ok(parse_workspace_edits(result?))
    }

    /// Resolve a relative path against the workspace root.
    fn resolve_path(&self, path: &str) -> String {
        let expanded = shellexpand::tilde(path).to_string();
        let p = PathBuf::from(&expanded);
        if p.is_absolute() {
            p.to_string_lossy().to_string()
        } else {
            self.root.join(p).to_string_lossy().to_string()
        }
    }

    /// Shut down all servers.
    pub fn shutdown(&mut self) {
        for (_, mut client) in self.clients.drain() {
            let _ = client.shutdown();
        }
    }
}

impl Drop for LspManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl LspClient {
    /// Start an LSP server process.
    fn start(cfg: &LspServerConfig, root: &Path) -> Result<Self, LspError> {
        let mut cmd = Command::new(&cfg.command);
        cmd.args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // stderr to null: piped-but-never-read deadlocks verbose servers
            // once the 64KB pipe buffer fills. Diagnostics-relevant chatter
            // is not needed for protocol operation.
            .stderr(Stdio::null())
            .current_dir(root);

        let mut process = cmd.spawn().map_err(|e| LspError::SpawnFailed {
            command: cfg.command.clone(),
            source: e,
        })?;

        let stdin: Box<dyn Write + Send> = Box::new(
            process
                .stdin
                .take()
                .ok_or(LspError::Io("no stdin".into()))?,
        );
        let stdout = BufReader::new(
            process
                .stdout
                .take()
                .ok_or(LspError::Io("no stdout".into()))?,
        );

        let mut client = Self {
            process,
            stdin,
            stdout,
            diagnostics: HashMap::new(),
            next_id: 1,
            initialized: false,
        };

        client.initialize(root)?;
        Ok(client)
    }

    /// Send the LSP initialize request.
    fn initialize(&mut self, root: &Path) -> Result<(), LspError> {
        let root_uri = path_to_uri(&root.to_string_lossy());
        let result = self.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "publishDiagnostics": { "relatedInformation": true }
                    }
                },
            }),
        )?;
        let _ = result; // capabilities — we proceed regardless

        // Send initialized notification.
        self.notify("initialized", json!({}))?;
        self.initialized = true;
        Ok(())
    }

    /// Open a document (didOpen).
    fn open_document(&mut self, path: &str) -> Result<(), LspError> {
        let uri = path_to_uri(path);
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let language_id = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("plaintext")
            .to_string();

        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": content
                }
            }),
        )?;
        Ok(())
    }

    /// Send a request and wait for the response.
    ///
    /// Bounded: a watchdog thread kills the server process after
    /// `REQUEST_TIMEOUT`, which unblocks the reader with EOF — a hung
    /// server can never hold the (manager-wide) LSP mutex forever. The
    /// next call respawns the server (start is retried per call).
    fn request(&mut self, method: &str, params: Value) -> Result<Value, LspError> {
        const REQUEST_TIMEOUT: u64 = 30;
        let pid = self.process.id();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel_flag = cancel.clone();
        let watchdog = std::thread::spawn(move || {
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_secs(REQUEST_TIMEOUT);
            loop {
                if cancel_flag.load(std::sync::atomic::Ordering::SeqCst) {
                    return; // request completed — cancel
                }
                if std::time::Instant::now() >= deadline {
                    // Still not done → kill the hung server (EOF unblocks
                    // the reader; next call respawns).
                    unsafe {
                        libc::kill(pid as i32, libc::SIGKILL);
                    }
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        });
        let result = self.request_inner(method, params);
        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = watchdog.join();
        result
    }

    fn request_inner(&mut self, method: &str, params: Value) -> Result<Value, LspError> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        self.send_message(&msg)?;
        // Read until we get the response with matching id.
        loop {
            let response = self.read_message()?;
            // Check if it's a notification (diagnostics).
            if response.get("method").and_then(|m| m.as_str()) == Some("textDocument/publishDiagnostics") {
                self.handle_diagnostics(&response);
                continue;
            }
            if response.get("id").and_then(|i| i.as_i64()) == Some(id) {
                if let Some(err) = response.get("error") {
                    return Err(LspError::ServerError(err.clone()));
                }
                return Ok(response.get("result").cloned().unwrap_or(Value::Null));
            }
            // Some other message — ignore.
        }
    }

    /// Send a notification (no response expected).
    fn notify(&mut self, method: &str, params: Value) -> Result<(), LspError> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        self.send_message(&msg)
    }

    /// Handle a publishDiagnostics notification.
    fn handle_diagnostics(&mut self, msg: &Value) {
        let Some(params) = msg.get("params") else { return };
        let uri = params
            .get("uri")
            .and_then(|u| u.as_str())
            .unwrap_or_default()
            .to_string();
        let path = uri_to_path(&uri);
        let diags = params
            .get("diagnostics")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|d| Diagnostic {
                        line: d
                            .pointer("/range/start/line")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as usize,
                        character: d
                            .pointer("/range/start/character")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as usize,
                        severity: severity_string(
                            d.get("severity").and_then(|s| s.as_u64()).unwrap_or(1),
                        ),
                        message: d
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("")
                            .to_string(),
                        source: d
                            .get("source")
                            .and_then(|s| s.as_str())
                            .map(String::from),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.diagnostics.insert(path, diags);
    }

    /// Send a JSON-RPC message (Content-Length framed).
    fn send_message(&mut self, msg: &Value) -> Result<(), LspError> {
        let body = serde_json::to_string(msg).map_err(|e| LspError::Io(e.to_string()))?;
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body)
            .map_err(|e| LspError::Io(e.to_string()))?;
        self.stdin
            .flush()
            .map_err(|e| LspError::Io(e.to_string()))?;
        Ok(())
    }

    /// Read a JSON-RPC message (Content-Length framed).
    fn read_message(&mut self) -> Result<Value, LspError> {
        // Read headers until empty line.
        let mut content_length = None;
        loop {
            let mut line = String::new();
            let n = self
                .stdout
                .read_line(&mut line)
                .map_err(|e| LspError::Io(e.to_string()))?;
            if n == 0 {
                return Err(LspError::Io("EOF from server".into()));
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(len) = trimmed.strip_prefix("Content-Length: ") {
                content_length = len.parse::<usize>().ok();
            }
        }
        let len = content_length.ok_or_else(|| LspError::Io("no Content-Length".into()))?;
        if len > MAX_MESSAGE_BYTES {
            return Err(LspError::Io(format!(
                "LSP message too large: Content-Length {} exceeds cap of {} bytes",
                len, MAX_MESSAGE_BYTES
            )));
        }
        let mut buf = vec![0u8; len];
        self.stdout
            .read_exact(&mut buf)
            .map_err(|e| LspError::Io(e.to_string()))?;
        let value: Value =
            serde_json::from_slice(&buf).map_err(|e| LspError::Io(e.to_string()))?;
        Ok(value)
    }

    /// Send shutdown + exit to the server.
    fn shutdown(&mut self) -> Result<(), LspError> {
        let _ = self.request("shutdown", Value::Null);
        let _ = self.notify("exit", Value::Null);
        let _ = self.process.kill();
        Ok(())
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────

/// Percent-encode a single byte if it is not URI-unreserved.
/// Unreserved = ALPHA / DIGIT / "-" / "." / "_" / "~" (RFC 3986 §2.3).
/// Finding #13 (review tools.md): LSP URIs must percent-encode reserved
/// bytes — without it, paths containing spaces, '#' or '%' produce URIs
/// the server re-parses differently, and diagnostics keyed under the
/// encoded URI never match the raw-path lookup.
fn encode_uri_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for &b in path.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Percent-decode a URI path component ('%' + 2 hex digits → byte).
/// Invalid/truncated escapes are passed through verbatim (defensive —
/// servers may send non-conforming URIs).
fn decode_uri_path(uri: &str) -> String {
    let bytes = uri.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| -> Option<u8> {
                match b {
                    b'0'..=b'9' => Some(b - b'0'),
                    b'a'..=b'f' => Some(b - b'a' + 10),
                    b'A'..=b'F' => Some(b - b'A' + 10),
                    _ => None,
                }
            };
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn path_to_uri(path: &str) -> String {
    let abs = if Path::new(path).is_absolute() {
        path.to_string()
    } else {
        std::env::current_dir()
            .ok()
            .map(|c| c.join(path).to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string())
    };
    format!("file://{}", encode_uri_path(&abs))
}

fn uri_to_path(uri: &str) -> String {
    let stripped = uri.strip_prefix("file://").unwrap_or(uri);
    decode_uri_path(stripped)
}

fn severity_string(severity: u64) -> String {
    match severity {
        1 => "error",
        2 => "warning",
        3 => "info",
        4 => "hint",
        _ => "error",
    }
    .to_string()
}

fn parse_locations(value: Value) -> Vec<Location> {
    let array = if value.is_array() {
        value
    } else if value.is_object() {
        Value::Array(vec![value])
    } else {
        return Vec::new();
    };
    array
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|loc| {
                    let uri = loc.get("uri").and_then(|u| u.as_str())?;
                    Some(Location {
                        file: uri_to_path(uri),
                        line: loc
                            .pointer("/range/start/line")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as usize,
                        character: loc
                            .pointer("/range/start/character")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as usize,
                        end_line: loc
                            .pointer("/range/end/line")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as usize),
                        end_character: loc
                            .pointer("/range/end/character")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as usize),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_symbols(value: Value) -> Vec<DocumentSymbol> {
    value
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|sym| {
                    Some(DocumentSymbol {
                        name: sym
                            .get("name")
                            .and_then(|n| n.as_str())?
                            .to_string(),
                        kind: symbol_kind_string(
                            sym.get("kind").and_then(|k| k.as_u64()).unwrap_or(1),
                        ),
                        line: sym
                            .pointer("/range/start/line")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as usize,
                        character: sym
                            .pointer("/range/start/character")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as usize,
                        detail: sym
                            .get("detail")
                            .and_then(|d| d.as_str())
                            .map(String::from),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_workspace_edits(value: Value) -> HashMap<String, Vec<TextEdit>> {
    let mut result = HashMap::new();
    if let Some(changes) = value.get("changes").and_then(|c| c.as_object()) {
        for (uri, edits) in changes {
            let path = uri_to_path(uri);
            let edit_list: Vec<TextEdit> = edits
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|e| {
                            Some(TextEdit {
                                line: e
                                    .pointer("/range/start/line")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0) as usize,
                                character: e
                                    .pointer("/range/start/character")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0) as usize,
                                end_line: e
                                    .pointer("/range/end/line")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0) as usize,
                                end_character: e
                                    .pointer("/range/end/character")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0) as usize,
                                new_text: e
                                    .get("newText")
                                    .and_then(|t| t.as_str())?
                                    .to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            result.insert(path, edit_list);
        }
    }
    result
}

fn symbol_kind_string(kind: u64) -> String {
    match kind {
        1 => "File",
        2 => "Module",
        3 => "Namespace",
        4 => "Package",
        5 => "Class",
        6 => "Method",
        7 => "Property",
        8 => "Field",
        9 => "Constructor",
        10 => "Enum",
        11 => "Interface",
        12 => "Function",
        13 => "Variable",
        14 => "Constant",
        15 => "String",
        16 => "Number",
        17 => "Boolean",
        18 => "Array",
        23 => "Struct",
        24 => "Event",
        25 => "Operator",
        26 => "TypeParameter",
        _ => "Symbol",
    }
    .to_string()
}

/// A text edit from rename.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextEdit {
    pub line: usize,
    pub character: usize,
    pub end_line: usize,
    pub end_character: usize,
    pub new_text: String,
}

/// LSP errors.
#[derive(Debug)]
pub enum LspError {
    NoServerForType,
    SpawnFailed { command: String, source: std::io::Error },
    Io(String),
    ServerError(Value),
}

impl std::fmt::Display for LspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LspError::NoServerForType => write!(f, "No LSP server configured for this file type"),
            LspError::SpawnFailed { command, source } => {
                write!(f, "Failed to spawn LSP server '{}': {}", command, source)
            }
            LspError::Io(msg) => write!(f, "LSP I/O error: {}", msg),
            LspError::ServerError(err) => write!(f, "LSP server error: {}", err),
        }
    }
}

impl std::error::Error for LspError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_uri_roundtrip() {
        let path = "/home/user/project/file.rs";
        let uri = path_to_uri(path);
        assert!(uri.starts_with("file://"));
        assert_eq!(uri_to_path(&uri), path);
    }

    #[test]
    fn severity_strings() {
        assert_eq!(severity_string(1), "error");
        assert_eq!(severity_string(2), "warning");
        assert_eq!(severity_string(3), "info");
        assert_eq!(severity_string(4), "hint");
    }

    #[test]
    fn parse_empty_locations() {
        let locs = parse_locations(Value::Null);
        assert!(locs.is_empty());
    }

    #[test]
    fn parse_single_location() {
        let val = json!([{
            "uri": "file:///home/user/file.rs",
            "range": {
                "start": { "line": 10, "character": 5 },
                "end": { "line": 10, "character": 15 }
            }
        }]);
        let locs = parse_locations(val);
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].file, "/home/user/file.rs");
        assert_eq!(locs[0].line, 10);
        assert_eq!(locs[0].character, 5);
    }

    #[test]
    fn symbol_kinds() {
        assert_eq!(symbol_kind_string(5), "Class");
        assert_eq!(symbol_kind_string(12), "Function");
        assert_eq!(symbol_kind_string(23), "Struct");
    }

    #[test]
    fn manager_with_empty_config() {
        let mgr = LspManager::new("/tmp", HashMap::new());
        assert!(!mgr.has_servers());
    }

    // ── FR-006/SC-005 regression tests ──────────────────────────────

    #[test]
    fn lsp_parse_malformed_json_does_not_panic() {
        // Malformed / degenerate values fed to every LSP JSON parser.
        // These exercise the SAFETY-guarded parsing paths and the
        // defensive `unwrap_or` / `filter_map` chains.

        // parse_locations with non-array / non-object / null.
        let locs = parse_locations(Value::Null);
        assert!(locs.is_empty());
        let locs = parse_locations(json!("just a string"));
        assert!(locs.is_empty());
        let locs = parse_locations(json!(42));
        assert!(locs.is_empty());

        // parse_locations with array of garbage entries.
        let locs = parse_locations(json!([
            { "not_uri": true },
            { "uri": "file:///x.rs" },           // missing range
            { "uri": 123 },                       // wrong type
            { "uri": "file:///y.rs", "range": "not-an-object" },
            null,
            "garbage"
        ]));
        // Should not panic; entries without uri+range are silently dropped.
        let _ = locs;

        // parse_symbols with non-array and garbage entries.
        let syms = parse_symbols(Value::Null);
        assert!(syms.is_empty());
        let syms = parse_symbols(json!("string"));
        assert!(syms.is_empty());
        let syms = parse_symbols(json!([
            { "no_name": true },
            { "name": "fn1", "kind": "not-a-number" },
            { "name": 99 },
            null,
            { "name": "ok", "kind": 12, "range": { "start": { "line": "x" } } }
        ]));
        let _ = syms;

        // parse_workspace_edits with malformed changes object.
        let edits = parse_workspace_edits(json!({
            "changes": "not-an-object"
        }));
        assert!(edits.is_empty());

        let edits = parse_workspace_edits(json!({
            "changes": {
                "file:///a.rs": "not-an-array",
                "file:///b.rs": [ { "no_newText": true }, null, { "newText": 42 } ]
            }
        }));
        let _ = edits;

        // parse_workspace_edits with no changes key at all.
        let edits = parse_workspace_edits(json!({ "result": null }));
        assert!(edits.is_empty());
    }

    // ── Finding #12 (review tools.md): dead-client eviction ────────────

    /// Minimal fake LSP server (python3). Answers `initialize` and every
    /// request with an empty result. Controlled by a marker file in the
    /// CWD (the manager sets CWD = workspace root): on the FIRST run
    /// (marker absent) it answers initialize, creates the marker, and
    /// exits — simulating a server that dies mid-session; on later runs
    /// (marker present) it serves forever.
    const FAKE_LSP_SERVER: &str = r#"
import sys, json, os

MARKER = "fake_lsp_first_run_done"
first_run = not os.path.exists(MARKER)

def readmsg():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line or line == b"\r\n":
            break
        k, _, v = line.decode().partition(":")
        headers[k.strip().lower()] = v.strip()
    n = int(headers.get("content-length", 0))
    return json.loads(sys.stdin.buffer.read(n))

def sendmsg(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()

while True:
    msg = readmsg()
    if msg is None:
        break
    if msg.get("method") == "initialize":
        sendmsg({"jsonrpc": "2.0", "id": msg["id"], "result": {"capabilities": {}}})
        if first_run:
            open(MARKER, "w").close()
            sys.exit(0)
    elif "id" in msg:
        sendmsg({"jsonrpc": "2.0", "id": msg["id"], "result": []})
"#;

    fn fake_lsp_config(root: &std::path::Path) -> HashMap<String, LspServerConfig> {
        let script = root.join("fake_lsp_server.py");
        std::fs::write(&script, FAKE_LSP_SERVER).unwrap();
        let mut configs = HashMap::new();
        configs.insert(
            "python".to_string(),
            LspServerConfig {
                command: "python3".to_string(),
                args: vec![script.to_string_lossy().to_string()],
                file_types: vec!["py".to_string()],
                root_markers: vec![],
                init_options: None,
            },
        );
        configs
    }

    /// #12 regression (public API): a request I/O error (write to a dead
    /// stdin / EOF after the server dies) must EVICT the dead client so
    /// the next call RESPAWNS the server instead of failing forever.
    /// Run 1 of the fake server answers `initialize` then exits; the first
    /// document_symbols call therefore fails with Io. Run 2 serves
    /// normally; the second call must succeed.
    #[test]
    fn io_error_evicts_dead_client_and_next_call_respawns() {
        let root = tempfile::tempdir().unwrap();
        let mut mgr = LspManager::new(root.path(), fake_lsp_config(root.path()));

        let file = root.path().join("x.py");
        std::fs::write(&file, "x = 1\n").unwrap();

        // First call: server dies right after initialize → request fails
        // with an I/O error (and the dead entry is evicted).
        let first = mgr.document_symbols(file.to_str().unwrap());
        assert!(
            matches!(first, Err(LspError::Io(_))),
            "first call should fail with Io (server died), got {:?}",
            first
        );
        assert!(
            !matches!(first, Err(LspError::ServerError(_))),
            "failure must be I/O (dead server), not a protocol error"
        );

        // Second call: ensure_client must have respawned the server —
        // previously the dead entry stayed in the map and this (and every
        // later) call failed forever.
        let second = mgr.document_symbols(file.to_str().unwrap());
        assert!(
            second.is_ok(),
            "second call must respawn the server and succeed, got {:?}",
            second
        );
    }

    /// #12 regression (watchdog-kill shape): an entry whose child process
    /// was SIGKILLed (exactly what the request watchdog leaves behind)
    /// must be detected by ensure_client and replaced by a fresh spawn.
    #[test]
    fn killed_client_entry_is_detected_and_respawned() {
        let root = tempfile::tempdir().unwrap();
        // Marker pre-created → the fake server serves forever on run 1.
        std::fs::write(root.path().join("fake_lsp_first_run_done"), "").unwrap();
        let mut mgr = LspManager::new(root.path(), fake_lsp_config(root.path()));

        // Manually insert a dead client: spawn a real child, kill it, wait
        // for it to be reaped, and register it under the "python" config
        // (simulating what the watchdog leaves in self.clients).
        let mut child = std::process::Command::new("sleep")
            .arg("300")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let stdin: Box<dyn Write + Send> = Box::new(child.stdin.take().unwrap());
        let stdout = BufReader::new(child.stdout.take().unwrap());
        child.kill().unwrap();
        child.wait().unwrap();
        mgr.clients.insert(
            "python".to_string(),
            LspClient {
                process: child,
                stdin,
                stdout,
                diagnostics: HashMap::new(),
                next_id: 1,
                initialized: false,
            },
        );

        let file = root.path().join("x.py");
        std::fs::write(&file, "x = 1\n").unwrap();

        // Without the fix this fails forever (dead stdin); with it, the
        // dead entry is dropped and a fresh server is spawned.
        let result = mgr.document_symbols(file.to_str().unwrap());
        assert!(
            result.is_ok(),
            "request after watchdog-kill must respawn the server, got {:?}",
            result
        );
    }

    // ── Finding #13 (review tools.md): URI percent-encoding ────────────

    /// #13: paths with spaces percent-encode and round-trip.
    #[test]
    fn uri_encoding_spaces_roundtrip() {
        let path = "/tmp/my dir/file name.rs";
        let uri = path_to_uri(path);
        assert_eq!(uri, "file:///tmp/my%20dir/file%20name.rs");
        assert_eq!(uri_to_path(&uri), path);
    }

    /// #13: '#' and '%' are reserved and must be encoded; round-trip holds.
    #[test]
    fn uri_encoding_hash_and_percent_roundtrip() {
        for path in ["/tmp/a#b/file.rs", "/tmp/100%/weird#name.py"] {
            let uri = path_to_uri(path);
            assert!(uri.contains("%23") || !path.contains('#'), "hash encoded: {uri}");
            assert!(uri.contains("%25") || !path.contains('%'), "percent encoded: {uri}");
            assert_eq!(uri_to_path(&uri), path, "round-trip must restore the raw path");
        }
    }

    /// #13: diagnostics published under an ENCODED URI must be keyed under
    /// the decoded raw path so `diagnostics.get(&abs)` finds them.
    #[test]
    fn diagnostics_keyed_under_decoded_path() {
        let raw = "/tmp/my dir/a#b.rs";
        let uri = path_to_uri(raw); // encoded
        let msg = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "diagnostics": [
                    { "range": { "start": { "line": 3, "character": 7 },
                                  "end": { "line": 3, "character": 9 } },
                      "severity": 1, "message": "boom" }
                ]
            }
        });
        let mut sleep_child = std::process::Command::new("sleep")
            .arg("300")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin: Box<dyn Write + Send> = Box::new(sleep_child.stdin.take().unwrap());
        let stdout = BufReader::new(sleep_child.stdout.take().unwrap());
        let mut client = LspClient {
            process: sleep_child,
            stdin,
            stdout,
            diagnostics: HashMap::new(),
            next_id: 1,
            initialized: false,
        };
        client.handle_diagnostics(&msg);
        let diags = client
            .diagnostics
            .get(raw) // lookup uses the RAW path (manager `diagnostics()`)
            .expect("diagnostics must be findable under the raw decoded path");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].message, "boom");
        assert_eq!(diags[0].line, 3);
        let _ = client.process.kill();
    }

    /// #13: malformed percent-escapes pass through verbatim (defensive).
    #[test]
    fn uri_decoding_malformed_escapes_verbatim() {
        assert_eq!(uri_to_path("file:///a%2"), "/a%2");
        assert_eq!(uri_to_path("file:///a%zz"), "/a%zz");
        assert_eq!(uri_to_path("file:///100%"), "/100%");
    }
}
