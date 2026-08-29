//! Per-project consent records (`consent.json` beside `graph.db`).
//!
//! State machine NeverAcknowledged → Acknowledged → Revoked (re-ack allowed)
//! with audit fields, per `specs/021-please-enhance-neurocode/data-model.md` §8.
//! Absent file ≡ NeverAcknowledged. Gates remote embedding backends only —
//! local/loopback backends are consent-free.
//!
//! Storage (R5): a sibling JSON file at
//! `~/.joey/neurocode/projects/<sha256-of-root>/consent.json` — deliberately
//! NOT in graph.db so consent state decouples from index rebuilds and stays
//! human-inspectable.
//!
//! Legal transitions (data-model.md §8):
//!
//! ```text
//! NeverAcknowledged ──(explicit CLI ack)──▶ Acknowledged
//! Acknowledged      ──(revoke, any time)──▶ Revoked
//! Revoked           ──(re-ack allowed)────▶ Acknowledged
//! ```
//!
//! Invariant (FR-012): only [`ConsentState::Acknowledged`] permits remote
//! embedding calls for this project; `NeverAcknowledged`, `Revoked`, or a
//! missing file forces local/keyword-only operation.

use std::fmt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use joey_neurocode::graph::project_graph_db_path;

/// File name of the consent record inside the per-project directory.
pub const CONSENT_FILE_NAME: &str = "consent.json";

/// Consent state for a project's remote embedding backend (data-model.md §8).
///
/// Serializes snake_case (`"never_acknowledged" | "acknowledged" | "revoked"`)
/// so `consent.json` uses the same vocabulary as the CLI display strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentState {
    /// No `consent.json` (or never acked): remote backends unusable.
    NeverAcknowledged,
    /// Explicit acknowledgement recorded: remote embedding permitted (FR-012).
    Acknowledged,
    /// Consent withdrawn: local/keyword-only operation until re-ack.
    Revoked,
}

impl ConsentState {
    /// Stable snake_case display string (used by the consent CLI, T031, and
    /// FR-013 status reporting: "never acknowledged / acknowledged / revoked").
    pub fn as_str(&self) -> &'static str {
        match self {
            ConsentState::NeverAcknowledged => "never_acknowledged",
            ConsentState::Acknowledged => "acknowledged",
            ConsentState::Revoked => "revoked",
        }
    }

    /// Parse the display string back (tolerates the JSON spelling).
    pub fn from_str_lossy(s: &str) -> Option<ConsentState> {
        match s {
            "never_acknowledged" | "NeverAcknowledged" => Some(ConsentState::NeverAcknowledged),
            "acknowledged" | "Acknowledged" => Some(ConsentState::Acknowledged),
            "revoked" | "Revoked" => Some(ConsentState::Revoked),
            _ => None,
        }
    }
}

impl fmt::Display for ConsentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-project remote-backend consent record (data-model.md §8).
///
/// Field names serialize EXACTLY as the spec table: `project_root`,
/// `remote_backend_url`, `state`, `acknowledged_at`, `revoked_at`,
/// `model_at_ack_time`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentRecord {
    /// Absolute path of the consented project.
    pub project_root: String,
    /// The remote embedding base_url consented to.
    pub remote_backend_url: String,
    /// Current state.
    pub state: ConsentState,
    /// When consent was given/re-given (RFC 3339).
    pub acknowledged_at: Option<String>,
    /// When consent was revoked (RFC 3339).
    pub revoked_at: Option<String>,
    /// embed_model in force at acknowledgement (audit trail).
    pub model_at_ack_time: Option<String>,
}

impl ConsentRecord {
    /// A fresh, never-acknowledged record (what an absent file means).
    pub fn never_acknowledged(project_root: impl Into<String>) -> Self {
        ConsentRecord {
            project_root: project_root.into(),
            remote_backend_url: String::new(),
            state: ConsentState::NeverAcknowledged,
            acknowledged_at: None,
            revoked_at: None,
            model_at_ack_time: None,
        }
    }

    /// FR-012 invariant: only `Acknowledged` permits remote embedding calls.
    pub fn permits_remote(&self) -> bool {
        self.state == ConsentState::Acknowledged
    }

