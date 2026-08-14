//! Registration of every built-in tool into a [`ToolRegistry`].

use std::sync::{Arc, Mutex};

use crate::registry::ToolRegistry;
use crate::tools::*;
use crate::tools::neurocode_tools::NeuroCodeBackend;

/// Register the self-contained built-in tools. Tools that need broader context
/// (session DB, the agent itself, the cron store) are registered by the higher
/// crates that own that context: `session_search`, `delegate_task`, `cronjob`,
/// `clarify`, `process`.
pub fn register_all(registry: &mut ToolRegistry) {
    registry.register(Arc::new(file_tools::ReadFile));
    registry.register(Arc::new(file_tools::WriteFile));
    registry.register(Arc::new(file_tools::Patch));
    registry.register(Arc::new(file_tools::MultiEdit));
    registry.register(Arc::new(file_tools::SearchFiles));
    registry.register(Arc::new(terminal_tool::Terminal));
    registry.register(Arc::new(todo_tool::Todo));
    registry.register(Arc::new(memory_tool::Memory));
    registry.register(Arc::new(web_tools::WebSearch));
    registry.register(Arc::new(web_tools::WebExtract));
    registry.register(Arc::new(skills_tool::SkillsList));
    registry.register(Arc::new(skills_tool::SkillView));
    registry.register(Arc::new(process_tool::Process));
    // LSP-backed tools (conditionally available — check() returns false when
    // no LSP manager is registered).
    registry.register(Arc::new(lsp_tools::LspDiagnostics));
    registry.register(Arc::new(lsp_tools::LspDefinition));
    registry.register(Arc::new(lsp_tools::LspReferences));
    registry.register(Arc::new(lsp_tools::LspSymbols));
}

/// Register the session_search tool with an optional session DB handle.
/// Conditionally registers (the tool's `check()` returns false when DB is None).
pub fn register_session_tools(
    registry: &mut ToolRegistry,
    session_db: Option<Arc<Mutex<joey_core::state::SessionDb>>>,
) {
    registry.register(Arc::new(session_search_tool::SessionSearch::new(
        session_db,
    )));
}

/// Register the clarify tool with an optional clarify channel.
/// Conditionally registers (the tool's `check()` returns false when channel is None or non-interactive).
pub fn register_clarify_tool(
    registry: &mut ToolRegistry,
    clarify_tx: Option<tokio::sync::mpsc::UnboundedSender<clarify_tool::ClarifyRequest>>,
) {
    registry.register(Arc::new(clarify_tool::Clarify::new(clarify_tx)));
}

/// Register the four NeuroCode tools (index, query, status, ingest) with an
/// optional backend. When `backend` is `None`, the tools are registered but
/// remain disabled (their `check()` returns false), so they are hidden from
/// the model. The concrete backend is supplied by higher crates that own the
/// `NeuroCodeEngine` handle (joey-tools cannot depend on joey-neurocode — DAG).
pub fn register_neurocode_tools(
    registry: &mut ToolRegistry,
    backend: Option<Arc<dyn NeuroCodeBackend>>,
) {
    neurocode_tools::register_neurocode_tools(registry, backend);
}
