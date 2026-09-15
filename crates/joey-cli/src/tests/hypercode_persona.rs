//! HyperCode orchestrator prompt + toolset invariants, role-model
//! precedence, and session-model selection tests.
//!
//! Covers the fixed ORCHESTRATOR_PROMPT roles-only surface, the restricted
//! orchestrator toolset, two-level role-model precedence (explicit role
//! table > parent inheritance), lead-model precedence, and the orchestrator
//! session model following the user-selected model.

fn config_with(yaml: &str) -> joey_core::Config {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), yaml).unwrap();
    joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap()
}

fn avail(models: &[&str]) -> joey_omo::AvailableModelSet {
    joey_omo::AvailableModelSet::from_models(models.iter().map(|s| s.to_string()).collect::<Vec<_>>())
}

fn overrides() -> joey_omo::agents::registry::ModelOverrides {
    joey_omo::agents::registry::ModelOverrides::new()
}

/// Post-revision: the fixed ORCHESTRATOR_PROMPT fallback must not
/// advertise named-agent routing the tool now rejects.
#[test]
fn fixed_orchestrator_prompt_is_roles_only() {
    let p = crate::hypercode::ORCHESTRATOR_PROMPT;
    assert!(p.contains("role:\"explorer\""), "explorer role advertised");
    assert!(p.contains("role:\"implementor\""), "implementor role advertised");
    assert!(p.contains("COMPLETE bench"), "complete-bench wording present");
    assert!(!p.contains("subagent_type:"), "must not advertise subagent_type: routing");
    assert!(!p.contains("sisyphus"), "must not advertise named agents");
}

/// Spec edge: the orchestrator toolset restriction is overlay-independent.
#[test]
fn orchestrator_toolset_stays_restricted_under_overlay() {
    let tools = crate::hypercode::orchestrator_tool_names();
    assert!(tools.contains(&"delegate_task".to_string()));
    assert!(!tools.contains(&"write_file".to_string()));
    assert!(!tools.iter().any(|t| t.contains("patch")));
}

// ── User Story 3 (T013/T015/T016): role-to-agent model mapping ──────

use crate::hypercode::{implementor_request, explorer_request};

fn hc_opts() -> crate::hypercode::HypercodeOptions {
    crate::hypercode::HypercodeOptions {
        provider: "prov".into(),
        ..Default::default()
    }
}

fn ws0() -> crate::hypercode::Workstream {
    crate::hypercode::Workstream {
        id: 0,
        focus: "f".into(),
    }
}

/// Two-level precedence: explicit role-table model > parent inheritance.
#[test]
fn role_model_precedence_override_inherit() {
    let opts = hc_opts();
    let ws = ws0();

    // (a) Explicit per-role configuration always wins.
    let mut cfg = crate::hypercode::HyperCodeConfig::default();
    cfg.set_explorer_config(
        "prov".into(),
        crate::hypercode::RoleConfig {
            model: "custom-explorer".into(),
            ..Default::default()
        },
    );
    cfg.set_implementor_config(
        "prov".into(),
        crate::hypercode::RoleConfig {
            model: "custom-impl".into(),
            ..Default::default()
        },
    );
    let ex = explorer_request(&ws, "g", &cfg, &opts, "parent-model", std::path::Path::new("/tmp"));
    assert_eq!(ex.model.as_deref(), Some("custom-explorer"), "role table wins over parent inheritance");
    let im = implementor_request(&ws, "g", "brief", &cfg, &opts, "parent-model", std::path::Path::new("/tmp"));
    assert_eq!(im.model.as_deref(), Some("custom-impl"), "role table wins over parent inheritance");

    // (b) Empty role tables → inherit the parent model.
    let cfg = crate::hypercode::HyperCodeConfig::default();
    let ex = explorer_request(&ws, "g", &cfg, &opts, "parent-model", std::path::Path::new("/tmp"));
    assert_eq!(ex.model.as_deref(), Some("parent-model"), "empty role tables → parent inheritance");
    let im = implementor_request(&ws, "g", "brief", &cfg, &opts, "parent-model", std::path::Path::new("/tmp"));
    assert_eq!(im.model.as_deref(), Some("parent-model"), "empty role tables → parent inheritance");
}

/// lead_request model precedence: explicit cfg.lead_model wins; else
/// None (inherit the orchestrator's model at dispatch).
#[test]
fn lead_request_model_precedence() {
    use crate::hypercode::{lead_request, TeamConfig};

    // (a) Explicit lead_model is forwarded verbatim.
    let cfg = TeamConfig {
        lead_model: "custom".to_string(),
        ..Default::default()
    };
    let req = lead_request("goal", "team-a", "lead", &cfg);
    assert_eq!(req.model.as_deref(), Some("custom"), "explicit lead_model wins");

    // (b) Empty lead_model ⇒ None (inherit).
    let cfg = TeamConfig::default();
    let req = lead_request("goal", "team-a", "lead", &cfg);
    assert!(req.model.is_none(), "empty lead_model ⇒ inherit");
}

