use crate::hypercode::{format_mode_decision, route_mode, ModeRoute, TeamConfig};

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
    let req = crate::hypercode::lead_request("objective text", "team-a", "lead", &cfg);
    assert!(req.model.is_none(), "empty lead_model must leave model unset (inherit)");
    assert_eq!(req.team.as_deref(), Some("team-a"));
    assert_eq!(req.name.as_deref(), Some("lead"));
    assert!(req.toolsets.iter().any(|t| t == "team"));
    assert!(req.prompt_append.as_deref().unwrap().contains("LEAD"));

    let mut pinned = cfg.clone();
    pinned.lead_model = "glm-4.7".to_string();
    let req = crate::hypercode::lead_request("objective text", "team-a", "lead", &pinned);
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
    let first = crate::hypercode::try_team_run(&cfg, "refusal fixture one", true, false, 2);
    assert!(matches!(first, Ok(Some(_))), "first team must start: {first:?}");
    let (team_name, _member) = first.unwrap().unwrap();
    // Second team (different objective) is refused while the first is active.
    let second = crate::hypercode::try_team_run(&cfg, "refusal fixture two", true, false, 3);
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
        crate::hypercode::try_team_run(&cfg, "single stream", true, false, 1),
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