    /// Transition: `NeverAcknowledged` or `Revoked` → `Acknowledged`
    /// (explicit CLI ack; re-ack after revoke is allowed).
    ///
    /// Records the audit fields `acknowledged_at` (given timestamp) and
    /// `model_at_ack_time`, and clears the stale `revoked_at` (the current
    /// record is no longer revoked; a future revoke re-stamps it).
    /// Illegal from `Acknowledged` (already acked) — returns an error, never
    /// panics.
    pub fn acknowledge(
        &mut self,
        remote_backend_url: impl Into<String>,
        model_at_ack_time: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Result<(), ConsentError> {
        match self.state {
            ConsentState::NeverAcknowledged | ConsentState::Revoked => {
                self.remote_backend_url = remote_backend_url.into();
                self.model_at_ack_time = Some(model_at_ack_time.into());
                self.acknowledged_at = Some(now.to_rfc3339());
                self.revoked_at = None;
                self.state = ConsentState::Acknowledged;
                Ok(())
            }
            ConsentState::Acknowledged => Err(ConsentError::IllegalTransition {
                from: self.state,
                action: "acknowledge".into(),
            }),
        }
    }

    /// Convenience [`Self::acknowledge`] stamping `Utc::now`.
    pub fn acknowledge_now(
        &mut self,
        remote_backend_url: impl Into<String>,
        model_at_ack_time: impl Into<String>,
    ) -> Result<(), ConsentError> {
        self.acknowledge(remote_backend_url, model_at_ack_time, Utc::now())
    }

    /// Transition: `Acknowledged` → `Revoked` (revoke, any time).
    ///
    /// Records `revoked_at` (given timestamp). Illegal from
    /// `NeverAcknowledged` (nothing consented to revoke) and from `Revoked`
    /// (already revoked) — returns an error, never panics.
    pub fn revoke(&mut self, now: DateTime<Utc>) -> Result<(), ConsentError> {
        match self.state {
            ConsentState::Acknowledged => {
                self.revoked_at = Some(now.to_rfc3339());
                self.state = ConsentState::Revoked;
                Ok(())
            }
            ConsentState::NeverAcknowledged | ConsentState::Revoked => {
                Err(ConsentError::IllegalTransition {
                    from: self.state,
                    action: "revoke".into(),
                })
            }
        }
    }

    /// Convenience [`Self::revoke`] stamping `Utc::now`.
    pub fn revoke_now(&mut self) -> Result<(), ConsentError> {
        self.revoke(Utc::now())
    }

    /// Load `consent.json` from a per-project directory.
    ///
    /// Absent file ≡ a never-acknowledged record (data-model.md §8). A
    /// present-but-corrupt file (bad JSON, unknown state, malformed audit
    /// timestamps) is a hard error — never silently treated as absence.
    pub fn load(dir: &Path) -> Result<ConsentRecord, ConsentError> {
        let path = dir.join(CONSENT_FILE_NAME);
        if !path.exists() {
            return Ok(ConsentRecord::never_acknowledged(String::new()));
        }
        let raw = std::fs::read_to_string(&path).map_err(|e| ConsentError::Io {
            path: path.clone(),
            source: e,
        })?;
        let record: ConsentRecord =
            serde_json::from_str(&raw).map_err(|e| ConsentError::Corrupt {
                path: path.clone(),
                message: e.to_string(),
            })?;
        record.validate(&path)?;
        Ok(record)
    }

    /// Load the consent record for a project root, resolving the per-project
    /// directory beside `graph.db`. On absent file the returned record keeps
    /// this `project_root` filled in for later acknowledgement.
    pub fn load_for_project(project_root: &Path) -> Result<ConsentRecord, ConsentError> {
        let path = consent_file_path(project_root);
        let dir = path
            .parent()
            .ok_or_else(|| ConsentError::Io {
                path: path.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "consent path has no parent directory",
                ),
            })?;
        let mut record = ConsentRecord::load(dir)?;
        if record.state == ConsentState::NeverAcknowledged && record.project_root.is_empty() {
            record.project_root = project_root.display().to_string();
        }
        Ok(record)
    }

    /// Persist to `<dir>/consent.json` (pretty-printed, human-inspectable).
    pub fn save(&self, dir: &Path) -> Result<(), ConsentError> {
        let path = dir.join(CONSENT_FILE_NAME);
        let json = serde_json::to_string_pretty(self).map_err(|e| ConsentError::Corrupt {
            path: path.clone(),
            message: e.to_string(),
        })?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ConsentError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        std::fs::write(&path, json).map_err(|e| ConsentError::Io {
            path: path.clone(),
            source: e,
        })
    }