/// The orchestrator is the MAIN session agent and always runs on the
/// currently selected model: build_agent_config must never swap it for an
/// OMO-derived model, whatever the provider (zai, copilot, ai-usage-hud...).
#[test]
fn orchestrator_model_follows_selected_model() {
    let yaml = "model:\n  provider: zai\n  default: user-chosen\nhypercode:\n  enabled: true\n  orchestrator_mode: true\n";
    let cfg = crate::repl::build_agent_config(&config_with(yaml), &crate::repl::Overrides::default());
    assert_eq!(cfg.model, "user-chosen", "selected model must be kept verbatim");
    let pinned = crate::repl::build_agent_config(
        &config_with(yaml),
        &crate::repl::Overrides { model: Some("pinned-model".into()), ..Default::default() },
    );
    assert_eq!(pinned.model, "pinned-model");
    assert!(crate::hypercode::ORCHESTRATOR_PROMPT.contains("ORCHESTRATOR"));
}

// ── T030 (US2/AC1): named delegations carry the agent's identity ──────

/// Named subagent_type resolution rides the agent's identity prompt through
/// prompt_append (production resolver, real registry).
#[test]
fn named_delegation_carries_agent_identity() {
    let resolver = crate::omo_resolver::OmoCategoryResolver::new();
    resolver.populate(joey_omo::AgentRegistry::build(avail(&["glm-5.2"]), &overrides()));
    use joey_orchestration::CategoryResolver;

    // sisyphus resolves glm-5.2 exactly and carries its identity text.
    let r = resolver.resolve_subagent_type("sisyphus").expect("sisyphus resolves");
    assert_eq!(r.model, "glm-5.2");
    assert!(
        r.prompt_append.as_ref().is_some_and(|p| p.contains("Sisyphus")),
        "identity text must ride prompt_append, got: {:?}",
        r.prompt_append.as_deref().map(|p| p.chars().take(80).collect::<String>())
    );

    // explore: assert just Some + non-empty model + non-empty append (avoid
    // text-marker drift on non-primary agent identities).
    let r = resolver.resolve_subagent_type("explore").expect("explore resolves");
    assert!(!r.model.is_empty());
    assert!(
        r.prompt_append.as_ref().is_some_and(|p| !p.is_empty()),
        "explore must carry a non-empty identity append"
    );

    // Unknown names still resolve to None.
    assert!(resolver.resolve_subagent_type("bogus").is_none());
}

// ── Workflow inheritance: skills + todo + task-graph on the orchestrator ──

/// The orchestrator toolset inherits the main agent's workflow toolsets:
/// todo, skills, and task-graph (const-level; task-graph resolution to a
/// `task_graph` tool name lands in a parallel change, so the resolved-name
/// assertion here only covers the stable entries).
#[test]
fn orchestrator_toolset_inherits_workflow_tools() {
    for ts in ["todo", "skills", "task-graph"] {
        assert!(
            crate::hypercode::ORCHESTRATOR_TOOLSET.iter().any(|t| *t == ts),
            "ORCHESTRATOR_TOOLSET must contain {ts}"
        );
    }
    let tools = crate::hypercode::orchestrator_tool_names();
    assert!(tools.contains(&"todo".to_string()), "resolved: todo tool");
    assert!(
        tools.contains(&"skills_list".to_string()),
        "resolved: skills_list tool"
    );
}

/// Inheritance must not widen the surface: delegation stays, direct file
/// mutation stays out.
#[test]
fn orchestrator_toolset_still_restricted() {
    let tools = crate::hypercode::orchestrator_tool_names();
    assert!(tools.contains(&"delegate_task".to_string()));
    assert!(!tools.contains(&"write_file".to_string()));
    assert!(!tools.iter().any(|t| t.contains("patch")));
}

/// The fixed orchestrator prompt carries the workflow-inheritance section.
#[test]
fn workflow_inheritance_guidance_in_fixed_prompt() {
    let prompt = crate::hypercode::ORCHESTRATOR_PROMPT;
    assert!(prompt.contains("## Workflow inheritance"));
    assert!(prompt.contains("skill_view(name)"));
    assert!(prompt.contains("task_graph"));
    assert!(prompt.contains("todo tool"));
}

// ── Strict-schema content in the fixed orchestrator prompt ──────────

/// The TASK GRAPH bullet carries the strict task_graph wire schema so
/// orchestrators get the graph right on the first call.
#[test]
fn task_graph_guidance_carries_strict_schema() {
    let g = crate::hypercode::ORCHESTRATOR_PROMPT;
    for needle in ["STRICT SCHEMA", "joey-taskgraph/1", "artifact_ids", "\"economical\"|\"frontier\"", "risk_triggered_review"] {
        assert!(g.contains(needle), "guidance must mention {needle}");
    }
}
