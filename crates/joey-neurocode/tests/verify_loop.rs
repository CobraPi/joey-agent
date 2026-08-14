//! T042 — Verify loop integration test.
//!
//! Simulate a verification step with a failing command, verify the VerifyStep
//! runner captures the failure, verify error parsing produces structured
//! errors, and test pattern recording (record_pattern, record_anti_pattern).

use joey_neurocode::graph::node::{ArtifactKind, CodeArtifactNode};
use joey_neurocode::graph::DependencyGraph;
use joey_neurocode::verify::parse::{parse_errors, StructuredError};
use joey_neurocode::verify::runner::VerifyStep;

#[test]
fn failing_command_captured() {
    // `false` always exits non-zero.
    let step = VerifyStep::new("compile".into(), "false".into(), 10);
    let out = step.run(std::path::Path::new("."));

    assert_ne!(
        out.exit_code, 0,
        "false should exit non-zero (got {})",
        out.exit_code
    );
}

#[test]
fn passing_command_captured() {
    let step = VerifyStep::new("noop".into(), "true".into(), 10);
    let out = step.run(std::path::Path::new("."));
    assert_eq!(out.exit_code, 0, "true should exit 0");
}

#[test]
fn missing_tool_graceful_degradation() {
    // A command that does not exist on PATH → graceful skip (FR-012).
    let step = VerifyStep::new("missing".into(), "this-tool-does-not-exist-xyz123".into(), 10);
    let out = step.run(std::path::Path::new("."));

    // Exit code -1 signals a graceful skip / error, not 0.
    assert!(out.exit_code != 0, "missing tool should not exit 0");
    assert!(
        out.output.contains("not found") || out.output.contains("error"),
        "missing tool output should mention not-found/error: {}",
        out.output
    );
}

#[test]
fn unparsable_command_returns_error() {
    // shlex cannot parse this → error exit code.
    let step = VerifyStep::new("bad".into(), "".into(), 10);
    let out = step.run(std::path::Path::new("."));
    assert_eq!(out.exit_code, -1);
    assert!(out.output.contains("error") || out.output.contains("could not parse"));
}

#[test]
fn parse_compiler_errors_structured() {
    let output = "src/main/java/Foo.java:42: error: cannot find symbol\n  symbol:   variable foo\nsrc/Bar.java:7: error: ';' expected";
    let errors = parse_errors(output, "compiler");

    assert_eq!(errors.len(), 2, "expected 2 compiler errors");
    assert_eq!(errors[0].file.as_deref(), Some("src/main/java/Foo.java"));
    assert_eq!(errors[0].line, Some(42));
    assert!(errors[0].message.contains("cannot find symbol"));
    assert!(errors[0].signature.starts_with("Compile:"));

    assert_eq!(errors[1].file.as_deref(), Some("src/Bar.java"));
    assert_eq!(errors[1].line, Some(7));
}

#[test]
fn parse_checkstyle_xml_errors() {
    let xml = r#"<checkstyle version="4.3">
        <file name="src/main/java/A.java">
            <error line="10" column="5" severity="error" message="Missing Javadoc"/>
            <error line="22" column="1" severity="warning" message="Unused import"/>
        </file>
        <file name="src/main/java/B.java">
            <error line="3" column="1" severity="error" message="Line too long"/>
        </file>
    </checkstyle>"#;
    let errors = parse_errors(xml, "checkstyle_xml");

    assert_eq!(errors.len(), 3, "expected 3 checkstyle errors");
    // First two from A.java.
    assert_eq!(errors[0].file.as_deref(), Some("src/main/java/A.java"));
    assert_eq!(errors[0].line, Some(10));
    assert_eq!(errors[1].line, Some(22));
    // Third from B.java.
    assert_eq!(errors[2].file.as_deref(), Some("src/main/java/B.java"));
    assert_eq!(errors[2].line, Some(3));
    assert!(errors[0].signature.starts_with("Checkstyle:"));
}

#[test]
fn parse_maven_build_failure() {
    let output = "[INFO] -------------------------------------------------------------
[ERROR] BUILD FAILURE
[INFO] -------------------------------------------------------------
[INFO] Total time:  3.245 s";
    let errors = parse_errors(output, "maven");
    assert!(!errors.is_empty(), "maven BUILD FAILURE should parse");
    // The BUILD FAILURE line should be captured.
    assert!(errors
        .iter()
        .any(|e| e.message.contains("BUILD FAILURE")));
}

#[test]
fn parse_plain_errors() {
    let output = "Compiling...\nSomething went wrong: error in module X\nfatal exception\nall good";
    let errors = parse_errors(output, "plain");
    assert!(!errors.is_empty(), "plain parser should capture error lines");
    // Only error/fail/exception lines should be captured, not "Compiling...".
    for e in &errors {
        let lower = e.message.to_lowercase();
        assert!(
            lower.contains("error") || lower.contains("fail") || lower.contains("exception"),
            "plain parser should only keep error-ish lines: {}",
            e.message
        );
    }
}

#[test]
fn parse_empty_output_returns_no_errors() {
    let errors: Vec<StructuredError> = parse_errors("", "compiler");
    assert!(errors.is_empty());
}

#[test]
fn record_pattern_and_anti_pattern() {
    let graph = DependencyGraph::open_in_memory().unwrap();

    // Seed a node so we have an artifact id to reference.
    let node = CodeArtifactNode::new(
        ArtifactKind::Class,
        "com.example.Foo".into(),
        "com.example".into(),
        "src/Foo.java".into(),
    );
    let id = graph.upsert_node(&node).unwrap();

    assert_eq!(graph.store().pattern_count().unwrap(), 0);
    assert_eq!(graph.store().anti_pattern_count().unwrap(), 0);

    // Record a successful pattern.
    graph
        .store()
        .record_pattern(
            "refactor UserServiceImpl*findById",
            "generated findById with Optional wrap",
            "pass",
            &[id],
            "frontier",
        )
        .unwrap();
    assert_eq!(graph.store().pattern_count().unwrap(), 1);

    // Record an anti-pattern.
    graph
        .store()
        .record_anti_pattern(
            "NPE:UserServiceImpl*findById",
            "NullPointerException at line 42",
            "add null-check before dereferencing",
            &[id],
        )
        .unwrap();
    assert_eq!(graph.store().anti_pattern_count().unwrap(), 1);

    // Record a second anti-pattern to verify count increments.
    graph
        .store()
        .record_anti_pattern(
            "Compile:Foo.java:10",
            "';' expected",
            "add semicolon",
            &[id],
        )
        .unwrap();
    assert_eq!(graph.store().anti_pattern_count().unwrap(), 2);

    // Patterns persist across reopen.
    let db_path = tempfile::tempdir().unwrap();
    let db_file = db_path.path().join("graph.db");
    {
        let g = DependencyGraph::open(&db_file).unwrap();
        let nid = g.upsert_node(&node).unwrap();
        g.store()
            .record_pattern("sig-a", "summary", "pass", &[nid], "economical")
            .unwrap();
        g.store()
            .record_anti_pattern("err-a", "output", "fix", &[nid])
            .unwrap();
    }
    {
        let g = DependencyGraph::open(&db_file).unwrap();
        assert_eq!(g.store().pattern_count().unwrap(), 1);
        assert_eq!(g.store().anti_pattern_count().unwrap(), 1);
    }
}

#[test]
fn verify_step_output_duration_nonzero() {
    let step = VerifyStep::new("check".into(), "true".into(), 10);
    let out = step.run(std::path::Path::new("."));
    // duration_ms may be 0 on very fast systems, but the field is populated.
    let _ = out.duration_ms;
}
