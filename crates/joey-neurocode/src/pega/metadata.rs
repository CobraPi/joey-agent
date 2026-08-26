//! `PegaMetadata` — structural metadata for Pega Platform artifacts
//! (data-model.md Entity 8).

use serde::{Deserialize, Serialize};

/// The Pega rule-class family (mapped from `Rule-Obj-*`/`Data-*`/`Work-*` patterns).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum PegaRuleFamily {
    /// Mapped from `Rule-Obj-*` patterns.
    RuleObj,
    /// Mapped from `Data-*` patterns.
    Data,
    /// Mapped from `Work-*` patterns.
    Work,
    /// Fallback for unrecognized patterns.
    Other,
}

impl PegaRuleFamily {
    pub fn as_str(&self) -> &str {
        match self {
            PegaRuleFamily::RuleObj => "RuleObj",
            PegaRuleFamily::Data => "Data",
            PegaRuleFamily::Work => "Work",
            PegaRuleFamily::Other => "Other",
        }
    }

    /// Detect the rule family from a FQCN or class name.
    pub fn from_fqcn(fqcn: &str) -> Self {
        if fqcn.contains("Rule-Obj-") || fqcn.starts_with("RuleObj") {
            PegaRuleFamily::RuleObj
        } else if fqcn.starts_with("Data-") || fqcn.contains("Data-") {
            PegaRuleFamily::Data
        } else if fqcn.starts_with("Work-") || fqcn.contains("Work-") {
            PegaRuleFamily::Work
        } else {
            PegaRuleFamily::Other
        }
    }
}

/// Structural metadata specific to Pega Platform artifacts (spec Key Entity).
///
/// Present only when `kind == PegaRule` or the Java type matches Pega rule patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PegaMetadata {
    pub rule_class_family: PegaRuleFamily,
    pub rule_name: String,
    /// Other rules this rule references/delegates to (drives ReferencesRule edges).
    pub references_rules: Vec<String>,
    /// Directed-inheritance parent rule class (drives InheritsRule edges).
    pub inherits_from: Option<String>,
    /// The detected Pega version this artifact's metadata is grounded in (Q4).
    pub pega_version: String,
}

