//! T061 — detached verify-loop orchestrator integration tests.
//!
//! (a) run_with_fixes with a failing step and a fix closure that flips a
//!     marker file so the command passes on retry;
//! (b) max_fix_iterations respected with a never-fixing closure;
//! (c) run_detached inside a tokio runtime completes and delivers the
//!     outcome through the oneshot channel.

use std::path::PathBuf;
use std::sync::Arc;

use joey_neurocode::config::{VerifyConfig, VerifyStepConfig};
use joey_neurocode::graph::DependencyGraph;
use joey_neurocode::verify::{VerifyLoop, VerifyOutcome};

/// A step command that passes iff `marker` exists in the working directory.
fn marker_step(name: &str, marker: &str) -> VerifyStepConfig {
    VerifyStepConfig {
        name: name.into(),
        command: format!("test -f {}", marker),
        parse: "plain".into(),
        timeout_sec: 10,
    }
}

fn config(steps: Vec<VerifyStepConfig>, max_fix_iterations: u32) -> VerifyConfig {
    VerifyConfig {
        steps,
        max_fix_iterations,
    }
}

#[test]
fn run_with_fixes_recovers_when_fix_flips_marker() {
    let dir = tempfile::tempdir().unwrap();
    let project_root = dir.path();

    // Step fails while `fixed.txt` is absent; the fix closure creates it.
    let mut cfg = config(vec![marker_step("compile", "fixed.txt")], 3);
    cfg.steps[0].command = "sh -c 'test -f fixed.txt'".into();

    let graph = Arc::new(DependencyGraph::open_in_memory().unwrap());
    let verify = VerifyLoop::new(cfg, graph);

    let root: PathBuf = project_root.to_path_buf();
    let marker_path = root.join("fixed.txt");
    let outcome = verify.run_with_fixes(&root, |_results| {
        // The correction pass: write the marker so the retry passes.
        std::fs::write(&marker_path, b"fixed").is_ok()
    });

    assert!(
        outcome.all_passed,
        "step should pass after the fix flips the marker; results: {:?}",
        outcome.results
    );
    assert!(
        outcome.fix_iterations_used >= 1,
        "at least one fix iteration should be consumed (got {})",
        outcome.fix_iterations_used
    );
    assert!(!outcome.escalated);
    assert_eq!(outcome.escalate_hint(), None);
}

#[test]
fn max_fix_iterations_respected_when_fix_never_works() {
    let dir = tempfile::tempdir().unwrap();
    let project_root = dir.path();

    // `false` never passes regardless of any fix.
    let cfg = config(
        vec![VerifyStepConfig {
            name: "compile".into(),
            command: "false".into(),
            parse: "plain".into(),
            timeout_sec: 10,
        }],
        2,
    );

    let graph = Arc::new(DependencyGraph::open_in_memory().unwrap());
    let verify = VerifyLoop::new(cfg, graph);

    let mut fix_calls = 0;
    let outcome = verify.run_with_fixes(project_root, |_results| {
        fix_calls += 1;
        true // claims to change something, but the step still fails
    });

    assert!(!outcome.all_passed);
    assert_eq!(
        outcome.fix_iterations_used, 2,
        "loop must stop at max_fix_iterations"
    );
    assert_eq!(fix_calls, 2, "fix called once per iteration");
    assert!(outcome.escalated, "exhausted + failing → escalated");
}

#[test]
fn fix_callback_returning_false_breaks_early() {
    let dir = tempfile::tempdir().unwrap();

    let cfg = config(
        vec![VerifyStepConfig {
            name: "compile".into(),
            command: "false".into(),
            parse: "plain".into(),
            timeout_sec: 10,
        }],
        5,
    );

    let graph = Arc::new(DependencyGraph::open_in_memory().unwrap());
    let verify = VerifyLoop::new(cfg, graph);

    let outcome = verify.run_with_fixes(dir.path(), |_results| false);

    assert!(!outcome.all_passed);
    assert_eq!(
        outcome.fix_iterations_used, 0,
        "no-op fix breaks before consuming an iteration"
    );
    // Budget not exhausted → no escalation.
    assert!(!outcome.escalated);
}

#[test]
fn all_passing_steps_consume_no_iterations() {
    let dir = tempfile::tempdir().unwrap();

    let cfg = config(
        vec![VerifyStepConfig {
            name: "noop".into(),
            command: "true".into(),
            parse: "plain".into(),
            timeout_sec: 10,
        }],
        3,
    );

    let graph = Arc::new(DependencyGraph::open_in_memory().unwrap());
    let verify = VerifyLoop::new(cfg, graph);

    let outcome = verify.run_with_fixes(dir.path(), |_results| {
        panic!("fix closure must not be called when everything passes")
    });

    assert!(outcome.all_passed);
    assert_eq!(outcome.fix_iterations_used, 0);
}

#[tokio::test]
async fn run_detached_completes_and_delivers_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let project_root = dir.path().to_path_buf();

    // A passing step — the detached run should complete promptly.
    let cfg = config(
        vec![VerifyStepConfig {
            name: "noop".into(),
            command: "true".into(),
            parse: "plain".into(),
            timeout_sec: 10,
        }],
        2,
    );
    let graph = Arc::new(DependencyGraph::open_in_memory().unwrap());

    let mut rx = VerifyLoop::run_detached(cfg, graph, project_root);
    let outcome: VerifyOutcome = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        &mut rx,
    )
    .await
    .expect("detached verify should complete")
    .expect("sender should not be dropped");

    assert!(outcome.all_passed);
    assert_eq!(outcome.results.len(), 1);
    assert_eq!(outcome.results[0].step_name, "noop");
}

#[tokio::test]
async fn run_detached_failing_step_reports_escalation() {
    let dir = tempfile::tempdir().unwrap();
    let project_root = dir.path().to_path_buf();

    // `false` never passes; detached machinery uses a no-op fix closure, so
    // the loop breaks immediately with a failing outcome.
    let cfg = config(
        vec![VerifyStepConfig {
            name: "compile".into(),
            command: "false".into(),
            parse: "plain".into(),
            timeout_sec: 10,
        }],
        2,
    );
    let graph = Arc::new(DependencyGraph::open_in_memory().unwrap());

    let mut rx = VerifyLoop::run_detached(cfg, graph, project_root);
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        &mut rx,
    )
    .await
    .expect("detached verify should complete")
    .expect("sender should not be dropped");

    assert!(!outcome.all_passed);
    assert!(!outcome.results[0].passed);
    // No-op fix → early break, budget not exhausted → no escalation.
    assert!(!outcome.escalated);
}

#[test]
fn run_detached_works_outside_a_runtime() {
    // No tokio runtime here — must fall back to a plain std::thread and
    // still deliver through the oneshot channel without panicking.
    let dir = tempfile::tempdir().unwrap();
    let project_root = dir.path().to_path_buf();

    let cfg = config(
        vec![VerifyStepConfig {
            name: "noop".into(),
            command: "true".into(),
            parse: "plain".into(),
            timeout_sec: 10,
        }],
        1,
    );
    let graph = Arc::new(DependencyGraph::open_in_memory().unwrap());

    let mut rx = VerifyLoop::run_detached(cfg, graph, project_root);
    loop {
        match rx.try_recv() {
            Ok(outcome) => {
                assert!(outcome.all_passed);
                break;
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                panic!("sender dropped without delivering the outcome");
            }
        }
    }
}
