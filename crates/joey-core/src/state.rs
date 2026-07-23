//! Session persistence (port of the load-bearing core of `hermes_state.py`).
//!
//! A single SQLite file under `~/.joey/state.db` with the EXACT upstream
//! schema (SCHEMA_VERSION 22): a hermes-created `state.db` opens and works
//! unchanged, and older joey databases are upgraded in place by the
//! declarative column reconciler (`_reconcile_columns` port).

use std::cell::Cell;
use std::path::PathBuf;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::constants;

/// Upstream schema version (hermes_state.py:155).
pub const SCHEMA_VERSION: i64 = 22;

/// Cap on user-controlled FTS5 query input before sanitizer processing.
pub const MAX_FTS5_QUERY_CHARS: usize = 2_048;

// ── Write-contention tuning (hermes_state.py:1002-1016) ──
const WRITE_MAX_RETRIES: u32 = 15;
const WRITE_RETRY_MIN_MS: u64 = 20;
const WRITE_RETRY_MAX_MS: u64 = 150;
const CHECKPOINT_EVERY_N_WRITES: u64 = 50;
const OPTIMIZE_EVERY_N_WRITES: u64 = 1000;

/// Verbatim port of upstream `SCHEMA_SQL` (hermes_state.py:758-905).
const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    user_id TEXT,
    session_key TEXT,
    chat_id TEXT,
    chat_type TEXT,
    thread_id TEXT,
    display_name TEXT,
    origin_json TEXT,
    expiry_finalized INTEGER DEFAULT 0,
    model TEXT,
    model_config TEXT,
    system_prompt TEXT,
    parent_session_id TEXT,
    started_at REAL NOT NULL,
    ended_at REAL,
    end_reason TEXT,
    message_count INTEGER DEFAULT 0,
    tool_call_count INTEGER DEFAULT 0,
    input_tokens INTEGER DEFAULT 0,
    output_tokens INTEGER DEFAULT 0,
    cache_read_tokens INTEGER DEFAULT 0,
    cache_write_tokens INTEGER DEFAULT 0,
    reasoning_tokens INTEGER DEFAULT 0,
    cwd TEXT,
    git_branch TEXT,
    git_repo_root TEXT,
    billing_provider TEXT,
    billing_base_url TEXT,
    billing_mode TEXT,
    estimated_cost_usd REAL,
    actual_cost_usd REAL,
    cost_status TEXT,
    cost_source TEXT,
    pricing_version TEXT,
    title TEXT,
    api_call_count INTEGER DEFAULT 0,
    handoff_state TEXT,
    handoff_platform TEXT,
    handoff_error TEXT,
    compression_failure_cooldown_until REAL,
    compression_failure_error TEXT,
    compression_fallback_streak INTEGER NOT NULL DEFAULT 0,
    profile_name TEXT,
    rewind_count INTEGER NOT NULL DEFAULT 0,
    archived INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (parent_session_id) REFERENCES sessions(id)
);

CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role TEXT NOT NULL,
    content TEXT,
    tool_call_id TEXT,
    tool_calls TEXT,
    tool_name TEXT,
    effect_disposition TEXT,
    timestamp REAL NOT NULL,
    token_count INTEGER,
    finish_reason TEXT,
    reasoning TEXT,
    reasoning_content TEXT,
    reasoning_details TEXT,
    codex_reasoning_items TEXT,
    codex_message_items TEXT,
    platform_message_id TEXT,
    observed INTEGER DEFAULT 0,
    active INTEGER NOT NULL DEFAULT 1,
    compacted INTEGER NOT NULL DEFAULT 0,
    api_content TEXT
);

CREATE TABLE IF NOT EXISTS session_model_usage (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    model TEXT NOT NULL,
    billing_provider TEXT NOT NULL DEFAULT '',
    billing_base_url TEXT NOT NULL DEFAULT '',
    billing_mode TEXT NOT NULL DEFAULT '',
    task TEXT NOT NULL DEFAULT '',
    api_call_count INTEGER NOT NULL DEFAULT 0,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    reasoning_tokens INTEGER NOT NULL DEFAULT 0,
    estimated_cost_usd REAL NOT NULL DEFAULT 0,
    actual_cost_usd REAL NOT NULL DEFAULT 0,
    cost_status TEXT,
    cost_source TEXT,
    first_seen REAL,
    last_seen REAL,
    PRIMARY KEY (session_id, model, billing_provider, billing_base_url, billing_mode, task)
);

CREATE TABLE IF NOT EXISTS state_meta (
    key TEXT PRIMARY KEY,
    value TEXT
);

CREATE TABLE IF NOT EXISTS gateway_routing (
    scope TEXT NOT NULL DEFAULT '',
    session_key TEXT NOT NULL,
    entry_json TEXT NOT NULL,
    updated_at REAL NOT NULL,
    PRIMARY KEY (scope, session_key)
);

CREATE TABLE IF NOT EXISTS compression_locks (
    session_id TEXT PRIMARY KEY,
    holder TEXT NOT NULL,
    acquired_at REAL NOT NULL,
    expires_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS async_delegations (
    delegation_id TEXT PRIMARY KEY,
    origin_session TEXT NOT NULL,
    origin_ui_session_id TEXT NOT NULL DEFAULT '',
    parent_session_id TEXT,
    state TEXT NOT NULL,
    dispatched_at REAL NOT NULL,
    completed_at REAL,
    updated_at REAL NOT NULL,
    event_json TEXT,
    result_json TEXT,
    delivery_state TEXT NOT NULL DEFAULT 'pending',
    delivery_attempts INTEGER NOT NULL DEFAULT 0,
    delivered_at REAL,
    owner_pid INTEGER,
    owner_started_at INTEGER,
    task_json TEXT,
    delivery_claim TEXT,
    delivery_claimed_at REAL
);

CREATE INDEX IF NOT EXISTS idx_sessions_source ON sessions(source);
CREATE INDEX IF NOT EXISTS idx_sessions_source_id ON sessions(source, id);
CREATE INDEX IF NOT EXISTS idx_sessions_parent ON sessions(parent_session_id);
CREATE INDEX IF NOT EXISTS idx_sessions_started ON sessions(started_at DESC);
CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, timestamp);
CREATE INDEX IF NOT EXISTS idx_compression_locks_expires ON compression_locks(expires_at);
CREATE INDEX IF NOT EXISTS idx_session_model_usage_session ON session_model_usage(session_id);
CREATE INDEX IF NOT EXISTS idx_session_model_usage_model ON session_model_usage(model);
CREATE INDEX IF NOT EXISTS idx_async_delegations_delivery
    ON async_delegations(delivery_state, completed_at);
"#;

/// Indexes referencing reconciler-added columns — created AFTER
/// `reconcile_columns` (hermes_state.py:907-921).
const DEFERRED_INDEX_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS idx_messages_session_active
    ON messages(session_id, active, timestamp);
CREATE INDEX IF NOT EXISTS idx_messages_active_null
    ON messages(active) WHERE active IS NULL;
CREATE INDEX IF NOT EXISTS idx_sessions_session_key
    ON sessions(session_key, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_sessions_gateway_peer
    ON sessions(source, user_id, chat_id, chat_type, thread_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_sessions_handoff_state
    ON sessions(handoff_state, started_at);
"#;

/// Standalone (inline-content) FTS5 table + triggers — verbatim upstream
/// `FTS_SQL` (hermes_state.py:923-947).
const FTS_SQL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    content
);

CREATE TRIGGER IF NOT EXISTS messages_fts_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, content) VALUES (
        new.id,
        COALESCE(new.content, '') || ' ' || COALESCE(new.tool_name, '') || ' ' || COALESCE(new.tool_calls, '')
    );
