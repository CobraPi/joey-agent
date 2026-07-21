//! The execution context handed to every tool.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use joey_core::Config;

/// Shared, cheaply-cloneable execution context for tools.
#[derive(Clone)]
pub struct ToolContext {
    inner: Arc<ContextInner>,
}

struct ContextInner {
    cwd: PathBuf,
    config: Config,
    session_id: String,
    /// Whether the session is interactive (gates tools like `clarify`).
    interactive: bool,
    /// Whether dangerous ops are auto-approved (`--yolo`).
    yolo: bool,
}

impl ToolContext {
    pub fn new(cwd: PathBuf, config: Config, session_id: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(ContextInner {
                cwd,
                config,
                session_id: session_id.into(),
                interactive: true,
                yolo: joey_core::utils::env_bool("JOEY_YOLO_MODE", false),
            }),
        }
    }

    pub fn with_interactive(mut self, interactive: bool) -> Self {
        Arc::make_mut(&mut self.inner_arc()).interactive = interactive;
        self
    }

    // Helper to get a mutable Arc for builder methods (only used pre-share).
    fn inner_arc(&mut self) -> &mut Arc<ContextInner> {
        &mut self.inner
    }

    pub fn cwd(&self) -> &Path {
        &self.inner.cwd
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn session_id(&self) -> &str {
        &self.inner.session_id
    }

    pub fn interactive(&self) -> bool {
        self.inner.interactive
    }

    pub fn yolo(&self) -> bool {
        self.inner.yolo
    }

    /// Resolve a possibly-relative path against the session cwd, expanding `~`.
    pub fn resolve_path(&self, path: &str) -> PathBuf {
        let expanded = shellexpand::tilde(path).to_string();
        let p = PathBuf::from(expanded);
        if p.is_absolute() {
            p
        } else {
            self.inner.cwd.join(p)
        }
    }
}

// ContextInner must be Clone for Arc::make_mut in builder methods.
impl Clone for ContextInner {
    fn clone(&self) -> Self {
        Self {
            cwd: self.cwd.clone(),
            config: self.config.clone(),
            session_id: self.session_id.clone(),
            interactive: self.interactive,
            yolo: self.yolo,
        }
    }
}
