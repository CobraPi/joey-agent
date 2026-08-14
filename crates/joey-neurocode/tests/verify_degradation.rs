//! FR-010/FR-012 regression tests: verify-step timeouts are enforced, and
//! skipped steps (missing tool / timeout) are graceful degradations — never
//! failures, fix-iteration triggers, or escalations.

use joey_neurocode::config::{VerifyConfig, VerifyStepConfig};
use joey_neurocode::graph::DependencyGraph;
use joey_neurocode::verify::runner::VerifyStep;
use joey_neurocode::verify::VerifyLoop;

#[test]
fn timeout_kills_hung_step() {
    // `sleep 30` with a 1s timeout must be killed and reported skipped.
    let step = VerifyStep::new("hang".into(), "sleep 30".into(), 1);
    let start = std::time::Instant::now();
    let out = step.run(std::path::Path::new("."));
    let elapsed = start.elapsed();

    assert!(out.skipped, "hung step must be reported skipped: {:?}", out.output);
    assert!(out.output.contains("timed out"), "output should say timed out: {}", out.output);
    // The timeout must actually fire near the deadline (allow slack for
    // process spawn + kill, but nowhere near the 30s sleep).
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "timeout should fire near 1s, took {:?}",
        elapsed
    );
}

#[test]
fn fast_step_unaffected_by_timeout() {
    let step = VerifyStep::new("noop".into(), "true".into(), 30);
    let out = step.run(std::path::Path::new("."));
    assert!(!out.skipped);
    assert_eq!(out.exit_code, 0);
}

#[test]
fn missing_tool_is_skipped_not_failed() {
    let step = VerifyStep::new("missing".into(), "no-such-tool-xyz-123".into(), 10);
    let out = step.run(std::path::Path::new("."));
    assert!(out.skipped);
    assert!(out.output.contains("not found"));
}

#[test]
fn skipped_step_does_not_trigger_fix_iterations_or_escalation() {
    // A verify config whose only step's tool is absent: the loop must
    // treat the run as all-passed (degraded, not failed), consume zero fix
    // iterations, and never escalate.
    let graph = DependencyGraph::open_in_memory().unwrap();
    let config = VerifyConfig {
        steps: vec![VerifyStepConfig {
            name: "lint".into(),
            command: "no-such-tool-xyz-123".into(),
            parse: "plain".into(),
            timeout_sec: 5,
        }],
        max_fix_iterations: 3,
    };
    let loop_ = VerifyLoop::new(config, std::sync::Arc::new(graph));
    let outcome = loop_.run_with_fixes(std::path::Path::new("."), |_| {
        panic!("fix callback must not be invoked for a skipped step");
    });

    assert!(outcome.all_passed, "skipped-only run is degraded, not failed");
    assert_eq!(outcome.fix_iterations_used, 0);
    assert!(!outcome.should_escalate());
    assert!(outcome.results[0].skipped);
}

#[test]
fn hung_step_in_loop_degrades_not_escalates() {
    // A step that times out inside run_with_fixes: same graceful semantics.
    let graph = DependencyGraph::open_in_memory().unwrap();
    let config = VerifyConfig {
        steps: vec![VerifyStepConfig {
            name: "hang".into(),
            command: "sleep 30".into(),
            parse: "plain".into(),
            timeout_sec: 1,
        }],
        max_fix_iterations: 3,
    };
    let loop_ = VerifyLoop::new(config, std::sync::Arc::new(graph));
    let outcome = loop_.run_with_fixes(std::path::Path::new("."), |_| false);

    assert!(outcome.all_passed, "timed-out step is a skip, not a failure");
    assert_eq!(outcome.fix_iterations_used, 0);
    assert!(!outcome.should_escalate());
}

#[test]
fn real_failure_still_triggers_fix_iterations() {
    // Guard against over-correcting: a genuinely failing step must still
    // drive the correction loop and escalate when the budget is exhausted.
    let graph = DependencyGraph::open_in_memory().unwrap();
    let config = VerifyConfig {
        steps: vec![VerifyStepConfig {
            name: "compile".into(),
            command: "false".into(), // exits 1, always fails
            parse: "plain".into(),
            timeout_sec: 5,
        }],
        max_fix_iterations: 2,
    };
    let loop_ = VerifyLoop::new(config, std::sync::Arc::new(graph));
    let mut fix_calls = 0;
    let outcome = loop_.run_with_fixes(std::path::Path::new("."), |_| {
        fix_calls += 1;
        true // pretend the correction pass changes something
    });

    assert!(!outcome.all_passed);
    assert_eq!(outcome.fix_iterations_used, 2);
    assert!(outcome.should_escalate());
    assert_eq!(fix_calls, 2);
}