END;

CREATE TRIGGER IF NOT EXISTS messages_fts_delete AFTER DELETE ON messages BEGIN
    DELETE FROM messages_fts WHERE rowid = old.id;
END;

CREATE TRIGGER IF NOT EXISTS messages_fts_update AFTER UPDATE ON messages BEGIN
    DELETE FROM messages_fts WHERE rowid = old.id;
    INSERT INTO messages_fts(rowid, content) VALUES (
        new.id,
        COALESCE(new.content, '') || ' ' || COALESCE(new.tool_name, '') || ' ' || COALESCE(new.tool_calls, '')
    );
END;
"#;

/// Trigram FTS5 twin for CJK substring search — verbatim upstream
/// `FTS_TRIGRAM_SQL` (hermes_state.py:949-981). Created only when the
/// SQLite build has the trigram tokenizer (>= 3.34).
const FTS_TRIGRAM_SQL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts_trigram USING fts5(
    content,
    tokenize='trigram'
);

CREATE TRIGGER IF NOT EXISTS messages_fts_trigram_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts_trigram(rowid, content) VALUES (
        new.id,
        COALESCE(new.content, '') || ' ' || COALESCE(new.tool_name, '') || ' ' || COALESCE(new.tool_calls, '')
    );
END;

CREATE TRIGGER IF NOT EXISTS messages_fts_trigram_delete AFTER DELETE ON messages BEGIN
    DELETE FROM messages_fts_trigram WHERE rowid = old.id;
END;

CREATE TRIGGER IF NOT EXISTS messages_fts_trigram_update AFTER UPDATE ON messages BEGIN
    DELETE FROM messages_fts_trigram WHERE rowid = old.id;
    INSERT INTO messages_fts_trigram(rowid, content) VALUES (
        new.id,
        COALESCE(new.content, '') || ' ' || COALESCE(new.tool_name, '') || ' ' || COALESCE(new.tool_calls, '')
    );
END;
"#;

const FTS_TRIGGERS: &[&str] = &[
    "messages_fts_insert",
    "messages_fts_delete",
    "messages_fts_update",
    "messages_fts_trigram_insert",
    "messages_fts_trigram_delete",
    "messages_fts_trigram_update",
];

/// A chat role in the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }

    pub fn from_label(s: &str) -> Role {
        match s {
            "system" => Role::System,
            "assistant" => Role::Assistant,
            "tool" => Role::Tool,
            _ => Role::User,
        }
    }
}

/// A persisted message row (the CLI-facing subset of upstream columns).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub id: Option<i64>,
    pub session_id: String,
    pub role: Role,
    pub content: String,
    /// JSON-encoded assistant tool_calls (OpenAI shape), if any.
    pub tool_calls: Option<String>,
    /// Tool-call id this message responds to (for tool-role messages).
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub timestamp: f64,
    pub token_count: Option<i64>,
    pub finish_reason: Option<String>,
    pub reasoning: Option<String>,
}

impl StoredMessage {
    pub fn new(session_id: impl Into<String>, role: Role, content: impl Into<String>) -> Self {
        Self {
            id: None,
            session_id: session_id.into(),
            role,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
            tool_name: None,
            timestamp: unix_now(),
            token_count: None,
            finish_reason: None,
            reasoning: None,
        }
    }
}

/// A session summary row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub source: String,
    pub model: Option<String>,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub started_at: f64,
    pub ended_at: Option<f64>,
    pub message_count: i64,
    pub tool_call_count: i64,
}

/// One full-text search result.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub session_id: String,
    pub message_id: i64,
    pub role: Role,
    pub snippet: String,
}

/// An active compression-failure cooldown row (hermes_state.py
/// `get_compression_failure_cooldown` return shape).
#[derive(Debug, Clone)]
pub struct CompressionCooldown {
    pub cooldown_until: f64,
    pub remaining_seconds: f64,
    pub error: Option<String>,
}

/// The session database handle.
pub struct SessionDb {
    conn: Connection,
    write_count: Cell<u64>,
    fts_enabled: bool,
    trigram_available: bool,
    /// The on-disk path (None for in-memory DBs). Lets the compression lock
    /// lease refresher open its own connection to the same file.
    path: Option<PathBuf>,
}

impl SessionDb {
    /// Open (creating if needed) the default state DB under `~/.joey/state.db`.
    pub fn open_default() -> Result<Self> {
        let path = constants::joey_home().join("state.db");
        Self::open(path)
    }