    /// Structural validation of a loaded record: known state (serde already
    /// enforces the enum), RFC 3339 audit timestamps where present.
    fn validate(&self, path: &Path) -> Result<(), ConsentError> {
        for (field, raw) in [
            ("acknowledged_at", &self.acknowledged_at),
            ("revoked_at", &self.revoked_at),
        ] {
            if let Some(ts) = raw {
                DateTime::parse_from_rfc3339(ts).map_err(|_| ConsentError::Corrupt {
                    path: path.to_path_buf(),
                    message: format!("field `{field}` is not RFC 3339: {ts:?}"),
                })?;
            }
        }
        Ok(())
    }
}

/// Resolve the consent file path for a project root: sibling of
/// `~/.joey/neurocode/projects/<sha256-of-root>/graph.db` (data-model.md §8).
pub fn consent_file_path(project_root: &Path) -> PathBuf {
    project_graph_db_path(project_root)
        .parent()
        .map(|dir| dir.join(CONSENT_FILE_NAME))
        .unwrap_or_else(|| PathBuf::from(CONSENT_FILE_NAME))
}

/// Consent-state errors — illegal transitions are reported, never panicked.
#[derive(Debug)]
pub enum ConsentError {
    /// The requested transition is not legal from the current state.
    IllegalTransition {
        /// Current state.
        from: ConsentState,
        /// What was attempted ("acknowledge" / "revoke").
        action: String,
    },
    /// Filesystem failure reading/writing the record.
    Io { path: PathBuf, source: std::io::Error },
    /// Present but unparseable/invalid `consent.json`.
    Corrupt {
        path: PathBuf,
        message: String,
    },
}

impl fmt::Display for ConsentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConsentError::IllegalTransition { from, action } => write!(
                f,
                "illegal consent transition: cannot {action} from state `{from}`"
            ),
            ConsentError::Io { path, source } => {
                write!(f, "consent I/O error at {}: {source}", path.display())
            }
            ConsentError::Corrupt { path, message } => {
                write!(f, "corrupt consent record at {}: {message}", path.display())
            }
        }
    }
}

