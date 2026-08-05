//! Lossless concrete syntax tree over Spec Kit markdown (FR-012/013).
//!
//! The CST preserves every byte of the source file (whitespace, comments,
//! unknown extensions, untouched ranges become `Raw` nodes). It is the
//! foundation that makes every later visual widget safe —
//! `parse(p, b)?.materialize() == b` is the round-trip invariant enforced by
//! `tests/cst_roundtrip.rs`.
//!
//! Layer: Meaning (P0 critical foundation). The CST sits *behind* the existing
//! lossy `parser/` modules from specs/001/010, which are preserved unchanged
//! for their contract surface (Constitution VII).

pub mod anchors;
pub mod fingerprint;
pub mod parser;
pub mod parser_trait;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Errors from CST operations. The parser is always total over input bytes
/// (contracts/cst-parser.md "Behavior with malformed input") — the only `Err`
/// path is I/O failure reading the file.
#[derive(Debug, thiserror::Error)]
pub enum CstError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("materialize error: {0}")]
    Materialize(String),
}

/// Opaque, `Copy` id for a CST node. Allocated deterministically by parse
/// order, stable across reparses of byte-identical content (so a UI holding a
/// `NodeId` does not need to re-fetch after an unrelated reparse). After any
/// byte change the document is reparsed and `NodeId`s are re-validated against
/// `fingerprint`; UIs re-bind via fingerprint, not raw `NodeId`, across edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u32);

impl NodeId {
    pub const ROOT: NodeId = NodeId(0);
}

// ---------------------------------------------------------------------
// §1 — CstNode / CstKind / CstProps / CstDocument  (data-model.md §1)
// ---------------------------------------------------------------------

/// The universal CST node type. One per syntactic construct (heading, list
/// item, code fence, table row, paragraph, raw range). See
/// `data-model.md §1` for the full field contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CstNode {
    pub id: NodeId,
    pub kind: CstKind,
    /// Inclusive UTF-8 byte offset into the source file (FR-013 anchor).
    pub byte_start: usize,
    /// Exclusive UTF-8 byte offset. `[byte_start, byte_end)` is owned range.
    pub byte_end: usize,
    /// The exact source bytes at parse time (FR-013). Verified before any
    /// write; a mismatch means the file changed and the patch is routed to
    /// three-way merge (FR-014/016).
    pub expected_bytes: String,
    /// SHA-256 of the file's full content at parse time (FR-013). Coarse
    /// drift detector shared across all nodes in a document.
    pub revision_hash: String,
    /// Structural fingerprint `"{kind}/{semantic_id}"`, e.g.
    /// `"requirement/FR-016"`, `"user_story/US2"`, `"task/T034"`. Used to pair
    /// nodes across three-way merge and track identity across edits (FR-013).
    pub fingerprint: String,
    /// Kind-specific extracted properties (see below). Extracted during parse,
    /// but never used to re-derive text — `expected_bytes` always wins.
    pub props: CstProps,
    /// Ordered child node ids. The CST is a tree; sibling ranges are
    /// contiguous and non-overlapping.
    pub children: Vec<NodeId>,
}

/// Exhaustive discriminant for Spec Kit markdown constructs. Every kind in
/// FR-009's mapping catalog has a parseable entry. Semantic-tinged kinds are
/// NOT separate variants — a `ListItem` stays a `ListItem`. Its semantic
/// classification (Requirement, Task, …) is assigned by the meaning layer
/// (§2), keeping the CST a pure syntactic representation (Constitution VI).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CstKind {
    Root,
    Heading { level: u8 },
    Paragraph,
    /// A `- ` or `* ` bullet (may carry a semantic pattern).
    ListItem,
    CodeFence { lang: Option<String> },
    Table,
    TableRow,
    TableCell,
    BlockQuote,
    ThematicBreak,
    /// Unrecognized bytes — preserved verbatim (lossless).
    Raw,
}

/// Kind-specific extracted properties. Stored on the node so the meaning
/// layer doesn't re-parse text, but always reconstructible from
/// `expected_bytes` (the bytes are authoritative).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "props", rename_all = "snake_case")]
pub enum CstProps {
    #[default]
    None,
    Heading { text: String },
    ListItem { marker: char, text: String },
    CodeFence { content: String },
    TableCell { text: String },
    Paragraph { text: String },
}

/// The top-level handle returned by the parser.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CstDocument {
    /// Repo-relative path (`specs/012-…/tasks.md`).
    pub artifact_path: String,
    /// SHA-256 of the file content at parse time.
    pub revision_hash: String,
    /// Source file length in bytes.
    pub byte_len: usize,
    /// Nodes ordered by `byte_start`.
    pub nodes: BTreeMap<NodeId, CstNode>,
    /// The root node (covers `[0, byte_len)`).
    pub root: NodeId,
}

impl CstDocument {
    /// Look up a node by id.
    pub fn get(&self, id: NodeId) -> Option<&CstNode> {
        self.nodes.get(&id)
    }

    /// Iterate nodes in byte-offset order.
    pub fn iter_in_order(&self) -> impl Iterator<Item = &CstNode> {
        self.nodes.values()
    }
}
