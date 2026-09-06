// Ad-hoc verification: exercise dispatch_system_prompt across all 11 agents
// and 6 model families to confirm variant selection and non-empty output.

#[test]
fn all_agents_all_families_produce_nonempty_prompts() {
    let agents = [
        "sisyphus", "atlas", "hephaestus", "prometheus", "oracle",
        "librarian", "explore", "multimodal-looker", "metis", "momus",
        "sisyphus-junior",
    ];
    let models = [
        ("claude-opus-4-8", "Anthropic"),
        ("gpt-5.6-sol", "Gpt"),
        ("kimi-k3", "Kimi"),
        ("glm-5.2", "Glm"),
        ("gemini-3.1-pro", "Gemini"),
        ("minimax-m3", "Minimax"),
    ];
    for &agent in &agents {
        for &(model, family) in &models {
            let prompt = joey_omo::agents::prompts::dispatch_system_prompt(agent, model);
            assert!(
                prompt.len() > 500,
                "{} + {} ({}): prompt too short ({} chars)",
                agent, model, family, prompt.len()
            );
        }
    }
}

#[test]
fn ultrawork_all_variants_have_mandatory_announcement() {
    let models = ["claude-opus-4-8", "gpt-5.6-sol", "glm-5.2", "gemini-3.1-pro"];
    for &model in &models {
        let prompt = joey_omo::agents::prompts::ultrawork_prompt(model);
        assert!(
            prompt.contains("ULTRAWORK MODE ENABLED!"),
            "ultrawork variant for {} must contain mandatory announcement",
            model
        );
    }
}

#[test]
fn ultrawork_planner_is_doctrine_not_activation() {
    let prompt = joey_omo::agents::prompts::ultrawork::planner();
    assert!(prompt.contains("Planner Doctrine"));
    assert!(!prompt.contains("ULTRAWORK MODE ENABLED!"));
}

#[test]
fn sisyphus_glm_variant_mentions_glm() {
    let prompt = joey_omo::agents::prompts::dispatch_system_prompt("sisyphus", "glm-5.2");
    assert!(prompt.contains("GLM"), "GLM sisyphus must mention GLM");
}

#[test]
fn every_glm_variant_mentions_glm() {
    // Agents that carry an explicit GLM prompt variant must mention GLM when
    // dispatched with a glm-5.2 model. These are the agents whose for_model()
    // now routes ModelFamily::Glm to a dedicated glm()/glm_5_2() function.
    let glm_agents = [
        "sisyphus",
        "sisyphus-junior",
        "atlas",
        "oracle",
        "momus",
        "metis",
        "hephaestus",
    ];
    for &agent in &glm_agents {
        let prompt = joey_omo::agents::prompts::dispatch_system_prompt(agent, "glm-5.2");
        assert!(
            prompt.contains("GLM"),
            "GLM variant for {} must mention GLM",
            agent
        );
    }
}

#[test]
fn glm_variants_contain_calibration_block() {
    // Every explicit GLM variant must carry the GLM 5.2 calibration overlay.
    let glm_agents = [
        "sisyphus",
        "sisyphus-junior",
        "atlas",
        "oracle",
        "momus",
        "metis",
        "hephaestus",
    ];
    for &agent in &glm_agents {
        let prompt = joey_omo::agents::prompts::dispatch_system_prompt(agent, "glm-5.2");
        assert!(
            prompt.contains("LITERAL FOLLOWING") || prompt.contains("glm_5_2_calibration") || prompt.contains("glm_52_calibration"),
            "GLM variant for {} must contain the GLM 5.2 calibration block",
            agent
        );
    }
}

#[test]
fn hephaestus_glm_variant_is_autonomous_worker() {
    let prompt = joey_omo::agents::prompts::dispatch_system_prompt("hephaestus", "glm-5.2");
    assert!(prompt.contains("Hephaestus"), "GLM Hephaestus must identify as Hephaestus");
    assert!(
        prompt.to_lowercase().contains("verify"),
        "GLM Hephaestus must keep the verification gate"
    );
}

