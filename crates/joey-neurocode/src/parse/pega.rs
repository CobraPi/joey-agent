//! Pega rule-pattern tree-sitter queries over the Java AST (FR-009, T031).
//!
//! Over the indexed Java, recognize `Rule-Obj-*`/`Data-*`/`Work-*` class
//! patterns, directed-inheritance declarations, and rule-reference patterns.

use crate::pega::metadata::{PegaMetadata, PegaRuleFamily};

/// Check whether a FQCN or type name matches a Pega rule pattern.
pub fn is_pega_rule(fqcn_or_name: &str) -> bool {
    fqcn_or_name.contains("Rule-Obj-")
        || fqcn_or_name.contains("Data-")
        || fqcn_or_name.contains("Work-")
        || fqcn_or_name.contains("Rule-")
        || fqcn_or_name.starts_with("com.pega")
}

/// Build PegaMetadata from a type name and optional detected version.
///
/// This is a lightweight pattern-based extraction (not a full Pega rules
/// engine). It recognizes rule class families from naming patterns and
/// extracts reference patterns from known fields.
pub fn extract_pega_metadata(
    fqcn: &str,
    annotations: &[String],
    declared_dependencies: &[String],
    pega_version: &str,
) -> Option<PegaMetadata> {
    if !is_pega_rule(fqcn) {
        return None;
    }

    let family = PegaRuleFamily::from_fqcn(fqcn);
    let rule_name = fqcn
        .rsplit('/')
        .next()
        .unwrap_or(fqcn)
        .to_string();

    // References: look for dependencies that are themselves Pega rules.
    let references_rules: Vec<String> = declared_dependencies
        .iter()
        .filter(|d| is_pega_rule(d))
        .cloned()
        .collect();

    // Inheritance: check annotations for "extends" patterns.
    let inherits_from = annotations
        .iter()
        .find_map(|a| {
            if a.starts_with("extends:") {
                Some(a[8..].to_string())
            } else {
                None
            }
        });

    Some(PegaMetadata {
        rule_class_family: family,
        rule_name,
        references_rules,
        inherits_from,
        pega_version: pega_version.to_string(),
    })
}

/// Match an ingested node's FQCN against a Pega rule reference (T058).
///
/// A reference (from `references_rules` / `inherits_from`) may be a `Rule-*`
/// style name or a Java-style identifier; it matches a node when it equals
/// the full FQCN, equals the simple (last) name segment of the FQCN, or the
/// FQCN's tail reconstructs the dotted reference.
pub fn node_matches_reference(fqcn: &str, reference: &str) -> bool {
    if fqcn == reference {
        return true;
    }
    let last = fqcn.rsplit('.').next().unwrap_or(fqcn);
    if last == reference {
        return true;
    }
    // Java-style dotted reference to a hyphenated rule name, e.g.
    // reference "com.pega.rules.Rule-Obj-Flow" vs fqcn
    // "com.pega.rules.Rule-Obj-Flow" (equal) — or a hyphen-free class name
    // for a hyphenated rule reference, e.g. fqcn tail "RuleObjFlow".
    let dehyphenate = |s: &str| s.replace('-', "").to_lowercase();
    !reference.contains(' ')
        && (dehyphenate(&fqcn.replace('.', "")) == dehyphenate(&reference.replace('.', ""))
            || dehyphenate(last) == dehyphenate(reference))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pega_rule_detection() {
        assert!(is_pega_rule("Rule-Obj-Activity"));
        assert!(is_pega_rule("Data-Admin-Security"));
        assert!(is_pega_rule("Work-Channel-Triage"));
        assert!(!is_pega_rule("com.example.MyService"));
    }

    #[test]
    fn extract_metadata_for_rule() {
        let meta = extract_pega_metadata(
            "Rule-Obj-Activity",
            &["Service".into()],
            &["Data-Admin-Security".into(), "Rule-Obj-Flow".into()],
            "8.8.0",
        );
        assert!(meta.is_some());
        let m = meta.unwrap();
        assert_eq!(m.rule_class_family, PegaRuleFamily::RuleObj);
        assert_eq!(m.pega_version, "8.8.0");
        assert_eq!(m.references_rules.len(), 2);
    }

    #[test]
    fn no_metadata_for_non_pega() {
        assert!(extract_pega_metadata("com.example.Foo", &[], &[], "1.0").is_none());
    }

    #[test]
    fn node_matches_reference_variants() {
        // Exact FQCN.
        assert!(node_matches_reference("Rule-Obj-Flow", "Rule-Obj-Flow"));
        // Simple name of a dotted FQCN.
        assert!(node_matches_reference(
            "com.pega.rules.FlowImplementation",
            "FlowImplementation"
        ));
        // Hyphenated rule reference matched by a dotted FQCN whose tail is the
        // de-hyphenated class name.
        assert!(node_matches_reference(
            "com.pega.rules.RuleObjFlow",
            "Rule-Obj-Flow"
        ));
        assert!(node_matches_reference(
            "com.pega.rules.Rule-Obj-Flow",
            "RuleObjFlow"
        ));
        // No match.
        assert!(!node_matches_reference("com.example.Foo", "Rule-Obj-Flow"));
        assert!(!node_matches_reference(
            "com.pega.rules.FlowImplementation",
            "Data-Admin-Security"
        ));
    }
}
