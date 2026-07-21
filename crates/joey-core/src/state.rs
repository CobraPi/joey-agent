//! Session persistence (port of the load-bearing core of `hermes_state.py`).
//!
//! A single SQLite file under `~/.joey/state.db` holds sessions + messages
//! with an FTS5 index for `session_search`. This ports the essential tables
//! (sessions, messages, messages_fts, state_meta) and the session/message CRUD
//! + full-text search; the peripheral gateway_routing / async_delegations /
//! compression_locks tables are added by their respective subsystems.

use std::path::PathBuf;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::{constants, time};

/// Schema version — bump when the table shape changes.
pub const SCHEMA_VERSION: i64 = 1;

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

    pub fn from_str(s: &str) -> Role {
        match s {
            "system" => Role::System,
            "assistant" => Role::Assistant,
            "tool" => Role::Tool,
            _ => Role::User,
        }
    }
}

/// A persisted message row.
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

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
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

CREATE TABLE IF NOT EXISTS messages (
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

CREATE TABLE IF NOT EXISTS state_meta (
    key TEXT PRIMARY KEY,
    value TEXT
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, timestamp);
CREATE INDEX IF NOT EXISTS idx_sessions_started ON sessions(started_at DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    content,
    content='messages',
    content_rowid='id'
);

CREATE TRIGGER IF NOT EXISTS messages_fts_insert AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
END;
CREATE TRIGGER IF NOT EXISTS messages_fts_delete AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.id, old.content);
END;
CREATE TRIGGER IF NOT EXISTS messages_fts_update AFTER UPDATE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, content) VALUES('delete', old.id, old.content);
    INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
END;
"#;

/// The session database handle.
pub struct SessionDb {
    conn: Connection,
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
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    /// Open an in-memory DB (tests / ephemeral runs).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(SCHEMA_SQL)?;
        self.conn.execute(
            "INSERT OR IGNORE INTO state_meta(key, value) VALUES('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    /// Generate a session id of the upstream shape `YYYYMMDD_HHMMSS_<hex6>`.
    pub fn new_session_id() -> String {
        let now = time::now();
        let stamp = now.format("%Y%m%d_%H%M%S");
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        format!("{}_{}", stamp, &suffix[..6])
    }

    /// Create a session row and return its id.
    pub fn create_session(&self, source: &str, model: Option<&str>, cwd: Option<&str>) -> Result<String> {
        let id = Self::new_session_id();
        self.conn.execute(
            "INSERT INTO sessions(id, source, model, cwd, started_at, message_count, tool_call_count)
             VALUES(?1, ?2, ?3, ?4, ?5, 0, 0)",
            params![id, source, model, cwd, unix_now()],
        )?;
        Ok(id)
    }

    /// Append a message and bump the session's counters.
    pub fn add_message(&self, msg: &StoredMessage) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO messages(session_id, role, content, tool_call_id, tool_calls,
                                  tool_name, timestamp, token_count, finish_reason, reasoning)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
        let row_id = self.conn.last_insert_rowid();
        let tool_delta = i64::from(msg.role == Role::Tool);
        self.conn.execute(
            "UPDATE sessions SET message_count = message_count + 1,
                    tool_call_count = tool_call_count + ?2
             WHERE id = ?1",
            params![msg.session_id, tool_delta],
        )?;
        Ok(row_id)
    }

    /// Fetch all active messages for a session in chronological order.
    pub fn messages(&self, session_id: &str) -> Result<Vec<StoredMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, role, content, tool_calls, tool_call_id, tool_name,
                    timestamp, token_count, finish_reason, reasoning
             FROM messages WHERE session_id = ?1 AND active = 1 ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |r| {
            Ok(StoredMessage {
                id: Some(r.get(0)?),
                session_id: r.get(1)?,
                role: Role::from_str(&r.get::<_, String>(2)?),
                content: r.get(3)?,
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
        self.conn.execute(
            "UPDATE sessions SET ended_at = ?2, end_reason = ?3 WHERE id = ?1",
            params![session_id, unix_now(), reason],
        )?;
        Ok(())
    }

    /// Set a session title.
    pub fn set_title(&self, session_id: &str, title: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET title = ?2 WHERE id = ?1",
            params![session_id, title],
        )?;
        Ok(())
    }

    /// Fetch a session summary.
    pub fn get_session(&self, session_id: &str) -> Result<Option<Session>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, source, model, title, cwd, started_at, ended_at,
                        message_count, tool_call_count
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
                        message_count: r.get(7)?,
                        tool_call_count: r.get(8)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Resolve a session id from a unique prefix (for `--resume <prefix>`).
    pub fn resolve_session_id(&self, prefix: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM sessions WHERE id LIKE ?1 ORDER BY started_at DESC LIMIT 2")?;
        let ids: Vec<String> = stmt
            .query_map(params![format!("{}%", prefix)], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        // Only resolve when unambiguous.
        Ok(if ids.len() == 1 { Some(ids[0].clone()) } else { None })
    }

    /// List recent sessions, most recent first.
    pub fn list_sessions(&self, limit: i64) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source, model, title, cwd, started_at, ended_at,
                    message_count, tool_call_count
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
                message_count: r.get(7)?,
                tool_call_count: r.get(8)?,
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

    /// Full-text search across message content. Returns (session_id, message_id,
    /// snippet) tuples.
    pub fn search(&self, query: &str, limit: i64) -> Result<Vec<SearchHit>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.session_id, m.id, m.role,
                    snippet(messages_fts, 0, '[', ']', ' … ', 12) AS snip
             FROM messages_fts
             JOIN messages m ON m.id = messages_fts.rowid
             WHERE messages_fts MATCH ?1
             ORDER BY rank LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![query, limit], |r| {
            Ok(SearchHit {
                session_id: r.get(0)?,
                message_id: r.get(1)?,
                role: Role::from_str(&r.get::<_, String>(2)?),
                snippet: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

/// One full-text search result.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub session_id: String,
    pub message_id: i64,
    pub role: Role,
    pub snippet: String,
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

        let hits = db.search("linked", 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, sid);

        // Prefix resolution
        let prefix = &sid[..8];
        assert_eq!(db.resolve_session_id(prefix).unwrap(), Some(sid.clone()));

        db.end_session(&sid, "done").unwrap();
    }

    #[test]
    fn session_id_shape() {
        let id = SessionDb::new_session_id();
        // YYYYMMDD_HHMMSS_hex6 → 8 + 1 + 6 + 1 + 6 = 22 chars
        assert_eq!(id.len(), 22);
        assert_eq!(id.matches('_').count(), 2);
    }
}