#[test]
fn oracle_glm_variant_is_read_only() {
    let prompt = joey_omo::agents::prompts::dispatch_system_prompt("oracle", "glm-5.2");
    assert!(
        prompt.to_lowercase().contains("read-only"),
        "GLM Oracle must declare read-only"
    );
}

#[test]
fn momus_glm_variant_keeps_verdict_format() {
    let prompt = joey_omo::agents::prompts::dispatch_system_prompt("momus", "glm-5.2");
    assert!(prompt.contains("OKAY"), "GLM Momus must keep the OKAY/REJECT verdict format");
    assert!(prompt.contains("REJECT"));
}

#[test]
fn sisyphus_glm_variant_is_full_port() {
    // The enriched Sisyphus GLM port must contain all the sections from OMO's
    // buildGlm52SisyphusPrompt: outcome_first, exploration, communication,
    // constraints, plus the glm_52_calibration block.
    let prompt = joey_omo::agents::prompts::dispatch_system_prompt("sisyphus", "glm-5.2");
    assert!(prompt.contains("<outcome_first>"), "Sisyphus GLM must include outcome_first");
    assert!(prompt.contains("<exploration>"), "Sisyphus GLM must include exploration");
    assert!(prompt.contains("<communication>"), "Sisyphus GLM must include communication");
    assert!(prompt.contains("<constraints>"), "Sisyphus GLM must include constraints");
    assert!(prompt.contains("LITERAL FOLLOWING"));
    assert!(prompt.contains("OVER-EXPLORATION"));
    assert!(prompt.contains("CAPABILITY UNDER-REACH"));
}

#[test]
fn sisyphus_gemini_variant_has_tool_call_mandate() {
    let prompt = joey_omo::agents::prompts::dispatch_system_prompt("sisyphus", "gemini-3.1-pro");
    assert!(prompt.contains("TOOL_CALL_MANDATE"), "Gemini sisyphus must have tool call mandate");
}

#[test]
fn oracle_is_read_only() {
    let prompt = joey_omo::agents::prompts::dispatch_system_prompt("oracle", "gpt-5.6-sol");
    assert!(
        prompt.to_lowercase().contains("read-only"),
        "Oracle must declare read-only"
    );
}

#[test]
fn atlas_never_writes_code() {
    for model in &["claude-opus-4-8", "gpt-5.6-sol", "glm-5.2", "gemini-3.1-pro"] {
        let prompt = joey_omo::agents::prompts::dispatch_system_prompt("atlas", model);
        assert!(
            prompt.to_lowercase().contains("never write"),
            "Atlas + {} must declare 'never write code'",
            model
        );
    }
}

#[test]
fn prometheus_loads_ulw_plan_skill() {
    let prompt = joey_omo::agents::prompts::dispatch_system_prompt("prometheus", "claude-opus-4-8");
    assert!(prompt.contains("ulw-plan"), "Prometheus must reference ulw-plan skill");
}

#[test]
fn unknown_agent_falls_back_to_sisyphus() {
    let prompt = joey_omo::agents::prompts::dispatch_system_prompt("bogus-agent", "claude-opus-4-8");
    assert!(prompt.contains("Sisyphus"), "Unknown agent should fall back to Sisyphus");
}

#[test]
fn omo_agent_system_prompt_dispatches() {
    use joey_omo::models::ModelRequirement;
    let agent = joey_omo::OmoAgent {
        name: "sisyphus".into(),
        display_name: "Sisyphus".into(),
        mode: joey_omo::AgentMode::Primary,
        color: "#3B82F6".into(),
        description: "test".into(),
        model_requirement: ModelRequirement::default(),
        resolved_model: Some("glm-5.2".into()),
        resolved_variant: None,
        temperature: 0.1,
        max_tokens: None,
        tool_permissions: joey_omo::ToolPermissions::default(),
    };
    let prompt = agent.system_prompt("glm-5.2");
    assert!(prompt.contains("GLM"), "OmoAgent.system_prompt should dispatch to GLM variant");
}

