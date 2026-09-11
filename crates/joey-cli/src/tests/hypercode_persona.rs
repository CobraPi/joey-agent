//! Feature 025 (User Story 1 + 4): persona-aware orchestrator overlay tests.
//!
//! Covers the delegation-first Conductor persona (FR-001), the activation
//! gate (orchestration off → fixed prompt; empty bench → fixed prompt,
//! FR-012), named-agent personas with appended hard rules + roster briefing
//! (FR-002/FR-007/FR-010), variant reselection per model family (SC-004),
//! persona-distinctness across agents, and toolset restriction being
//! persona-independent.

fn config_with(yaml: &str) -> joey_core::Config {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), yaml).unwrap();
    joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap()
}

fn hc_on() -> joey_core::Config {
    config_with("model:\n  provider: zai\n  default: glm-4.5-flash\nhypercode:\n  enabled: true\n  orchestrator_mode: true\n")
}

fn avail(models: &[&str]) -> joey_omo::AvailableModelSet {
    joey_omo::AvailableModelSet::from_models(models.iter().map(|s| s.to_string()).collect::<Vec<_>>())
}

fn overrides() -> joey_omo::agents::registry::ModelOverrides {
    joey_omo::agents::registry::ModelOverrides::new()
}

/// SC-002: the default persona is the delegation-first Conductor.
#[test]
fn default_persona_is_delegation_first() {
    let overlay = crate::hypercode::orchestrator_persona_overlay(
        &hc_on(),
        None,
        "glm-5.2",
        &avail(&["glm-5.2"]),
        &overrides(),
    );
    assert!(overlay.contains("Conductor"), "default persona identity");
    assert!(overlay.contains("DELEGATE"), "delegation-first doctrine");
    assert!(overlay.contains("PARALLEL"), "parallel-by-default doctrine");
    assert!(
        overlay.contains("NEVER write, patch, or delete files yourself"),
        "hard no-direct-writes rule"
    );
    assert!(!overlay.contains("{HARD_RULES}"), "no unexpanded placeholder");
}

/// Post-FR-010 revision: the conductor overlay advertises ONLY the two
/// HyperCode roles — no subagent_type / named-agent delegation surface.
#[test]
fn conductor_overlay_advertises_only_hypercode_roles() {
    let overlay = crate::hypercode::orchestrator_persona_overlay(
        &hc_on(),
        None,
        "glm-5.2",
        &avail(&["glm-5.2"]),
        &overrides(),
    );
    assert!(
        overlay.contains("role:\"explorer\""),
        "overlay must advertise role:\"explorer\""
    );
    assert!(
        overlay.contains("role:\"implementor\""),
        "overlay must advertise role:\"implementor\""
    );
    assert!(
        !overlay.contains("subagent_type"),
        "overlay must not advertise subagent_type delegation"
    );
}

/// FR-012: with the integration inactive (either flag off), the overlay is
/// byte-identical to the fixed ORCHESTRATOR_PROMPT.
#[test]
fn inactive_integration_keeps_fixed_prompt() {
    for yaml in [
        "model:\n  provider: zai\n  default: glm-4.5-flash\nhypercode:\n  enabled: true\n  orchestrator_mode: false\n",
        "model:\n  provider: zai\n  default: glm-4.5-flash\nhypercode:\n  enabled: false\n  orchestrator_mode: true\n",
    ] {
        let config = config_with(yaml);
        let overlay = crate::hypercode::orchestrator_persona_overlay(
            &config,
            None,
            "glm-5.2",
            &avail(&["glm-5.2"]),
            &overrides(),
        );
        assert_eq!(
            overlay,
            crate::hypercode::ORCHESTRATOR_PROMPT.to_string(),
            "inactive integration must keep the fixed prompt byte-identical"
        );
    }
}

