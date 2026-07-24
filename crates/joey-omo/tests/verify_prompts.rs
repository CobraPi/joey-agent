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