// ---------------------------------------------------------------------------
// Kimi K2.6 prompt-selection regression tests (FR-001, FR-002, SC-003).
// These assert the bug fix: k2.6 model ids must resolve to kimi_k2_6(),
// NOT kimi_k2_7() (the bug was that both k2.7 and k2.6 arms returned
// kimi_k2_7()). Pointer-equality is used because the functions return
// &'static str — if two arms returned the same function pointer, the bug
// is still present.
// ---------------------------------------------------------------------------

#[test]
fn kimi_k2_6_model_id_dot_resolves_to_k2_6_prompt() {
    use joey_omo::agents::prompts::junior;
    let prompt = junior::for_model("kimi-k2.6");
    assert!(
        std::ptr::eq(prompt, junior::kimi_k2_6()),
        "k2.6 model id must resolve to kimi_k2_6(), not kimi_k2_7()"
    );
}

#[test]
fn kimi_k2_6_model_id_dash_resolves_to_k2_6_prompt() {
    use joey_omo::agents::prompts::junior;
    let prompt = junior::for_model("kimi-k2-6");
    assert!(
        std::ptr::eq(prompt, junior::kimi_k2_6()),
        "k2-6 model id must resolve to kimi_k2_6(), not kimi_k2_7()"
    );
}

#[test]
fn kimi_k2_7_model_id_dot_resolves_to_k2_7_prompt() {
    use joey_omo::agents::prompts::junior;
    let prompt = junior::for_model("kimi-k2.7");
    assert!(
        std::ptr::eq(prompt, junior::kimi_k2_7()),
        "k2.7 model id must resolve to kimi_k2_7() (regression guard)"
    );
}

#[test]
fn kimi_k2_7_model_id_dash_resolves_to_k2_7_prompt() {
    use joey_omo::agents::prompts::junior;
    let prompt = junior::for_model("kimi-k2-7");
    assert!(
        std::ptr::eq(prompt, junior::kimi_k2_7()),
        "k2-7 model id must resolve to kimi_k2_7() (regression guard)"
    );
}

#[test]
fn kimi_k3_model_id_falls_through_to_k3_prompt() {
    use joey_omo::agents::prompts::junior;
    let prompt = junior::for_model("kimi-k3");
    assert!(
        std::ptr::eq(prompt, junior::kimi_k3()),
        "k3 model id must fall through to kimi_k3()"
    );
}

#[test]
fn kimi_k2_6_and_k2_7_prompts_are_distinct() {
    use joey_omo::agents::prompts::junior;
    // The bug made them the same pointer. They must now be distinct.
    assert!(
        !std::ptr::eq(junior::kimi_k2_6(), junior::kimi_k2_7()),
        "kimi_k2_6() and kimi_k2_7() must be distinct prompts"
    );
}

#[test]
fn kimi_k2_6_prompt_mentions_k2_6() {
    use joey_omo::agents::prompts::junior;
    let prompt = junior::kimi_k2_6();
    assert!(
        prompt.contains("K2.6"),
        "kimi_k2_6() prompt must identify the model as K2.6"
    );
}

#[test]
fn non_kimi_model_id_unchanged() {
    use joey_omo::agents::prompts::junior;
    let prompt = junior::for_model("gpt-5.6-sol");
    // Non-Kimi ids should not resolve to any Kimi prompt.
    assert!(
        !std::ptr::eq(prompt, junior::kimi_k2_6()),
        "non-Kimi model must not resolve to kimi_k2_6()"
    );
    assert!(
        !std::ptr::eq(prompt, junior::kimi_k2_7()),
        "non-Kimi model must not resolve to kimi_k2_7()"
    );
    assert!(
        !std::ptr::eq(prompt, junior::kimi_k3()),
        "non-Kimi model must not resolve to kimi_k3()"
    );
}

// ---------------------------------------------------------------------------
// GPT-5.6 variant tests (T020-T023) + conductor persona tests (T004).
// Pointer-equality asserts variant identity: for_model must resolve to the
// exact same 'static str the module exposes.
// ---------------------------------------------------------------------------

