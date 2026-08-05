//! Byte-anchor patch engine (FR-014/016, P0 critical foundation).
//!
//! The single point that writes developer-accepted edits to artifact files.
//! Every visual edit compiles to `PatchOp`s and flows through this contract.
//! Enforces the six lossless-patch rules in FR-014/016 and SC-005/006.

pub mod guard;
pub mod merge;
pub mod node_lock;
pub mod surgical;
pub mod transaction;

use async_trait::async_trait;
use std::path::PathBuf;

use crate::cst::parser::parse_bytes;
use crate::patch::transaction::{atomic_write, execute, TransactionOutcome};

use crate::cst::{CstDocument, NodeId};

/// The atomic unit the patch engine applies. A visual edit compiles to one or
/// more `PatchOp`s; FR-014 mandates surgical, transactional application.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PatchOp {
    /// Rewrite only the byte range of `node` with `new_bytes`.
    Replace { node: NodeId, new_bytes: String },
    /// Insert `new_bytes` immediately after `anchor`'s range. Used by the
    /// defect-fix scaffold (FR-023) and inline insertions.
    InsertAfter { anchor: NodeId, new_bytes: String },
    /// Remove the byte range of `node`.
    Delete { node: NodeId },
}

/// The outcome of a patch transaction (contracts/patch-engine.md).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum PatchResult {
    /// The patch applied cleanly. `undo` is the verified inverse `PatchOp`
    /// list — applying it restores the pre-patch bytes exactly (FR-014).
    Applied {
        new_revision_hash: String,
        undo: Vec<PatchOp>,
    },
    /// A guard check failed: the file changed on disk since the edit was
    /// based on it. The engine produced a three-way merge at semantic-block
    /// level (FR-016).
    Conflict(ThreeWayMerge),
    /// The node's anchor no longer resolves in the current CST (structure
    /// changed underneath). The node degrades to read-only with a reopen
    /// prompt; the engine never guesses a new range (FR-016 Edge Cases).
    AnchorUnresolved { node: NodeId },
    /// The patched buffer failed CST re-parse or validation. No file is
    /// replaced; the proposed buffer and diagnostics are kept for repair.
    ValidationFailed {
        proposed_bytes: String,
        diagnostics: Vec<String>,
    },
}

/// Three-way merge at the semantic-block (CST node) level (FR-016).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThreeWayMerge {
    /// The version the developer's edit was based on.
    pub base: CstDocument,
    /// The file's current on-disk content.
    pub current: CstDocument,
    /// The developer's proposed change.
    pub proposed: Vec<PatchOp>,
    /// Nodes whose `expected_bytes` differ on both sides; auto-mergeable
    /// nodes resolve silently.
    pub conflicts: Vec<MergeConflict>,
}

/// One conflict surfaced by the three-way merge.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MergeConflict {
    /// Structural id of the conflicting node.
    pub node_fingerprint: String,
    pub base_bytes: String,
    pub current_bytes: String,
    pub proposed_bytes: String,
    /// `None` until the developer chooses.
    pub resolution: Option<Resolution>,
}

/// A developer's resolution choice for a merge conflict.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    TakeBase,
    TakeCurrent,
    TakeProposed,
    Edit(String),
}

/// The narrow patch-engine trait (contracts/patch-engine.md).
#[async_trait]
pub trait PatchEngine: Send + Sync {
    /// Apply a patch transactionally. See patch-engine.md for the full
    /// guard/validate/atomic-replace contract.
    async fn apply(&self, artifact_path: &str, ops: Vec<PatchOp>) -> PatchResult;
}

/// Default patch engine: reads from a repo root, applies ops through the
/// guard/surgical/transaction pipeline, and atomically writes the result.
pub struct DefaultPatchEngine {
    repo_root: PathBuf,
}

impl DefaultPatchEngine {
    pub fn new(repo_root: PathBuf) -> Self {
        DefaultPatchEngine { repo_root }
    }

    fn resolve(&self, artifact_path: &str) -> PathBuf {
        // `artifact_path` is repo-relative (e.g. "specs/012-…/tasks.md").
        if artifact_path.starts_with('/') {
            PathBuf::from(artifact_path)
        } else {
            self.repo_root.join(artifact_path)
        }
    }
}

#[async_trait]
impl PatchEngine for DefaultPatchEngine {
    async fn apply(&self, artifact_path: &str, ops: Vec<PatchOp>) -> PatchResult {
        let path = self.resolve(artifact_path);
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                return PatchResult::ValidationFailed {
                    proposed_bytes: String::new(),
                    diagnostics: vec![format!("io error reading {artifact_path}: {e}")],
                };
            }
        };

        let doc = parse_bytes(artifact_path, source.as_bytes());
        match execute(&doc, &source, &ops) {
            TransactionOutcome::Applied { new_bytes, new_revision_hash, undo } => {
                if let Err(e) = atomic_write(&path, &new_bytes) {
                    return PatchResult::ValidationFailed {
                        proposed_bytes: new_bytes,
                        diagnostics: vec![format!("atomic write failed: {e}")],
                    };
                }
                PatchResult::Applied { new_revision_hash, undo }
            }
            TransactionOutcome::Conflict(three_way) => PatchResult::Conflict(three_way),
            TransactionOutcome::AnchorUnresolved { node } => PatchResult::AnchorUnresolved { node },
            TransactionOutcome::ValidationFailed { proposed_bytes, diagnostics } => {
                PatchResult::ValidationFailed { proposed_bytes, diagnostics }
            }
        }
    }
}

/// Convenience: apply a patch in-memory (no file I/O). Used by tests and the
/// scaffold path that wants to validate before writing.
pub fn apply_in_memory(
    doc: &CstDocument,
    source: &str,
    ops: &[PatchOp],
) -> PatchResult {
    match execute(doc, source, ops) {
        TransactionOutcome::Applied { new_revision_hash, undo, .. } => {
            PatchResult::Applied { new_revision_hash, undo }
        }
        TransactionOutcome::Conflict(t) => PatchResult::Conflict(t),
        TransactionOutcome::AnchorUnresolved { node } => PatchResult::AnchorUnresolved { node },
        TransactionOutcome::ValidationFailed { proposed_bytes, diagnostics } => {
            PatchResult::ValidationFailed { proposed_bytes, diagnostics }
        }
    }
}
