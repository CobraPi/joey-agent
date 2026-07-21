//! Layered configuration (port of upstream `hermes_cli/config.py` + `.env`).
//!
//! Resolution order (lowest → highest precedence):
//!   DEFAULT_CONFIG  <  ~/.joey/config.yaml  <  ~/.joey/.env (env wins over yaml)  <  CLI flags
//!
//! Config is held as a `serde_yaml::Value` tree so arbitrary/unknown keys are
//! preserved on round-trip (matching the Python deep-merge-under-defaults
//! behavior). Typed access goes through dotted-path helpers.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_yaml::Value;

use crate::{constants, utils};

/// The embedded default configuration. Mirrors the load-bearing subset of
/// upstream `DEFAULT_CONFIG`; unknown user keys merge on top and survive saves.
pub const DEFAULT_CONFIG_YAML: &str = r#"
model:
  default: "anthropic/claude-opus-4.6"
  provider: "auto"
  base_url: "https://openrouter.ai/api/v1"
agent:
  max_turns: 60
  verbose: false
  reasoning_effort: "medium"
  reasoning_overrides: {}
  api_max_retries: 3
  gateway_timeout: 1800
terminal:
  backend: "local"
  cwd: "."
  timeout: 180
toolsets:
  - "joey-cli"
compression:
  enabled: true
  threshold: 0.50
  target_ratio: 0.20
  protect_last_n: 20
  protect_first_n: 3
prompt_caching:
  cache_ttl: "5m"
memory:
  memory_enabled: true
  user_profile_enabled: true
  memory_char_limit: 2200
  user_char_limit: 1375
  nudge_interval: 10
  flush_min_turns: 6
skills:
  creation_nudge_interval: 15
  external_dirs: []
delegation:
  max_iterations: 50
  max_concurrent_children: 3
  max_spawn_depth: 1
code_execution:
  timeout: 300
  max_tool_calls: 50
display:
  compact: false
  tool_progress: "all"
  show_reasoning: false
  streaming: true
  timestamps: false
  skin: "default"
tool_output:
  max_bytes: 50000
  max_lines: 2000
  max_line_length: 2000
file_read_max_chars: 100000
cron:
  provider: "in-process"
"#;

/// A loaded, layered configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Merged view: defaults ← user yaml. Env overlays are applied lazily by
    /// typed getters that consult `JOEY_*`/known env vars first.
    root: Value,
    /// Path the user config was loaded from (for `config path` / saves).
    path: PathBuf,
}

impl Config {
    /// Load config layering DEFAULT_CONFIG under `~/.joey/config.yaml`, then
    /// load `~/.joey/.env` into the process environment (env wins at read time).
    pub fn load() -> Result<Self> {
        let path = constants::config_path();
        Self::load_from(path)
    }

    pub fn load_from(path: PathBuf) -> Result<Self> {
        // Load .env into the process environment first (does not override
        // already-set vars — matches dotenv semantics; explicit env wins).
        let env_path = constants::env_path();
        if env_path.exists() {
            let _ = dotenvy::from_path(&env_path);
        }

        let mut root: Value =
            serde_yaml::from_str(DEFAULT_CONFIG_YAML).expect("DEFAULT_CONFIG_YAML must parse");

        if !env_bool_ignore_user_config() && path.exists() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading config {}", path.display()))?;
            if !text.trim().is_empty() {
                let user: Value = serde_yaml::from_str(&text)
                    .with_context(|| format!("parsing config {}", path.display()))?;
                deep_merge(&mut root, user);
            }
        }