#[test]
fn gpt_5_6_ids_select_gpt_5_6_variants() {
    use joey_omo::agents::prompts::{atlas, conductor, prometheus, sisyphus};
    for id in ["gpt-5.6-sol", "GPT-5-6-high", "gpt-5.6"] {
        let p = sisyphus::for_model(id);
        assert!(
            std::ptr::eq(p, sisyphus::gpt_5_6()),
            "sisyphus + {id} must resolve to gpt_5_6()"
        );
        let p = atlas::for_model(id);
        assert!(
            std::ptr::eq(p, atlas::gpt_5_6()),
            "atlas + {id} must resolve to gpt_5_6()"
        );
        let p = prometheus::for_model(id);
        assert!(
            std::ptr::eq(p, prometheus::gpt_5_6()),
            "prometheus + {id} must resolve to gpt_5_6()"
        );
        let p = conductor::for_model(id);
        assert!(
            std::ptr::eq(p, conductor::gpt_5_6()),
            "conductor + {id} must resolve to gpt_5_6()"
        );
    }
}

#[test]
fn generic_gpt_ids_still_select_generic_variants() {
    use joey_omo::agents::prompts::{atlas, conductor, prometheus, sisyphus};
    assert!(
        std::ptr::eq(sisyphus::for_model("gpt-5.4"), sisyphus::gpt()),
        "sisyphus + gpt-5.4 must resolve to gpt()"
    );
    assert!(
        std::ptr::eq(atlas::for_model("gpt-5.4"), atlas::gpt()),
        "atlas + gpt-5.4 must resolve to gpt()"
    );
    assert!(
        std::ptr::eq(prometheus::for_model("gpt-5.4"), prometheus::gpt()),
        "prometheus + gpt-5.4 must resolve to gpt()"
    );
    assert!(
        std::ptr::eq(conductor::for_model("gpt-5.4"), conductor::gpt()),
        "conductor + gpt-5.4 must resolve to gpt()"
    );
}

#[test]
fn hephaestus_gpt_5_6_dispatch_unchanged() {
    use joey_omo::agents::prompts::hephaestus;
    assert!(
        std::ptr::eq(hephaestus::for_model("gpt-5.6-sol"), hephaestus::gpt_5_6()),
        "hephaestus + gpt-5.6-sol must resolve to gpt_5_6()"
    );
    assert!(
        std::ptr::eq(hephaestus::for_model("gpt-5.5"), hephaestus::gpt_5_5()),
        "hephaestus + gpt-5.5 must resolve to gpt_5_5()"
    );
}

#[test]
fn non_gpt_families_keep_existing_variants() {
    use joey_omo::agents::prompts::{atlas, conductor, prometheus, sisyphus};
    assert!(
        std::ptr::eq(sisyphus::for_model("glm-5.2"), sisyphus::glm()),
        "sisyphus + glm-5.2 must resolve to glm()"
    );
    assert!(
        std::ptr::eq(atlas::for_model("kimi-k3"), atlas::kimi_k3()),
        "atlas + kimi-k3 must resolve to kimi_k3()"
    );
    assert!(
        std::ptr::eq(prometheus::for_model("claude-opus-4-8"), prometheus::default()),
        "prometheus + claude-opus-4-8 must resolve to default()"
    );
    assert!(
        std::ptr::eq(conductor::for_model("claude-opus-4-8"), conductor::default()),
        "conductor + claude-opus-4-8 must resolve to default()"
    );
}

#[test]
fn delegation_only_agents_fall_back_without_error() {
    // FR-009: agents without a dedicated GPT-5.6 variant fall back to the
    // nearest existing variant without error.
    use joey_omo::agents::prompts::{explore, junior, librarian};
    assert!(
        std::ptr::eq(junior::for_model("gpt-5.6-sol"), junior::gpt_5_5()),
        "junior + gpt-5.6-sol must fold to the existing gpt_5_5() variant"
    );
    assert!(
        std::ptr::eq(librarian::for_model("gpt-5.6-sol"), librarian::default()),
        "librarian + gpt-5.6-sol must resolve to default()"
    );
    assert!(
        std::ptr::eq(explore::for_model("gpt-5.6-sol"), explore::default()),
        "explore + gpt-5.6-sol must resolve to default()"
    );
}

