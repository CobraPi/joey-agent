//! Policy layer resolution (spec 023, FR-002/FR-003).

pub mod resolver;
pub mod sources;

pub use resolver::{combine, CombinedPolicy, PolicyConflict, layer_order};

/// A layer of the policy hierarchy, from broad (Organization) to narrow
/// (TaskContract). `combine` precedence is low to high.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PolicyLayer {
    Organization,
    Repository,
    Module,
    ScopedRule,
    TaskContract,
}

/// One directive sourced from one policy layer, scoped by path globs.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PolicyBinding {
    pub layer: PolicyLayer,
    pub source_path: std::path::PathBuf,
    pub applies_to: Vec<String>,
    pub directive: String,
    pub conflicts_with: Vec<String>,
}
