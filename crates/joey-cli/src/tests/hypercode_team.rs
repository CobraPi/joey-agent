use crate::hypercode::{
    execution_hint_from_graph, format_mode_decision, plan_team_seed, route_mode, try_team_run,
    ModeRoute, OmoRoleDefaults, TeamConfig,
};

use std::sync::Mutex;

static TEAM_ENV_LOCK: Mutex<()> = Mutex::new(());

fn config_from_yaml(yaml: &str) -> joey_core::Config {
    // unique temp file under std temp dir (no new deps)
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("joey-team-cfg-{}-{n}.yaml", std::process::id()));
    std::fs::write(&path, yaml).unwrap();
    let cfg = joey_core::Config::load_from(path.clone()).unwrap();
    let _ = std::fs::remove_file(&path);
    cfg
}

#[test]
fn team_config_defaults() {
    let parsed = TeamConfig::from_config(&joey_core::Config::defaults());
    let default = TeamConfig::default();
    assert_eq!(parsed, default);
    assert!(!parsed.enabled);
    assert_eq!(parsed.lead_model, "");
    assert_eq!(parsed.max_members, 8);
    assert_eq!(parsed.max_parallel_members, 4);
    assert_eq!(parsed.message_limit, 10);
    assert_eq!(parsed.poll_interval_ms, 500);
    assert_eq!(parsed.cleanup_days, 7);
}

#[test]
fn team_config_overrides() {
    let yaml = "hypercode:\n  team:\n    enabled: true\n    lead_model: \"glm-4.7\"\n    max_members: 12\n    max_parallel_members: 6\n    message_limit: 5\n    poll_interval_ms: 250\n    cleanup_days: 3\n";
    let cfg = config_from_yaml(yaml);
    let parsed = TeamConfig::from_config(&cfg);
    assert!(parsed.enabled);
    assert_eq!(parsed.lead_model, "glm-4.7");
    assert_eq!(parsed.max_members, 12);
    assert_eq!(parsed.max_parallel_members, 6);
    assert_eq!(parsed.message_limit, 5);
    assert_eq!(parsed.poll_interval_ms, 250);
    assert_eq!(parsed.cleanup_days, 3);
}

#[test]
fn mode_decision_format() {
    assert_eq!(
        format_mode_decision("team", "research docs", "independent parts"),
        "mode=team task=research docs rationale=independent parts"
    );
}

#[test]
fn sc005_disabled_parity_routes_subagent() {
    assert_eq!(route_mode(false, false, 10), ModeRoute::Subagent);
    assert_eq!(route_mode(false, true, 5), ModeRoute::Subagent);
}

#[test]
fn route_team_only_for_independent_decomposition() {
    assert_eq!(route_mode(true, false, 3), ModeRoute::Team);
    assert_eq!(route_mode(true, false, 1), ModeRoute::Subagent);
    // explicit workstreams keep the pipeline shape
    assert_eq!(route_mode(true, true, 3), ModeRoute::Subagent);
}

#[test]
fn overlay_documents_mode_guidance() {
    let overlay = crate::hypercode::orchestrator_overlay();
    assert!(overlay.contains("## Execution Modes (feature 022: agent teams)"));
    assert!(overlay.contains("prefer the cheaper mode"));
    assert!(overlay.contains("one active team per session"));
}

#[test]
fn team_slug_sanitizes_goal() {
    use crate::hypercode::team_slug;
    // take(24) = "Research agent-team patt" → lowercase → sanitize
    // (space→'_', hyphen kept) → trim('_') is a no-op → expected slug.
    assert_eq!(
        team_slug("Research agent-team patterns!! AND independently implement"),
        "hc-research_agent-team_patt"
    );
    assert_eq!(team_slug("  "), "hc-");
}

