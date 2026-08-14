//! `CodeArtifactNode` — a unit of parsed code in the structural knowledge graph
//! (data-model.md Entity 3).

use serde::{Deserialize, Serialize};

use crate::pega::metadata::PegaMetadata;

/// The kind of a code artifact (FR-005).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum ArtifactKind {
    Class,
    Interface,
    Enum,
    Method,
    Field,
    PegaRule,
}

impl ArtifactKind {
    pub fn as_str(&self) -> &str {
        match self {
            ArtifactKind::Class => "Class",
            ArtifactKind::Interface => "Interface",
            ArtifactKind::Enum => "Enum",
            ArtifactKind::Method => "Method",
            ArtifactKind::Field => "Field",
            ArtifactKind::PegaRule => "PegaRule",
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "Class" => Ok(ArtifactKind::Class),
            "Interface" => Ok(ArtifactKind::Interface),
            "Enum" => Ok(ArtifactKind::Enum),
            "Method" => Ok(ArtifactKind::Method),
            "Field" => Ok(ArtifactKind::Field),
            "PegaRule" => Ok(ArtifactKind::PegaRule),
            other => Err(format!("unknown artifact kind '{}'", other)),
        }
    }
}

/// Lifecycle status of an artifact node (data-model.md Entity 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum ArtifactStatus {
    Active,
    Stale,
    Deleted,
}

impl ArtifactStatus {
    pub fn as_str(&self) -> &str {
        match self {
            ArtifactStatus::Active => "Active",
            ArtifactStatus::Stale => "Stale",
            ArtifactStatus::Deleted => "Deleted",
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "Active" => Ok(ArtifactStatus::Active),
            "Stale" => Ok(ArtifactStatus::Stale),
            "Deleted" => Ok(ArtifactStatus::Deleted),
            other => Err(format!("unknown artifact status '{}'", other)),
        }
    }
}

impl Default for ArtifactStatus {
    fn default() -> Self {
        ArtifactStatus::Active
    }
}

/// A unit of parsed code stored in the structural knowledge graph (spec Key Entity).
///
/// One row per parsed Java type/method/field or Pega rule (data-model.md Entity 3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeArtifactNode {
    /// Internal primary key (SQLite rowid). 0 for not-yet-inserted nodes.
    pub id: super::NodeId,
    pub kind: ArtifactKind,
    /// Fully-qualified canonical name.
    pub fqcn: String,
    /// Enclosing type name for methods/fields (FR-005).
    pub enclosing_type: Option<String>,
    pub package: String,
    /// Interfaces this type implements (FR-005).
    pub implemented_interfaces: Vec<String>,
    /// Framework annotations/declarations (FR-005).
    pub annotations: Vec<String>,
    /// Injected/declared dependencies (FR-005).
    pub declared_dependencies: Vec<String>,
    pub source_path: String,
    /// Byte range in the source file (tree-sitter span).
    pub source_span: Option<(u32, u32)>,
    /// Present only for Pega artifacts (FR-005, FR-009).
    pub pega_metadata: Option<PegaMetadata>,
    /// Detected framework version (e.g. `Spring Boot 3.2`).
    pub framework_version: Option<String>,
    pub status: ArtifactStatus,
    /// ISO-8601 timestamp of last ingestion.
    pub indexed_at: String,
}

impl CodeArtifactNode {
    /// Create a new node with default fields (Active status, now timestamp).
    pub fn new(kind: ArtifactKind, fqcn: String, package: String, source_path: String) -> Self {
        Self {
            id: 0,
            kind,
            fqcn,
            enclosing_type: None,
            package,
            implemented_interfaces: Vec::new(),
            annotations: Vec::new(),
            declared_dependencies: Vec::new(),
            source_path,
            source_span: None,
            pega_metadata: None,
            framework_version: None,
            status: ArtifactStatus::default(),
            indexed_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_kind_roundtrip() {
        for kind in [
            ArtifactKind::Class,
            ArtifactKind::Interface,
            ArtifactKind::Enum,
            ArtifactKind::Method,
            ArtifactKind::Field,
            ArtifactKind::PegaRule,
        ] {
            assert_eq!(ArtifactKind::parse(kind.as_str()).unwrap(), kind);
        }
    }

    #[test]
    fn node_new_defaults() {
        let n = CodeArtifactNode::new(
            ArtifactKind::Class,
            "com.example.Foo".into(),
            "com.example".into(),
            "src/Foo.java".into(),
        );
        assert_eq!(n.id, 0);
        assert_eq!(n.status, ArtifactStatus::Active);
        assert!(n.implemented_interfaces.is_empty());
        assert!(n.pega_metadata.is_none());
    }
}