/// Core Pega Platform rule-type metadata for generation grounding
/// (FR-009/FR-013, T060).
///
/// Returns `(rule_type_name, description)` pairs covering the main rule
/// families: activities, flows, data transforms, decision rules, data types,
/// and cases. Descriptions explain instance/reference semantics at a level
/// useful for grounding code generation (what the rule IS, when to use it).
///
/// The version selects variant wording: Pega Infinity ('23/'24, 23.x/24.x)
/// uses Constellation-era wording; legacy 8.x and unknown versions get the
/// classic Pega 8 wording.
pub fn rule_type_metadata_for_version(version: &str) -> Vec<(String, String)> {
    let is_infinity = version.contains("24")
        || version.contains("23")
        || version.to_lowercase().contains("infinity");
    if is_infinity {
        vec![
            (
                "Rule-Obj-Activity".into(),
                "A sequence of steps that automates processing. In Constellation-era Infinity, "
                    .to_string()
                    + "prefer Case Types, Data Transforms, and declaratives first — use activities "
                    + "only for advanced integration/service logic not covered by App Studio.",
            ),
            (
                "Rule-Obj-Flow".into(),
                "Defines the lifecycle path of a case (stages, steps, connectors). Infinity "
                    .to_string()
                    + "Constellation derives case lifecycle from Case Type rules instead of "
                    + "traditional flows — existing flows run only in legacy Dev Studio portals.",
            ),
            (
                "Rule-Obj-CaseType".into(),
                "The primary Infinity building block: defines a case's stages, views, and "
                    .to_string()
                    + "behavior. Use Case Types for new work in Constellation — they replace "
                    + "flow-based case design.",
            ),
            (
                "Rule-Obj-DataTransform".into(),
                "Sets property values on a page without procedural steps. Preferred over "
                    .to_string()
                    + "activities for mapping data between pages and initializing records.",
            ),
            (
                "Rule-Obj-Report-Definition".into(),
                "Defines a reusable query over class data (filters, columns, sorting). The "
                    .to_string()
                    + "basis of Insights and list views in Infinity; reference by name from "
                    + "components and APIs.",
            ),
            (
                "Rule-Obj-DecisionTable".into(),
                "A table of conditions → results evaluated top-down, first match wins. Use for "
                    .to_string()
                    + "rule-based business decisions without procedural code.",
            ),
            (
                "Rule-Obj-DecisionTree".into(),
                "A branching structure of conditions evaluated at runtime to choose a result. "
                    .to_string()
                    + "Use when decisions are naturally hierarchical rather than tabular.",
            ),
            (
                "Rule-Obj-When".into(),
                "A single boolean condition evaluated true or false. Referenced by flows, "
                    .to_string()
                    + "validations, and declarative networks to guard behavior.",
            ),
            (
                "Rule-Obj-Section".into(),
                "A UI layout assembling other components/sections. In Constellation, views are "
                    .to_string()
                    + "generated from Case Types and fields; hand-built sections apply to "
                    + "legacy UI only.",
            ),
            (
                "Data-Admin-*".into(),
                "System-data instance classes (operators, access groups, data types, node "
                    .to_string()
                    + "definitions). Instances hold environment configuration — never "
                    + "hardcode values owned by a Data-Admin- class.",
            ),
            (
                "Data-*".into(),
                "Data type classes holding business data records (sourced by data pages or "
                    .to_string()
                    + "the platform's local data storage). Reference data instances via "
                    + "data pages, never direct queries.",
            ),
            (
                "Work-*".into(),
                "Case instance classes: a Work- object IS a case (a unit of work) with its "
                    .to_string()
                    + "status, assignment history, and parties. Case processing is driven by "
                    + "the Case Type / flow that instantiated it.",
            ),
            (
                "Rule-Obj-Class".into(),
                "Defines a class in the platform's directed-inheritance hierarchy. Rule "
                    .to_string()
                    + "resolution walks the class hierarchy at runtime — patterns like "
                    + "circumstance and specialization select among instances of a class.",
            ),
        ]
    } else {
        vec![
            (
                "Rule-Obj-Activity".into(),
                "A sequenced set of processing steps (steps, when conditions, transitions). "
                    .to_string()
                    + "An activity instance is a saved rule; it is referenced by name from "
                    + "flows, harnesses, and other activities. Prefer declaratives and data "
                    + "transforms where possible.",
            ),
            (
                "Rule-Obj-Flow".into(),
                "Defines case lifecycle routing: shapes (assignments, decisions, utilities) "
                    .to_string()
                    + "connected by links. A flow instance is executed per case; main flows "
                    + "drive stage progression.",
            ),
            (
                "Rule-Obj-DataTransform".into(),
                "Declaratively sets and copies property values between pages. Referenced by "
                    .to_string()
                    + "name from activities, flows, and case types — no procedural steps.",
            ),
            (
                "Rule-Obj-Report-Definition".into(),
                "A reusable, parameterized query (filters, columns) over instances of a "
                    .to_string()
                    + "class. Referenced by sections, activities, and other reports.",
            ),
            (
                "Rule-Obj-DecisionTable".into(),
                "Conditions-to-results table evaluated top-down. Instances are referenced by "
                    .to_string()
                    + "flows/activities to make business decisions.",
            ),
            (
                "Rule-Obj-DecisionTree".into(),
                "Hierarchical if/then evaluation structure returning one result. Referenced "
                    .to_string()
                    + "by name wherever a decision value is needed.",
            ),
            (
                "Rule-Obj-When".into(),
                "A reusable boolean condition rule. Referenced by flow connectors, "
                    .to_string()
                    + "validations, and circumstances to gate processing.",
            ),
            (
                "Rule-Obj-Section".into(),
                "A UI form/harness fragment composing layouts and controls. Sections "
                    .to_string()
                    + "reference other sections and are referenced by harnesses.",
            ),
            (
                "Data-Admin-*".into(),
                "Administrative/system data instance classes (operators, access roles, "
                    .to_string()
                    + "node settings). Instances configure the environment — treat as "
                    + "configuration, not application data.",
            ),
            (
                "Data-*".into(),
                "Data classes holding business records; instances are accessed through data "
                    .to_string()
                    + "pages, which cache and source the data. Reference data via data "
                    + "page names in expressions.",
            ),
            (
                "Work-*".into(),
                "Work/case classes: an instance IS a case carrying status, assignments, and "
                    .to_string()
                    + "history. Flows and assignments operate on open Work- instances.",
            ),
            (
                "Rule-Obj-Class".into(),
                "A class definition in the directed-inheritance hierarchy; rule resolution "
                    .to_string()
                    + "walks up the class chain to find the applicable instance.",
            ),
        ]
    }
}