impl std::error::Error for ConsentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConsentError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    // ---- absent file ≡ NeverAcknowledged ----

    #[test]
    fn absent_file_is_never_acknowledged() {
        let dir = tmp();
        let rec = ConsentRecord::load(dir.path()).expect("load absent");
        assert_eq!(rec.state, ConsentState::NeverAcknowledged);
        assert_eq!(rec.state.as_str(), "never_acknowledged");
        assert_eq!(rec.state.to_string(), "never_acknowledged");
        assert!(!rec.permits_remote());
        assert_eq!(rec.acknowledged_at, None);
        assert_eq!(rec.revoked_at, None);
        assert_eq!(rec.model_at_ack_time, None);
    }

    // ---- legal transitions ----

    #[test]
    fn ack_from_never_acknowledged_populates_audit_fields() {
        let mut rec = ConsentRecord::never_acknowledged("/repo");
        rec.acknowledge("https://embed.example.com", "nomic-embed-text-v1.5", ts(1000))
            .expect("ack legal");
        assert_eq!(rec.state, ConsentState::Acknowledged);
        assert!(rec.permits_remote());
        assert_eq!(rec.remote_backend_url, "https://embed.example.com");
        assert_eq!(rec.model_at_ack_time.as_deref(), Some("nomic-embed-text-v1.5"));
        let ack = DateTime::parse_from_rfc3339(rec.acknowledged_at.as_deref().unwrap())
            .expect("acknowledged_at is RFC 3339");
        assert_eq!(ack.timestamp(), 1000);
        assert_eq!(rec.revoked_at, None);
    }

    #[test]
    fn revoke_from_acknowledged_stamps_revoked_at() {
        let mut rec = acknowledged("/repo", 1000);
        rec.revoke(ts(2000)).expect("revoke legal");
        assert_eq!(rec.state, ConsentState::Revoked);
        assert!(!rec.permits_remote());
        let rev = DateTime::parse_from_rfc3339(rec.revoked_at.as_deref().unwrap())
            .expect("revoked_at is RFC 3339");
        assert_eq!(rev.timestamp(), 2000);
        // ack audit trail untouched by revoke
        assert!(rec.acknowledged_at.is_some());
    }

    #[test]
    fn re_ack_after_revoke_allowed_with_updated_audit() {
        let mut rec = acknowledged("/repo", 1000);
        rec.revoke(ts(2000)).expect("revoke");
        rec.acknowledge("https://other.example.com", "CodeRankEmbed", ts(3000))
            .expect("re-ack after revoke is legal");
        assert_eq!(rec.state, ConsentState::Acknowledged);
        assert!(rec.permits_remote());
        // revoked_at cleared on re-ack: current record is not revoked
        assert_eq!(rec.revoked_at, None);
        // acknowledged_at re-stamped ("given/re-given"), strictly later
        let ack = DateTime::parse_from_rfc3339(rec.acknowledged_at.as_deref().unwrap())
            .unwrap();
        assert_eq!(ack.timestamp(), 3000);
        assert!(ack.timestamp() > 2000);
        // backend + model updated to the re-ack values
        assert_eq!(rec.remote_backend_url, "https://other.example.com");
        assert_eq!(rec.model_at_ack_time.as_deref(), Some("CodeRankEmbed"));
    }

    #[test]
    fn full_cycle_timestamps_monotonic() {
        let mut rec = ConsentRecord::never_acknowledged("/repo");
        let stamps: Vec<i64> = vec![1000, 2000, 3000, 4000, 5000];
        rec.acknowledge("https://a.example", "m1", ts(stamps[0])).unwrap();
        rec.revoke(ts(stamps[1])).unwrap();
        rec.acknowledge("https://a.example", "m2", ts(stamps[2])).unwrap();
        rec.revoke(ts(stamps[3])).unwrap();
        rec.acknowledge("https://a.example", "m3", ts(stamps[4])).unwrap();
        let ack = DateTime::parse_from_rfc3339(rec.acknowledged_at.as_deref().unwrap())
            .unwrap()
            .timestamp();
        assert_eq!(ack, 5000);
        assert_eq!(rec.state, ConsentState::Acknowledged);
    }

    // ---- illegal transitions: errors, never panics ----

    #[test]
    fn double_ack_is_illegal() {
        let mut rec = acknowledged("/repo", 1000);
        let err = rec
            .acknowledge("https://embed.example.com", "m", ts(2000))
            .unwrap_err();
        assert!(matches!(
            err,
            ConsentError::IllegalTransition { ref from, ref action }
                if *from == ConsentState::Acknowledged && action == "acknowledge"
        ));
        assert_eq!(rec.state, ConsentState::Acknowledged); // unchanged
        assert_eq!(
            DateTime::parse_from_rfc3339(rec.acknowledged_at.as_deref().unwrap())
                .unwrap()
                .timestamp(),
            1000
        ); // audit not clobbered by the rejected attempt
    }

    #[test]
    fn revoke_from_never_acknowledged_is_illegal() {
        let mut rec = ConsentRecord::never_acknowledged("/repo");
        let err = rec.revoke(ts(1000)).unwrap_err();
        assert!(matches!(
            err,
            ConsentError::IllegalTransition { ref from, ref action }
                if *from == ConsentState::NeverAcknowledged && action == "revoke"
        ));
        assert_eq!(rec.state, ConsentState::NeverAcknowledged);
        assert_eq!(rec.revoked_at, None);
    }

    #[test]
    fn revoke_from_revoked_is_illegal() {
        let mut rec = acknowledged("/repo", 1000);
        rec.revoke(ts(2000)).unwrap();
        let err = rec.revoke(ts(3000)).unwrap_err();
        assert!(matches!(
            err,
            ConsentError::IllegalTransition { ref from, ref action }
                if *from == ConsentState::Revoked && action == "revoke"
        ));
        assert_eq!(rec.state, ConsentState::Revoked);
        // revoked_at still the FIRST revoke stamp
        assert_eq!(
            DateTime::parse_from_rfc3339(rec.revoked_at.as_deref().unwrap())
                .unwrap()
                .timestamp(),
            2000
        );
    }

    // ---- persistence: exact spec field names, round-trip ----

    #[test]
    fn save_json_uses_spec_field_names_exactly() {
        let dir = tmp();
        let mut rec = ConsentRecord::never_acknowledged("/repo");
        rec.acknowledge("https://embed.example.com", "nomic-embed-text-v1.5", ts(1000))
            .unwrap();
        rec.save(dir.path()).expect("save");
        let raw = std::fs::read_to_string(dir.path().join(CONSENT_FILE_NAME)).unwrap();
        for key in [
            "\"project_root\"",
            "\"remote_backend_url\"",
            "\"state\"",
            "\"acknowledged_at\"",
            "\"revoked_at\"",
            "\"model_at_ack_time\"",
        ] {
            assert!(raw.contains(key), "missing spec field {key} in: {raw}");
        }
        assert!(raw.contains("\"acknowledged\""), "state string: {raw}");
        assert!(!raw.contains("NeverAcknowledged"), "snake_case spelling: {raw}");
    }

    #[test]
    fn save_load_round_trip_preserves_all_fields() {
        let dir = tmp();
        let mut rec = ConsentRecord::never_acknowledged("/repo");
        rec.acknowledge("https://embed.example.com", "m", ts(1000)).unwrap();
        rec.revoke(ts(2000)).unwrap();
        rec.save(dir.path()).unwrap();
        let loaded = ConsentRecord::load(dir.path()).expect("load");
        assert_eq!(loaded, rec);
        assert_eq!(loaded.state, ConsentState::Revoked);
        assert!(!loaded.permits_remote());
    }

    #[test]
    fn load_for_project_absent_fills_project_root() {
        let dir = tmp();
        // Redirect the joey home so we never touch the real ~/.joey.
        let home = dir.path().join("home");
        std::env::set_var("JOEY_HOME", &home);
        let project = dir.path().join("repo");
        std::fs::create_dir_all(&project).unwrap();
        let rec = ConsentRecord::load_for_project(&project).expect("load_for_project");
        std::env::remove_var("JOEY_HOME");
        assert_eq!(rec.state, ConsentState::NeverAcknowledged);
        assert_eq!(rec.project_root, project.display().to_string());
    }

    // ---- corrupt files are errors, not silent absence ----

    #[test]
    fn corrupt_json_is_an_error() {
        let dir = tmp();
        std::fs::write(dir.path().join(CONSENT_FILE_NAME), "{ not json").unwrap();
        assert!(matches!(
            ConsentRecord::load(dir.path()),
            Err(ConsentError::Corrupt { .. })
        ));
    }

    #[test]
    fn unknown_state_string_is_an_error() {
        let dir = tmp();
        let raw = r#"{
            "project_root": "/repo",
            "remote_backend_url": "",
            "state": "maybe",
            "acknowledged_at": null,
            "revoked_at": null,
            "model_at_ack_time": null
        }"#;
        std::fs::write(dir.path().join(CONSENT_FILE_NAME), raw).unwrap();
        assert!(matches!(
            ConsentRecord::load(dir.path()),
            Err(ConsentError::Corrupt { .. })
        ));
    }

    #[test]
    fn non_rfc3339_audit_timestamp_is_an_error() {
        let dir = tmp();
        let raw = r#"{
            "project_root": "/repo",
            "remote_backend_url": "https://embed.example.com",
            "state": "acknowledged",
            "acknowledged_at": "yesterday-ish",
            "revoked_at": null,
            "model_at_ack_time": "m"
        }"#;
        std::fs::write(dir.path().join(CONSENT_FILE_NAME), raw).unwrap();
        assert!(matches!(
            ConsentRecord::load(dir.path()),
            Err(ConsentError::Corrupt { .. })
        ));
    }

    #[test]
    fn error_display_messages_are_informative() {
        let mut rec = ConsentRecord::never_acknowledged("/repo");
        let err = rec.revoke(ts(1)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("revoke"), "{msg}");
        assert!(msg.contains("never_acknowledged"), "{msg}");
    }

    // ---- helpers ----

    fn acknowledged(root: &str, at: i64) -> ConsentRecord {
        let mut rec = ConsentRecord::never_acknowledged(root);
        rec.acknowledge("https://embed.example.com", "nomic-embed-text-v1.5", ts(at))
            .expect("setup ack");
        rec
    }
}
