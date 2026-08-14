//! T058 — Pega ingestion integration test.
//!
//! Ingest a temp project containing Pega-style Java files (package
//! `com.pega.*` qualifies via the `com.pega` prefix) plus a Gradle build file
//! pinning a Pega version. Verify that:
//! - ingested rule nodes carry `pega_metadata` (with the build-file version),
//! - `ReferencesRule`/`InheritsRule` edges connect rule nodes,
//! - the Pega version was detected from the build file.

use std::fs;

use joey_neurocode::graph::edge::EdgeKind;
use joey_neurocode::graph::node::ArtifactKind;
use joey_neurocode::graph::DependencyGraph;
use joey_neurocode::parse::ingest_project;
use joey_neurocode::pega::metadata::PegaRuleFamily;

#[test]
fn pega_ingestion_populates_metadata_edges_and_version() {
    let tmp = tempfile::tempdir().unwrap();

    // Build file pinning the Pega version (build-file detection path).
    fs::write(
        tmp.path().join("build.gradle"),
        "dependencies {\n    implementation 'com.pega:prweb:8.8.0'\n}\n",
    )
    .unwrap();

    let src = tmp.path().join("src").join("main").join("java").join("com").join("pega").join("rules");
    fs::create_dir_all(&src).unwrap();

    // A Pega-style activity class: package `com.pega.*` qualifies it as a
    // Pega rule. It references RuleObjFlow via an injected dependency and
    // inherits from it via `implements` (surfaced as a pseudo
    // `extends:` annotation for extract_pega_metadata).
    fs::write(
        src.join("MyActivityActivity.java"),
        r#"
package com.pega.rules;

import org.springframework.beans.factory.annotation.Autowired;

public class MyActivityActivity implements RuleObjFlow {

    @Autowired
    private RuleObjFlow flow;
}
"#,
    )
    .unwrap();

    // The referenced rule as a plain Java class (Pega rule via com.pega prefix).
    fs::write(
        src.join("RuleObjFlow.java"),
        r#"
package com.pega.rules;

public class RuleObjFlow {
}
"#,
    )
    .unwrap();

    let graph = DependencyGraph::open_in_memory().unwrap();
    let result = ingest_project(&graph, tmp.path());

    assert!(result.errors.is_empty(), "ingestion errors: {:?}", result.errors);
    assert_eq!(result.files_scanned, 2);

    // The activity node: PegaRule kind + metadata.
    let activity = graph
        .query_fts("MyActivityActivity", 10)
        .unwrap()
        .into_iter()
        .find(|n| n.fqcn == "com.pega.rules.MyActivityActivity")
        .expect("activity node should be ingested");
    assert_eq!(activity.kind, ArtifactKind::PegaRule, "activity should be a PegaRule");
    let meta = activity
        .pega_metadata
        .as_ref()
        .expect("activity node should carry pega_metadata");
    assert_eq!(meta.rule_class_family, PegaRuleFamily::Other);
    assert_eq!(meta.rule_name, "com.pega.rules.MyActivityActivity");
    assert_eq!(meta.pega_version, "8.8.0", "version should come from build.gradle");
    assert!(
        meta.references_rules.iter().any(|r| r.contains("RuleObjFlow")),
        "references_rules should include RuleObjFlow, got: {:?}",
        meta.references_rules
    );
    assert!(
        meta.inherits_from
            .as_deref()
            .map_or(false, |p| p.ends_with("RuleObjFlow")),
        "inherits_from should reference RuleObjFlow, got: {:?}",
        meta.inherits_from
    );

    // The referenced flow node is also a Pega rule with the detected version.
    let flow = graph
        .query_fts("RuleObjFlow", 10)
        .unwrap()
        .into_iter()
        .find(|n| n.fqcn == "com.pega.rules.RuleObjFlow" && n.kind == ArtifactKind::PegaRule)
        .expect("flow node should be ingested as a PegaRule");
    assert_eq!(
        flow.pega_metadata.as_ref().unwrap().pega_version,
        "8.8.0",
        "flow node metadata should carry the build-file version"
    );

    // ReferencesRule and InheritsRule edges from the activity to the flow.
    let mut kinds = Vec::new();
    for (to_id, kind) in graph.traverse_edges(activity.id, None).unwrap() {
        if to_id == flow.id {
            kinds.push(kind);
        }
    }
    assert!(
        kinds.contains(&EdgeKind::ReferencesRule),
        "expected ReferencesRule edge, got: {:?}",
        kinds
    );
    assert!(
        kinds.contains(&EdgeKind::InheritsRule),
        "expected InheritsRule edge, got: {:?}",
        kinds
    );

    // Edges were counted in the ingestion result.
    assert!(
        result.edges_created > 0,
        "ingestion should have created edges (got {})",
        result.edges_created
    );
}

#[test]
fn pega_ingestion_without_build_file_still_extracts_metadata() {
    // No build file: no version detected, but pattern-based rule extraction
    // still applies (generic-Java mode with an empty version string).
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src").join("com").join("pega").join("rules");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("WorkCase.java"),
        r#"
package com.pega.rules;

public class WorkCase {
}
"#,
    )
    .unwrap();

    let graph = DependencyGraph::open_in_memory().unwrap();
    let result = ingest_project(&graph, tmp.path());
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let node = graph
        .query_fts("WorkCase", 10)
        .unwrap()
        .into_iter()
        .find(|n| n.fqcn == "com.pega.rules.WorkCase")
        .expect("WorkCase node should be ingested");
    assert_eq!(node.kind, ArtifactKind::PegaRule);
    let meta = node.pega_metadata.as_ref().expect("metadata expected");
    assert_eq!(meta.pega_version, "", "no build file → empty version string");
}

#[test]
fn non_pega_project_gets_no_pega_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src").join("com").join("example");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("Foo.java"),
        r#"
package com.example;

public class Foo {
}
"#,
    )
    .unwrap();

    let graph = DependencyGraph::open_in_memory().unwrap();
    ingest_project(&graph, tmp.path());

    let node = graph
        .query_fts("Foo", 10)
        .unwrap()
        .into_iter()
        .find(|n| n.fqcn == "com.example.Foo")
        .expect("Foo node should be ingested");
    assert_eq!(node.kind, ArtifactKind::Class);
    assert!(node.pega_metadata.is_none(), "non-Pega class must not carry pega_metadata");
}
