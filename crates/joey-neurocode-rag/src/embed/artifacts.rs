//! Model artifact integrity.
//!
//! SHA-256 verification of `model.onnx` / `tokenizer.json` against the pinned
//! `rag_model_artifacts` row, refusing mismatches (`ModelFilesCorrupt`), plus
//! self-registration: when no row exists for a profile, the first successful
//! load computes and writes the row (manual-placement path). Per
//! contracts/embedding-backend.md § Local Model Artifacts:
//!
//! - Self-registration on manual placement: when no `rag_model_artifacts` row
//!   exists for the profile, the **first successful load computes the SHA-256
//!   of the present files and WRITES the row** (first-load trust, later
//!   immutability).
//! - Every subsequent load verifies against the stored row and **refuses on
//!   mismatch** (`ModelFilesCorrupt`) — a corrupted or tampered artifact set
//!   cannot poison the index.
//!
//! The `ArtifactError` enum below is the artifact-integrity slice of the
//! `EmbedError` taxonomy (contracts/embedding-backend.md § Error taxonomy);
//! T010 formalizes the full enum and is expected to lift/re-export these
//! variants verbatim, which is why they live here importable and standalone.

use std::fs;
use std::io::Read;
use std::path::Path;

use joey_neurocode::graph::GraphStore;
use rusqlite::params;
use sha2::{Digest, Sha256};

/// File names required inside `neurocode.rag.local.model_dir`
/// (contracts/embedding-backend.md § Local Model Artifacts).
pub const MODEL_FILE: &str = "model.onnx";
pub const TOKENIZER_FILE: &str = "tokenizer.json";

/// Artifact-integrity errors — the `ModelFilesMissing` / `ModelFilesCorrupt`
/// members of the `EmbedError` taxonomy (T010), plus the storage/IO failures
/// this module can hit while hashing or reading/writing the pinned row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactError {
    /// `model_dir` absent or `model.onnx` / `tokenizer.json` missing.
    ModelFilesMissing(String),
    /// SHA-256 mismatch against the pinned `rag_model_artifacts` row —
    /// a corrupted or tampered artifact set must not poison the index.
    ModelFilesCorrupt(String),
    /// Filesystem failure while reading an artifact for hashing.
    Io(String),
    /// SQLite failure reading/writing the `rag_model_artifacts` row.
    Db(String),
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArtifactError::ModelFilesMissing(m) => write!(f, "model files missing: {}", m),
            ArtifactError::ModelFilesCorrupt(m) => write!(f, "model files corrupt: {}", m),
            ArtifactError::Io(m) => write!(f, "artifact io error: {}", m),
            ArtifactError::Db(m) => write!(f, "artifact db error: {}", m),
        }
    }
}

impl std::error::Error for ArtifactError {}

/// Computed integrity digest + size of a model artifact set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactHashes {
    /// Lowercase hex SHA-256 of `model.onnx` (64 chars).
    pub model_sha256: String,
    /// Lowercase hex SHA-256 of `tokenizer.json` (64 chars).
    pub tokenizer_sha256: String,
    /// Byte length of `model.onnx` (recorded in the pinned row).
    pub model_size_bytes: i64,
}

/// Result of a successful [`verify_or_register`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedArtifacts {
    pub hashes: ArtifactHashes,
    /// `true` when no `rag_model_artifacts` row existed and this call
    /// self-registered one (manual-placement first-load trust).
    pub registered: bool,
}

/// Stream a file through SHA-256, returning lowercase hex (64 chars).
fn sha256_file(path: &Path) -> Result<String, ArtifactError> {
    let mut file = fs::File::open(path)
        .map_err(|e| ArtifactError::Io(format!("open {}: {}", path.display(), e)))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| ArtifactError::Io(format!("read {}: {}", path.display(), e)))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().fold(String::with_capacity(64), |s, b| {
        s + &format!("{:02x}", b)
    }))
}

