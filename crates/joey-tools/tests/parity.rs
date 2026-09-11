//! Feature 028 scratchpad parity — FR-013.
//!
//! Scratchpad-side parity suite (US6):
//! * registry output is byte-identical when the scratchpad tool is disabled
//!   via config — only the scratchpad definition disappears;
//! * `stats`/`path` behave sanely on a missing store;
//! * a brand-new JOEY home needs zero setup for the scratchpad to work
//!   (US6 acceptance 1).
//!
//! Quickstart: `cargo test -p joey-tools parity`.
//!
//! The joey-home override is process-global; the in-crate `test_env_lock`
//! helper is `pub(crate)` and unavailable here, so these tests serialize on
//! a local static Mutex instead. `invalidate_check_cache()` (registry.rs)
//! IS `pub`, so the check-cache TTL is cleared explicitly rather than
//! relying on cross-test ordering.

use std::path::Path;

use joey_core::constants::HomeOverrideGuard;
use joey_core::Config;
use joey_tools::registry::invalidate_check_cache;
use joey_tools::resolve_toolsets;
use joey_tools::tools::scratchpad_tool::{path, stats};
use joey_tools::{ToolContext, ToolRegistry};
use serde_json::{json, Value};

/// Serializes tests that mutate the process-global joey-home override.
static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn def_names(defs: &[Value]) -> Vec<String> {
    defs.iter()
        .map(|d| {
            d["function"]["name"]
                .as_str()
                .expect("definition has a function.name")
                .to_string()
        })
        .collect()
}

mod parity {
    use super::*;

    /// FR-013 / US6: disabling `scratchpad.enabled` removes exactly the
    /// scratchpad tool from the registry's schema output — every other
    /// definition is byte-identical.
    #[test]
    fn registry_output_byte_identical_when_scratchpad_disabled() {
        let _lock = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let _guard = HomeOverrideGuard::new(home.clone());

        let config_a = Config::defaults();
        let cfg_dir = tempfile::tempdir().unwrap();
        let cfg_path = cfg_dir.path().join("config.yaml");
        std::fs::write(&cfg_path, "scratchpad:\n  enabled: false\n").unwrap();
        let config_b = Config::load_from(cfg_path).unwrap();
        assert!(config_a.scratchpad_enabled(), "premise: defaults enable scratchpad");
        assert!(!config_b.scratchpad_enabled(), "premise: yaml disables scratchpad");

        let enabled = resolve_toolsets(&config_a.get_str_list("toolsets"));
        let ctx_a = ToolContext::new(std::env::temp_dir(), config_a, "parity-a");
        let ctx_b = ToolContext::new(std::env::temp_dir(), config_b, "parity-b");

        let defs_a = ToolRegistry::with_builtins().definitions(&enabled, &ctx_a);
        // check_cached TTL cache is process-global (30s): drop cached results
        // so ctx_b's probe re-runs against the disabled config.
        invalidate_check_cache();
        let defs_b = ToolRegistry::with_builtins().definitions(&enabled, &ctx_b);

        let names_a = def_names(&defs_a);
        let names_b = def_names(&defs_b);
        assert!(names_a.contains(&"scratchpad".to_string()), "enabled config exposes scratchpad");
        assert!(!names_b.contains(&"scratchpad".to_string()), "disabled config hides scratchpad");
        assert!(names_b.contains(&"todo".to_string()));
        assert!(names_b.contains(&"read_file".to_string()));

        // BYTE PARITY: defsA minus scratchpad serializes to the exact same
        // bytes as defsB.
        let filtered_a: Vec<Value> = defs_a
            .iter()
            .filter(|d| d["function"]["name"].as_str() != Some("scratchpad"))
            .cloned()
            .collect();
        let bytes_a = serde_json::to_string(&filtered_a).unwrap();
        let bytes_b = serde_json::to_string(&defs_b).unwrap();
        assert_eq!(bytes_a, bytes_b, "registry output must be byte-identical modulo scratchpad");
    }

    /// `stats` returns None and `path` is a pure string fn on a session with
    /// no scratchpad store (no file is created).
    #[test]
    fn stats_none_on_missing_store() {
        let _lock = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let _guard = HomeOverrideGuard::new(home);

        assert!(stats("parity-missing-session").is_none());
        let p = path("parity-missing-session");
        assert!(p.ends_with(".md"), "path ends with .md: {p}");
        assert!(p.contains("scratchpads"), "path names the scratchpads dir: {p}");
        assert!(!Path::new(&p).exists(), "path() must not create the file: {p}");
    }

    /// US6 acceptance 1: on a brand-new JOEY home (no pre-created dirs) the
    /// scratchpad works with zero setup — append, read, stats, and the file
    /// land under `<home>/scratchpads/`.
    #[tokio::test]
    async fn fresh_joey_home_zero_setup_smoke() {
        let _lock = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let _guard = HomeOverrideGuard::new(home.clone());

        let registry = ToolRegistry::with_builtins();
        let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "fresh-smoke");

        let appended = registry
            .dispatch(
                "scratchpad",
                json!({"action": "append", "text": "k=v smoke", "label": "l"}),
                &ctx,
            )
            .await;
        assert!(!appended.is_error(), "append must succeed: {}", appended.to_content_string());
        let v: Value = serde_json::from_str(&appended.to_content_string())
            .expect("append returns a JSON ok envelope");
        assert_eq!(v["ok"], true, "ok envelope: {v}");

        let read = registry
            .dispatch("scratchpad", json!({"action": "read"}), &ctx)
            .await
            .to_content_string();
        assert!(read.contains("k=v smoke"), "read returns the entry: {read}");

        assert_eq!(stats("fresh-smoke").unwrap().entries, 1);

        let p = path("fresh-smoke");
        assert!(Path::new(&p).exists(), "scratchpad file created: {p}");
        let parent = Path::new(&p).parent().expect("scratchpad file has a parent dir");
        assert!(parent.starts_with(&home), "file lives under {home:?}: {p}");
    }
}
