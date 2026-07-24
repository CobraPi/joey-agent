use joey_omo::agents::registry::ModelOverrides;
use joey_omo::models::ModelFamily;
use joey_omo::{AgentRegistry, AvailableModelSet};

fn main() {
    println!("=== OMO Agent Registry — native z.ai / GLM 5.2 resolution ===\n");

    let profile = joey_providers::profile::get_profile("zai").unwrap();
    println!("zai profile: {} (aliases: {:?})", profile.name, profile.aliases);

    let available = AvailableModelSet::from_connected(&profile, "glm-5.2");
    println!("\nAvailable providers (billing plan aliases):");
    for p in &[
        "zai",
        "zai-coding-plan",
        "opencode-go",
        "bailian-coding-plan",
        "moonshotai-cn",
        "vercel",
        "opencode",
    ] {
        println!("  {p}? {}", available.has_provider(p));
    }

    println!("\nAvailable models:");
    for m in &["glm-5.2", "glm-5", "glm-4-9b", "glm-4.6v"] {
        println!("  {m}? {}", available.contains_exact(m));
    }

    let registry = AgentRegistry::build(available, &ModelOverrides::new());

    println!("\nAll 11 agents resolved against a z.ai-only setup:");
    let mut all_glm = true;
    for agent in registry.all() {
        let model = agent.effective_model();
        let family = ModelFamily::detect(model);
        let flag = if family == ModelFamily::Glm { "GLM" } else { "!!" };
        if family != ModelFamily::Glm {
            all_glm = false;
        }
        println!(
            "  [{flag}] {:<18} {:<18} {}",
            agent.name, model, agent.display_name
        );
    }
    println!(
        "\nEvery agent resolved a GLM model on z.ai-only: {all_glm} ({} agents total)",
        registry.all().len()
    );

    println!("\nAvailable primary agents (tab order):");
    for agent in registry.tab_order() {
        println!("  {} ({}): {}", agent.display_name, agent.name, agent.effective_model());
    }

    // Verify the Sisyphus GLM prompt is the enriched, faithful port.
    let prompt = joey_omo::agents::prompts::dispatch_system_prompt("sisyphus", "glm-5.2");
    println!(
        "\nSisyphus GLM prompt length: {} chars (contains outcome_first, exploration, communication, constraints: {})",
        prompt.len(),
        prompt.contains("<outcome_first>")
            && prompt.contains("<exploration>")
            && prompt.contains("<communication>")
            && prompt.contains("<constraints>")
    );

    println!("\n=== Test Complete ===");
}