/// Compute the integrity hashes of the artifact set in `model_dir`.
///
/// Both `model.onnx` and `tokenizer.json` must exist and be regular files;
/// anything else is [`ArtifactError::ModelFilesMissing`] (the taxonomy's
/// "model_dir absent or incomplete").
pub fn compute_hashes(model_dir: &Path) -> Result<ArtifactHashes, ArtifactError> {
    for name in [MODEL_FILE, TOKENIZER_FILE] {
        let p = model_dir.join(name);
        if !p.is_file() {
            return Err(ArtifactError::ModelFilesMissing(format!(
                "expected {} at {}",
                name,
                p.display()
            )));
        }
    }
    let model_sha256 = sha256_file(&model_dir.join(MODEL_FILE))?;
    let tokenizer_sha256 = sha256_file(&model_dir.join(TOKENIZER_FILE))?;
    let model_size_bytes = fs::metadata(model_dir.join(MODEL_FILE))
        .map_err(|e| ArtifactError::Io(format!("stat {}: {}", MODEL_FILE, e)))?
        .len() as i64;
    Ok(ArtifactHashes {
        model_sha256,
        tokenizer_sha256,
        model_size_bytes,
    })
}

/// Read the pinned `rag_model_artifacts` row for `profile`, if any.
///
/// Exposed for T010 (`auto` resolution probes) and T011 (`model fetch`
/// writes the row from project-recorded hashes).
pub fn pinned_hashes(
    store: &GraphStore,
    profile: &str,
) -> Result<Option<ArtifactHashes>, ArtifactError> {
    let conn = store.conn();
    match conn.query_row(
        "SELECT model_sha256, tokenizer_sha256, model_size_bytes
         FROM rag_model_artifacts WHERE profile = ?1",
        params![profile],
        |row| {
            Ok(ArtifactHashes {
                model_sha256: row.get(0)?,
                tokenizer_sha256: row.get(1)?,
                model_size_bytes: row.get(2)?,
            })
        },
    ) {
        Ok(h) => Ok(Some(h)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(ArtifactError::Db(format!(
            "read rag_model_artifacts for {}: {}",
            profile, e
        ))),
    }
}

/// Verify the artifact set in `model_dir` against the pinned row — or, when
/// no row exists for `profile`, self-register one from the present files.
///
/// This is THE load-time integrity gate of
/// contracts/embedding-backend.md § Local Model Artifacts:
///
/// 1. Hash the present files (missing files → `ModelFilesMissing`).
/// 2. Row absent → **first-load trust**: INSERT the computed hashes
///    (`mirror_url_used = NULL` — manual placement, not `/neurocode model
///    fetch`), return `registered = true`.
/// 3. Row present → **later immutability**: refuse on any SHA-256 mismatch
///    with `ModelFilesCorrupt` (the pinned row is never overwritten here —
///    only `/neurocode model fetch` may write project-recorded hashes).
pub fn verify_or_register(
    store: &GraphStore,
    profile: &str,
    model_dir: &Path,
) -> Result<VerifiedArtifacts, ArtifactError> {
    let hashes = compute_hashes(model_dir)?;

    match pinned_hashes(store, profile)? {
        Some(pinned) => {
            // Case-insensitive hex compare — tolerate hand-written rows.
            if !pinned
                .model_sha256
                .eq_ignore_ascii_case(&hashes.model_sha256)
            {
                return Err(ArtifactError::ModelFilesCorrupt(format!(
                    "{}: SHA-256 mismatch (pinned {}, found {}) for profile {}",
                    MODEL_FILE, pinned.model_sha256, hashes.model_sha256, profile
                )));
            }
            if !pinned
                .tokenizer_sha256
                .eq_ignore_ascii_case(&hashes.tokenizer_sha256)
            {
                return Err(ArtifactError::ModelFilesCorrupt(format!(
                    "{}: SHA-256 mismatch (pinned {}, found {}) for profile {}",
                    TOKENIZER_FILE, pinned.tokenizer_sha256, hashes.tokenizer_sha256, profile
                )));
            }
            Ok(VerifiedArtifacts {
                hashes,
                registered: false,
            })
        }
        None => {
            let fetched_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            store
                .conn()
                .execute(
                    "INSERT INTO rag_model_artifacts (profile, model_sha256, tokenizer_sha256,
                        model_size_bytes, fetched_at, mirror_url_used)
                     VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
                    params![
                        profile,
                        hashes.model_sha256,
                        hashes.tokenizer_sha256,
                        hashes.model_size_bytes,
                        fetched_at
                    ],
                )
                .map_err(|e| {
                    ArtifactError::Db(format!(
                        "self-register rag_model_artifacts for {}: {}",
                        profile, e
                    ))
                })?;
            Ok(VerifiedArtifacts {
                hashes,
                registered: true,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (inline per task assignment — siblings work in tests/ in parallel).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the hashing itself against the FIPS 180-2 "abc" vector.
    #[test]
    fn sha256_matches_known_vector() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("f");
        fs::write(&f, b"abc").unwrap();
        assert_eq!(
            sha256_file(&f).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn write_fake_artifacts(dir: &Path, model: &[u8], tokenizer: &[u8]) {
        fs::write(dir.join(MODEL_FILE), model).unwrap();
        fs::write(dir.join(TOKENIZER_FILE), tokenizer).unwrap();
    }

    fn row_exists(store: &GraphStore, profile: &str) -> bool {
        let n: i64 = store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM rag_model_artifacts WHERE profile = ?1",
                params![profile],
                |row| row.get(0),
            )
            .unwrap();
        n > 0
    }

    /// Missing files → `ModelFilesMissing`, and NO row is written —
    /// self-registration only happens on a *successful* load.
    #[test]
    fn missing_files_fail_before_registration() {
        let store = GraphStore::open_in_memory().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        // Empty dir — no model.onnx / tokenizer.json.
        let err = verify_or_register(&store, "nomic-embed-text-v1.5", tmp.path()).unwrap_err();
        assert!(
            matches!(err, ArtifactError::ModelFilesMissing(_)),
            "wrong error: {:?}",
            err
        );
        assert!(!row_exists(&store, "nomic-embed-text-v1.5"));

        // Half-present is equally incomplete.
        fs::write(tmp.path().join(MODEL_FILE), b"model-bytes").unwrap();
        let err = verify_or_register(&store, "nomic-embed-text-v1.5", tmp.path()).unwrap_err();
        assert!(matches!(err, ArtifactError::ModelFilesMissing(_)));
        assert!(!row_exists(&store, "nomic-embed-text-v1.5"));
    }

    /// Bad pinned hash → refuse to load (`ModelFilesCorrupt`), and the
    /// pinned row is NOT repaired/overwritten (later immutability).
    #[test]
    fn bad_hash_refuses_load_and_does_not_repair_row() {
        let store = GraphStore::open_in_memory().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        write_fake_artifacts(tmp.path(), b"model-bytes", b"tokenizer-bytes");

        // A pinned row whose hashes do NOT match the files on disk.
        store
            .conn()
            .execute(
                "INSERT INTO rag_model_artifacts (profile, model_sha256, tokenizer_sha256,
                    model_size_bytes, fetched_at, mirror_url_used)
                 VALUES ('nomic-embed-text-v1.5', '0000000000000000000000000000000000000000000000000000000000000000',
                         '1111111111111111111111111111111111111111111111111111111111111111',
                         11, '2026-08-27T00:00:00Z', NULL)",
                [],
            )
            .unwrap();

        let err = verify_or_register(&store, "nomic-embed-text-v1.5", tmp.path()).unwrap_err();
        match &err {
            ArtifactError::ModelFilesCorrupt(msg) => {
                assert!(msg.contains(MODEL_FILE), "message lacks file name: {}", msg);
            }
            other => panic!("expected ModelFilesCorrupt, got {:?}", other),
        }

        // The pinned row survives untouched — corruption cannot re-pin itself.
        let (m, t): (String, String) = store
            .conn()
            .query_row(
                "SELECT model_sha256, tokenizer_sha256 FROM rag_model_artifacts
                 WHERE profile = 'nomic-embed-text-v1.5'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(m, "0".repeat(64));
        assert_eq!(t, "1".repeat(64));
    }

    /// Self-registration: first load writes the row (manual placement,
    /// `mirror_url_used = NULL`); identical second load verifies without
    /// re-registering; a tampered subsequent load is refused.
    #[test]
    fn self_registration_then_tamper_refuses() {
        let store = GraphStore::open_in_memory().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let profile = "CodeRankEmbed";
        write_fake_artifacts(tmp.path(), b"model-bytes-v1", b"tokenizer-bytes-v1");

        // First load: no row exists → self-register.
        let first = verify_or_register(&store, profile, tmp.path()).unwrap();
        assert!(first.registered);
        assert_eq!(first.hashes.model_sha256.len(), 64);
        assert_eq!(first.hashes.tokenizer_sha256.len(), 64);
        assert_eq!(first.hashes.model_size_bytes, b"model-bytes-v1".len() as i64);

        // Row written from the computed hashes; manual placement → NULL mirror.
        let (m, t, size, mirror): (String, String, i64, Option<String>) = store
            .conn()
            .query_row(
                "SELECT model_sha256, tokenizer_sha256, model_size_bytes, mirror_url_used
                 FROM rag_model_artifacts WHERE profile = ?1",
                params![profile],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!((m.as_str(), t.as_str()), (first.hashes.model_sha256.as_str(), first.hashes.tokenizer_sha256.as_str()));
        assert_eq!(size, b"model-bytes-v1".len() as i64);
        assert_eq!(mirror, None);

        // Second load: same files → verifies against the row, no re-register.
        let second = verify_or_register(&store, profile, tmp.path()).unwrap();
        assert!(!second.registered);
        assert_eq!(second.hashes, first.hashes);

        // Tamper with the tokenizer after registration → refuse.
        fs::write(tmp.path().join(TOKENIZER_FILE), b"tokenizer-bytes-EVIL").unwrap();
        let err = verify_or_register(&store, profile, tmp.path()).unwrap_err();
        match &err {
            ArtifactError::ModelFilesCorrupt(msg) => {
                assert!(msg.contains(TOKENIZER_FILE), "message lacks file name: {}", msg);
            }
            other => panic!("expected ModelFilesCorrupt, got {:?}", other),
        }

        // Tamper the model file too → refuse, naming model.onnx.
        write_fake_artifacts(tmp.path(), b"model-bytes-v1", b"tokenizer-bytes-v1");
        fs::write(tmp.path().join(MODEL_FILE), b"model-bytes-EVIL").unwrap();
        let err = verify_or_register(&store, profile, tmp.path()).unwrap_err();
        match &err {
            ArtifactError::ModelFilesCorrupt(msg) => {
                assert!(msg.contains(MODEL_FILE), "message lacks file name: {}", msg);
            }
            other => panic!("expected ModelFilesCorrupt, got {:?}", other),
        }
    }

    /// Profiles are independent rows: registering one does not affect another.
    #[test]
    fn profiles_are_pinned_independently() {
        let store = GraphStore::open_in_memory().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        write_fake_artifacts(tmp.path(), b"shared-bytes", b"shared-bytes");

        let a = verify_or_register(&store, "nomic-embed-text-v1.5", tmp.path()).unwrap();
        assert!(a.registered);
        // Same files, different profile → separate row, also self-registers.
        let b = verify_or_register(&store, "CodeRankEmbed", tmp.path()).unwrap();
        assert!(b.registered);
        assert_eq!(a.hashes, b.hashes);

        let n: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM rag_model_artifacts", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(n, 2);
    }
}
