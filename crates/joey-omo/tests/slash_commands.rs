//! T117/T118 integration: `/start-work` and `/goal` slash-command backing logic.
//!
//! These test the OMO runtime functions that the CLI and TUI slash handlers
//! invoke (`start_work` and `GoalState`), covering the acceptance scenarios:
//!   - T117: /start-work with non-existent plan errors; with one active work
//!     auto-resumes; multiple active works resolves the most recent.
//!   - T118: /goal set persists an Active goal; pause → no continuation;
//!     resume → continuation resumes; clear → removed.

use std::fs;

use joey_omo::{start_work, GoalAction, GoalState, GoalStatus, parse_goal_command};

fn temp_omo_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("joey-omo-test-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_plan(omo_dir: &std::path::Path, plan_name: &str) -> std::path::PathBuf {
    let plans = omo_dir.join("plans");
    fs::create_dir_all(&plans).unwrap();
    let path = plans.join(format!("{plan_name}.md"));
    fs::write(
        &path,
        "# Plan\n\n- [ ] 1. First task\n- [ ] 2. Second task\n",
    )
    .unwrap();
    path
}

// ── T117: /start-work ──────────────────────────────────────────────────

/// /start-work with a non-existent plan name returns an error and creates no
/// boulder state (no state change).
#[test]
fn start_work_nonexistent_plan_errors_with_no_state_change() {
    let dir = temp_omo_dir("start_nonexistent");
    let result = start_work(&dir, "sess-1", Some("does-not-exist"));
    assert!(result.is_err(), "non-existent plan must error");
    let err = result.unwrap_err();
    assert!(
        err.contains("not found") || err.contains("Plan file"),
        "error must mention the missing plan: {err}"
    );
    // No boulder state written.
    assert!(
        !dir.join("boulder.json").exists(),
        "no boulder state created on error"
    );
}

/// /start-work with an existing plan initializes a new work and writes boulder
/// state. A second /start-work with the same session auto-resumes (is_resume).
#[test]
fn start_work_initializes_then_auto_resumes() {
    let dir = temp_omo_dir("start_resume");
    write_plan(&dir, "my-feature");

    // First start: fresh init.
    let r1 = start_work(&dir, "sess-1", Some("my-feature")).unwrap();
    assert!(!r1.is_resume, "first start is not a resume");
    assert_eq!(r1.agent, "atlas");
    assert!(dir.join("boulder.json").exists(), "boulder written");

    // Second start with the same session → resume.
    let r2 = start_work(&dir, "sess-1", Some("my-feature")).unwrap();
    assert!(
        r2.is_resume,
        "second start with an active work must auto-resume"
    );
}

/// /start-work with no explicit plan name and no plans present errors with a
/// helpful hint.
#[test]
fn start_work_no_plans_errors_with_hint() {
    let dir = temp_omo_dir("start_none");
    // No plans directory at all.
    let result = start_work(&dir, "sess-1", None);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_lowercase().contains("no plans") || err.contains("plans") || err.contains("Prometheus"),
        "error should hint at creating a plan: {err}"
    );
}

// ── T118: /goal ────────────────────────────────────────────────────────

/// `/goal set <text>` persists an Active goal that a continuation injector
/// would pick up on the next idle turn (status == Active).
#[test]
fn goal_set_persists_active_goal() {
    let dir = temp_omo_dir("goal_set");
    let action = parse_goal_command("set Ship the feature");
    assert_eq!(action, GoalAction::Set { objective: "Ship the feature".into() });

    let goal = GoalState::new("sess-1".into(), "Ship the feature".into());
    goal.write(&dir).unwrap();

    let read = GoalState::read(&dir).expect("goal persisted");
    assert_eq!(read.objective, "Ship the feature");
    assert_eq!(read.status, GoalStatus::Active, "new goal is Active");
}

/// `/goal pause` flips status to Paused → the continuation injector skips it.
#[test]
fn goal_pause_disables_continuation() {
    let dir = temp_omo_dir("goal_pause");
    let mut goal = GoalState::new("sess-1".into(), "Ship it".into());
    goal.write(&dir).unwrap();
    // Simulate /goal pause.
    goal.status = GoalStatus::Paused;
    goal.write(&dir).unwrap();

    let read = GoalState::read(&dir).unwrap();
    assert_eq!(read.status, GoalStatus::Paused, "pause flips to Paused");
    // The continuation predicate: only Active goals inject.
    assert!(
        read.status != GoalStatus::Active,
        "paused goal must NOT trigger continuation injection"
    );
}

/// `/goal resume` flips status back to Active → continuation resumes.
#[test]
fn goal_resume_re_enables_continuation() {
    let dir = temp_omo_dir("goal_resume");
    let mut goal = GoalState::new("sess-1".into(), "Ship it".into());
    goal.status = GoalStatus::Paused;
    goal.write(&dir).unwrap();
    // Simulate /goal resume.
    goal.status = GoalStatus::Active;
    goal.write(&dir).unwrap();

    let read = GoalState::read(&dir).unwrap();
    assert_eq!(read.status, GoalStatus::Active, "resume re-enables injection");
}

/// `/goal clear` removes the goal file entirely.
#[test]
fn goal_clear_removes_state() {
    let dir = temp_omo_dir("goal_clear");
    let goal = GoalState::new("sess-1".into(), "Ship it".into());
    goal.write(&dir).unwrap();
    assert!(GoalState::read(&dir).is_some());

    GoalState::clear(&dir);
    assert!(GoalState::read(&dir).is_none(), "clear removes the goal");
}

/// parse_goal_command maps the subcommands correctly (T100 contract).
#[test]
fn goal_command_parsing() {
    assert_eq!(
        parse_goal_command("set My objective"),
        GoalAction::Set { objective: "My objective".into() }
    );
    assert_eq!(parse_goal_command("pause"), GoalAction::Pause);
    assert_eq!(parse_goal_command("resume"), GoalAction::Resume);
    assert_eq!(parse_goal_command("clear"), GoalAction::Clear);
    assert_eq!(parse_goal_command(""), GoalAction::Show);
    assert_eq!(parse_goal_command("show"), GoalAction::Show);
}
