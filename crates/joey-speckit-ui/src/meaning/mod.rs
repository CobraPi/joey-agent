//! Derived semantic graph + mapping catalog (FR-009/040, P0/P2).
//!
//! The `SemanticGraph` is the derived, in-memory projection the meaning
//! widgets render and the traceability/coverage analysis runs against. It is
//! derived purely from CST(s) by pattern-matching (the mapping catalog in
//! FR-009); it is **never** a source of truth (Constitution III) and is
//! **never** persisted. Rebuilt lazily by the semantic cache on file changes
//! (research.md §4).

pub mod cache;
pub mod coverage;
pub mod graph;
pub mod mapping;

// Re-export the builder trait at the module root for convenience.
pub use graph::SemanticGraphBuilder;
pub use mapping::classify;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::cst::NodeId;

// ---------------------------------------------------------------------
// §2 — SemanticNode / SemanticKind / SemanticProps / SemanticId / OriginTag
// ---------------------------------------------------------------------

/// Stable within a graph version: `"requirement:FR-016"`,
/// `"user_story:US2"`, `"task:T034"`, etc.
pub type SemanticId = String;

/// One per *meaningful* CST node. A CST node with no semantic classification
/// produces no semantic node (data-model.md §2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticNode {
    pub id: SemanticId,
    pub kind: SemanticKind,
    /// Back-reference to the CST node (path + NodeId). The bridge to byte
    /// anchors for editing.
    pub origin: NodeOrigin,
    pub props: SemanticProps,
    /// `Source` (read from markdown), `Derived` (computed from graph),
    /// `Overlay` (external/private). FR-010 visual distinction.
    pub origin_tag: OriginTag,
    pub edges: Vec<Edge>,
}

/// Back-reference from a semantic node to its CST origin (byte-anchor bridge).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeOrigin {
    pub artifact: String,
    pub node: NodeId,
    pub byte_start: usize,
    pub byte_end: usize,
}

/// Exhaustive discriminant for Spec Kit semantic kinds (data-model.md §2).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticKind {
    Principle,
    UserStory,
    AcceptanceScenario,
    Requirement,
    SuccessCriterion,
    KeyEntity,
    EntityRelationship,
    Task,
    Phase,
    Checkpoint,
    Check,
    TechnicalContextField,
    ConstitutionGate,
    ComplexityViolation,
    ProjectStructureNode,
    ClarifyMarker,
}

/// Kind-specific extracted properties (data-model.md §2 highlights).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum SemanticProps {
    #[default]
    None,
    Requirement {
        id: String,
        modality: Modality,
        text: String,
    },
    UserStory {
        id: String,
        priority: Priority,
        title: String,
    },
    AcceptanceScenario {
        given: String,
        when: String,
        then: String,
    },
    SuccessCriterion {
        id: String,
        target_value: Option<f64>,
        unit: Option<String>,
        direction: Option<Direction>,
        text: String,
    },
    Task {
        id: String,
        parallel_eligible: bool,
        target_files: Vec<String>,
        user_story_ref: Option<SemanticId>,
        completed: bool,
        /// FR-NNN requirement ids referenced in the task description.
        /// Populated by classify(); consumed by wire_edges() to emit
        /// Implements edges. Additive (serde default = empty).
        #[serde(default)]
        implements_refs: Vec<String>,
    },
    KeyEntity {
        name: String,
        fields: Vec<String>,
    },
    EntityRelationship {
        source: String,
        verb: String,
        target: String,
        confidence: Confidence,
    },
    ClarifyMarker {
        text: String,
        owning_requirement: Option<SemanticId>,
    },
    Phase {
        number: u32,
        title: String,
    },
    Checkpoint {
        label: String,
        blocking: Option<bool>,
    },
    ConstitutionGate {
        principle: String,
        result: GateResultKind,
        evidence: String,
    },
    ComplexityViolation {
        rule: String,
        why_needed: String,
        rejected_alternative: String,
    },
}

/// Modality (MUST / SHOULD / MAY / MUST NOT).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    Must,
    Should,
    May,
    MustNot,
    #[default]
    Unparsed,
}

/// Priority (P1 / P2 / P3).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    P1,
    P2,
    P3,
    #[default]
    Unparsed,
}

/// Measurement direction for a success criterion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    HigherIsBetter,
    LowerIsBetter,
    Equal,
}

/// Confidence for an entity relationship (FR-011).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Explicit,
    Proposed,
}

/// Pass/fail/warn for a constitution gate row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateResultKind {
    Pass,
    Fail,
    Warn,
}

/// FR-010 origin tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginTag {
    /// Read directly from markdown.
    Source,
    /// Computed from the graph.
    Derived,
    /// External evidence / private state.
    Overlay,
}

impl Default for OriginTag {
    fn default() -> Self {
        OriginTag::Source
    }
}

/// One edge in the semantic graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub target: SemanticId,
    pub rel: EdgeKind,
}

/// Edge kind (data-model.md §2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Requirement → UserStory.
    DeliversValueFor,
    /// Task → Requirement.
    Implements,
    /// Task → ProjectStructureNode (file).
    Changes,
    /// Check → Requirement (or Task).
    Verifies,
    /// UserStory/Requirement → Principle.
    Governs,
    /// Phase → Task, UserStory → AcceptanceScenario.
    Contains,
    /// Task → Task (from "(depends on T012)" clauses).
    DependsOn,
    /// KeyEntity → KeyEntity (proposed, FR-011).
    ProposesRelationship,
}

/// The derived semantic graph (data-model.md §2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticGraph {
    pub feature_id: String,
    /// Per-artifact revision the graph was derived from. Used by the cache
    /// to detect staleness.
    pub revision_hashes: HashMap<String, String>,
    pub nodes: HashMap<SemanticId, SemanticNode>,
    /// Precomputed traceability defects (§3).
    pub defects: Vec<Defect>,
}

impl SemanticGraph {
    pub fn new(feature_id: String) -> Self {
        SemanticGraph {
            feature_id,
            revision_hashes: HashMap::new(),
            nodes: HashMap::new(),
            defects: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------
// §3 — Defect / DefectClass / Scaffold / InsertionMode / GenerativeFollowon
// ---------------------------------------------------------------------

/// The four defect classes (FR-023), each carrying the nodes involved and the
/// one-click fix affordance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Defect {
    pub id: String,
    pub class: DefectClass,
    pub source_nodes: Vec<SemanticId>,
    pub impact: String,
    pub scaffold: Scaffold,
    #[serde(default)]
    pub generative_followon: Option<GenerativeFollowon>,
}

/// Defect classification (FR-023).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefectClass {
    /// A Requirement with zero incoming Implements edges from Tasks.
    OrphanRequirement,
    /// A Task with no outgoing Implements edge to any Requirement.
    RogueTask,
    /// A Task (or implemented requirement) with no incoming Verifies edge.
    Unverified,
    /// A Task violating a principle with no Complexity Tracking entry.
    ConstitutionBreach,
}

/// The deterministic, instant, free part of a one-click fix (clarification Q3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scaffold {
    pub target_artifact: String,
    pub anchor_node: SemanticId,
    pub stub_bytes: String,
    pub insertion_mode: InsertionMode,
}

/// Where a scaffold stub is inserted relative to its anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertionMode {
    After,
    Within,
    Before,
}

/// Optional agent-generated follow-on for a defect fix (clarification Q3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerativeFollowon {
    pub prompt: String,
    pub target_artifact: String,
}