#[test]
fn sisyphus_gpt_5_6_keeps_hard_invariants() {
    use joey_omo::agents::prompts::sisyphus;
    let prompt = sisyphus::gpt_5_6();
    assert!(prompt.contains("Never use `as any`"));
    assert!(prompt.contains("Never delete a failing test"));
    assert!(prompt.contains("Never use destructive git commands"));
    assert!(prompt.contains("Never deliver the final answer while a consulted Oracle is still running"));
}

#[test]
fn atlas_gpt_5_6_keeps_critical_rules() {
    use joey_omo::agents::prompts::atlas;
    let prompt = atlas::gpt_5_6();
    assert!(prompt.contains("NEVER: Write/edit code yourself"));
    assert!(prompt.contains("ALWAYS: Default to PARALLEL fan-out"));
}

#[test]
fn prometheus_gpt_variants_still_load_ulw_plan() {
    use joey_omo::agents::prompts::prometheus;
    for prompt in [prometheus::gpt(), prometheus::gpt_5_6(), prometheus::default()] {
        assert!(prompt.contains("ulw-plan"), "prometheus variant must reference ulw-plan");
        assert!(prompt.contains("You are a PLANNER"), "prometheus variant must declare planner identity");
    }
    for prompt in [prometheus::gpt(), prometheus::gpt_5_6()] {
        assert!(prompt.contains("never implement"), "GPT prometheus variant must keep the planner paragraph");
    }
}

#[test]
fn conductor_variants_carry_hard_rules_and_doctrine() {
    use joey_omo::agents::prompts::conductor;
    for (name, prompt) in [
        ("default", conductor::default()),
        ("gpt", conductor::gpt()),
        ("gpt_5_6", conductor::gpt_5_6()),
    ] {
        assert!(
            prompt.contains("NEVER write, patch, or delete files yourself"),
            "{name} conductor variant must embed the no-direct-writes hard rule"
        );
        assert!(
            prompt.contains("FINAL") && prompt.contains("GATE"),
            "{name} conductor variant must embed the final gate rule"
        );
        assert!(
            prompt.contains("SPEC-KIT LIFECYCLE DOCTRINE"),
            "{name} conductor variant must carry spec-kit doctrine"
        );
        assert!(
            prompt.contains(".specify/feature.json"),
            "{name} conductor variant must carry step-detection procedure"
        );
        for agent in [
            "sisyphus", "hephaestus", "prometheus", "atlas", "oracle",
            "librarian", "explore", "multimodal-looker", "metis", "momus",
            "sisyphus-junior",
        ] {
            assert!(
                prompt.contains(agent),
                "{name} conductor variant must brief roster agent {agent}"
            );
        }
    }
}

#[test]
fn conductor_dispatch_selection() {
    use joey_omo::agents::prompts::conductor;
    assert!(std::ptr::eq(conductor::for_model("gpt-5.6-sol"), conductor::gpt_5_6()));
    assert!(std::ptr::eq(conductor::for_model("gpt-5-6"), conductor::gpt_5_6()));
    assert!(std::ptr::eq(conductor::for_model("gpt-5.4"), conductor::gpt()));
    assert!(std::ptr::eq(conductor::for_model("glm-5.2"), conductor::default()));
    assert!(std::ptr::eq(conductor::for_model("claude-opus-4-8"), conductor::default()));
}

#[test]
fn conductor_is_not_a_registered_agent() {
    let available = joey_omo::AvailableModelSet::from_models(vec!["gpt-5.6-sol".to_string()]);
    let overrides = joey_omo::agents::registry::ModelOverrides::new();
    let registry = joey_omo::AgentRegistry::build(available, &overrides);
    assert!(registry.get("conductor").is_none(), "conductor must not be a registered OMO agent");
    assert_eq!(registry.all().len(), 11, "registry must still have exactly the 11 built-in agents");
    // Unknown-name dispatch falls back to the sisyphus default, which must not
    // carry the conductor persona.
    let prompt = joey_omo::dispatch_system_prompt("conductor", "gpt-5.6-sol");
    assert!(
        !prompt.contains("Conductor"),
        "unknown-name dispatch must fall back to sisyphus default, not the conductor persona"
    );
}

