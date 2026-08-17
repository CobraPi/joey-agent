//! Browser configuration resolved from joey-core dotted keys
//! (contracts/cdp-session.md + data-model.md §8).
//!
//! All keys are additive read-path surface; none are secrets, so none route
//! to `.env`. Clamping ranges per data-model.md.

use std::time::Duration;

use joey_core::config::Config;

/// Headless policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadlessPolicy {
    /// Headless iff no display is available.
    Auto,
    Always,
    Never,
}

/// Overlay auto-dismissal policy (research.md D5; default conservative).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayPolicy {
    Never,
    Conservative,
    Aggressive,
}

/// Snapshot budget knobs.
#[derive(Debug, Clone)]
pub struct SnapshotBudgets {
    /// Per-step textual material budget (bytes).
    pub max_step_bytes: usize,
    /// Cumulative per-task delta budget (bytes).
    pub cumulative_cap_bytes: usize,
    /// Viewport heights of "near view" beyond the visible rect.
    pub viewport_margin: f64,
}

/// Resolved browser configuration.
#[derive(Debug, Clone)]
pub struct BrowserConfig {
    /// Attach endpoint (e.g. `http://127.0.0.1:9222`).
    pub cdp_url: String,
    /// Skip discovery; use this browser executable.
    pub executable_path: Option<String>,
    pub headless: HeadlessPolicy,
    pub overlay_policy: OverlayPolicy,
    /// Expert gate for raw CDP passthrough.
    pub allow_raw_cdp: bool,
    pub quiet_window: Duration,
    pub hard_timeout: Duration,
    pub budgets: SnapshotBudgets,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            cdp_url: "http://127.0.0.1:9222".to_string(),
            executable_path: None,
            headless: HeadlessPolicy::Auto,
            overlay_policy: OverlayPolicy::Conservative,
            allow_raw_cdp: false,
            quiet_window: Duration::from_millis(1500),
            hard_timeout: Duration::from_millis(10_000),
            budgets: SnapshotBudgets {
                max_step_bytes: 8192,
                cumulative_cap_bytes: 65_536,
                viewport_margin: 1.0,
            },
        }
    }
}

impl BrowserConfig {
    /// Resolve from a joey-core config handle (dotted keys, clamped).
    pub fn from_config(config: &Config) -> Self {
        let exec = config.get_str("browser.executable_path", "");
        BrowserConfig {
            cdp_url: config.get_str("browser.cdp_url", "http://127.0.0.1:9222"),
            executable_path: (!exec.is_empty()).then(|| exec),
            headless: parse_headless(&config.get_str("browser.headless", "auto")),
            overlay_policy: parse_overlay(&config.get_str("browser.overlay_policy", "conservative")),
            allow_raw_cdp: config.get_bool("browser.allow_raw_cdp", false),
            quiet_window: Duration::from_millis(
                config.get_i64("browser.settle.quiet_ms", 1500).clamp(250, 5000) as u64,
            ),
            hard_timeout: Duration::from_millis(
                config
                    .get_i64("browser.settle.hard_timeout_ms", 10_000)
                    .clamp(2000, 60_000) as u64,
            ),
            budgets: SnapshotBudgets {
                max_step_bytes: config
                    .get_i64("browser.snapshot.max_step_bytes", 8192)
                    .clamp(1024, 1_048_576) as usize,
                cumulative_cap_bytes: config
                    .get_i64("browser.snapshot.cumulative_cap_bytes", 65_536)
                    .clamp(8192, 8_388_608) as usize,
                viewport_margin: config
                    .get_f64("browser.snapshot.viewport_margin", 1.0)
                    .clamp(0.0, 5.0),
            },
        }
    }
}

fn parse_headless(s: &str) -> HeadlessPolicy {
    match s.trim().to_ascii_lowercase().as_str() {
        "always" => HeadlessPolicy::Always,
        "never" => HeadlessPolicy::Never,
        _ => HeadlessPolicy::Auto,
    }
}