#[test]
fn route_mode_is_the_team_gate() {
    // belt-and-braces next to the branch
    use crate::hypercode::{route_mode, ModeRoute};
    assert_eq!(route_mode(true, false, 2), ModeRoute::Team);
    assert_eq!(route_mode(true, false, 1), ModeRoute::Subagent);
    assert_eq!(route_mode(false, false, 5), ModeRoute::Subagent);
    assert_eq!(route_mode(true, true, 5), ModeRoute::Subagent);
}

#[test]
fn sc001_mode_selection_score() {
    // SC-001 fixed evaluation set: 10 canonical tasks (5 team-suited:
    // independent parallelizable parts; 5 subagent-suited: sequential /
    // same-file / single-focus). Scored against the routing decision
    // point (route_mode on the planner decomposition), research.md D5.
    let team_suited: &[usize] = &[2, 3, 4, 5, 2]; // decomposition counts
    let subagent_suited: &[usize] = &[1, 1, 1, 1, 1];
    let mut correct = 0;
    for &c in team_suited {
        if crate::hypercode::route_mode(true, false, c) == crate::hypercode::ModeRoute::Team { correct += 1; }
    }
    for &c in subagent_suited {
        if crate::hypercode::route_mode(true, false, c) == crate::hypercode::ModeRoute::Subagent { correct += 1; }
    }
    assert!(correct >= 9, "SC-001: scored {correct}/10, bar is 9");
}

#[test]
fn lead_request_inherits_orchestrator_model_when_unset_per_fr019() {
    // T024 / FR-019: empty hypercode.team.lead_model => the lead request
    // carries no model override and inherits the orchestrator's effective
    // model at dispatch; a pinned lead_model is forwarded verbatim.
    let cfg = TeamConfig::default();
    let req = crate::hypercode::lead_request("objective text", "team-a", "lead", &cfg, &OmoRoleDefaults::default());
    assert!(req.model.is_none(), "empty lead_model must leave model unset (inherit)");
    assert_eq!(req.team.as_deref(), Some("team-a"));
    assert_eq!(req.name.as_deref(), Some("lead"));
    assert!(req.toolsets.iter().any(|t| t == "team"));
    assert!(req.prompt_append.as_deref().unwrap().contains("LEAD"));

    let mut pinned = cfg.clone();
    pinned.lead_model = "glm-4.7".to_string();
    let req = crate::hypercode::lead_request("objective text", "team-a", "lead", &pinned, &OmoRoleDefaults::default());
    assert_eq!(req.model.as_deref(), Some("glm-4.7"));
}