/// Ingest Pega rule-type metadata for a version into the graph store's
/// domain-knowledge tables (T060).
///
/// For each `(name, description)` pair from [`rule_type_metadata_for_version`],
/// records a `PegaRuleType` domain-knowledge source and indexes the
/// `"{name}: {description}"` content into the domain FTS. Returns the number
/// of entries ingested.
pub fn ingest_rule_type_metadata(
    store: &crate::graph::store::GraphStore,
    version: &str,
) -> usize {
    let entries = rule_type_metadata_for_version(version);
    let mut count = 0usize;
    for (name, description) in &entries {
        let source = format!("rule-type:{}", name);
        if store
            .upsert_domain_knowledge(
                "PegaRuleType",
                &source,
                Some(version),
                "Pega Platform rule-type metadata",
            )
            .is_err()
        {
            continue;
        }
        let content = format!("{}: {}", name, description);
        let _ = store.index_domain_content(&content, "Pega rule-type metadata", Some(version));
        count += 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_family_detection() {
        assert_eq!(
            PegaRuleFamily::from_fqcn("Rule-Obj-Activity"),
            PegaRuleFamily::RuleObj
        );
        assert_eq!(
            PegaRuleFamily::from_fqcn("Data-Admin-Security"),
            PegaRuleFamily::Data
        );
        assert_eq!(
            PegaRuleFamily::from_fqcn("Work-Channel-Triage"),
            PegaRuleFamily::Work
        );
        assert_eq!(
            PegaRuleFamily::from_fqcn("com.example.MyService"),
            PegaRuleFamily::Other
        );
    }

    #[test]
    fn rule_type_metadata_non_empty_and_recognizable() {
        for version in ["8.8", "24.1.0", "Infinity '24", "unknown"] {
            let entries = rule_type_metadata_for_version(version);
            assert!(
                entries.len() >= 10,
                "expected >=10 rule-type entries for version {}, got {}",
                version,
                entries.len()
            );
            let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
            assert!(names.contains(&"Rule-Obj-Activity"), "names: {:?}", names);
            assert!(names.contains(&"Rule-Obj-Flow"), "names: {:?}", names);
            assert!(names.contains(&"Data-*"), "names: {:?}", names);
            assert!(names.contains(&"Work-*"), "names: {:?}", names);
            for (name, desc) in &entries {
                assert!(!desc.is_empty(), "{} should have a description", name);
            }
        }
    }

    #[test]
    fn ingest_rule_type_metadata_populates_domain_tables() {
        let store = crate::graph::store::GraphStore::open_in_memory().unwrap();
        let count = ingest_rule_type_metadata(&store, "8.8.0");
        assert!(count >= 10, "ingested {} entries, expected >= 10", count);

        let sources = store.list_domain_sources().unwrap();
        assert_eq!(sources.len(), count);
        assert!(sources
            .iter()
            .all(|s| s.category == "PegaRuleType" && s.version_tag.as_deref() == Some("8.8.0")));
        assert!(sources
            .iter()
            .any(|s| s.source_path == "rule-type:Rule-Obj-Activity"));

        let hits = store
            .query_domain_fts("Rule-Obj-Flow", 10)
            .unwrap();
        assert!(!hits.is_empty(), "FTS should find Rule-Obj-Flow entries");
        assert!(hits.iter().any(|h| h.content.contains("Rule-Obj-Flow")));
    }
}
