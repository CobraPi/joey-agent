use joey_agent_core::AgentEvent;

fn usage(p: u64, c: u64) -> joey_providers::Usage {
    joey_providers::Usage { prompt_tokens: p, completion_tokens: c, total_tokens: p + c, ..Default::default() }
}

fn main() {
    let mut app = joey_tui::AppState::new("s", "m");
    let start = std::time::Instant::now();
    for turn in 0..2000 {
        app.record_user(&format!("question {turn}"));
        app.apply(AgentEvent::TurnStart { max_iterations: 90 });
        for it in 0..5 {
            app.apply(AgentEvent::IterationStart { iteration: it, max_iterations: 90 });
            app.apply(AgentEvent::ApiCallStart);
            app.apply(AgentEvent::ReasoningDelta("thinking ".repeat(20)));
            app.apply(AgentEvent::ContentDelta("partial content ".repeat(20)));
            app.apply(AgentEvent::ToolStart { name: "terminal".into(), emoji: "⚡".into(), summary: "ls".into() });
            app.apply(AgentEvent::ToolEnd { name: "terminal".into(), is_error: false, result_preview: "output".repeat(50), duration_secs: 0.1 });
            app.apply(AgentEvent::ApiCallEnd { usage: usage(100, 50) });
        }
        app.apply(AgentEvent::Done { final_text: format!("answer {turn}"), usage: usage(500, 250), iterations: 5 });
        if turn % 200 == 0 {
            println!("turn {turn}: elapsed {:?} transcript_len {} active_agents {}", start.elapsed(), app.transcript_len(), app.active_agents.len());
        }
    }
    println!("done: {:?}", start.elapsed());
}