#[test]
fn refused_second_team_records_subagent_decision_per_fr018() {
    // T026 / FR-018 + edge case 4: while a team is active, a second team
    // start is refused; the refusal surfaces as a subagent mode decision
    // (fall back to subagents, never queue behind the team).
    let _g = TEAM_ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!("joey-t026-{}-{}", std::process::id(), std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let prev = std::env::var("JOEY_HOME").ok();
    std::env::set_var("JOEY_HOME", &home);
    let cfg = config_from_yaml("hypercode:\n  team:\n    enabled: true\n");
    // First team starts fine (2 independent workstreams).
    let first = crate::hypercode::try_team_run(&cfg, "refusal fixture one", true, false, 2, None);
    assert!(matches!(first, Ok(Some(_))), "first team must start: {first:?}");
    let (team_name, _member) = first.unwrap().unwrap();
    // Second team (different objective) is refused while the first is active.
    let second = crate::hypercode::try_team_run(&cfg, "refusal fixture two", true, false, 3, None);
    match second {
        Err(e) => {
            assert!(e.contains("one active team per session"), "refusal reason: {e}");
            let decision = crate::hypercode::format_mode_decision(
                "subagent",
                "refusal fixture two",
                &format!("team start refused ({e}); ran via subagents"),
            );
            assert!(decision.starts_with("mode=subagent"));
            assert!(decision.contains("team start refused"));
        }
        other => panic!("second team must be refused, got {other:?}"),
    }
    // Non-team routing never touches the registry (Ok(None)).
    assert!(matches!(
        crate::hypercode::try_team_run(&cfg, "single stream", true, false, 1, None),
        Ok(None)
    ));
    // Cleanup: close the record, restore env, remove temp home.
    if let Some(rec) = joey_orchestration::team::global_teams().get(&team_name) {
        rec.lock().unwrap().close();
    }
    match prev {
        Some(v) => std::env::set_var("JOEY_HOME", v),
        None => std::env::remove_var("JOEY_HOME"),
    }
    let _ = std::fs::remove_dir_all(&home);
}

// ---- T026: graph-derived routing + team pre-seeding (FR-023/FR-024) ----

use joey_orchestration::evaluator::VerificationPlanView;
use joey_orchestration::task_graph::{
    default_isolation, AcceptanceCriterion, LegacyWorkstream, ModelTier, RiskLevel, TaskGraph,
    TaskId, TaskNode, TaskStatus, WorkerRole,
};

fn tid(s: &str) -> TaskId {
    TaskId::new(s).unwrap()
}

/// Mirror of `TaskGraph::from_workstreams` node construction (task_graph.rs
/// ~872-920) for direct struct fixtures.
fn gnode(id_str: &str, deps: &[&str], read: &[&str], write: &[&str]) -> TaskNode {
    TaskNode {
        id: tid(id_str),
        objective: format!("do {}", id_str),
        dependencies: deps.iter().map(|d| tid(d)).collect(),
        read_set: read.iter().map(std::path::PathBuf::from).collect(),
        write_set: write.iter().map(std::path::PathBuf::from).collect(),
        artifact_ids: vec![],
        role: WorkerRole::Implementor,
        model_tier: ModelTier::Economical,
        risk: RiskLevel::Low,
        acceptance: vec![AcceptanceCriterion {
            criterion: format!("Workstream delivered: {}", id_str),
            kind: "manual".to_string(),
        }],
        verification: VerificationPlanView::default(),
        isolation: default_isolation(&write.iter().map(std::path::PathBuf::from).collect::<Vec<_>>()),
        status: TaskStatus::Pending,
        attempts: 0,
    }
}

fn graph_of(nodes: Vec<TaskNode>) -> TaskGraph {
    TaskGraph {
        nodes: nodes.into_iter().map(|n| (n.id.clone(), n)).collect(),
        baseline_revision: "x".into(),
        run_id: String::new(),
    }
}

#[test]
fn hint_flags_write_overlap_even_when_sequenced() {
    // a writes src/one.rs; b writes src/one.rs and depends on a — valid
    // under WRITE_OVERLAP (ancestor-sequenced) but the routing hint is
    // deliberately broader: any overlap => single worker.
    let g = graph_of(vec![
        gnode("a", &[], &[], &["src/one.rs"]),
        gnode("b", &["a"], &[], &["src/one.rs"]),
    ]);
    assert!(execution_hint_from_graph(&g).write_overlap);
}

#[test]
fn hint_depth_counts_chain_in_nodes() {
    // Chain a→b→c (c dep b, b dep a): longest chain is 3 NODES.
    let g = graph_of(vec![
        gnode("a", &[], &[], &[]),
        gnode("b", &["a"], &[], &[]),
        gnode("c", &["b"], &[], &[]),
    ]);
    let hint = execution_hint_from_graph(&g);
    assert_eq!(hint.strict_dependency_depth, 3);
    assert_eq!(hint.independent_components, 1);
}

#[test]
fn hint_counts_independent_components() {
    // Three tasks, no deps, empty sets: three independent components.
    let g = TaskGraph::from_workstreams(
        &[
            LegacyWorkstream { id: "1".into(), focus: "one".into() },
            LegacyWorkstream { id: "2".into(), focus: "two".into() },
            LegacyWorkstream { id: "3".into(), focus: "three".into() },
        ],
        "deadbeef",
    );
    let hint = execution_hint_from_graph(&g);
    assert_eq!(hint.independent_components, 3);
    assert!(!hint.write_overlap);
    assert!(!hint.cross_component_coordination);
}

#[test]
fn hint_detects_cross_component_coordination() {
    // a (component 1) writes src/lib.rs; b (component 2, no dep on a)
    // reads src/lib.rs — independent but needs coordination.
    let g = graph_of(vec![
        gnode("a", &[], &[], &["src/lib.rs"]),
        gnode("b", &[], &["src/lib.rs"], &["src/other.rs"]),
    ]);
    let hint = execution_hint_from_graph(&g);
    assert!(hint.cross_component_coordination);
    assert_eq!(hint.independent_components, 2);
}

#[test]
fn hint_empty_graph_is_all_zero() {
    let g = TaskGraph {
        nodes: std::collections::BTreeMap::new(),
        baseline_revision: "x".into(),
        run_id: String::new(),
    };
    let hint = execution_hint_from_graph(&g);
    assert_eq!(hint.write_overlap, false);
    assert_eq!(hint.strict_dependency_depth, 0);
    assert_eq!(hint.independent_components, 0);
    assert_eq!(hint.cross_component_coordination, false);
}

#[test]
fn plan_team_seed_orders_topologically_and_translates_deps() {
    // Diamond: a; b dep a; c dep a; d dep b,c.
    let g = graph_of(vec![
        gnode("a", &[], &[], &[]),
        gnode("b", &["a"], &[], &[]),
        gnode("c", &["a"], &[], &[]),
        gnode("d", &["b", "c"], &[], &[]),
    ]);
    let plan = plan_team_seed(&g);
    assert_eq!(plan.len(), 4);
    assert_eq!(plan[0].graph_id, "a");
    assert_eq!(plan[1].graph_id, "b");
    assert_eq!(plan[2].graph_id, "c");
    assert_eq!(plan[3].graph_id, "d");
    assert_eq!(plan[3].dependencies, vec!["b".to_string(), "c".to_string()]);
    // Every dependency references an EARLIER item's graph_id.
    for (i, item) in plan.iter().enumerate() {
        for dep in &item.dependencies {
            assert!(
                plan[..i].iter().any(|p| &p.graph_id == dep),
                "dep {dep} of item {i} must reference an earlier item"
            );
        }
    }
}

#[test]
fn try_team_run_honors_graph_route() {
    let _g = TEAM_ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!(
        "joey-t026-graph-route-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let prev = std::env::var("JOEY_HOME").ok();
    std::env::set_var("JOEY_HOME", &home);
    let cfg = config_from_yaml("hypercode:\n  team:\n    enabled: true\n");

    // Unique goal per run so the team name never collides.
    let goal = format!("graph route fixture {}", std::process::id());

    // (i) graph-routed Team with count 0 still starts a team.
    let first = try_team_run(&cfg, &goal, true, false, 0, Some(ModeRoute::Team));
    let (team_name, _member) = first.unwrap().unwrap();
    // Cleanup (existing pattern): close the record in the same test.
    joey_orchestration::team::global_teams()
        .get(&team_name)
        .unwrap()
        .lock()
        .unwrap()
        .close();

    // (ii) non-Team graph route => Ok(None) even when eligible by count.
    assert!(matches!(
        try_team_run(&cfg, &goal, true, false, 5, Some(ModeRoute::ParallelSubagents)),
        Ok(None)
    ));
    // (iii) Team graph route but explicit workstreams => Ok(None).
    assert!(matches!(
        try_team_run(&cfg, &goal, true, true, 5, Some(ModeRoute::Team)),
        Ok(None)
    ));
    // (iv) legacy path (None) with team disabled => Ok(None).
    assert!(matches!(
        try_team_run(&cfg, &goal, false, false, 5, None),
        Ok(None)
    ));

    match prev {
        Some(v) => std::env::set_var("JOEY_HOME", v),
        None => std::env::remove_var("JOEY_HOME"),
    }
    let _ = std::fs::remove_dir_all(&home);
}