        Ok(Self { root, path })
    }

    /// Build config purely from defaults (no disk) — used in tests / headless.
    pub fn defaults() -> Self {
        let root = serde_yaml::from_str(DEFAULT_CONFIG_YAML).expect("defaults parse");
        Self {
            root,
            path: constants::config_path(),
        }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn root(&self) -> &Value {
        &self.root
    }

    /// Look up a dotted key path (e.g. `terminal.backend`) in the merged tree.
    pub fn get(&self, dotted: &str) -> Option<&Value> {
        let mut cur = &self.root;
        for part in dotted.split('.') {
            cur = cur.get(part)?;
        }
        Some(cur)
    }

    /// Dotted string lookup with a fallback.
    pub fn get_str(&self, dotted: &str, default: &str) -> String {
        self.get(dotted)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| default.to_string())
    }

    /// Dotted integer lookup with a fallback.
    pub fn get_i64(&self, dotted: &str, default: i64) -> i64 {
        self.get(dotted).and_then(value_as_i64).unwrap_or(default)
    }

    /// Dotted float lookup with a fallback.
    pub fn get_f64(&self, dotted: &str, default: f64) -> f64 {
        self.get(dotted)
            .and_then(|v| v.as_f64())
            .unwrap_or(default)
    }

    /// Dotted bool lookup with a fallback.
    pub fn get_bool(&self, dotted: &str, default: bool) -> bool {
        self.get(dotted).and_then(|v| v.as_bool()).unwrap_or(default)
    }

    /// Dotted string-list lookup.
    pub fn get_str_list(&self, dotted: &str) -> Vec<String> {
        self.get(dotted)
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The effective default model, honoring `model` as string or mapping.
    pub fn model(&self) -> String {
        if let Some(over) = std::env::var("JOEY_INFERENCE_MODEL").ok().filter(|s| !s.is_empty()) {
            return over;
        }
        match self.get("model") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Mapping(_)) => self
                .get("model.default")
                .or_else(|| self.get("model.model"))
                .and_then(|v| v.as_str())
                .unwrap_or("anthropic/claude-opus-4.6")
                .to_string(),
            _ => "anthropic/claude-opus-4.6".to_string(),
        }
    }

    /// Override the effective model in memory only (per-invocation `--model`),
    /// without persisting to disk.
    pub fn set_model_override(&mut self, model: &str) {
        set_nested(&mut self.root, "model.default", Value::String(model.to_string()));
    }

    /// Set a dotted key to a scalar value in the user tree (coercing
    /// bool/int/float from the string form) and persist to disk. Env-shaped
    /// keys (API keys/tokens) are routed to `.env` instead, mirroring upstream.
    pub fn set_and_save(&mut self, dotted: &str, raw_value: &str) -> Result<()> {
        if is_env_config_key(dotted) {
            return self.set_env_value(dotted, raw_value);
        }
        let coerced = coerce_scalar(raw_value);
        set_nested(&mut self.root, dotted, coerced);
        self.save()
    }

    fn set_env_value(&self, dotted: &str, raw_value: &str) -> Result<()> {
        let key = dotted.rsplit('.').next().unwrap_or(dotted).to_uppercase();
        let env_path = constants::env_path();
        let mut lines: Vec<String> = if env_path.exists() {
            std::fs::read_to_string(&env_path)?
                .lines()
                .map(str::to_string)
                .collect()
        } else {
            Vec::new()
        };
        let prefix = format!("{}=", key);
        let new_line = format!("{}={}", key, raw_value);
        if let Some(existing) = lines.iter_mut().find(|l| l.starts_with(&prefix)) {
            *existing = new_line;
        } else {
            lines.push(new_line);
        }
        let mut body = lines.join("\n");
        body.push('\n');
        utils::atomic_replace(&env_path, body.as_bytes())?;
        constants::secure_parent_dir(&env_path);
        Ok(())
    }

    /// Persist the current (merged) tree to the user config path. Note: like a
    /// serde_yaml round-trip this does not preserve comments in the user file.
    pub fn save(&self) -> Result<()> {
        utils::atomic_yaml_write(&self.path, &self.root)?;
        constants::secure_parent_dir(&self.path);
        Ok(())
    }
}

fn env_bool_ignore_user_config() -> bool {
    utils::env_bool("JOEY_IGNORE_USER_CONFIG", false)
}