    pub fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating state dir {}", parent.display()))?;
        }
        let conn = Connection::open(&path)
            .with_context(|| format!("opening state db {}", path.display()))?;
        Self::from_connection(conn, Some(path))
    }

    /// Open an in-memory DB (tests / ephemeral runs).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::from_connection(conn, None)
    }

    fn from_connection(conn: Connection, path: Option<PathBuf>) -> Result<Self> {
        // Short SQLite timeout (1.0s) — application-level jittered retries
        // handle contention instead of SQLite's convoy-prone busy handler.
        conn.busy_timeout(std::time::Duration::from_secs(1))?;
        apply_wal_with_fallback(&conn, path.as_deref().map(|p| p.display().to_string()));
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut db = Self {
            conn,
            write_count: Cell::new(0),
            fts_enabled: false,
            trigram_available: false,
            path,
        };
        db.init_schema()?;
        Ok(db)
    }

    /// The backing file path (None for in-memory DBs).
    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    /// Whether full-text search is available.
    pub fn fts_enabled(&self) -> bool {
        self.fts_enabled
    }

    /// Whether the trigram (CJK substring) index is available.
    pub fn trigram_available(&self) -> bool {
        self.trigram_available
    }

    // ── Schema management ───────────────────────────────────────────────────

    fn init_schema(&mut self) -> Result<()> {
        self.conn.execute_batch(SCHEMA_SQL)?;

        // Declarative column reconciliation: diff live tables against
        // SCHEMA_SQL and ADD any missing columns, so an old joey DB (or a
        // hermes DB from an earlier schema) upgrades on open.
        self.reconcile_columns()?;

        // Index referencing a reconciler-added column.
        let _ = self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_platform_msg_id \
             ON messages(session_id, platform_message_id) \
             WHERE platform_message_id IS NOT NULL",
            [],
        );
        self.conn.execute_batch(DEFERRED_INDEX_SQL)?;

        // Heal NULL `active` rows unconditionally on every startup.
        let _ = self
            .conn
            .execute("UPDATE messages SET active = 1 WHERE active IS NULL", []);

        let fts5_available = self.sqlite_supports_fts5();
        if !fts5_available {
            self.drop_fts_triggers();
        }

        // Legacy FTS shape (external-content `content='messages'` table from
        // the pre-port joey schema, or a pre-v11 hermes DB): drop and rebuild
        // as the inline-mode table, then backfill (mirror of the v11 step).
        let mut needs_backfill = false;
        if fts5_available && self.fts_table_is_legacy("messages_fts") {
            self.drop_fts_triggers();
            let _ = self.conn.execute_batch(
                "DROP TABLE IF EXISTS messages_fts; DROP TABLE IF EXISTS messages_fts_trigram;",
            );
            needs_backfill = true;
        }

        // Schema version bookkeeping.
        let current: Option<i64> = self
            .conn
            .query_row("SELECT version FROM schema_version LIMIT 1", [], |r| r.get(0))
            .optional()?;
        match current {
            None => {
                self.conn.execute(
                    "INSERT INTO schema_version (version) VALUES (?1)",
                    params![SCHEMA_VERSION],
                )?;
            }
            Some(v) if v < SCHEMA_VERSION => {
                self.conn.execute(
                    "UPDATE schema_version SET version = ?1",
                    params![SCHEMA_VERSION],
                )?;
            }
            _ => {}
        }

        // Unique title index — always ensure it exists (dupes cleared first).
        let title_index_sql = "CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_title_unique \
             ON sessions(title) WHERE title IS NOT NULL";
        if self.conn.execute(title_index_sql, []).is_err() {
            let _ = self.conn.execute(
                "UPDATE sessions AS older SET title = NULL \
                 WHERE title IS NOT NULL AND EXISTS ( \
                     SELECT 1 FROM sessions AS newer \
                     WHERE newer.title = older.title AND newer.rowid > older.rowid)",
                [],
            );
            let _ = self.conn.execute(title_index_sql, []);
        }

        if fts5_available {
            self.fts_enabled = self.conn.execute_batch(FTS_SQL).is_ok();
            if self.fts_enabled {
                // Trigram twin is optional — SQLite builds lacking the
                // trigram tokenizer fall back to base FTS only.
                self.trigram_available = self.conn.execute_batch(FTS_TRIGRAM_SQL).is_ok();
                if !self.trigram_available {
                    tracing::info!(
                        "SQLite trigram tokenizer unavailable; CJK/substring search will fall back"
                    );
                }
                if needs_backfill {
                    let _ = self.conn.execute(
                        "INSERT INTO messages_fts(rowid, content) \
                         SELECT id, COALESCE(content, '') || ' ' || COALESCE(tool_name, '') \
                                || ' ' || COALESCE(tool_calls, '') FROM messages",
                        [],
                    );
                    if self.trigram_available {
                        let _ = self.conn.execute(
                            "INSERT INTO messages_fts_trigram(rowid, content) \
                             SELECT id, COALESCE(content, '') || ' ' || COALESCE(tool_name, '') \
                                    || ' ' || COALESCE(tool_calls, '') FROM messages",
                            [],
                        );
                    }
                }
            }
        }

        Ok(())
    }

    fn sqlite_supports_fts5(&self) -> bool {
        let probe = self
            .conn
            .execute_batch("CREATE VIRTUAL TABLE temp._joey_fts5_probe USING fts5(x); DROP TABLE temp._joey_fts5_probe;");
        probe.is_ok()
    }

    fn drop_fts_triggers(&self) {
        for trigger in FTS_TRIGGERS {
            let _ = self
                .conn
                .execute(&format!("DROP TRIGGER IF EXISTS {}", trigger), []);
        }
    }

    /// True when the named FTS table exists with the legacy external-content
    /// shape (`content=` option in its declaration).
    fn fts_table_is_legacy(&self, table: &str) -> bool {
        let sql: Option<String> = self
            .conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
                params![table],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        match sql {
            Some(s) => s.contains("content="),
            None => false,
        }
    }

    /// Extract expected columns per table from SCHEMA_SQL using an in-memory
    /// reference database (port of `_parse_schema_columns` — SQLite itself
    /// parses the DDL, zero regex edge cases).
    #[allow(clippy::type_complexity)]
    fn parse_schema_columns() -> Result<Vec<(String, Vec<(String, String)>)>> {
        let reference = Connection::open_in_memory()?;
        reference.execute_batch(SCHEMA_SQL)?;
        let mut tables: Vec<(String, Vec<(String, String)>)> = Vec::new();
        let names: Vec<String> = {
            let mut stmt = reference.prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for tbl in names {
            let mut cols: Vec<(String, String)> = Vec::new();
            let mut stmt =
                reference.prepare(&format!("PRAGMA table_info(\"{}\")", tbl.replace('"', "\"\"")))?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(1)?,           // name
                    r.get::<_, Option<String>>(2)?,   // type
                    r.get::<_, i64>(3)?,              // notnull
                    r.get::<_, Option<String>>(4)?,   // dflt_value
                    r.get::<_, i64>(5)?,              // pk
                ))
            })?;
            for row in rows {
                let (name, col_type, notnull, default, pk) = row?;
                let mut parts: Vec<String> = Vec::new();
                if let Some(t) = col_type.filter(|t| !t.is_empty()) {
                    parts.push(t);
                }
                if notnull != 0 && pk == 0 {
                    parts.push("NOT NULL".to_string());
                }
                if let Some(d) = default {
                    parts.push(format!("DEFAULT {}", d));
                }
                cols.push((name, parts.join(" ")));
            }
            tables.push((tbl, cols));
        }
        Ok(tables)
    }

    /// Ensure live tables have every column declared in SCHEMA_SQL
    /// (port of `_reconcile_columns`).
    fn reconcile_columns(&self) -> Result<()> {
        for (table, declared) in Self::parse_schema_columns()? {
            let live: Vec<String> = {
                let mut stmt = self
                    .conn
                    .prepare(&format!("PRAGMA table_info(\"{}\")", table.replace('"', "\"\"")))?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(1));
                match rows {
                    Ok(it) => it.collect::<rusqlite::Result<Vec<_>>>()?,
                    Err(_) => continue,
                }
            };
            let live_set: std::collections::HashSet<&str> =
                live.iter().map(|s| s.as_str()).collect();
            for (col, col_type) in &declared {
                if !live_set.contains(col.as_str()) {
                    let safe = col.replace('"', "\"\"");
                    if let Err(e) = self.conn.execute(
                        &format!("ALTER TABLE \"{}\" ADD COLUMN \"{}\" {}", table, safe, col_type),
                        [],
                    ) {
                        tracing::debug!("reconcile {}.{}: {}", table, col, e);
                    }
                }
            }
        }
        Ok(())
    }

    // ── Core write helper ───────────────────────────────────────────────────

    /// Execute a write transaction with BEGIN IMMEDIATE + jittered retry
    /// (port of `_execute_write`): on lock contention, sleep a random
    /// 20-150ms and retry up to 15 times; every 50th successful write runs
    /// a passive WAL checkpoint, every 1000th an FTS merge.
    fn execute_write<T>(&self, f: impl Fn(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let mut last_err: Option<rusqlite::Error> = None;
        for attempt in 0..WRITE_MAX_RETRIES {
            let result = (|| -> rusqlite::Result<T> {
                self.conn.execute_batch("BEGIN IMMEDIATE")?;
                match f(&self.conn) {
                    Ok(v) => {
                        self.conn.execute_batch("COMMIT")?;
                        Ok(v)
                    }
                    Err(e) => {
                        let _ = self.conn.execute_batch("ROLLBACK");
                        Err(e)
                    }
                }
            })();
            match result {
                Ok(v) => {
                    let n = self.write_count.get() + 1;
                    self.write_count.set(n);
                    if n.is_multiple_of(CHECKPOINT_EVERY_N_WRITES) {
                        let _ = self
                            .conn
                            .execute_batch("PRAGMA wal_checkpoint(PASSIVE)");
                    }
                    if n.is_multiple_of(OPTIMIZE_EVERY_N_WRITES) && self.fts_enabled {
                        let _ = self.conn.execute_batch(
                            "INSERT INTO messages_fts(messages_fts) VALUES('optimize')",
                        );
                        if self.trigram_available {
                            let _ = self.conn.execute_batch(
                                "INSERT INTO messages_fts_trigram(messages_fts_trigram) VALUES('optimize')",
                            );
                        }
                    }
                    return Ok(v);
                }
                Err(e) => {
                    let msg = e.to_string().to_lowercase();
                    let locked = msg.contains("locked") || msg.contains("busy");
                    if locked && attempt < WRITE_MAX_RETRIES - 1 {
                        last_err = Some(e);
                        std::thread::sleep(std::time::Duration::from_millis(retry_jitter_ms()));
                        continue;
                    }
                    return Err(e.into());
                }
            }
        }
        Err(last_err
            .map(Into::into)
            .unwrap_or_else(|| anyhow::anyhow!("database is locked after max retries")))
    }

    // ── Session lifecycle ───────────────────────────────────────────────────

    /// Generate a session id of the upstream shape `YYYYMMDD_HHMMSS_<hex6>`.
    /// Timestamps use the naive server-local clock (upstream
    /// `datetime.now()` — agent/agent_init.py:1273-1281), NOT the
    /// configured-timezone clock.
    pub fn new_session_id() -> String {
        let now = chrono::Local::now();
        let stamp = now.format("%Y%m%d_%H%M%S");
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        format!("{}_{}", stamp, &suffix[..6])
    }

    /// Create a session row and return its id.
    pub fn create_session(
        &self,
        source: &str,
        model: Option<&str>,
        cwd: Option<&str>,
    ) -> Result<String> {
        let id = Self::new_session_id();
        self.execute_write(|conn| {
            conn.execute(
                "INSERT INTO sessions (id, source, model, cwd, started_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(id) DO UPDATE SET \
                     model = COALESCE(sessions.model, excluded.model), \
                     cwd = COALESCE(sessions.cwd, excluded.cwd)",
                params![id, source, model, cwd, unix_now()],
            )?;
            Ok(())
        })?;
        Ok(id)
    }

    /// Append a message and bump the session's counters.
    ///
    /// `tool_call_count` increments by the number of entries in the
    /// message's `tool_calls` array (assistant rows carrying calls) — NOT
    /// for tool-role result rows (hermes_state.py:4221-4268).
    pub fn add_message(&self, msg: &StoredMessage) -> Result<i64> {
        let num_tool_calls: i64 = match msg.tool_calls.as_deref() {
            None | Some("") => 0,
            Some(json) => match serde_json::from_str::<serde_json::Value>(json) {
                Ok(serde_json::Value::Array(a)) => a.len() as i64,
                Ok(serde_json::Value::Null) => 0,
                Ok(_) => 1,
                Err(_) => 1,
            },
        };
        self.execute_write(|conn| {
            conn.execute(
                "INSERT INTO messages (session_id, role, content, tool_call_id, tool_calls, \
                                       tool_name, timestamp, token_count, finish_reason, \
                                       reasoning, active) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1)",
                params![
                    msg.session_id,
                    msg.role.as_str(),
                    msg.content,
                    msg.tool_call_id,
                    msg.tool_calls,
                    msg.tool_name,
                    msg.timestamp,
                    msg.token_count,
                    msg.finish_reason,
                    msg.reasoning,
                ],
            )?;
            let row_id = conn.last_insert_rowid();
            if num_tool_calls > 0 {
                conn.execute(
                    "UPDATE sessions SET message_count = message_count + 1, \
                            tool_call_count = tool_call_count + ?2 WHERE id = ?1",
                    params![msg.session_id, num_tool_calls],
                )?;
            } else {
                conn.execute(
                    "UPDATE sessions SET message_count = message_count + 1 WHERE id = ?1",
                    params![msg.session_id],
                )?;
            }
            Ok(row_id)
        })
    }

    /// Fetch all active messages for a session in chronological order.
    pub fn messages(&self, session_id: &str) -> Result<Vec<StoredMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, role, content, tool_calls, tool_call_id, tool_name, \
                    timestamp, token_count, finish_reason, reasoning \
             FROM messages WHERE session_id = ?1 AND active = 1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |r| {
            Ok(StoredMessage {
                id: Some(r.get(0)?),
                session_id: r.get(1)?,
                role: Role::from_label(&r.get::<_, String>(2)?),
                content: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                tool_calls: r.get(4)?,
                tool_call_id: r.get(5)?,
                tool_name: r.get(6)?,
                timestamp: r.get(7)?,
                token_count: r.get(8)?,
                finish_reason: r.get(9)?,
                reasoning: r.get(10)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// End a session (set ended_at + reason).
    pub fn end_session(&self, session_id: &str, reason: &str) -> Result<()> {
        self.execute_write(|conn| {
            conn.execute(
                "UPDATE sessions SET ended_at = ?2, end_reason = ?3 WHERE id = ?1",
                params![session_id, unix_now(), reason],
            )?;
            Ok(())
        })
    }

    /// Set a session title.
    pub fn set_title(&self, session_id: &str, title: &str) -> Result<()> {
        self.execute_write(|conn| {
            conn.execute(
                "UPDATE sessions SET title = ?2 WHERE id = ?1",
                params![session_id, title],
            )?;
            Ok(())
        })
    }

    /// Fetch a session summary.
    pub fn get_session(&self, session_id: &str) -> Result<Option<Session>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, source, model, title, cwd, started_at, ended_at, \
                        message_count, tool_call_count \
                 FROM sessions WHERE id = ?1",
                params![session_id],
                |r| {
                    Ok(Session {
                        id: r.get(0)?,
                        source: r.get(1)?,
                        model: r.get(2)?,
                        title: r.get(3)?,
                        cwd: r.get(4)?,
                        started_at: r.get(5)?,
                        ended_at: r.get(6)?,
                        message_count: r.get::<_, Option<i64>>(7)?.unwrap_or(0),
                        tool_call_count: r.get::<_, Option<i64>>(8)?.unwrap_or(0),
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Resolve an exact or uniquely prefixed session ID to the full ID
    /// (port of `resolve_session_id`, hermes_state.py:3260-3283): exact
    /// match first, then a LIKE prefix with `\`, `%`, `_` escaped.
    pub fn resolve_session_id(&self, session_id_or_prefix: &str) -> Result<Option<String>> {
        if let Some(exact) = self.get_session(session_id_or_prefix)? {
            return Ok(Some(exact.id));
        }
        let escaped = session_id_or_prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let mut stmt = self.conn.prepare(
            "SELECT id FROM sessions WHERE id LIKE ?1 ESCAPE '\\' \
             ORDER BY started_at DESC LIMIT 2",
        )?;
        let ids: Vec<String> = stmt
            .query_map(params![format!("{}%", escaped)], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(if ids.len() == 1 { Some(ids[0].clone()) } else { None })
    }

    /// List recent sessions, most recent first.
    pub fn list_sessions(&self, limit: i64) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source, model, title, cwd, started_at, ended_at, \
                    message_count, tool_call_count \
             FROM sessions WHERE archived = 0 ORDER BY started_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok(Session {
                id: r.get(0)?,
                source: r.get(1)?,
                model: r.get(2)?,
                title: r.get(3)?,
                cwd: r.get(4)?,
                started_at: r.get(5)?,
                ended_at: r.get(6)?,
                message_count: r.get::<_, Option<i64>>(7)?.unwrap_or(0),
                tool_call_count: r.get::<_, Option<i64>>(8)?.unwrap_or(0),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The id of the most recent session, if any (for `--continue`).
    pub fn most_recent_session(&self) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM sessions ORDER BY started_at DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Full-text search across message content, tool names, and tool calls.
    /// The query is sanitized for FTS5 and snippets use the upstream
    /// `'>>>' '<<<' '...' 40` parameters (hermes_state.py:5543).
    pub fn search(&self, query: &str, limit: i64) -> Result<Vec<SearchHit>> {
        if !self.fts_enabled {
            return Ok(Vec::new());
        }
        let sanitized = sanitize_fts5_query(query);
        if sanitized.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT m.session_id, m.id, m.role, \
                    snippet(messages_fts, 0, '>>>', '<<<', '...', 40) AS snippet \
             FROM messages_fts \
             JOIN messages m ON m.id = messages_fts.rowid \
             WHERE messages_fts MATCH ?1 AND (m.active = 1 OR m.compacted = 1) \
             ORDER BY rank LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![sanitized, limit], |r| {
            Ok(SearchHit {
                session_id: r.get(0)?,
                message_id: r.get(1)?,
                role: Role::from_label(&r.get::<_, String>(2)?),
                snippet: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ── Compression state (hermes_state.py:2494-2612) ───────────────────────

    /// Persist the active compression-failure cooldown for a session
    /// (`record_compression_failure_cooldown`).
    pub fn record_compression_failure_cooldown(
        &self,
        session_id: &str,
        cooldown_until: f64,
        error: Option<&str>,
    ) -> Result<()> {
        if session_id.is_empty() {
            return Ok(());
        }
        self.execute_write(|conn| {
            conn.execute(
                "UPDATE sessions SET compression_failure_cooldown_until = ?1, \
                 compression_failure_error = ?2 WHERE id = ?3",
                params![cooldown_until, error, session_id],
            )?;
            Ok(())
        })
    }

    /// Return the active compression-failure cooldown for `session_id`
    /// (`get_compression_failure_cooldown`): None when no row, no value, or
    /// the stored deadline is already in the past.
    pub fn get_compression_failure_cooldown(
        &self,
        session_id: &str,
    ) -> Result<Option<CompressionCooldown>> {
        if session_id.is_empty() {
            return Ok(None);
        }
        let now = unix_now();
        let row: Option<(Option<f64>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT compression_failure_cooldown_until, compression_failure_error \
                 FROM sessions WHERE id = ?1",
                params![session_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((Some(cooldown_until), error)) = row else { return Ok(None) };
        if cooldown_until <= now {
            return Ok(None);
        }
        Ok(Some(CompressionCooldown {
            cooldown_until,
            remaining_seconds: cooldown_until - now,
            error,
        }))
    }

    /// Clear any persisted compression-failure cooldown for a session.
    pub fn clear_compression_failure_cooldown(&self, session_id: &str) -> Result<()> {
        if session_id.is_empty() {
            return Ok(());
        }
        self.execute_write(|conn| {
            conn.execute(
                "UPDATE sessions SET compression_failure_cooldown_until = NULL, \
                 compression_failure_error = NULL WHERE id = ?1",
                params![session_id],
            )?;
            Ok(())
        })
    }

    /// Return the persisted deterministic-fallback streak (0 on any miss).
    pub fn get_compression_fallback_streak(&self, session_id: &str) -> i64 {
        if session_id.is_empty() {
            return 0;
        }
        let value: Option<Option<i64>> = self
            .conn
            .query_row(
                "SELECT compression_fallback_streak FROM sessions WHERE id = ?1",
                params![session_id],
                |r| r.get(0),
            )
            .optional()
            .unwrap_or(None);
        value.flatten().unwrap_or(0).max(0)
    }

    /// Persist the deterministic-fallback streak for one session.
    pub fn set_compression_fallback_streak(&self, session_id: &str, streak: i64) -> Result<()> {
        if session_id.is_empty() {
            return Ok(());
        }
        let normalized = streak.max(0);
        self.execute_write(|conn| {
            conn.execute(
                "UPDATE sessions SET compression_fallback_streak = ?1 WHERE id = ?2",
                params![normalized, session_id],
            )?;
            Ok(())
        })
    }

    // ── Compression locks (hermes_state.py:2635-2770) ───────────────────────

    /// Extend the compression lock lease iff `holder` still owns it
    /// (`refresh_compression_lock`). Returns false on lost ownership or error.
    pub fn refresh_compression_lock(
        &self,
        session_id: &str,
        holder: &str,
        ttl_seconds: f64,
    ) -> bool {
        if session_id.is_empty() || holder.is_empty() {
            return false;
        }
        let now = unix_now();
        let expires_at = now + ttl_seconds;
        self.execute_write(|conn| {
            let n = conn.execute(
                "UPDATE compression_locks SET expires_at = ?1 \
                 WHERE session_id = ?2 AND holder = ?3 AND expires_at >= ?4",
                params![expires_at, session_id, holder, now],
            )?;
            Ok(n > 0)
        })
        .unwrap_or(false)
    }

    /// Try to atomically acquire the compression lock for `session_id`
    /// (`try_acquire_compression_lock`): reclaim expired rows, INSERT OR
    /// IGNORE, then confirm ownership via SELECT. Fails closed on DB errors.
    pub fn try_acquire_compression_lock(
        &self,
        session_id: &str,
        holder: &str,
        ttl_seconds: f64,
    ) -> bool {
        if session_id.is_empty() {
            return false;
        }
        let now = unix_now();
        let expires_at = now + ttl_seconds;
        self.execute_write(|conn| {
            // First: reclaim any expired lock for this session_id.
            conn.execute(
                "DELETE FROM compression_locks WHERE session_id = ?1 AND expires_at < ?2",
                params![session_id, now],
            )?;
            // Then: try to insert. INSERT OR IGNORE returns no rowcount
            // difference — verify ownership via SELECT.
            conn.execute(
                "INSERT OR IGNORE INTO compression_locks \
                 (session_id, holder, acquired_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
                params![session_id, holder, now, expires_at],
            )?;
            let owner: Option<String> = conn
                .query_row(
                    "SELECT holder FROM compression_locks WHERE session_id = ?1",
                    params![session_id],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(owner.as_deref() == Some(holder))
        })
        .unwrap_or(false)
    }

    /// Release the compression lock iff we own it (`release_compression_lock`).
    /// Idempotent; the holder check prevents clobbering a fresh lock.
    pub fn release_compression_lock(&self, session_id: &str, holder: &str) {
        if session_id.is_empty() {
            return;
        }
        let _ = self.execute_write(|conn| {
            conn.execute(
                "DELETE FROM compression_locks WHERE session_id = ?1 AND holder = ?2",
                params![session_id, holder],
            )?;
            Ok(())
        });
    }

    /// The current (non-expired) lock holder, or None (diagnostic helper).
    pub fn get_compression_lock_holder(&self, session_id: &str) -> Option<String> {
        if session_id.is_empty() {
            return None;
        }
        let now = unix_now();
        self.conn
            .query_row(
                "SELECT holder FROM compression_locks WHERE session_id = ?1 AND expires_at >= ?2",
                params![session_id, now],
                |r| r.get(0),
            )
            .optional()
            .unwrap_or(None)
    }

    /// Whether another path already rotated this compression parent
    /// (conversation_compression.py `_session_was_rotated_by_compression`):
    /// ended_at set AND end_reason == "compression".
    pub fn session_was_rotated_by_compression(&self, session_id: &str) -> bool {
        let row: Option<(Option<f64>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT ended_at, end_reason FROM sessions WHERE id = ?1",
                params![session_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .unwrap_or(None);
        matches!(row, Some((Some(_), Some(reason))) if reason == "compression")
    }

    // ── In-place compaction (hermes_state.py `archive_and_compact`) ─────────

    /// Non-destructive in-place compaction for a single durable session id:
    /// soft-archive every currently-active message (`active = 0, compacted =
    /// 1`) and insert `compacted_messages` as fresh active rows — atomically,
    /// in one write transaction. `message_count`/`tool_call_count` are set to
    /// the ACTIVE (compacted) totals. Returns the new active count.
    pub fn archive_and_compact(
        &self,
        session_id: &str,
        compacted_messages: &[StoredMessage],
    ) -> Result<i64> {
        self.execute_write(|conn| {
            conn.execute(
                "UPDATE messages SET active = 0, compacted = 1 \
                 WHERE session_id = ?1 AND active = 1",
                params![session_id],
            )?;
            let mut inserted: i64 = 0;
            let mut tool_calls_total: i64 = 0;
            for msg in compacted_messages {
                conn.execute(
                    "INSERT INTO messages (session_id, role, content, tool_call_id, tool_calls, \
                                           tool_name, timestamp, token_count, finish_reason, \
                                           reasoning, active) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1)",
                    params![
                        session_id,
                        msg.role.as_str(),
                        msg.content,
                        msg.tool_call_id,
                        msg.tool_calls,
                        msg.tool_name,
                        msg.timestamp,
                        msg.token_count,
                        msg.finish_reason,
                        msg.reasoning,
                    ],
                )?;
                inserted += 1;
                if let Some(json) = msg.tool_calls.as_deref() {
                    if let Ok(serde_json::Value::Array(a)) =
                        serde_json::from_str::<serde_json::Value>(json)
                    {
                        tool_calls_total += a.len() as i64;
                    }
                }
            }
            conn.execute(
                "UPDATE sessions SET message_count = ?1, tool_call_count = ?2 WHERE id = ?3",
                params![inserted, tool_calls_total, session_id],
            )?;
            Ok(inserted)
        })
    }

    /// Store the full assembled system prompt snapshot
    /// (`update_system_prompt`).
    pub fn update_system_prompt(&self, session_id: &str, system_prompt: &str) -> Result<()> {
        self.execute_write(|conn| {
            conn.execute(
                "UPDATE sessions SET system_prompt = ?1 WHERE id = ?2",
                params![system_prompt, session_id],
            )?;
            Ok(())
        })
    }
}

/// Set `journal_mode=WAL`, falling back to DELETE when the filesystem
/// cannot support WAL (port of `apply_wal_with_fallback`; the on-disk
/// header probe and macOS checkpoint-fullfsync barrier are not ported).
fn apply_wal_with_fallback(conn: &Connection, db_label: Option<String>) {
    let current: Option<String> = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .optional()
        .ok()
        .flatten();
    if current.as_deref().map(|m| m.eq_ignore_ascii_case("wal")).unwrap_or(false) {
        return;
    }
    if conn.pragma_update(None, "journal_mode", "WAL").is_err() {
        tracing::warn!(
            "{}: WAL journal_mode unsupported on this filesystem — falling back to journal_mode=DELETE",
            db_label.unwrap_or_else(|| "state.db".to_string()),
        );
        let _ = conn.pragma_update(None, "journal_mode", "DELETE");
    }
}

fn retry_jitter_ms() -> u64 {
    // Pseudo-random jitter from the clock's sub-ms noise (no rand dep):
    // uniform-ish in [WRITE_RETRY_MIN_MS, WRITE_RETRY_MAX_MS].
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    WRITE_RETRY_MIN_MS + nanos % (WRITE_RETRY_MAX_MS - WRITE_RETRY_MIN_MS + 1)
}

/// Sanitize user input for safe use in FTS5 MATCH queries (port of
/// `_sanitize_fts5_query`, hermes_state.py:5335):
/// preserve balanced quoted phrases, strip unmatched special characters,
/// collapse `*` runs, drop dangling boolean operators, and quote
/// dotted/hyphenated terms so FTS5 treats them as phrases.
pub fn sanitize_fts5_query(query: &str) -> String {
    use once_cell::sync::Lazy;
    static SPECIALS: Lazy<regex::Regex> =
        Lazy::new(|| regex::Regex::new(r#"[+{}():"^]"#).unwrap());
    static STARS: Lazy<regex::Regex> = Lazy::new(|| regex::Regex::new(r"\*+").unwrap());
    static LEAD_STAR: Lazy<regex::Regex> = Lazy::new(|| regex::Regex::new(r"(^|\s)\*").unwrap());
    static LEAD_BOOL: Lazy<regex::Regex> =
        Lazy::new(|| regex::Regex::new(r"(?i)^(AND|OR|NOT)\b\s*").unwrap());
    static TRAIL_BOOL: Lazy<regex::Regex> =
        Lazy::new(|| regex::Regex::new(r"(?i)\s+(AND|OR|NOT)\s*$").unwrap());
    static DOTTED_TERM: Lazy<regex::Regex> =
        Lazy::new(|| regex::Regex::new(r"\b(\w+(?:[._-]\w+)+)\b").unwrap());

    // Cap user-controlled input before any regex processing.
    let query: String = query.chars().take(MAX_FTS5_QUERY_CHARS).collect();

    // Step 1: extract balanced double-quoted phrases via a linear scan and
    // protect them with numbered placeholders.
    let mut quoted_parts: Vec<String> = Vec::new();
    let mut pieces = String::with_capacity(query.len());
    let chars: Vec<char> = query.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch != '"' {
            pieces.push(ch);
            i += 1;
            continue;
        }
        match chars[i + 1..].iter().position(|&c| c == '"') {
            None => {
                // Unmatched quote → whitespace.
                pieces.push(' ');
                i += 1;
            }
            Some(rel) => {
                let end = i + 1 + rel;
                let phrase: String = chars[i..=end].iter().collect();
                quoted_parts.push(phrase);
                pieces.push_str(&format!("\u{0}Q{}\u{0}", quoted_parts.len() - 1));
                i = end + 1;
            }
        }
    }

    // Step 2: strip remaining FTS5-special characters.
    let mut sanitized = SPECIALS.replace_all(&pieces, " ").into_owned();

    // Step 3: collapse `*` runs; remove leading `*`.
    sanitized = STARS.replace_all(&sanitized, "*").into_owned();
    sanitized = LEAD_STAR.replace_all(&sanitized, "${1}").into_owned();

    // Step 4: remove dangling boolean operators at start/end.
    sanitized = LEAD_BOOL.replace(sanitized.trim(), "").into_owned();
    sanitized = TRAIL_BOOL.replace(sanitized.trim(), "").into_owned();

    // Step 5: wrap unquoted dotted/hyphenated terms in double quotes.
    sanitized = DOTTED_TERM.replace_all(&sanitized, "\"${1}\"").into_owned();

    // Step 6: restore preserved quoted phrases.
    for (i, quoted) in quoted_parts.iter().enumerate() {
        sanitized = sanitized.replace(&format!("\u{0}Q{}\u{0}", i), quoted);
    }

    sanitized.trim().to_string()
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_lifecycle_and_search() {
        let db = SessionDb::open_in_memory().unwrap();
        let sid = db.create_session("cli", Some("test-model"), Some("/tmp")).unwrap();

        db.add_message(&StoredMessage::new(&sid, Role::User, "how do I reverse a linked list"))
            .unwrap();
        db.add_message(&StoredMessage::new(&sid, Role::Assistant, "walk the list swapping next pointers"))
            .unwrap();

        let msgs = db.messages(&sid).unwrap();
        assert_eq!(msgs.len(), 2);

        let sess = db.get_session(&sid).unwrap().unwrap();
        assert_eq!(sess.message_count, 2);
        assert_eq!(sess.tool_call_count, 0);

        let hits = db.search("linked", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, sid);
        assert!(hits[0].snippet.contains(">>>linked<<<"), "snippet: {}", hits[0].snippet);

        let prefix = &sid[..8];
        assert_eq!(db.resolve_session_id(prefix).unwrap(), Some(sid.clone()));
        assert_eq!(db.resolve_session_id(&sid).unwrap(), Some(sid.clone()));

        db.end_session(&sid, "done").unwrap();
    }

    #[test]
    fn tool_call_count_semantics() {
        let db = SessionDb::open_in_memory().unwrap();
        let sid = db.create_session("cli", None, None).unwrap();

        // Assistant message with TWO tool calls → +2.
        let mut m = StoredMessage::new(&sid, Role::Assistant, "");
        m.tool_calls = Some(r#"[{"id":"a","function":{"name":"x"}},{"id":"b","function":{"name":"y"}}]"#.into());
        db.add_message(&m).unwrap();

        // Tool-role RESULT row → +0.
        let mut t = StoredMessage::new(&sid, Role::Tool, "result");
        t.tool_call_id = Some("a".into());
        t.tool_name = Some("x".into());
        db.add_message(&t).unwrap();

        let sess = db.get_session(&sid).unwrap().unwrap();
        assert_eq!(sess.message_count, 2);
        assert_eq!(sess.tool_call_count, 2, "len(tool_calls), not tool-role rows");
    }

    #[test]
    fn schema_matches_upstream_shape() {
        let db = SessionDb::open_in_memory().unwrap();
        // schema_version table with version 22.
        let v: i64 = db
            .conn
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);

        // All upstream tables exist.
        for table in [
            "sessions",
            "messages",
            "session_model_usage",
            "state_meta",
            "gateway_routing",
            "compression_locks",
            "async_delegations",
            "messages_fts",
        ] {
            let n: i64 = db
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                    params![table],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(n >= 1, "missing table {}", table);
        }

        // sessions has all 46 upstream columns; messages all 21.
        let count = |t: &str| -> i64 {
            db.conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM pragma_table_info('{}')", t),
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(count("sessions"), 46);
        assert_eq!(count("messages"), 21);
        assert_eq!(count("session_model_usage"), 18);
    }

    /// A database created with the upstream (hermes) schema SQL — including
    /// data — must open and work unchanged.
    #[test]
    fn opens_hermes_shaped_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_SQL).unwrap();
            conn.execute_batch(FTS_SQL).unwrap();
            conn.execute_batch(FTS_TRIGRAM_SQL).unwrap();
            conn.execute(
                "INSERT INTO schema_version (version) VALUES (?1)",
                params![SCHEMA_VERSION],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions (id, source, model, started_at, message_count, tool_call_count) \
                 VALUES ('20260101_000000_abc123', 'cli', 'hermes-model', 1735689600.0, 1, 0)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO messages (session_id, role, content, timestamp, active) \
                 VALUES ('20260101_000000_abc123', 'user', 'hello from hermes', 1735689601.0, 1)",
                [],
            )
            .unwrap();
        }

        let db = SessionDb::open(path).unwrap();
        let sess = db.get_session("20260101_000000_abc123").unwrap().unwrap();
        assert_eq!(sess.model.as_deref(), Some("hermes-model"));
        let msgs = db.messages("20260101_000000_abc123").unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello from hermes");
        // FTS rows written by the hermes triggers are searchable.
        let hits = db.search("hermes", 5).unwrap();
        assert_eq!(hits.len(), 1);
        // Appending through the port works on the hermes DB.
        db.add_message(&StoredMessage::new("20260101_000000_abc123", Role::Assistant, "hi"))
            .unwrap();
        assert_eq!(db.messages("20260101_000000_abc123").unwrap().len(), 2);
    }

    /// An old-joey-shaped DB (pre-audit schema: fewer columns, external
    /// content FTS) upgrades in place via the column reconciler.
    #[test]
    fn reconciles_old_joey_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        {
            // The genuine pre-rewrite joey schema (schema_version 1 in
            // state_meta, external-content FTS, DELETE-command triggers).
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                r#"
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    source TEXT DEFAULT 'cli',
    user_id TEXT,
    session_key TEXT,
    model TEXT,
    system_prompt TEXT,
    parent_session_id TEXT,
    started_at REAL,
    ended_at REAL,
    end_reason TEXT,
    message_count INTEGER DEFAULT 0,
    tool_call_count INTEGER DEFAULT 0,
    input_tokens INTEGER DEFAULT 0,
    output_tokens INTEGER DEFAULT 0,
    cwd TEXT,
    git_branch TEXT,
    title TEXT,
    profile_name TEXT,
    archived INTEGER DEFAULT 0
);
CREATE TABLE messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    role TEXT NOT NULL,
    content TEXT,
    tool_call_id TEXT,
    tool_calls TEXT,
    tool_name TEXT,
    timestamp REAL,
    token_count INTEGER,
    finish_reason TEXT,
    reasoning TEXT,
    active INTEGER DEFAULT 1,
    FOREIGN KEY(session_id) REFERENCES sessions(id)
);
CREATE TABLE state_meta (key TEXT PRIMARY KEY, value TEXT);
INSERT INTO state_meta(key, value) VALUES('schema_version', '1');
CREATE INDEX idx_messages_session ON messages(session_id, timestamp);
CREATE INDEX idx_sessions_started ON sessions(started_at DESC);
CREATE VIRTUAL TABLE messages_fts USING fts5(
    content,
    content='messages',
    content_rowid='id'
);
CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
END;
CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.id, old.content);
END;
CREATE TRIGGER messages_fts_update AFTER UPDATE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.id, old.content);
    INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
END;
INSERT INTO sessions (id, source, model, started_at) VALUES ('old_joey_session_1', 'cli', 'm', 1.0);
INSERT INTO messages (session_id, role, content, timestamp) VALUES ('old_joey_session_1', 'user', 'legacy joey message', 2.0);
"#,
            )
            .unwrap();
        }

        let db = SessionDb::open(path).unwrap();
        // Reconciler added the new columns.
        let cols: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('sessions')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cols, 46);
        // Old rows still readable; legacy FTS replaced and backfilled.
        let msgs = db.messages("old_joey_session_1").unwrap();
        assert_eq!(msgs.len(), 1);
        let hits = db.search("legacy", 5).unwrap();
        assert_eq!(hits.len(), 1);
        // New writes work.
        db.add_message(&StoredMessage::new("old_joey_session_1", Role::Assistant, "ok"))
            .unwrap();
    }

    #[test]
    fn fts_sanitization_table() {
        // Balanced quoted phrases preserved.
        assert_eq!(sanitize_fts5_query(r#""exact phrase""#), r#""exact phrase""#);
        // Specials stripped (replaced by spaces — whitespace is NOT collapsed,
        // matching upstream).
        assert_eq!(sanitize_fts5_query("TODO: fix"), "TODO  fix");
        assert_eq!(sanitize_fts5_query("a + b (c)"), "a   b  c");
        // Star collapsing and leading-star removal.
        assert_eq!(sanitize_fts5_query("foo***"), "foo*");
        assert_eq!(sanitize_fts5_query("*foo"), "foo");
        // Dangling booleans dropped.
        assert_eq!(sanitize_fts5_query("hello AND"), "hello");
        assert_eq!(sanitize_fts5_query("OR world"), "world");
        // Dotted/hyphenated terms quoted as phrases.
        assert_eq!(sanitize_fts5_query("chat-send"), "\"chat-send\"");
        assert_eq!(sanitize_fts5_query("my-app.config.ts"), "\"my-app.config.ts\"");
        // Unmatched quote replaced with whitespace.
        assert_eq!(sanitize_fts5_query("foo\"bar"), "foo bar");
    }

    #[test]
    fn fts_search_survives_hostile_query() {
        let db = SessionDb::open_in_memory().unwrap();
        let sid = db.create_session("cli", None, None).unwrap();
        db.add_message(&StoredMessage::new(&sid, Role::User, "P2.2 chat-send milestone"))
            .unwrap();
        // Raw FTS5 syntax that would error unsanitized.
        for q in ["chat-send", "P2.2", "TODO: chat-send", "(chat-send", "\"chat-send\"", "*"] {
            let _ = db.search(q, 5).unwrap(); // must not error
        }
        assert_eq!(db.search("chat-send", 5).unwrap().len(), 1);
    }

    #[test]
    fn prefix_resume_escapes_like_wildcards() {
        let db = SessionDb::open_in_memory().unwrap();
        db.execute_write(|conn| {
            conn.execute(
                "INSERT INTO sessions (id, source, started_at) VALUES ('abc_def', 'cli', 1.0)",
                [],
            )?;
            conn.execute(
                "INSERT INTO sessions (id, source, started_at) VALUES ('abcxdef', 'cli', 2.0)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        // `_` must be treated literally, not as a LIKE wildcard: the prefix
        // "abc_" matches only 'abc_def' (unescaped it would match both and
        // resolve to None as ambiguous).
        assert_eq!(db.resolve_session_id("abc_").unwrap(), Some("abc_def".to_string()));
        // Exact id wins immediately.
        assert_eq!(db.resolve_session_id("abcxdef").unwrap(), Some("abcxdef".to_string()));
    }

    #[test]
    fn compression_lock_protocol() {
        let db = SessionDb::open_in_memory().unwrap();
        let sid = db.create_session("cli", None, None).unwrap();

        // Acquire → contend → holder visible.
        assert!(db.try_acquire_compression_lock(&sid, "holder-a", 300.0));
        assert!(!db.try_acquire_compression_lock(&sid, "holder-b", 300.0));
        assert_eq!(db.get_compression_lock_holder(&sid).as_deref(), Some("holder-a"));
        // Re-acquire by the same holder is idempotent ownership confirmation.
        assert!(db.try_acquire_compression_lock(&sid, "holder-a", 300.0));

        // Refresh extends only for the owner.
        assert!(db.refresh_compression_lock(&sid, "holder-a", 300.0));
        assert!(!db.refresh_compression_lock(&sid, "holder-b", 300.0));

        // Release is holder-qualified: a stranger's release is a no-op.
        db.release_compression_lock(&sid, "holder-b");
        assert_eq!(db.get_compression_lock_holder(&sid).as_deref(), Some("holder-a"));
        db.release_compression_lock(&sid, "holder-a");
        assert!(db.get_compression_lock_holder(&sid).is_none());
        assert!(db.try_acquire_compression_lock(&sid, "holder-b", 300.0));

        // Expired locks are reclaimed transparently (crashed holder).
        db.release_compression_lock(&sid, "holder-b");
        db.execute_write(|conn| {
            conn.execute(
                "INSERT INTO compression_locks (session_id, holder, acquired_at, expires_at) \
                 VALUES (?1, 'crashed', 1.0, 2.0)",
                params![sid],
            )?;
            Ok(())
        })
        .unwrap();
        assert!(db.get_compression_lock_holder(&sid).is_none(), "expired lock is not live");
        assert!(db.try_acquire_compression_lock(&sid, "holder-c", 300.0), "expired row reclaimed");
        // A refresh from the crashed holder can't resurrect it.
        assert!(!db.refresh_compression_lock(&sid, "crashed", 300.0));
    }

    #[test]
    fn compression_cooldown_and_streak_roundtrip() {
        let db = SessionDb::open_in_memory().unwrap();
        let sid = db.create_session("cli", None, None).unwrap();

        assert!(db.get_compression_failure_cooldown(&sid).unwrap().is_none());
        let until = unix_now() + 600.0;
        db.record_compression_failure_cooldown(&sid, until, Some("summary LLM 429")).unwrap();
        let state = db.get_compression_failure_cooldown(&sid).unwrap().unwrap();
        assert!((state.cooldown_until - until).abs() < 0.001);
        assert!(state.remaining_seconds > 599.0 && state.remaining_seconds <= 600.0);
        assert_eq!(state.error.as_deref(), Some("summary LLM 429"));

        // An expired deadline reads as no cooldown.
        db.record_compression_failure_cooldown(&sid, unix_now() - 1.0, Some("old")).unwrap();
        assert!(db.get_compression_failure_cooldown(&sid).unwrap().is_none());

        db.record_compression_failure_cooldown(&sid, unix_now() + 60.0, None).unwrap();
        db.clear_compression_failure_cooldown(&sid).unwrap();
        assert!(db.get_compression_failure_cooldown(&sid).unwrap().is_none());

        // Fallback streak round-trip (missing session/row → 0; negatives clamp).
        assert_eq!(db.get_compression_fallback_streak(&sid), 0);
        db.set_compression_fallback_streak(&sid, 2).unwrap();
        assert_eq!(db.get_compression_fallback_streak(&sid), 2);
        db.set_compression_fallback_streak(&sid, -5).unwrap();
        assert_eq!(db.get_compression_fallback_streak(&sid), 0);
        assert_eq!(db.get_compression_fallback_streak("nope"), 0);
    }

    #[test]
    fn archive_and_compact_soft_archives() {
        let db = SessionDb::open_in_memory().unwrap();
        let sid = db.create_session("cli", None, None).unwrap();
        for i in 0..6 {
            db.add_message(&StoredMessage::new(&sid, Role::User, format!("original {}", i)))
                .unwrap();
        }
        let compacted = vec![
            StoredMessage::new(&sid, Role::User, "summary handoff"),
            StoredMessage::new(&sid, Role::Assistant, "tail"),
        ];
        let inserted = db.archive_and_compact(&sid, &compacted).unwrap();
        assert_eq!(inserted, 2);
        // Live load returns ONLY the compacted set.
        let live = db.messages(&sid).unwrap();
        assert_eq!(live.len(), 2);
        assert_eq!(live[0].content, "summary handoff");
        // Counters reflect the ACTIVE set.
        let sess = db.get_session(&sid).unwrap().unwrap();
        assert_eq!(sess.message_count, 2);
        // Old rows stay on disk as active=0/compacted=1 (non-destructive).
        let (archived, compacted_rows): (i64, i64) = db
            .conn
            .query_row(
                "SELECT COUNT(*), SUM(compacted) FROM messages \
                 WHERE session_id = ?1 AND active = 0",
                params![sid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(archived, 6);
        assert_eq!(compacted_rows, 6);

        // Rotation detector: only ended_at + end_reason="compression" counts.
        assert!(!db.session_was_rotated_by_compression(&sid));
        db.end_session(&sid, "compression").unwrap();
        assert!(db.session_was_rotated_by_compression(&sid));
    }

    #[test]
    fn session_id_shape() {
        let id = SessionDb::new_session_id();
        // YYYYMMDD_HHMMSS_hex6 → 8 + 1 + 6 + 1 + 6 = 22 chars
        assert_eq!(id.len(), 22);
        assert_eq!(id.matches('_').count(), 2);
        let re = regex::Regex::new(r"^\d{8}_\d{6}_[0-9a-f]{6}$").unwrap();
        assert!(re.is_match(&id), "bad id shape: {}", id);
    }
}
