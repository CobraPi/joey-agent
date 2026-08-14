//! `EdgeKind` — typed dependency-graph relationships (data-model.md Entity 4).

use serde::{Deserialize, Serialize};
use std::fmt;

/// The typed relationship between two [`super::CodeArtifactNode`]s (FR-004).
///
/// Drives graph expansion during context assembly (FR-007).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum EdgeKind {
    /// `from` implements `to` (an interface).
    Implements,
    /// Inverse of Implements.
    IsImplementedBy,
    /// `from` injects/depends-on `to` (a declared type).
    Injects,
    /// `from` exchanges a DTO/type with `to`.
    ExchangesType,
    /// Pega: `from` references/delegates to `to` (rule-to-rule).
    ReferencesRule,
    /// Pega: directed inheritance (rule class hierarchy).
    InheritsRule,
}

impl EdgeKind {
    /// The inverse edge kind, if one exists.
    pub fn inverse(self) -> Option<EdgeKind> {
        match self {
            EdgeKind::Implements => Some(EdgeKind::IsImplementedBy),
            EdgeKind::IsImplementedBy => Some(EdgeKind::Implements),
            _ => None,
        }
    }

    /// Stable string tag used in the SQLite `edge_kind` column.
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeKind::Implements => "Implements",
            EdgeKind::IsImplementedBy => "IsImplementedBy",
            EdgeKind::Injects => "Injects",
            EdgeKind::ExchangesType => "ExchangesType",
            EdgeKind::ReferencesRule => "ReferencesRule",
            EdgeKind::InheritsRule => "InheritsRule",
        }
    }

    /// Parse from the SQLite string tag.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "Implements" => Ok(EdgeKind::Implements),
            "IsImplementedBy" => Ok(EdgeKind::IsImplementedBy),
            "Injects" => Ok(EdgeKind::Injects),
            "ExchangesType" => Ok(EdgeKind::ExchangesType),
            "ReferencesRule" => Ok(EdgeKind::ReferencesRule),
            "InheritsRule" => Ok(EdgeKind::InheritsRule),
            other => Err(format!("unknown edge kind '{}'", other)),
        }
    }
}

impl fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_kind_roundtrip() {
        for kind in [
            EdgeKind::Implements,
            EdgeKind::IsImplementedBy,
            EdgeKind::Injects,
            EdgeKind::ExchangesType,
            EdgeKind::ReferencesRule,
            EdgeKind::InheritsRule,
        ] {
            let s = kind.as_str();
            assert_eq!(EdgeKind::parse(s).unwrap(), kind);
        }
    }

    #[test]
    fn edge_kind_inverse() {
        assert_eq!(EdgeKind::Implements.inverse(), Some(EdgeKind::IsImplementedBy));
        assert_eq!(EdgeKind::IsImplementedBy.inverse(), Some(EdgeKind::Implements));
        assert_eq!(EdgeKind::Injects.inverse(), None);
    }
}
