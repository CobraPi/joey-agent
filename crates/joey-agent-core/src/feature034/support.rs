//! Shared fixture for feature-034 story tests.

use std::sync::{Arc, Mutex};

use joey_providers::{NormalizedResponse, ProviderError, ProviderRequest, StreamEvent};
use tokio::sync::mpsc;

use crate::agent::{Agent, AgentConfig, Transport};
use joey_tools::{ToolContext, ToolRegistry};

/// Scripted transport: records every ProviderRequest and replies from a
/// scripted queue (compression/loop_tests.rs pattern).
pub struct ScriptedTransport {
    pub requests: Mutex<Vec<ProviderRequest>>,
    responses: Mutex<std::collections::VecDeque<Result<NormalizedResponse, ProviderError>>>,
}

impl ScriptedTransport {
    pub fn new(responses: Vec<Result<NormalizedResponse, ProviderError>>) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            responses: Mutex::new(responses.into()),
        })
    }
}

#[async_trait::async_trait]
impl Transport for ScriptedTransport {
    async fn complete(&self, req: &ProviderRequest) -> Result<NormalizedResponse, ProviderError> {
        self.requests.lock().unwrap().push(req.clone());
        self.responses.lock().unwrap().pop_front().unwrap_or_else(|| Ok(NormalizedResponse::empty()))
    }

    async fn stream(&self, req: &ProviderRequest, _tx: mpsc::UnboundedSender<StreamEvent>) -> Result<NormalizedResponse, ProviderError> {
        self.complete(req).await
    }
}

/// Serializes tests that override the process-global joey home
/// (tests/memory_injection.rs pattern).
pub fn lock() -> std::sync::MutexGuard<'static, ()> {
    joey_core::constants::TEST_HOME_OVERRIDE_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn load_config(home: &std::path::Path, yaml: &str) -> joey_core::Config {
    let path = home.join("config.yaml");
    std::fs::write(&path, yaml).unwrap();
    joey_core::Config::load_from(path).unwrap()
}

pub struct Fixture {
    pub transport: Arc<ScriptedTransport>,
    pub agent: Agent,
    pub home: tempfile::TempDir,
    _guard: joey_core::constants::HomeOverrideGuard,
    _lock: std::sync::MutexGuard<'static, ()>,
}

pub fn fixture(yaml: &str, responses: Vec<Result<NormalizedResponse, ProviderError>>) -> Fixture {
    fixture_with(yaml, responses, ToolRegistry::new(), vec![])
}

pub fn fixture_with_tools(yaml: &str, responses: Vec<Result<NormalizedResponse, ProviderError>>, enabled_tools: Vec<&str>) -> Fixture {
    fixture_with(yaml, responses, ToolRegistry::with_builtins(), enabled_tools)
}

fn fixture_with(yaml: &str, responses: Vec<Result<NormalizedResponse, ProviderError>>, registry: ToolRegistry, enabled_tools: Vec<&str>) -> Fixture {
    let lock = lock();
    let home = tempfile::tempdir().expect("tempdir");
    let guard = joey_core::constants::HomeOverrideGuard::new(home.path().to_path_buf());
    let config = load_config(home.path(), yaml);
    let ctx = ToolContext::new(std::env::temp_dir(), config, "feature034-session");
    let agent_cfg = AgentConfig {
        model: "test-model".into(),
        provider: "openrouter".into(),
        base_url: "https://openrouter.ai/api/v1".into(),
        api_key: None,
        max_turns: 10,
        api_max_retries: 3,
        tool_delay: 0.0,
        reasoning: None,
        enabled_tools: enabled_tools.into_iter().map(str::to_string).collect(),
        max_tokens: None,
        stream: false,
        pass_session_id: false,
        model_pinned: false,
    };
    let transport = ScriptedTransport::new(responses);
    let mut agent = Agent::new(agent_cfg, registry, ctx).expect("agent");
    agent.set_transport_for_tests(transport.clone());
    Fixture { transport, agent, home, _guard: guard, _lock: lock }
}

impl Fixture {
    /// Run one turn; the event channel receiver stays alive so sends succeed.
    pub async fn turn(&mut self, input: &str) {
        let (tx, _rx) = mpsc::unbounded_channel();
        self.agent.run_turn(input, tx).await;
    }
}
