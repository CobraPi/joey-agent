//! Feature 022 (T015): team-tools integration tests.
//!
//! External, pub-API-only coverage of the team registry lifecycle:
//! on-disk round trips, direct teammate messaging, concurrent claim
//! single-winner, dependency blocking, mailbox drop-oldest, toolset
//! resolution for team children, and close_all wind-down.
//!
//! Every test that touches the global registry (or TeamRecord ops, which
//! persist via default_home()) runs under a shared ENV_LOCK with a private
//! JOEY_HOME tempdir — no parallel env races.

use std::sync::atomic::{AtomicUsize, Ordering};

use joey_orchestration::team::{
    bound_team_tools, global_teams, load_tasks, register_spawn, MemberStatus, TeamMember,
    TeamState, TeamTaskStatus,
};
use joey_orchestration::{ManagerConfig, SubagentManager};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn unique_team() -> String {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("it-{}-{}", std::process::id(), n)
}

fn with_temp_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
    let _env = ENV_LOCK.lock().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("JOEY_HOME", home.path());
    let out = f(home.path());
    std::env::remove_var("JOEY_HOME");
    out
}

fn enabled_tree() -> joey_core::Config {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), "hypercode:\n  team:\n    enabled: true\n").unwrap();
    joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap()
}

fn member(name: &str) -> TeamMember {
    TeamMember {
        name: name.to_string(),
        role: "implementor".to_string(),
        model: String::new(),
        status: MemberStatus::Idle,
    }
}

// 1. Full registry lifecycle with on-disk round trip.
#[test]
fn registry_lifecycle_on_disk_round_trip() {
    with_temp_home(|home| {
        let tree = enabled_tree();
        let team = unique_team();
        let lead = register_spawn(&tree, &team, None, "obj", "explorer").unwrap();
        assert_eq!(lead.member, "lead");
        assert!(lead.is_lead);
        let mate = register_spawn(&tree, &team, Some("alice"), "obj", "implementor").unwrap();
        assert_eq!(mate.member, "alice");
        assert!(!mate.is_lead);

        let rec = global_teams().get(&team).expect("team exists");
        let (a, b) = {
            let mut r = rec.lock().unwrap();
            let a = r.add_task("A", vec![]);
            let b = r.add_task("B", vec![a.clone()]);
            r.claim(&a, "alice").unwrap();
            r.complete(&a, true).unwrap();
            r.claim(&b, "alice").unwrap();
            r.send("lead", "alice", "go", 10).unwrap();
            r.send("alice", "lead", "done with A", 10).unwrap();
            (a, b)
        };
        let got = rec.lock().unwrap().receive("alice");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].from, "lead");
        assert_eq!(got[0].content, "go");

        let dir = home.join("teams").join(joey_orchestration::team::sanitize_name(&team));
        let cfg = std::fs::read_to_string(dir.join("config.json")).unwrap();
        assert!(cfg.contains("\"lead\""), "cfg: {cfg}");
        assert!(cfg.contains("\"alice\""), "cfg: {cfg}");
        let tasks_raw = std::fs::read_to_string(dir.join("tasks.json")).unwrap();
        assert!(tasks_raw.contains("\"running\""), "raw: {tasks_raw}");
        assert!(dir.join("inboxes").join("lead.json").exists());

        let round = load_tasks(home, &team).expect("tasks round-trip");
        let ta = round.iter().find(|t| t.id == a).unwrap();
        assert_eq!(ta.status, TeamTaskStatus::Done);
        let tb = round.iter().find(|t| t.id == b).unwrap();
        assert_eq!(tb.status, TeamTaskStatus::Running);

        rec.lock().unwrap().close();
    });
}

// 2. FR-002: teammate-to-teammate messages go direct, no lead involvement.
#[test]
fn teammate_to_teammate_message_direct() {
    with_temp_home(|_home| {
        let team = unique_team();
        let rec = global_teams().create(&team, "obj", "lead").unwrap();
        {
            let mut r = rec.lock().unwrap();
            r.add_member(member("alice"), None).unwrap();
            r.add_member(member("bob"), None).unwrap();
            r.send("alice", "bob", "direct hello", 10).unwrap();
        }
        let got = rec.lock().unwrap().receive("bob");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].from, "alice");
        assert_eq!(got[0].to, "bob");
        assert_eq!(got[0].content, "direct hello");
        rec.lock().unwrap().close();
    });
}

