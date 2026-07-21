//! Registration of every built-in tool into a [`ToolRegistry`].

use std::sync::Arc;

use crate::registry::ToolRegistry;
use crate::tools::*;

/// Register the self-contained built-in tools. Tools that need broader context
/// (session DB, the agent itself, the cron store) are registered by the higher
/// crates that own that context: `session_search`, `delegate_task`, `cronjob`,
/// `clarify`, `process`.
pub fn register_all(registry: &mut ToolRegistry) {
    registry.register(Arc::new(file_tools::ReadFile));
    registry.register(Arc::new(file_tools::WriteFile));
    registry.register(Arc::new(file_tools::Patch));
    registry.register(Arc::new(file_tools::SearchFiles));
    registry.register(Arc::new(terminal_tool::Terminal));
    registry.register(Arc::new(todo_tool::Todo));
    registry.register(Arc::new(memory_tool::Memory));
    registry.register(Arc::new(web_tools::WebSearch));
    registry.register(Arc::new(web_tools::WebExtract));
    registry.register(Arc::new(skills_tool::SkillsList));
    registry.register(Arc::new(skills_tool::SkillView));
}