fn parse_overlay(s: &str) -> OverlayPolicy {
    match s.trim().to_ascii_lowercase().as_str() {
        "never" => OverlayPolicy::Never,
        "aggressive" => OverlayPolicy::Aggressive,
        _ => OverlayPolicy::Conservative,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a Config from a YAML string via a unique temp file — the only
    /// public constructor that parses user YAML.
    fn cfg_from_yaml(y: &str) -> Config {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, y).expect("write yaml");
        Config::load_from(path).expect("config load")
    }

    #[test]
    fn defaults_match_data_model() {
        let c = BrowserConfig::default();
        assert_eq!(c.cdp_url, "http://127.0.0.1:9222");
        assert_eq!(c.headless, HeadlessPolicy::Auto);
        assert_eq!(c.overlay_policy, OverlayPolicy::Conservative);
        assert!(!c.allow_raw_cdp);
        assert_eq!(c.quiet_window, Duration::from_millis(1500));
        assert_eq!(c.hard_timeout, Duration::from_millis(10_000));
        assert_eq!(c.budgets.max_step_bytes, 8192);
        assert_eq!(c.budgets.cumulative_cap_bytes, 65_536);
        assert!((c.budgets.viewport_margin - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn defaults_from_empty_config() {
        let c = cfg_from_yaml("");
        let bc = BrowserConfig::from_config(&c);
        assert_eq!(bc.cdp_url, "http://127.0.0.1:9222");
        assert_eq!(bc.headless, HeadlessPolicy::Auto);
        assert_eq!(bc.overlay_policy, OverlayPolicy::Conservative);
        assert!(!bc.allow_raw_cdp);
        assert!(bc.executable_path.is_none());
        assert_eq!(bc.quiet_window, Duration::from_millis(1500));
        assert_eq!(bc.hard_timeout, Duration::from_millis(10_000));
    }

    #[test]
    fn enum_parsing() {
        assert_eq!(parse_headless("AUTO"), HeadlessPolicy::Auto);
        assert_eq!(parse_headless("always"), HeadlessPolicy::Always);
        assert_eq!(parse_headless(" Never "), HeadlessPolicy::Never);
        assert_eq!(parse_headless("garbage"), HeadlessPolicy::Auto);
        assert_eq!(parse_overlay("conservative"), OverlayPolicy::Conservative);
        assert_eq!(parse_overlay("AGGRESSIVE"), OverlayPolicy::Aggressive);
        assert_eq!(parse_overlay("never"), OverlayPolicy::Never);
        assert_eq!(parse_overlay("garbage"), OverlayPolicy::Conservative);
    }

    #[test]
    fn clamping_applies() {
        let c = cfg_from_yaml(
            "browser:\n  settle:\n    quiet_ms: 1\n    hard_timeout_ms: 99999999\n  snapshot:\n    viewport_margin: 99.0\n    max_step_bytes: 1\n",
        );
        let bc = BrowserConfig::from_config(&c);
        assert_eq!(bc.quiet_window, Duration::from_millis(250));
        assert_eq!(bc.hard_timeout, Duration::from_millis(60_000));
        assert_eq!(bc.budgets.viewport_margin, 5.0);
        assert_eq!(bc.budgets.max_step_bytes, 1024);
    }

    #[test]
    fn overrides_apply() {
        let c = cfg_from_yaml(
            "browser:\n  cdp_url: http://localhost:9333\n  executable_path: /usr/bin/brave\n  headless: always\n  overlay_policy: aggressive\n  allow_raw_cdp: true\n",
        );
        let bc = BrowserConfig::from_config(&c);
        assert_eq!(bc.cdp_url, "http://localhost:9333");
        assert_eq!(bc.executable_path.as_deref(), Some("/usr/bin/brave"));
        assert_eq!(bc.headless, HeadlessPolicy::Always);
        assert_eq!(bc.overlay_policy, OverlayPolicy::Aggressive);
        assert!(bc.allow_raw_cdp);
    }
}