/// FR-012 spec edge: an empty OMO registry degrades to the fixed prompt.
#[test]
fn empty_registry_degrades_to_fixed_prompt() {
    let overlay = crate::hypercode::orchestrator_persona_overlay(
        &hc_on(),
        None,
        "glm-5.2",
        &avail(&[]),
        &overrides(),
    );
    assert_eq!(overlay, crate::hypercode::ORCHESTRATOR_PROMPT.to_string());
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

/// Spec edge: the orchestrator toolset restriction is persona-independent.
#[test]
fn orchestrator_toolset_stays_restricted_under_any_persona() {
    let tools = crate::hypercode::orchestrator_tool_names();
    assert!(tools.contains(&"delegate_task".to_string()));
    assert!(!tools.contains(&"write_file".to_string()));
    assert!(!tools.iter().any(|t| t.contains("patch")));
}

/// FR-007: a named agent's persona is used, with the hard rules and roster
/// briefing appended underneath.
#[test]
fn named_agent_persona_used_with_hard_rules() {
    let overlay = crate::hypercode::orchestrator_persona_overlay(
        &hc_on(),
        Some("sisyphus"),
        "glm-5.2",
        &avail(&["glm-5.2"]),
        &overrides(),
    );
    assert!(overlay.contains("Sisyphus"), "named persona identity");
    assert!(
        overlay.contains("NEVER write, patch, or delete files yourself"),
        "hard rules appended under the named persona"
    );
    assert!(overlay.contains("YOUR BENCH"), "roster briefing appended");
}

/// SC-004: the GPT-5.6 conductor variant is selected for 5.6/5-6 models and
/// not for other GPT models.
#[test]
fn gpt_5_6_variant_selected_for_default_persona() {
    let a = crate::hypercode::orchestrator_persona_overlay(
        &hc_on(),
        None,
        "gpt-5.6-sol",
        &avail(&["gpt-5.6-sol"]),
        &overrides(),
    );
    assert!(a.contains("GPT-5.6"));
    let b = crate::hypercode::orchestrator_persona_overlay(
        &hc_on(),
        None,
        "gpt-5-6-high",
        &avail(&["gpt-5.6-sol"]),
        &overrides(),
    );
    assert!(b.contains("GPT-5.6"));
    let c = crate::hypercode::orchestrator_persona_overlay(
        &hc_on(),
        None,
        "gpt-5.4",
        &avail(&["gpt-5.6-sol"]),
        &overrides(),
    );
    assert!(!c.contains("calibrated for GPT-5.6"));
}

/// T018/T019: a model-family change keeps the persona and reselects the
/// family variant.
#[test]
fn model_family_change_keeps_persona_and_reselects_variant() {
    let a = crate::hypercode::orchestrator_persona_overlay(
        &hc_on(),
        Some("atlas"),
        "gpt-5.6-sol",
        &avail(&["gpt-5.6-sol"]),
        &overrides(),
    );
    let b = crate::hypercode::orchestrator_persona_overlay(
        &hc_on(),
        Some("atlas"),
        "claude-opus-4-8",
        &avail(&["gpt-5.6-sol", "claude-opus-4-8"]),
        &overrides(),
    );
    for overlay in [&a, &b] {
        assert!(overlay.contains("Atlas"), "persona kept");
        assert!(
            overlay.contains("NEVER write, patch, or delete files yourself"),
            "hard rules kept"
        );
    }
    assert!(a.contains("GPT-5.6"), "GPT-5.6 variant selected on GPT-5.6");
    assert_ne!(a, b, "variant differs across model families");
}

/// Spec edge: two agents resolving to the same model swap distinct personas
/// (agent switch swaps ONLY the persona).
#[test]
fn same_model_agents_swap_distinct_persona() {
    let s = crate::hypercode::orchestrator_persona_overlay(
        &hc_on(),
        Some("sisyphus"),
        "glm-5.2",
        &avail(&["glm-5.2"]),
        &overrides(),
    );
    let t = crate::hypercode::orchestrator_persona_overlay(
        &hc_on(),
        Some("atlas"),
        "glm-5.2",
        &avail(&["glm-5.2"]),
        &overrides(),
    );
    assert_ne!(s, t);
    assert!(s.contains("Sisyphus"));
    assert!(t.contains("Atlas"));
}

/// SC-003: switching across all four primary agents swaps the persona (all
/// four overlays pairwise distinct, each carrying the rails + bench).
#[test]
fn switch_across_all_primaries_swaps_persona() {
    let mut overlays = Vec::new();
    for name in ["sisyphus", "hephaestus", "prometheus", "atlas"] {
        let overlay = crate::hypercode::orchestrator_persona_overlay(
            &hc_on(),
            Some(name),
            "glm-5.2",
            &avail(&["glm-5.2"]),
            &overrides(),
        );
        assert!(
            overlay.contains("NEVER write, patch, or delete files yourself"),
            "{name} persona keeps the hard rules"
        );
        assert!(
            overlay.contains("YOUR BENCH"),
            "{name} persona keeps the roster briefing"
        );
        overlays.push(overlay);
    }
    for i in 0..overlays.len() {
        for j in (i + 1)..overlays.len() {
            assert_ne!(
                overlays[i], overlays[j],
                "personas for agents {i} and {j} must be distinct"
            );
        }
    }
}

/// Unknown agent names fall back to the Conductor persona.
#[test]
fn unknown_agent_name_falls_back_to_conductor() {
    let overlay = crate::hypercode::orchestrator_persona_overlay(
        &hc_on(),
        Some("bogus"),
        "glm-5.2",
        &avail(&["glm-5.2"]),
        &overrides(),
    );
    assert!(overlay.contains("Conductor"));
}

// ── User Story 3 (T013/T015/T016): role-to-agent model mapping ──────

use crate::hypercode::{implementor_request, explorer_request, OmoRoleDefaults};

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

/// FR-005 three-level precedence, tested through the pub builder surface:
/// explicit role-table model > OMO-chain default > parent inheritance.
#[test]
fn role_model_precedence_override_chain_inherit() {
    let chain = OmoRoleDefaults {
        explorer: Some("chain-explorer".into()),
        implementor: Some("chain-impl".into()),
        ..Default::default()
    };
    let none = OmoRoleDefaults::default();
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
    let ex = explorer_request(&ws, "g", &cfg, &opts, &chain, "parent-model", std::path::Path::new("/tmp"));
    assert_eq!(ex.model.as_deref(), Some("custom-explorer"), "role table wins over chain");
    let im = implementor_request(&ws, "g", "brief", &cfg, &opts, &chain, "parent-model", std::path::Path::new("/tmp"));
    assert_eq!(im.model.as_deref(), Some("custom-impl"), "role table wins over chain");

    // (b) Empty role model + chain default → the chain model applies.
    let cfg = crate::hypercode::HyperCodeConfig::default();
    let ex = explorer_request(&ws, "g", &cfg, &opts, &chain, "parent-model", std::path::Path::new("/tmp"));
    assert_eq!(ex.model.as_deref(), Some("chain-explorer"), "chain default applies when role table empty");
    let im = implementor_request(&ws, "g", "brief", &cfg, &opts, &chain, "parent-model", std::path::Path::new("/tmp"));
    assert_eq!(im.model.as_deref(), Some("chain-impl"), "chain default applies when role table empty");

    // (c) Empty role model + no chain default → inherit the parent (legacy).
    let ex = explorer_request(&ws, "g", &cfg, &opts, &none, "parent-model", std::path::Path::new("/tmp"));
    assert_eq!(ex.model.as_deref(), Some("parent-model"), "no chain → parent inheritance");
    let im = implementor_request(&ws, "g", "brief", &cfg, &opts, &none, "parent-model", std::path::Path::new("/tmp"));
    assert_eq!(im.model.as_deref(), Some("parent-model"), "no chain → parent inheritance");
}

/// FR-005: each role's chain resolves via the first resolvable member
/// (standard AgentRegistry exact-then-family-fuzzy resolution). Pure
/// chain fn — covers the toggle-OFF (legacy) path only; the toggle-ON
/// direct mapping is covered by specialists_on_direct_models.
#[test]
fn omo_role_chain_legacy_off_path() {
    // Orchestrator chain head (sisyphus): claude-opus-4-8 entry resolves
    // family-fuzzy to the available claude-sonnet-4-6.
    let out = crate::hypercode::omo_role_default("orchestrator", &avail(&["claude-sonnet-4-6"]), &overrides());
    assert_eq!(out.model.as_deref(), Some("claude-sonnet-4-6"));
    assert!(!out.unresolvable);

    // Explorer chain (explore → librarian) resolves on a GLM-only bench.
    let out = crate::hypercode::omo_role_default("explorer", &avail(&["glm-5.2"]), &overrides());
    assert!(out.model.is_some(), "explore chain must resolve on glm-5.2");
    assert!(!out.unresolvable);

    // Implementor chain (momus) resolves family-fuzzy on gpt-5.6-sol.
    let out = crate::hypercode::omo_role_default("implementor", &avail(&["gpt-5.6-sol"]), &overrides());
    assert_eq!(out.model.as_deref(), Some("gpt-5.6-sol"));
    assert!(!out.unresolvable);
}

/// FR-006 unresolvable outcome + FR-012 gates on omo_role_defaults.
#[test]
fn omo_role_default_unresolvable_and_gates() {
    // Empty bench: a chain applies but no member resolves.
    let out = crate::hypercode::omo_role_default("explorer", &avail(&[]), &overrides());
    assert!(out.model.is_none());
    assert!(out.unresolvable, "empty bench ⇒ unresolvable (FR-006)");

    // Active orchestration + resolvable bench ⇒ both slots populated.
    // Toggle ON (default): explorer = explore's resolved model;
    // implementor = hephaestus's. On this from_models bench no providers
    // are connected, so hephaestus's requires_provider gate never passes —
    // the implementor slot degrades to the FR-006 warning path instead.
    let d = crate::hypercode::omo_role_defaults(&hc_on(), &avail(&["claude-sonnet-4-6"]));
    assert!(d.explorer.is_some(), "explorer chain resolves");
    assert!(d.implementor.is_none(), "hephaestus is provider-gated off a from_models bench");
    assert!(
        d.warnings.iter().any(|w| w.contains("OMO specialist agent 'hephaestus' unresolved")),
        "unresolved hephaestus must warn, got: {:?}",
        d.warnings
    );
    // Atlas resolves family-fuzzy to claude-sonnet-4-6 ⇒ orchestrator slot set.
    assert_eq!(d.orchestrator.as_deref(), Some("claude-sonnet-4-6"));

    // Gate: orchestrator_mode off ⇒ no derivation at all, even with models.
    let off = config_with(
        "model:\n  provider: zai\n  default: glm-4.5-flash\nhypercode:\n  enabled: true\n  orchestrator_mode: false\n",
    );
    let d = crate::hypercode::omo_role_defaults(&off, &avail(&["claude-sonnet-4-6"]));
    assert_eq!(d, OmoRoleDefaults::default(), "gate holds though models are available");

    // Empty bench ⇒ silent degrade to all-None.
    let d = crate::hypercode::omo_role_defaults(&hc_on(), &avail(&[]));
    assert_eq!(d, OmoRoleDefaults::default(), "empty bench degrades silently");
}

/// T015: orchestrator session model from OMO + repl gates.
#[test]
fn orchestrator_session_model_from_chain_and_gates() {
    // Toggle ON (default): atlas resolves glm-5.2 (chain entry 4).
    assert_eq!(
        crate::hypercode::omo_orchestrator_session_model(&hc_on(), &avail(&["glm-5.2"])).as_deref(),
        Some("glm-5.2")
    );
    // Toggle OFF: legacy chain head sisyphus resolves glm-5.2 exactly.
    let off_yaml =
        "model:\n  provider: zai\n  default: glm-4.5-flash\nhypercode:\n  enabled: true\n  orchestrator_mode: true\n  omo_specialists:\n    enabled: false\n";
    assert_eq!(
        crate::hypercode::omo_orchestrator_session_model(&config_with(off_yaml), &avail(&["glm-5.2"])).as_deref(),
        Some("glm-5.2")
    );
    // Empty bench ⇒ None (caller keeps the configured model).
    assert!(crate::hypercode::omo_orchestrator_session_model(&hc_on(), &avail(&[])).is_none());

    // hc_on() sets model.default, so use a yaml WITHOUT it for the
    // chain-derived case; assert non-empty (catalog contents vary).
    let chain_yaml =
        "model:\n  provider: zai\nhypercode:\n  enabled: true\n  orchestrator_mode: true\n";
    let cfg = crate::repl::build_agent_config(
        &config_with(chain_yaml),
        &crate::repl::Overrides::default(),
    );
    assert!(!cfg.model.is_empty(), "session model derived from the sisyphus chain");

    // Pinned --model still wins over the chain.
    let cfg = crate::repl::build_agent_config(
        &config_with(chain_yaml),
        &crate::repl::Overrides {
            model: Some("pinned-model".into()),
            ..Default::default()
        },
    );
    assert_eq!(cfg.model, "pinned-model");

    // A user-configured model.default wins over the chain.
    let user_yaml =
        "model:\n  provider: zai\n  default: user-chosen\nhypercode:\n  enabled: true\n  orchestrator_mode: true\n";
    let cfg = crate::repl::build_agent_config(
        &config_with(user_yaml),
        &crate::repl::Overrides::default(),
    );
    assert_eq!(cfg.model, "user-chosen");
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

// ── T031/T032: startup notices + shared session-model gate ───────────

/// T031: empty bench ⇒ the empty-bench degradation notice.
#[test]
fn startup_notice_empty_bench() {
    let notice = crate::hypercode::orchestrator_startup_notice(&hc_on(), false, &avail(&[]));
    let notice = notice.expect("empty bench must produce a notice");
    assert!(notice.contains("no OMO agents resolved"), "got: {notice}");
    assert!(notice.contains("empty bench"), "got: {notice}");
}

/// Healthy bench + no pin/config ⇒ no notice.
#[test]
fn startup_notice_healthy_is_none() {
    // hc_on() sets model.default, so the FR-006 arm is user-configured-off;
    // the empty-bench arm is off because the registry resolves.
    assert_eq!(
        crate::hypercode::orchestrator_startup_notice(&hc_on(), false, &avail(&["glm-5.2"])),
        None
    );
}

/// Orchestration off ⇒ no notice, even with an empty bench.
#[test]
fn startup_notice_off_is_none() {
    let off = config_with(
        "model:\n  provider: zai\n  default: glm-4.5-flash\nhypercode:\n  enabled: true\n  orchestrator_mode: false\n",
    );
    assert_eq!(
        crate::hypercode::orchestrator_startup_notice(&off, false, &avail(&[])),
        None
    );
}

/// T032: a pinned --model or a user-configured model.default suppresses the
/// FR-006 arm (the gate is shared with build_agent_config).
#[test]
fn startup_notice_respects_pin_and_config() {
    // Pinned: no notice on a bench where the chain WOULD otherwise be checked.
    // (hc_on() sets model.default, so build a yaml WITHOUT it to make the pin
    // the deciding factor; glm-5.2 resolves the chain, so no FR-006 text —
    // the assertion is that the pinned gate alone yields None.)
    let chain_yaml =
        "model:\n  provider: zai\nhypercode:\n  enabled: true\n  orchestrator_mode: true\n";
    let cfg = config_with(chain_yaml);
    assert_eq!(
        crate::hypercode::orchestrator_startup_notice(&cfg, true, &avail(&["glm-5.2"])),
        None,
        "pinned model must suppress the session-model notice arm"
    );
    // User-configured model.default: same suppression.
    let user_yaml =
        "model:\n  provider: zai\n  default: user-model\nhypercode:\n  enabled: true\n  orchestrator_mode: true\n";
    let cfg = config_with(user_yaml);
    assert_eq!(
        crate::hypercode::orchestrator_startup_notice(&cfg, false, &avail(&["glm-5.2"])),
        None,
        "user-configured model.default must suppress the session-model notice arm"
    );
}

/// T031 helper: omo_registry_empty mirrors the empty-bench predicate.
#[test]
fn omo_registry_empty_helper() {
    assert!(crate::hypercode::omo_registry_empty(&avail(&[])));
    assert!(!crate::hypercode::omo_registry_empty(&avail(&["glm-5.2"])));
}

/// T031: warnings fill only on unresolvable chains. The
/// unresolvable-with-non-empty-bench case cannot be constructed here — every
/// role family has a member that resolves on a glm-only bench — so this
/// asserts the healthy/off paths and that the field exists and plumbs
/// through Default (the wiring itself is exercised via run_hypercode).
#[test]
fn role_defaults_warnings_filled_only_on_unresolvable() {
    // Healthy (specialists ON, default): explore and atlas resolve on a
    // glm bench; hephaestus cannot (requires_provider gate on a
    // from_models bench) so exactly the hephaestus warning is present.
    let d = crate::hypercode::omo_role_defaults(&hc_on(), &avail(&["glm-5.2"]));
    assert_eq!(d.warnings.len(), 1, "only hephaestus warns, got: {:?}", d.warnings);
    assert!(d.warnings[0].contains("OMO specialist agent 'hephaestus' unresolved"));
    // Toggle OFF: the legacy chains all resolve on glm-5.2 ⇒ no warnings.
    let off = config_with(
        "model:\n  provider: zai\n  default: glm-4.5-flash\nhypercode:\n  enabled: true\n  orchestrator_mode: true\n  omo_specialists:\n    enabled: false\n",
    );
    let d = crate::hypercode::omo_role_defaults(&off, &avail(&["glm-5.2"]));
    assert!(d.warnings.is_empty(), "legacy chains must not warn, got: {:?}", d.warnings);
    // Default construction: warnings empty.
    assert!(OmoRoleDefaults::default().warnings.is_empty());
}

// ── OMO specialists: direct tier→agent model mapping ────────────────

/// hypercode.omo_specialists.enabled defaults to true; explicit false
/// turns it off.
#[test]
fn omo_specialists_toggle_default_and_off() {
    // Key absent ⇒ default true (hc_on() does not set it).
    assert!(crate::hypercode::omo_specialists_enabled(&hc_on()));
    // A minimal config also defaults to true.
    assert!(crate::hypercode::omo_specialists_enabled(&config_with("")));
    // Explicit false ⇒ false.
    let off = config_with(
        "model:\n  provider: zai\n  default: glm-4.5-flash\nhypercode:\n  enabled: true\n  orchestrator_mode: true\n  omo_specialists:\n    enabled: false\n",
    );
    assert!(!crate::hypercode::omo_specialists_enabled(&off));
}

/// Direct tier→agent mapping: explorer→explore, implementor→hephaestus,
/// orchestrator→atlas; anything else → None.
#[test]
fn omo_specialist_for_direct_mapping() {
    assert_eq!(crate::hypercode::omo_specialist_for("explorer"), Some("explore"));
    assert_eq!(crate::hypercode::omo_specialist_for("implementor"), Some("hephaestus"));
    assert_eq!(crate::hypercode::omo_specialist_for("orchestrator"), Some("atlas"));
    assert_eq!(crate::hypercode::omo_specialist_for("bogus"), None);
}

/// Specialists ON: explorer = explore's resolved model, orchestrator =
/// atlas's; implementor = hephaestus's, which can NEVER resolve on a
/// from_models bench (requires_provider gate — no connected providers),
/// so it degrades to the FR-006 warning path.
#[test]
fn specialists_on_direct_models() {
    let d = crate::hypercode::omo_role_defaults(&hc_on(), &avail(&["glm-5.2"]));
    assert_eq!(d.explorer.as_deref(), Some("glm-5.2"), "explore resolves glm-5.2");
    assert_eq!(d.orchestrator.as_deref(), Some("glm-5.2"), "atlas resolves glm-5.2");
    assert!(d.implementor.is_none(), "hephaestus is provider-gated on a from_models bench");
    assert!(
        d.warnings.iter().any(|w| w.contains("OMO specialist agent 'hephaestus' unresolved")),
        "unresolved hephaestus must warn, got: {:?}",
        d.warnings
    );
}

/// Specialists OFF: byte-identical legacy chains — explorer/implementor/
/// orchestrator slots equal the chains' first-resolvable models on the
/// same bench.
#[test]
fn specialists_off_legacy_chains() {
    let off = config_with(
        "model:\n  provider: zai\n  default: glm-4.5-flash\nhypercode:\n  enabled: true\n  orchestrator_mode: true\n  omo_specialists:\n    enabled: false\n",
    );
    let d = crate::hypercode::omo_role_defaults(&off, &avail(&["glm-5.2"]));
    // Legacy chains on glm-5.2: explore chain → glm-5.2, momus → glm-5.2,
    // sisyphus chain → glm-5.2.
    assert_eq!(d.explorer.as_deref(), Some("glm-5.2"));
    assert_eq!(d.implementor.as_deref(), Some("glm-5.2"));
    assert_eq!(d.orchestrator.as_deref(), Some("glm-5.2"));
    assert!(d.warnings.is_empty(), "legacy chains resolve on glm-5.2, got: {:?}", d.warnings);
}

/// lead_request model precedence: explicit cfg.lead_model wins; else
/// omo.orchestrator (atlas under specialists ON); else None (inherit).
#[test]
fn lead_request_model_precedence() {
    use crate::hypercode::{lead_request, TeamConfig};

    // (a) Explicit lead_model wins over omo.orchestrator.
    let mut cfg = TeamConfig::default();
    cfg.lead_model = "custom".to_string();
    let omo = OmoRoleDefaults {
        orchestrator: Some("atlas-model".into()),
        ..Default::default()
    };
    let req = lead_request("goal", "team-a", "lead", &cfg, &omo);
    assert_eq!(req.model.as_deref(), Some("custom"), "explicit lead_model wins");

    // (b) Empty lead_model + orchestrator default ⇒ the default applies.
    let cfg = TeamConfig::default();
    let req = lead_request("goal", "team-a", "lead", &cfg, &omo);
    assert_eq!(req.model.as_deref(), Some("atlas-model"), "omo.orchestrator applies when lead_model empty");

    // (c) Both empty ⇒ None (inherit the orchestrator's model).
    let none = OmoRoleDefaults::default();
    let req = lead_request("goal", "team-a", "lead", &cfg, &none);
    assert!(req.model.is_none(), "both empty ⇒ inherit");
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

/// Every ACTIVE persona overlay carries the workflow-inheritance guidance
/// (default conductor persona AND named-agent personas).
#[test]
fn workflow_inheritance_guidance_in_persona_overlays() {
    let default = crate::hypercode::orchestrator_persona_overlay(
        &hc_on(),
        None,
        "glm-5.2",
        &avail(&["glm-5.2"]),
        &overrides(),
    );
    assert!(
        default.contains("## Workflow inheritance"),
        "default (None) persona overlay must carry the workflow guidance"
    );
    let named = crate::hypercode::orchestrator_persona_overlay(
        &hc_on(),
        Some("sisyphus"),
        "glm-5.2",
        &avail(&["glm-5.2"]),
        &overrides(),
    );
    assert!(
        named.contains("## Workflow inheritance"),
        "named-agent persona overlay must carry the workflow guidance"
    );
}

/// FR-012 stays explicit next to the new section: with the integration
/// inactive, the overlay remains byte-identical to the fixed prompt (which
/// now embeds the workflow-inheritance section).
#[test]
fn inactive_integration_still_byte_identical() {
    let overlay = crate::hypercode::orchestrator_persona_overlay(
        &config_with(
            "model:\n  provider: zai\n  default: glm-4.5-flash\nhypercode:\n  enabled: true\n  orchestrator_mode: false\n",
        ),
        None,
        "glm-5.2",
        &avail(&["glm-5.2"]),
        &overrides(),
    );
    assert_eq!(
        overlay,
        crate::hypercode::ORCHESTRATOR_PROMPT.to_string(),
        "inactive integration must keep the fixed prompt byte-identical"
    );
}

// ── Workflow-inheritance sync guard + strict-schema content ─────────

/// The Workflow-inheritance section embedded in ORCHESTRATOR_PROMPT must
/// stay byte-identical to the WORKFLOW_INHERITANCE_GUIDANCE const (the
/// duplication is deliberate — edit both together).
#[test]
fn workflow_inheritance_embedded_copy_matches_const() {
    assert!(
        crate::hypercode::ORCHESTRATOR_PROMPT.contains(crate::hypercode::WORKFLOW_INHERITANCE_GUIDANCE),
        "the embedded Workflow-inheritance section in ORCHESTRATOR_PROMPT must stay byte-identical to the WORKFLOW_INHERITANCE_GUIDANCE const"
    );
}

/// The TASK GRAPH bullet carries the strict task_graph wire schema so
/// orchestrators get the graph right on the first call.
#[test]
fn task_graph_guidance_carries_strict_schema() {
    let g = crate::hypercode::WORKFLOW_INHERITANCE_GUIDANCE;
    for needle in ["STRICT SCHEMA", "joey-taskgraph/1", "artifact_ids", "\"economical\"|\"frontier\"", "risk_triggered_review"] {
        assert!(g.contains(needle), "guidance must mention {needle}");
    }
}
