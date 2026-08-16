//! joey-neurocode — multi-language coding agent engine (originally
//! "Enterprise Java & Pega Rule System", spec 015).
//!
//! Classifies incoming coding requests by complexity and routes them between
//! an economical and frontier model tier (composing with spec 011's
//! `ModelAllocator`), maintains a structural dependency graph of the target
//! codebase in SQLite+FTS5 (built via tree-sitter grammars for Java, Python,
//! JS/TS/TSX, Go, and Rust, plus a heuristic fallback extractor for the long
//! tail of languages), assembles a dependency-aware context graph per
//! request, understands the Pega Platform rule system in a version-adaptive
//! way (Java projects), runs an asynchronous build/verify feedback loop that
//! records successes as patterns and failures as anti-patterns, and ingests
//! domain knowledge.
//!
//! `joey-agent-core` consumes only the narrow [`engine::NeuroCodeEngine`]
//! trait; the graph store, ingestion pipeline, classifier internals, and
//! feedback loop are all private to this crate (Constitution VI).

pub mod classifier;
pub mod config;
pub mod context;
pub mod engine;
pub mod graph;
pub mod memory;
pub mod parse;
pub mod pega;
pub mod tier_resolver;
pub mod verify;
pub mod auto_index;

pub use auto_index::{AutoIndexProgress, AutoIndexState};

pub use classifier::{
    ClassificationSignal, ComplexityClassifier, ComplexityRoute, ComplexityTier, SignalKind,
};
pub use config::NeuroCodeConfig;
pub use context::{
    AssembledContext, ContextAssembler, ContextGraphSnapshot, EdgeSnapshot, ExpandedNode,
    ExpansionOutcome, ExpansionReason, MemberSnapshot, NodeSnapshot,
};
pub use engine::{CodingRequest, DefaultEngine, NeuroCodeCommands, NeuroCodeEngine};
pub use graph::{DependencyGraph, EdgeKind, GraphEdge, NodeId};
pub use graph::node::{ArtifactKind, ArtifactStatus, CodeArtifactNode};
pub use memory::domain::{
    ConflictReport, DomainKnowledge, KnowledgeCategory, KnowledgeSource,
};
pub use memory::patterns::{LearnedAntiPattern, LearnedPattern};
pub use pega::metadata::{PegaMetadata, PegaRuleFamily};
pub use tier_resolver::TierModelResolver;
pub use verify::parse::VerifyParseFormat;
pub use verify::runner::VerifyStep;

/// The on-disk NeuroCode schema version (contracts/graph-store-schema.md).
/// v2: additive `signature` column on `code_artifacts` (declaration headers
/// for methods/fields, surfaced in assembled context). v1 databases migrate
/// in place on open; rows keep NULL signatures until re-indexed.
pub const NEUROCODE_SCHEMA_VERSION: u32 = 2;