// 3. Exactly one of 8 concurrent claimers wins.
#[test]
fn concurrent_claim_single_winner() {
    with_temp_home(|_home| {
        let tree = enabled_tree();
        let team = unique_team();
        register_spawn(&tree, &team, None, "obj", "explorer").unwrap();
        let rec = global_teams().get(&team).expect("team exists");
        let task_id = {
            let mut r = rec.lock().unwrap();
            for i in 0..8 {
                r.add_member(member(&format!("m{i}")), None).unwrap();
            }
            r.add_task("only", vec![])
        };
        let mut handles = Vec::new();
        for i in 0..8 {
            let rec = rec.clone();
            let tid = task_id.clone();
            handles.push(std::thread::spawn(move || {
                rec.lock().unwrap().claim(&tid, &format!("m{i}"))
            }));
        }
        let mut ok = 0;
        let mut errs = 0;
        for h in handles {
            match h.join().unwrap() {
                Ok(_) => ok += 1,
                Err(e) => {
                    assert_eq!(e, "task not claimable");
                    errs += 1;
                }
            }
        }
        assert_eq!(ok, 1);
        assert_eq!(errs, 7);
        rec.lock().unwrap().close();
    });
}

// 4. B (dep A) is not claimable until A is Done.
#[test]
fn dependency_blocking_integration() {
    with_temp_home(|_home| {
        let team = unique_team();
        let rec = global_teams().create(&team, "obj", "lead").unwrap();
        let mut r = rec.lock().unwrap();
        r.add_member(member("m1"), None).unwrap();
        let a = r.add_task("A", vec![]);
        let b = r.add_task("B", vec![a.clone()]);
        let err = r.claim(&b, "m1").unwrap_err();
        assert_eq!(err, "task not claimable");
        r.claim(&a, "m1").unwrap();
        r.complete(&a, true).unwrap();
        assert!(r.claim(&b, "m1").is_ok());
        r.close();
    });
}

// 5. Mailbox drops the OLDEST messages beyond the limit.
#[test]
fn mailbox_drop_oldest_integration() {
    with_temp_home(|_home| {
        let team = unique_team();
        let rec = global_teams().create(&team, "obj", "lead").unwrap();
        let mut r = rec.lock().unwrap();
        r.add_member(member("alice"), None).unwrap();
        r.add_member(member("bob"), None).unwrap();
        for i in 0..12 {
            r.send("alice", "bob", &format!("msg-{i}"), 10).unwrap();
        }
        let got = r.poll("bob");
        assert_eq!(got.len(), 10);
        let contents: Vec<&str> = got.iter().map(|m| m.content.as_str()).collect();
        assert!(!contents.contains(&"msg-0"));
        assert!(!contents.contains(&"msg-1"));
        assert!(contents.contains(&"msg-11"));
        r.close();
    });
}

// 6. Toolset resolution for team children: exactly the three team tools,
// never delegate_task for teammates; lead detection via register_spawn.
#[test]
fn toolset_resolution_team_children() {
    // resolve_toolset("team") -> exactly the three team tools.
    let resolved = joey_tools::resolve_toolset("team");
    let mut names: Vec<&str> = resolved.iter().map(|s| s.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["team_message", "team_status", "team_tasks"]);

    with_temp_home(|_home| {
        let tree = enabled_tree();
        let team = unique_team();
        let lead = register_spawn(&tree, &team, None, "obj", "explorer").unwrap();
        assert!(lead.is_lead);
        let tools = bound_team_tools(&team, "alice", 10);
        let mut tool_names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        tool_names.sort_unstable();
        assert_eq!(tool_names, vec!["team_message", "team_status", "team_tasks"]);
        assert!(!tool_names.contains(&"delegate_task"), "FR-010: teammates never get delegate_task");
        let mate = register_spawn(&tree, &team, Some("w1"), "obj", "implementor").unwrap();
        assert!(!mate.is_lead);
        global_teams().get(&team).unwrap().lock().unwrap().close();
    });
}

// 7. close_all winds down + closes every active team; tasks.json retained.
#[test]
fn close_all_stops_active_teams() {
    with_temp_home(|home| {
        let tree = enabled_tree();
        let team = unique_team();
        register_spawn(&tree, &team, None, "obj", "explorer").unwrap();
        let rec = global_teams().get(&team).expect("team exists");
        let task_id = {
            let mut r = rec.lock().unwrap();
            let id = r.add_task("only", vec![]);
            r.claim(&id, "lead").unwrap();
            id
        };
        let mgr = SubagentManager::new(ManagerConfig::default());
        global_teams().close_all(&mgr);
        {
            let r = rec.lock().unwrap();
            assert_eq!(r.team.state, TeamState::Closed);
            let t = r.tasks.iter().find(|t| t.id == task_id).unwrap();
            assert_eq!(t.status, TeamTaskStatus::Pending);
        }
        let dir = home.join("teams").join(joey_orchestration::team::sanitize_name(&team));
        assert!(dir.join("tasks.json").exists());
    });
}