/// True for config keys that should live in `.env` (API keys, tokens, secrets).
pub fn is_env_config_key(dotted: &str) -> bool {
    let leaf = dotted.rsplit('.').next().unwrap_or(dotted).to_uppercase();
    ["API_KEY", "TOKEN", "SECRET", "PASSWORD", "KEY"]
        .iter()
        .any(|marker| leaf.ends_with(marker) || leaf.contains(marker))
}

fn value_as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Coerce a CLI string into the most specific scalar Value (bool → int → float → string).
fn coerce_scalar(raw: &str) -> Value {
    let t = raw.trim();
    if let Some(b) = utils::parse_bool(t) {
        // Only treat canonical bool words as bool, not "0"/"1" which are more
        // useful as ints in config.
        if matches!(t.to_lowercase().as_str(), "true" | "false" | "yes" | "no" | "on" | "off") {
            return Value::Bool(b);
        }
    }
    if let Ok(i) = t.parse::<i64>() {
        return Value::Number(i.into());
    }
    if let Ok(f) = t.parse::<f64>() {
        if let Some(n) = serde_yaml::Number::from(f).into() {
            return Value::Number(n);
        }
    }
    Value::String(raw.to_string())
}

/// Set a dotted path in a mapping tree, creating intermediate maps as needed.
pub fn set_nested(root: &mut Value, dotted: &str, value: Value) {
    if !root.is_mapping() {
        *root = Value::Mapping(serde_yaml::Mapping::new());
    }
    let parts: Vec<&str> = dotted.split('.').collect();
    let mut cur = root;
    for (i, part) in parts.iter().enumerate() {
        let map = match cur {
            Value::Mapping(m) => m,
            _ => {
                *cur = Value::Mapping(serde_yaml::Mapping::new());
                match cur {
                    Value::Mapping(m) => m,
                    _ => unreachable!(),
                }
            }
        };
        let key = Value::String((*part).to_string());
        if i == parts.len() - 1 {
            map.insert(key, value);
            return;
        }
        cur = map.entry(key).or_insert_with(|| Value::Mapping(serde_yaml::Mapping::new()));
    }
}

/// Deep-merge `overlay` onto `base`: mappings merge key-by-key; every other
/// value type (including sequences) is replaced wholesale by the overlay.
pub fn deep_merge(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Mapping(base_map), Value::Mapping(overlay_map)) => {
            for (k, v) in overlay_map {
                match base_map.get_mut(&k) {
                    Some(existing) => deep_merge(existing, v),
                    None => {
                        base_map.insert(k, v);
                    }
                }
            }
        }
        (base_slot, overlay_val) => {
            *base_slot = overlay_val;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse_and_read() {
        let cfg = Config::defaults();
        assert_eq!(cfg.get_str("terminal.backend", "x"), "local");
        assert_eq!(cfg.get_i64("agent.max_turns", 0), 60);
        assert_eq!(cfg.model(), "anthropic/claude-opus-4.6");
        assert!(cfg.get_bool("compression.enabled", false));
    }

    #[test]
    fn deep_merge_overrides_leaf_keeps_siblings() {
        let mut base: Value = serde_yaml::from_str("a:\n  b: 1\n  c: 2\n").unwrap();
        let overlay: Value = serde_yaml::from_str("a:\n  b: 9\n").unwrap();
        deep_merge(&mut base, overlay);
        assert_eq!(base["a"]["b"].as_i64(), Some(9));
        assert_eq!(base["a"]["c"].as_i64(), Some(2));
    }

    #[test]
    fn env_key_detection() {
        assert!(is_env_config_key("model.api_key"));
        assert!(is_env_config_key("OPENROUTER_API_KEY"));
        assert!(!is_env_config_key("terminal.backend"));
    }

    #[test]
    fn set_nested_creates_path() {
        let mut root = Value::Mapping(Default::default());
        set_nested(&mut root, "x.y.z", Value::Bool(true));
        assert_eq!(root["x"]["y"]["z"].as_bool(), Some(true));
    }
}