/// Feature 025 T034: `conductor_prompt` is the module's dispatch surface and
/// must stay byte-identical to `conductor::for_model` for every family.
#[test]
fn conductor_prompt_dispatch_surface() {
    for model in ["gpt-5.6-sol", "glm-5.2"] {
        assert_eq!(
            joey_omo::agents::prompts::conductor_prompt(model),
            joey_omo::agents::prompts::conductor::for_model(model).to_string(),
            "conductor_prompt must dispatch to the same variant as conductor::for_model for {model}"
        );
    }
}

/// Feature 026 T024/FR-008: the dynamic CURRENT LIFECYCLE STATE block is
/// appended AFTER the full static doctrine, inside the {SPEC_KIT} slot.
#[test]
fn conductor_lifecycle_block_appended_after_doctrine() {
    use joey_omo::agents::prompts::{conductor, conductor_prompt_with_lifecycle};
    let snap = conductor::LifecycleSnapshot {
        feature: "specs/026-please-fully-integrate".to_string(),
        step: "Implement".to_string(),
        guidance: "fan out implementors for unblocked tasks".to_string(),
        spec_present: true,
        plan_present: true,
        tasks_present: true,
    };
    for (name, model) in [
        ("default", "glm-5.2"),
        ("gpt", "gpt-5.4"),
        ("gpt_5_6", "gpt-5.6-sol"),
    ] {
        let out = conductor_prompt_with_lifecycle(model, Some(&snap));
        assert!(
            out.contains("SPEC-KIT LIFECYCLE DOCTRINE"),
            "{name} variant must still embed the static doctrine"
        );
        assert!(
            out.contains("CURRENT LIFECYCLE STATE (detected from disk at session start)"),
            "{name} variant must carry the dynamic lifecycle block header"
        );
        assert!(
            out.contains("specs/026-please-fully-integrate"),
            "{name} variant must name the active feature"
        );
        assert!(
            out.contains("Step: Implement"),
            "{name} variant must name the current step"
        );
        assert!(
            out.contains("[present]"),
            "{name} variant must mark present artifacts"
        );
        // The doctrine END must appear BEFORE the dynamic block start.
        let doctrine_tail = out.find("synthesize the completion report").unwrap();
        let block_start = out.find("CURRENT LIFECYCLE STATE").unwrap();
        assert!(
            doctrine_tail < block_start,
            "{name} variant: doctrine tail must precede the lifecycle block"
        );
    }
}

/// Feature 026 T024/FR-008: `None` snapshot renders byte-identically to the
/// pre-feature static prompt.
#[test]
fn conductor_lifecycle_none_is_byte_identical() {
    use joey_omo::agents::prompts::{conductor_prompt, conductor_prompt_with_lifecycle};
    for model in ["glm-5.2", "gpt-5.4", "gpt-5.6-sol"] {
        assert_eq!(
            conductor_prompt_with_lifecycle(model, None),
            conductor_prompt(model).as_str(),
            "None snapshot must render byte-identically to conductor_prompt for {model}"
        );
    }
}

/// Feature 026 T024/FR-008: the plain dispatch surface never carries the
/// dynamic block.
#[test]
fn conductor_doctrine_text_unchanged_by_snapshot() {
    use joey_omo::agents::prompts::conductor_prompt;
    for model in ["glm-5.2", "gpt-5.4", "gpt-5.6-sol"] {
        let prompt = conductor_prompt(model);
        assert!(
            !prompt.contains("CURRENT LIFECYCLE STATE"),
            "plain conductor_prompt must not carry the dynamic lifecycle block for {model}"
        );
    }
}
