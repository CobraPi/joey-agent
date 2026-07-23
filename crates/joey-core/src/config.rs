//! Layered configuration (port of upstream `hermes_cli/config.py` +
//! `hermes_cli/env_loader.py`).
//!
//! Resolution order (lowest → highest precedence):
//!   DEFAULT_CONFIG  <  ~/.joey/config.yaml  <  `${VAR}` expansion from the
//!   process env (after ~/.joey/.env is loaded with OVERRIDE semantics).
//!
//! The struct holds BOTH the raw user document (what `save` persists — only
//! user-set keys, never the merged defaults tree) and the merged view used
//! for reads. Parse failures never fail the load: the last-known-good config
//! (or defaults) is served, with a stderr warning and a timestamped backup
//! of the corrupt file.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use once_cell::sync::Lazy;
use serde_yaml::{Mapping, Value};

use crate::{constants, utils};

/// The embedded default configuration. Mirrors the load-bearing subset of
/// upstream `DEFAULT_CONFIG` (config.py:998) plus the CLI-tree model shape
/// (cli.py:441); unknown user keys merge on top and survive saves.
pub const DEFAULT_CONFIG_YAML: &str = r#"
model:
  default: "glm-5.2"
  provider: "zai"
  base_url: ""
agent:
  max_turns: 90
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
  hygiene_hard_message_limit: 5000
  protect_first_n: 3
  abort_on_summary_failure: false
auxiliary:
  compression:
    provider: "auto"
    model: ""
    base_url: ""
    api_key: ""
    timeout: 120
    extra_body: {}
    reasoning_effort: ""
prompt_caching:
  cache_ttl: "5m"
memory:
  memory_enabled: true
  user_profile_enabled: true
  memory_char_limit: 2200
  user_char_limit: 1375
  nudge_interval: 10
skills:
  creation_nudge_interval: 10
  external_dirs: []
delegation:
  max_iterations: 50
  max_concurrent_children: 3
  max_spawn_depth: 1
code_execution:
  mode: "project"
display:
  compact: false
  tool_progress: "all"
  show_reasoning: true
  streaming: false
  timestamps: false
  skin: "default"
tool_output:
  max_bytes: 50000
  max_lines: 2000
  max_line_length: 2000
file_read_max_chars: 100000
approvals:
  mode: "smart"
  timeout: 60
  cron_mode: "deny"
  deny: []
security:
  redact_secrets: true
logging:
  level: "INFO"
  max_size_mb: 5
  backup_count: 3
timezone: ""
cron:
  provider: ""
_config_version: 33
"#;

/// Config schema version written on save (upstream `_config_version`).
pub const CONFIG_VERSION: i64 = 33;

static DEFAULTS: Lazy<Value> =
    Lazy::new(|| serde_yaml::from_str(DEFAULT_CONFIG_YAML).expect("DEFAULT_CONFIG_YAML must parse"));

/// Last successfully expanded config per path — served when the on-disk YAML
/// becomes unparseable mid-process (port of `_LAST_EXPANDED_CONFIG_BY_PATH`).
static LAST_GOOD_BY_PATH: Lazy<Mutex<HashMap<PathBuf, Value>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// (path, mtime_ns, size) triples already warned about (port of
/// `_CONFIG_PARSE_WARNED`) — re-warns when the file changes.
static PARSE_WARNED: Lazy<Mutex<HashSet<(String, u128, u64)>>> =
    Lazy::new(|| Mutex::new(HashSet::new()));

/// A loaded, layered configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// The raw user document (config.yaml content). This — and ONLY this —
    /// is what `save` persists, so schema defaults never contaminate the
    /// user's file.
    user_doc: Value,
    /// Merged view: defaults ← user, normalized and `${VAR}`-expanded.
    root: Value,
    /// Path the user config was loaded from (for `config path` / saves).
    path: PathBuf,
}

impl Config {
    /// Load config: `.env` into the process environment (user values override
    /// stale shell exports), then DEFAULT_CONFIG under `~/.joey/config.yaml`.
    pub fn load() -> Result<Self> {
        load_joey_dotenv(None, None);
        Self::load_from(constants::config_path())
    }

    /// Load from an explicit config.yaml path (no .env side effects).
    ///
    /// A YAML parse failure NEVER fails the load: the last-known-good merged
    /// config for this path (or the defaults) is served, a stderr warning is
    /// emitted, and the corrupt file is backed up with a timestamp
    /// (config.py:7345-7397).
    pub fn load_from(path: PathBuf) -> Result<Self> {
        match read_user_doc(&path) {
            Ok(user_doc) => {
                let root = build_root(&user_doc);
                LAST_GOOD_BY_PATH
                    .lock()
                    .expect("config lkg lock")
                    .insert(path.clone(), root.clone());
                Ok(Self { user_doc, root, path })
            }
            Err(parse_err) => {
                let lkg = LAST_GOOD_BY_PATH
                    .lock()
                    .expect("config lkg lock")
                    .get(&path)
                    .cloned();
                warn_config_parse_failure(&path, &parse_err, lkg.is_some());
                let root = match lkg {
                    Some(good) => good,
                    None => build_root(&Value::Mapping(Mapping::new())),
                };
                Ok(Self {
                    user_doc: Value::Mapping(Mapping::new()),
                    root,
                    path,
                })
            }
        }
    }

    /// Build config purely from defaults (no disk) — used in tests / headless.
    pub fn defaults() -> Self {
        Self {
            user_doc: Value::Mapping(Mapping::new()),
            root: DEFAULTS.clone(),
            path: constants::config_path(),
        }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// The merged (defaults ← user, expanded) view.
    pub fn root(&self) -> &Value {
        &self.root
    }

    /// The raw user document (exactly what `save` persists).
    pub fn user_doc(&self) -> &Value {
        &self.user_doc
    }

    /// Look up a dotted key path (e.g. `terminal.backend`) in the merged
    /// tree. Numeric segments index into sequences (`providers.0.name`).
    pub fn get(&self, dotted: &str) -> Option<&Value> {
        get_nested(&self.root, dotted)
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
        self.get(dotted).and_then(|v| v.as_f64()).unwrap_or(default)
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
    /// Unset model = empty string (callers surface setup guidance).
    ///
    /// Note: `JOEY_INFERENCE_MODEL` is deliberately NOT consulted here —
    /// upstream applies `HERMES_INFERENCE_MODEL` only in the oneshot/TUI
    /// layers, so the CLI crate re-applies it at its own layer.
    pub fn model(&self) -> String {
        match self.get("model") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Mapping(_)) => self
                .get("model.default")
                .or_else(|| self.get("model.model"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            _ => String::new(),
        }
    }

    /// Override the effective model in memory only (per-invocation `--model`),
    /// without persisting to disk.
    pub fn set_model_override(&mut self, model: &str) {
        let _ = set_nested(&mut self.root, "model.default", Value::String(model.to_string()));
    }

    /// Set a dotted key and persist. Env-shaped keys (see
    /// [`is_env_config_key`]) route to `.env`; everything else lands in the
    /// USER document only (never the merged defaults tree), with upstream's
    /// set-time coercion guards and the `api_base` → `base_url` alias.
    pub fn set_and_save(&mut self, dotted: &str, raw_value: &str) -> Result<()> {
        if is_env_config_key(dotted) {
            return save_env_value(&dotted.to_uppercase(), raw_value);
        }
        let coerced = coerce_set_value(dotted, raw_value);
        if !self.user_doc.is_mapping() {
            self.user_doc = Value::Mapping(Mapping::new());
        }
        set_nested(&mut self.user_doc, dotted, coerced)?;
        // Normalize the api_base → base_url alias at set-time too, so a fresh
        // `joey config set model.api_base …` lands on the canonical key.
        let alias_norm = dotted.trim().to_lowercase();
        if alias_norm == "model.api_base" || alias_norm == "api_base" {
            self.user_doc = normalize_root_model_keys(self.user_doc.clone());
        }
        self.save()?;
        self.rebuild_root();
        Ok(())
    }

    /// Set a dotted key to an arbitrary YAML value and persist — the
    /// structured sibling of [`Config::set_and_save`] for callers that write
    /// non-scalar values (e.g. the setup wizard's `custom_providers` list).
    /// No coercion, no env routing.
    pub fn set_value_and_save(&mut self, dotted: &str, value: Value) -> Result<()> {
        if !self.user_doc.is_mapping() {
            self.user_doc = Value::Mapping(Mapping::new());
        }
        set_nested(&mut self.user_doc, dotted, value)?;
        self.save()?;
        self.rebuild_root();
        Ok(())
    }

    /// Remove a dotted key from the user document and persist. Returns
    /// whether anything was removed (port of `_unset_nested`).
    pub fn unset(&mut self, dotted: &str) -> Result<bool> {
        let removed = unset_nested(&mut self.user_doc, dotted);
        if removed {
            self.save()?;
            self.rebuild_root();
        }
        Ok(removed)
    }

    /// Persist ONLY the user document (plus `_config_version`) to disk.
    /// The merged defaults tree is never written.
    pub fn save(&self) -> Result<()> {
        let mut doc = match &self.user_doc {
            Value::Mapping(m) => m.clone(),
            _ => Mapping::new(),
        };
        doc.insert(
            Value::String("_config_version".to_string()),
            Value::Number(CONFIG_VERSION.into()),
        );
        utils::atomic_yaml_write(&self.path, &Value::Mapping(doc))?;
        secure_file(&self.path);
        Ok(())
    }

    fn rebuild_root(&mut self) {
        self.root = build_root(&self.user_doc);
        LAST_GOOD_BY_PATH
            .lock()
            .expect("config lkg lock")
            .insert(self.path.clone(), self.root.clone());
    }
}

// ─── Load pipeline ───────────────────────────────────────────────────────────

fn ignore_user_config() -> bool {
    // Upstream accepts exactly "1" (cli.py:431).
    std::env::var("JOEY_IGNORE_USER_CONFIG").as_deref() == Ok("1")
}

/// Read the raw user document. `Ok` for every readable state (missing,
/// empty, non-mapping → `{}`), `Err(parse error)` only for broken YAML.
fn read_user_doc(path: &Path) -> std::result::Result<Value, String> {
    if ignore_user_config() || !path.exists() {
        return Ok(Value::Mapping(Mapping::new()));
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Ok(Value::Mapping(Mapping::new())),
    };
    if text.trim().is_empty() {
        return Ok(Value::Mapping(Mapping::new()));
    }
    match serde_yaml::from_str::<Value>(&text) {
        Ok(v @ Value::Mapping(_)) => Ok(v),
        Ok(_) => Ok(Value::Mapping(Mapping::new())),
        Err(e) => Err(e.to_string()),
    }
}

/// Build the merged, normalized, expanded view from a user document. When
/// the user doc is empty because of a parse failure, the caller's fallback
/// is handled by `load_from` retaining the previous last-known-good root.
fn build_root(user_doc: &Value) -> Value {
    let mut merged = DEFAULTS.clone();
    let user = pre_move_root_max_turns(user_doc.clone());
    deep_merge(&mut merged, user);
    let normalized = normalize_root_model_keys(normalize_max_turns(merged));
    expand_env_vars(&normalized)
}

/// Load-time pre-merge normalization: a root-level `max_turns` in the user
/// document moves under `agent` before merging (config.py:7350-7355).
fn pre_move_root_max_turns(user: Value) -> Value {
    let mut map = match user {
        Value::Mapping(m) => m,
        other => return other,
    };
    let key = Value::String("max_turns".to_string());
    if let Some(root_val) = map.get(&key).cloned() {
        let agent_key = Value::String("agent".to_string());
        let mut agent = match map.get(&agent_key) {
            Some(Value::Mapping(m)) => m.clone(),
            _ => Mapping::new(),
        };
        let mt = Value::String("max_turns".to_string());
        let agent_has = matches!(agent.get(&mt), Some(v) if !v.is_null());
        if !agent_has {
            agent.insert(mt, root_val);
        }
        map.insert(agent_key, Value::Mapping(agent));
        map.remove(&key);
    }
    Value::Mapping(map)
}

/// Post-merge `max_turns` normalization (config.py:6945-6973). The merged
/// tree always carries `agent`, so this is mostly a no-op after the
/// pre-merge move — kept for shape fidelity.
fn normalize_max_turns(config: Value) -> Value {
    let mut map = match config {
        Value::Mapping(m) => m,
        other => return other,
    };
    let root_key = Value::String("max_turns".to_string());
    let agent_key = Value::String("agent".to_string());
    let mt = Value::String("max_turns".to_string());
    let mut agent = match map.get(&agent_key) {
        Some(Value::Mapping(m)) => m.clone(),
        _ => Mapping::new(),
    };
    let had_root = map.contains_key(&root_key);
    let had_agent = agent.contains_key(&mt);
    if had_root && !had_agent {
        if let Some(v) = map.get(&root_key).cloned() {
            agent.insert(mt.clone(), v);
        }
    }
    if (had_root || had_agent) && !agent.contains_key(&mt) {
        agent.insert(mt, Value::Number(90.into()));
    }
    map.insert(agent_key, Value::Mapping(agent));
    map.remove(&root_key);
    Value::Mapping(map)
}

/// Python-style truthiness for YAML scalars/collections.
fn value_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Sequence(s) => !s.is_empty(),
        Value::Mapping(m) => !m.is_empty(),
        Value::Tagged(t) => value_truthy(&t.value),
    }
}

fn skey(s: &str) -> Value {
    Value::String(s.to_string())
}

/// Move stale root-level `provider`/`base_url`/`context_length` into the
/// model section, alias `api_base` → `base_url`, and canonicalize the model
/// id to `model.default` (port of `_normalize_root_model_keys`,
/// config.py:6861-6942).
pub fn normalize_root_model_keys(config: Value) -> Value {
    let mut map = match config {
        Value::Mapping(m) => m,
        other => return other,
    };

    let model_in = map.get(skey("model")).cloned();
    let model_has_alias = matches!(
        &model_in,
        Some(Value::Mapping(m)) if m.get(skey("api_base")).map(value_truthy).unwrap_or(false)
    );
    let model_needs_canon = matches!(
        &model_in,
        Some(Value::Mapping(m))
            if m.get(skey("model")).map(value_truthy).unwrap_or(false)
                || m.get(skey("name")).map(value_truthy).unwrap_or(false)
    );
    let has_root = ["provider", "base_url", "context_length", "api_base"]
        .iter()
        .any(|k| map.get(skey(k)).map(value_truthy).unwrap_or(false));

    if !has_root && !model_has_alias && !model_needs_canon {
        return Value::Mapping(map);
    }

    let mut model: Mapping = match model_in {
        Some(Value::Mapping(m)) => m,
        Some(other) if value_truthy(&other) => {
            let mut m = Mapping::new();
            m.insert(skey("default"), other);
            m
        }
        _ => Mapping::new(),
    };

    for key in ["provider", "base_url", "context_length"] {
        let root_val = map.get(skey(key)).cloned();
        if let Some(v) = root_val {
            if value_truthy(&v) && !model.get(skey(key)).map(value_truthy).unwrap_or(false) {
                model.insert(skey(key), v);
            }
        }
        map.remove(skey(key));
    }

    // api_base is an alias for base_url, at the root OR inside model.
    for v in [map.get(skey("api_base")).cloned(), model.get(skey("api_base")).cloned()].into_iter().flatten() {
        if value_truthy(&v) && !model.get(skey("base_url")).map(value_truthy).unwrap_or(false) {
            model.insert(skey("base_url"), v);
        }
    }
    map.remove(skey("api_base"));
    model.remove(skey("api_base"));

    // Canonicalize the model id to `default`; `model` and `name` are
    // last-resort aliases (in that order), then dropped.
    let default_truthy = model.get(skey("default")).map(value_truthy).unwrap_or(false);
    if !default_truthy {
        let alias = model
            .get(skey("model"))
            .filter(|v| value_truthy(v))
            .or_else(|| model.get(skey("name")).filter(|v| value_truthy(v)))
            .cloned();
        if let Some(alias) = alias {
            model.insert(skey("default"), alias);
        }
    }
    if model.get(skey("default")).map(value_truthy).unwrap_or(false) {
        model.remove(skey("model"));
        model.remove(skey("name"));
    }

    map.insert(skey("model"), Value::Mapping(model));
    Value::Mapping(map)
}

/// Recursively expand `${VAR}` references in every string config value from
/// the process environment. Unresolved references are kept verbatim
/// (config.py:6664-6681).
pub fn expand_env_vars(value: &Value) -> Value {
    static VAR_RE: Lazy<regex::Regex> =
        Lazy::new(|| regex::Regex::new(r"\$\{([^}]+)\}").expect("var regex"));
    match value {
        Value::String(s) => {
            let expanded = VAR_RE.replace_all(s, |caps: &regex::Captures| {
                std::env::var(&caps[1]).unwrap_or_else(|_| caps[0].to_string())
            });
            Value::String(expanded.into_owned())
        }
        Value::Mapping(m) => {
            let mut out = Mapping::new();
            for (k, v) in m {
                out.insert(k.clone(), expand_env_vars(v));
            }
            Value::Mapping(out)
        }
        Value::Sequence(seq) => Value::Sequence(seq.iter().map(expand_env_vars).collect()),
        other => other.clone(),
    }
}

// ─── Parse-failure handling ──────────────────────────────────────────────────

/// Preserve a corrupted config.yaml as `config.yaml.corrupt.<ts>.bak`
/// (best-effort; symlinks are never followed, same-size siblings dedupe).
fn backup_corrupt_config(path: &Path) -> Option<PathBuf> {
    let meta = path.symlink_metadata().ok()?;
    if meta.file_type().is_symlink() {
        return None;
    }
    let size = meta.len();
    if size == 0 {
        return None;
    }
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let name = path.file_name()?.to_string_lossy().to_string();
    let backup_path = path.with_file_name(format!("{}.corrupt.{}.bak", name, ts));
    // Same-size sibling backup → assume this corruption is already preserved.
    if let Some(parent) = path.parent() {
        if let Ok(entries) = std::fs::read_dir(parent) {
            for entry in entries.flatten() {
                let fname = entry.file_name().to_string_lossy().to_string();
                if fname.starts_with(&format!("{}.corrupt.", name)) && fname.ends_with(".bak") {
                    if let Ok(m) = entry.metadata() {
                        if m.len() == size {
                            return None;
                        }
                    }
                }
            }
        }
    }
    if backup_path.exists() {
        return None;
    }
    std::fs::copy(path, &backup_path).ok()?;
    Some(backup_path)
}

fn warn_config_parse_failure(path: &Path, err: &str, has_lkg: bool) {
    let key = match path.metadata() {
        Ok(m) => (
            path.display().to_string(),
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            m.len(),
        ),
        Err(_) => (path.display().to_string(), 0, 0),
    };
    {
        let mut warned = PARSE_WARNED.lock().expect("parse warn lock");
        if !warned.insert(key) {
            return;
        }
    }
    let backup = backup_corrupt_config(path);
    let mut msg = if has_lkg {
        format!(
            "Failed to parse {}: {}. Keeping the previously loaded config for this process — \
             edits to config.yaml are being IGNORED until the YAML is fixed.",
            path.display(),
            err
        )
    } else {
        format!(
            "Failed to parse {}: {}. Falling back to default config — every user override \
             (auxiliary providers, fallback chain, model settings) is being IGNORED. \
             Fix the YAML and restart.",
            path.display(),
            err
        )
    };
    if let Some(bp) = backup {
        msg.push_str(&format!(" A copy of the corrupted file was saved to {}.", bp.display()));
    }
    tracing::warn!("{}", msg);
    eprintln!("Warning: {}", msg);
}

// ─── Dotted-path navigation (dict + list, port of _get/_set/_unset_nested) ──

/// Dotted-path lookup with numeric list indexing.
pub fn get_nested<'a>(root: &'a Value, dotted: &str) -> Option<&'a Value> {
    let mut cur = root;
    for part in dotted.split('.') {
        cur = match cur {
            Value::Mapping(m) => m.get(skey(part))?,
            Value::Sequence(seq) => {
                let idx: usize = part.parse().ok()?;
                seq.get(idx)?
            }
            _ => return None,
        };
    }
    Some(cur)
}

/// Set a value at an arbitrarily nested dotted key path.
///
/// Dict segments create intermediate dicts on demand (replacing scalar
/// leaves); list segments are parsed as numeric indices and must already
/// exist — lists are never grown or destroyed (upstream #17876 guard).
pub fn set_nested(root: &mut Value, dotted: &str, value: Value) -> Result<()> {
    let parts: Vec<&str> = dotted.split('.').collect();
    let mut cur = root;
    for part in &parts[..parts.len().saturating_sub(1)] {
        match cur {
            Value::Sequence(seq) => {
                let idx: usize = part.parse().map_err(|_| {
                    anyhow::anyhow!(
                        "Cannot navigate into list at key '{}': segment '{}' is not a numeric index",
                        dotted,
                        part
                    )
                })?;
                let len = seq.len();
                cur = seq
                    .get_mut(idx)
                    .ok_or_else(|| anyhow::anyhow!("list index {} out of range (len {})", idx, len))?;
            }
            Value::Mapping(m) => {
                let key = skey(part);
                let needs_fresh = !matches!(m.get(&key), Some(Value::Mapping(_)) | Some(Value::Sequence(_)));
                if needs_fresh {
                    m.insert(key.clone(), Value::Mapping(Mapping::new()));
                }
                cur = m.get_mut(&key).expect("just inserted");
            }
            other => {
                bail!(
                    "Cannot navigate into {} at key '{}'",
                    value_type_name(other),
                    dotted
                );
            }
        }
    }
    let last = parts[parts.len() - 1];
    match cur {
        Value::Sequence(seq) => {
            let idx: usize = last
                .parse()
                .map_err(|_| anyhow::anyhow!("segment '{}' is not a numeric index", last))?;
            let len = seq.len();
            let slot = seq
                .get_mut(idx)
                .ok_or_else(|| anyhow::anyhow!("list index {} out of range (len {})", idx, len))?;
            *slot = value;
        }
        Value::Mapping(m) => {
            m.insert(skey(last), value);
        }
        other => {
            bail!("Cannot set into {} at key '{}'", value_type_name(other), dotted);
        }
    }
    Ok(())
}

fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "str",
        Value::Sequence(_) => "list",
        Value::Mapping(_) => "dict",
        Value::Tagged(_) => "tagged",
    }
}

/// Remove a dotted-path value; prunes empty dict containers left behind
/// (preserving user-authored empty lists). Returns whether removal happened.
pub fn unset_nested(root: &mut Value, dotted: &str) -> bool {
    fn remove_at(cur: &mut Value, parts: &[&str]) -> Option<bool> {
        if parts.len() == 1 {
            return match cur {
                Value::Sequence(seq) => {
                    let idx: usize = parts[0].parse().ok()?;
                    if idx < seq.len() {
                        seq.remove(idx);
                        Some(true)
                    } else {
                        None
                    }
                }
                Value::Mapping(m) => m.remove(skey(parts[0])).map(|_| true),
                _ => None,
            };
        }
        let (head, rest) = (parts[0], &parts[1..]);
        let child = match cur {
            Value::Sequence(seq) => {
                let idx: usize = head.parse().ok()?;
                seq.get_mut(idx)?
            }
            Value::Mapping(m) => m.get_mut(skey(head))?,
            _ => return None,
        };
        let removed = remove_at(child, rest)?;
        // Prune the child if the deletion left an empty dict behind.
        let child_is_empty_map = matches!(child, Value::Mapping(m) if m.is_empty());
        if removed && child_is_empty_map {
            match cur {
                Value::Sequence(seq) => {
                    if let Ok(idx) = head.parse::<usize>() {
                        if idx < seq.len() {
                            seq.remove(idx);
                        }
                    }
                }
                Value::Mapping(m) => {
                    m.remove(skey(head));
                }
                _ => {}
            }
        }
        Some(removed)
    }
    let parts: Vec<&str> = dotted.split('.').collect();
    if parts.is_empty() {
        return false;
    }
    remove_at(root, &parts).unwrap_or(false)
}

/// Deep-merge `overlay` onto `base`: mappings merge key-by-key; a Null
/// override of a mapping default is IGNORED (an empty `section:` key in
/// YAML parses as null and must not wipe the section — config.py:6634-6635);
/// every other value type (including sequences) is replaced wholesale.
pub fn deep_merge(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Mapping(base_map), Value::Mapping(overlay_map)) => {
            for (k, v) in overlay_map {
                match base_map.get_mut(&k) {
                    Some(existing) => {
                        if existing.is_mapping() && v.is_null() {
                            continue;
                        }
                        deep_merge(existing, v);
                    }
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

// ─── .env routing + set-time coercion ────────────────────────────────────────

/// Exact allowlist of env-var names `config set` routes to `.env`
/// (config.py:4862-4874).
const ENV_API_KEYS: &[&str] = &[
    "OPENROUTER_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "VOICE_TOOLS_OPENAI_KEY",
    "EXA_API_KEY",
    "PARALLEL_API_KEY",
    "FIRECRAWL_API_KEY",
    "FIRECRAWL_API_URL",
    "FIRECRAWL_GATEWAY_URL",
    "TOOL_GATEWAY_DOMAIN",
    "TOOL_GATEWAY_SCHEME",
    "TOOL_GATEWAY_USER_TOKEN",
    "TAVILY_API_KEY",
    "BROWSERBASE_API_KEY",
    "BROWSERBASE_PROJECT_ID",
    "BROWSER_USE_API_KEY",
    "FAL_KEY",
    "TELEGRAM_BOT_TOKEN",
    "DISCORD_BOT_TOKEN",
    "TERMINAL_SSH_HOST",
    "TERMINAL_SSH_USER",
    "TERMINAL_SSH_KEY",
    "SUDO_PASSWORD",
    "SLACK_BOT_TOKEN",
    "SLACK_APP_TOKEN",
    "GITHUB_TOKEN",
    "HONCHO_API_KEY",
];

/// Return whether `config set` routes this key to `.env` (port of
/// `_is_env_config_key`, config.py:4858-4878). Dotted keys NEVER route:
/// `model.api_key` belongs in config.yaml.
pub fn is_env_config_key(key: &str) -> bool {
    if key.contains('.') {
        return false;
    }
    let key_upper = key.to_uppercase();
    ENV_API_KEYS.contains(&key_upper.as_str())
        || key_upper.ends_with("_API_KEY")
        || key_upper.ends_with("_TOKEN")
        || key_upper.starts_with("TERMINAL_SSH")
}

/// The leaf default declared for `dotted` in DEFAULT_CONFIG (None for
/// unknown keys and non-leaf paths) — port of `_default_value_for_key`.
fn default_value_for_key(dotted: &str) -> Option<&'static Value> {
    let mut node: &Value = &DEFAULTS;
    for part in dotted.split('.') {
        node = match node {
            Value::Mapping(m) => m.get(skey(part))?,
            _ => return None,
        };
    }
    if node.is_mapping() {
        None
    } else {
        Some(node)
    }
}

/// Set-time coercion with upstream guards (config.py:8789-8799):
/// never coerce when the schema default at the path is a string (enum
/// members like `approvals.mode: "off"` must not become YAML booleans);
/// bools only from the word sets; ints via isdigit (no negatives/exponents).
fn coerce_set_value(key: &str, value: &str) -> Value {
    if matches!(default_value_for_key(key), Some(Value::String(_))) {
        return Value::String(value.to_string());
    }
    let lower = value.to_lowercase();
    if matches!(lower.as_str(), "true" | "yes" | "on") {
        return Value::Bool(true);
    }
    if matches!(lower.as_str(), "false" | "no" | "off") {
        return Value::Bool(false);
    }
    let is_digits = !value.is_empty() && value.chars().all(|c| c.is_ascii_digit());
    if is_digits {
        if let Ok(i) = value.parse::<i64>() {
            return Value::Number(i.into());
        }
    }
    let float_form = value.replacen('.', "", 1);
    if !float_form.is_empty() && float_form.chars().all(|c| c.is_ascii_digit()) && value.contains('.')
    {
        if let Ok(f) = value.parse::<f64>() {
            return Value::Number(serde_yaml::Number::from(f));
        }
    }
    Value::String(value.to_string())
}

pub fn value_as_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

// ─── .env loader (port of hermes_cli/env_loader.py) ─────────────────────────

/// Env var name suffixes that indicate credential values — the only vars
/// whose values are sanitized on load.
const CREDENTIAL_SUFFIXES: &[&str] = &["_API_KEY", "_TOKEN", "_SECRET", "_KEY"];

static WARNED_CRED_KEYS: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

/// Load joey environment files with user config taking precedence
/// (port of `load_hermes_dotenv`, env_loader.py:288-338):
/// - `~/.joey/.env` OVERRIDES stale shell-exported values when present.
/// - `~/.joey/.op.env` loads after it WITHOUT override, and only when
///   `OP_SERVICE_ACCOUNT_TOKEN` isn't already set.
/// - a project `.env` acts as a dev fallback: fills missing values when the
///   user env exists, overrides stale shell vars when it doesn't.
///
/// Corrupt files are pre-sanitized (BOM/NUL stripping, concatenated-line
/// splitting for known keys) and credential values are scrubbed to ASCII.
/// Returns the list of files loaded.
pub fn load_joey_dotenv(joey_home: Option<&Path>, project_env: Option<&Path>) -> Vec<PathBuf> {
    let mut loaded: Vec<PathBuf> = Vec::new();
    let home: PathBuf = match joey_home {
        Some(p) => p.to_path_buf(),
        None => match std::env::var("JOEY_HOME") {
            Ok(v) if !v.trim().is_empty() => PathBuf::from(v.trim()),
            _ => constants::user_home_dir().join(crate::branding::HOME_DIR_NAME),
        },
    };
    let user_env = home.join(".env");

    if user_env.exists() {
        sanitize_env_file_if_needed(&user_env);
    }
    if let Some(pe) = project_env {
        if pe.exists() {
            sanitize_env_file_if_needed(pe);
        }
    }

    if user_env.exists() {
        load_dotenv_file(&user_env, true);
        loaded.push(user_env.clone());
    }

    // .op.env AFTER .env so .env values win; bootstrap token only.
    let op_env = home.join(".op.env");
    if op_env.exists()
        && std::env::var("OP_SERVICE_ACCOUNT_TOKEN")
            .map(|v| v.is_empty())
            .unwrap_or(true)
    {
        load_dotenv_file(&op_env, false);
    }

    if let Some(pe) = project_env {
        if pe.exists() {
            load_dotenv_file(pe, loaded.is_empty());
            loaded.push(pe.to_path_buf());
        }
    }

    sanitize_loaded_credentials();
    loaded
}

/// Parse a dotenv file and apply it to the process environment.
/// `override=true` — user `.env` beats already-set shell vars (upstream
/// `load_dotenv(..., override=True)`); dotenvy's own loader never
/// overrides, so entries are applied manually.
fn load_dotenv_file(path: &Path, override_existing: bool) {
    let iter = match dotenvy::from_path_iter(path) {
        Ok(it) => it,
        Err(_) => return,
    };
    for item in iter {
        let Ok((key, value)) = item else { continue };
        if override_existing || std::env::var_os(&key).is_none() {
            std::env::set_var(&key, &value);
        }
    }
}

/// Strip non-ASCII characters from credential env vars in the process env
/// (env_loader.py:118-159). Warns once per key on stderr.
fn sanitize_loaded_credentials() {
    for (key, value) in std::env::vars() {
        if !CREDENTIAL_SUFFIXES.iter().any(|s| key.ends_with(s)) {
            continue;
        }
        if value.is_ascii() {
            continue;
        }
        let cleaned: String = value.chars().filter(|c| c.is_ascii()).collect();
        std::env::set_var(&key, &cleaned);
        {
            let mut warned = WARNED_CRED_KEYS.lock().expect("cred warn lock");
            if !warned.insert(key.clone()) {
                continue;
            }
        }
        let stripped = value.chars().count() - cleaned.chars().count();
        eprintln!(
            "  Warning: {} contained {} non-ASCII character{} — stripped so the key can be sent as an HTTP header.",
            key,
            stripped,
            if stripped != 1 { "s" } else { "" },
        );
        eprintln!(
            "  This usually means the key was copy-pasted from a PDF, rich-text editor, or web page that substituted lookalike\n  Unicode glyphs for ASCII letters. If authentication fails (e.g. \"API key not valid\"), re-copy the key from the\n  provider's dashboard and run `joey setup` (or edit the .env file in a plain-text editor)."
        );
    }
}

const STRUCTURED_VALUE_MARKERS: &[&str] = &["://", "?", "&"];

/// True when `value` looks like a URL/query string or holds whitespace —
/// treated as one opaque secret by the concatenation splitter.
fn looks_like_structured_value(value: &str) -> bool {
    STRUCTURED_VALUE_MARKERS.iter().any(|m| value.contains(m))
        || value.chars().any(|c| c.is_whitespace())
}

/// The known-keys set used to split concatenated `.env` lines. Upstream
/// derives this from its full `OPTIONAL_ENV_VARS` catalog; the port uses
/// the `.env` routing allowlist plus the always-present provider keys —
/// a subset covering the credentials joey itself writes.
fn known_env_keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = ENV_API_KEYS.to_vec();
    keys.extend_from_slice(&[
        "OPENAI_BASE_URL",
        "ANTHROPIC_TOKEN",
        "TERMINAL_ENV",
        "TERMINAL_SSH_PORT",
    ]);
    keys
}

/// Fix corrupted .env lines before reading or writing (port of
/// `_sanitize_env_lines`): splits concatenated KEY=VALUE pairs on a single
/// line, guarded so structured values (URLs, query strings) never split.
fn sanitize_env_lines(lines: &[String]) -> Vec<String> {
    let known = known_env_keys();
    let mut sanitized: Vec<String> = Vec::with_capacity(lines.len());

    for line in lines {
        let raw = line.trim_end_matches(['\r', '\n']);
        let stripped = raw.trim();

        if stripped.is_empty() || stripped.starts_with('#') {
            sanitized.push(format!("{}\n", raw));
            continue;
        }

        // Collect KEY= needle ranges; drop matches fully contained within a
        // longer overlapping needle (suffix collisions like LM_ in GLM_).
        let mut match_ranges: Vec<(usize, usize)> = Vec::new();
        for key in &known {
            let needle = format!("{}=", key);
            let mut start = 0;
            while let Some(pos) = stripped[start..].find(&needle) {
                let abs = start + pos;
                match_ranges.push((abs, abs + needle.len()));
                start = abs + needle.len();
            }
        }
        let mut split_positions: Vec<usize> = match_ranges
            .iter()
            .filter(|&&(s, e)| {
                !match_ranges
                    .iter()
                    .any(|&(s2, e2)| s2 <= s && e2 >= e && (s2, e2) != (s, e))
            })
            .map(|&(s, _)| s)
            .collect();
        split_positions.sort_unstable();
        split_positions.dedup();

        let mut split_into_entries = false;
        let mut segments: Vec<&str> = Vec::new();
        if split_positions.len() > 1 && split_positions[0] == 0 {
            for (i, pos) in split_positions.iter().enumerate() {
                let end = split_positions.get(i + 1).copied().unwrap_or(stripped.len());
                segments.push(&stripped[*pos..end]);
            }
            split_into_entries = segments[..segments.len() - 1].iter().all(|seg| {
                let value = seg.split_once('=').map(|(_, v)| v).unwrap_or("");
                !looks_like_structured_value(value)
            });
        }

        if split_into_entries {
            for seg in segments {
                let part = seg.trim();
                if !part.is_empty() {
                    sanitized.push(format!("{}\n", part));
                }
            }
        } else {
            sanitized.push(format!("{}\n", stripped));
        }
    }
    sanitized
}

/// Pre-sanitize a .env file before dotenv parses it: BOM strip, NUL strip,
/// concatenated-line split. UTF-16/UTF-32 recoding is not ported (report:
/// requires BOM-aware transcoding; files are read lossily as UTF-8).
fn sanitize_env_file_if_needed(path: &Path) {
    let Ok(raw) = std::fs::read(path) else { return };
    // Strip a UTF-8 BOM if present (utf-8-sig semantics).
    let body = raw.strip_prefix(b"\xef\xbb\xbf".as_slice()).unwrap_or(&raw);
    let text = String::from_utf8_lossy(body);
    if text.starts_with('\u{fffd}') {
        // Undecodable leading bytes — leave the file untouched rather than
        // persist the mangling (upstream guard).
        return;
    }
    let original: Vec<String> = text.split_inclusive('\n').map(|s| s.to_string()).collect();
    let stripped: Vec<String> = original.iter().map(|l| l.replace('\0', "")).collect();
    let sanitized = sanitize_env_lines(&stripped);
    if sanitized != original {
        let joined = sanitized.concat();
        let _ = utils::atomic_replace(path, joined.as_bytes());
    }
}

// ─── .env writer (port of save_env_value / remove_env_value) ────────────────

static ENV_NAME_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("env name regex"));

/// Env var names that influence how the next subprocess executes — never
/// writable through the env writer (port of `_ENV_VAR_NAME_DENYLIST`,
/// with the runtime-location names rebranded).
const ENV_VAR_NAME_DENYLIST: &[&str] = &[
    // Loader / linker
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
    "LD_DEBUG",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "DYLD_FRAMEWORK_PATH",
    "DYLD_FALLBACK_LIBRARY_PATH",
    "DYLD_FALLBACK_FRAMEWORK_PATH",
    // Python
    "PYTHONPATH",
    "PYTHONHOME",
    "PYTHONSTARTUP",
    "PYTHONUSERBASE",
    "PYTHONEXECUTABLE",
    "PYTHONNOUSERSITE",
    // Node
    "NODE_OPTIONS",
    "NODE_PATH",
    // General
    "PATH",
    "SHELL",
    "BROWSER",
    "EDITOR",
    "VISUAL",
    "PAGER",
    // Git
    "GIT_SSH_COMMAND",
    "GIT_EXEC_PATH",
    "GIT_SHELL",
    // Joey runtime location — never via the env writer.
    "JOEY_HOME",
    "JOEY_PROFILE",
    "JOEY_CONFIG",
    "JOEY_ENV",
];

/// Quote .env values containing characters with special dotenv meaning
/// (port of `_quote_env_value`, config.py:7921).
fn quote_env_value(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let needs_quoting = value.contains('#')
        || value.contains('"')
        || value.contains('\'')
        || value != value.trim()
        || value.chars().any(|c| c.is_whitespace());
    if !needs_quoting {
        return value.to_string();
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{}\"", escaped)
}

/// True when a .env line assigns `key` — plain or `export`-prefixed.
fn env_line_defines_key(line: &str, key: &str) -> bool {
    let mut stripped = line.trim();
    if let Some(rest) = stripped.strip_prefix("export ") {
        stripped = rest.trim_start();
    }
    stripped.starts_with(&format!("{}=", key))
}

/// Warn and strip non-ASCII characters from a credential value
/// (port of `_check_non_ascii_credential`).
fn check_non_ascii_credential(key: &str, value: &str) -> String {
    if value.is_ascii() {
        return value.to_string();
    }
    let sanitized: String = value.chars().filter(|c| c.is_ascii()).collect();
    let mut bad: Vec<String> = Vec::new();
    for (i, ch) in value.chars().enumerate() {
        if !ch.is_ascii() {
            bad.push(format!("  position {}: {:?} (U+{:04X})", i, ch, ch as u32));
        }
    }
    let more = if bad.len() > 5 { "\n  ... and more" } else { "" };
    eprintln!(
        "\n  Warning: {} contains non-ASCII characters that will break API requests.\n  This usually happens when copy-pasting from a PDF, rich-text editor,\n  or web page that substitutes lookalike Unicode glyphs for ASCII letters.\n\n{}{}\n\n  The non-ASCII characters have been stripped automatically.\n  If authentication fails, re-copy the key from the provider's dashboard.\n",
        key,
        bad.iter().take(5).map(|l| format!("  {}", l)).collect::<Vec<_>>().join("\n"),
        more,
    );
    sanitized
}

fn secure_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.exists() {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Save or update a value in `~/.joey/.env` (port of `save_env_value`,
/// config.py:7956-8047): name validation, denylist, newline strip,
/// non-ASCII scrub, quoting, `export KEY=` recognition, atomic write with
/// mode preservation (0600 default) — then update the process env.
pub fn save_env_value(key: &str, value: &str) -> Result<()> {
    if !ENV_NAME_RE.is_match(key) {
        bail!("Invalid environment variable name: {:?}", key);
    }
    if ENV_VAR_NAME_DENYLIST.contains(&key) {
        bail!(
            "Environment variable {:?} is on the writer denylist. Names that influence \
             subprocess execution (LD_PRELOAD, PYTHONPATH, PATH, EDITOR, ...) or Joey \
             runtime location (JOEY_HOME, JOEY_PROFILE, ...) cannot be persisted via the \
             env writer. If you really need this, edit ~/.joey/.env directly.",
            key
        );
    }
    let value = value.replace(['\n', '\r'], "");
    let value = check_non_ascii_credential(key, &value);

    let env_path = constants::env_path();
    let mut lines: Vec<String> = if env_path.exists() {
        let raw = std::fs::read(&env_path).unwrap_or_default();
        let body = raw.strip_prefix(b"\xef\xbb\xbf".as_slice()).unwrap_or(&raw);
        let text = String::from_utf8_lossy(body).to_string();
        let split: Vec<String> = text.split_inclusive('\n').map(|s| s.to_string()).collect();
        sanitize_env_lines(&split)
    } else {
        Vec::new()
    };

    let original_mode = file_mode(&env_path);
    let serialized = quote_env_value(&value);
    let new_line = format!("{}={}\n", key, serialized);
    let mut found = false;
    for line in lines.iter_mut() {
        if env_line_defines_key(line, key) {
            *line = new_line.clone();
            found = true;
            break;
        }
    }
    if !found {
        if let Some(last) = lines.last_mut() {
            if !last.ends_with('\n') {
                last.push('\n');
            }
        }
        lines.push(new_line);
    }

    let body = lines.concat();
    utils::atomic_replace(&env_path, body.as_bytes())
        .with_context(|| format!("writing {}", env_path.display()))?;
    match original_mode {
        Some(mode) => restore_mode(&env_path, mode),
        None => secure_file(&env_path),
    }

    std::env::set_var(key, &value);
    Ok(())
}

/// Remove a key from `~/.joey/.env` and the process env. Returns whether
/// the key was found and removed.
pub fn remove_env_value(key: &str) -> Result<bool> {
    let env_path = constants::env_path();
    if !env_path.exists() {
        std::env::remove_var(key);
        return Ok(false);
    }
    let raw = std::fs::read(&env_path).unwrap_or_default();
    let text = String::from_utf8_lossy(&raw).to_string();
    let lines: Vec<String> = text.split_inclusive('\n').map(|s| s.to_string()).collect();
    let kept: Vec<String> = lines
        .iter()
        .filter(|l| !env_line_defines_key(l, key))
        .cloned()
        .collect();
    let removed = kept.len() != lines.len();
    if removed {
        let original_mode = file_mode(&env_path);
        utils::atomic_replace(&env_path, kept.concat().as_bytes())?;
        match original_mode {
            Some(mode) => restore_mode(&env_path, mode),
            None => secure_file(&env_path),
        }
    }
    std::env::remove_var(key);
    Ok(removed)
}

#[cfg(unix)]
fn file_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.mode() & 0o7777)
}

#[cfg(not(unix))]
fn file_mode(_path: &Path) -> Option<u32> {
    None
}

#[cfg(unix)]
fn restore_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn restore_mode(_path: &Path, _mode: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn cfg_from(yaml: &str) -> Config {
        let user_doc: Value = if yaml.trim().is_empty() {
            Value::Mapping(Mapping::new())
        } else {
            serde_yaml::from_str(yaml).unwrap()
        };
        let root = build_root(&user_doc);
        Config {
            user_doc,
            root,
            path: PathBuf::from("/nonexistent/config.yaml"),
        }
    }

    #[test]
    fn defaults_match_upstream() {
        let cfg = Config::defaults();
        // Local deviation: upstream ships model.default "" + provider "auto"
        // (unset → first-run setup). This install bakes Z.AI/GLM as the
        // out-of-box default (DEFAULT_CONFIG_YAML); see PORTING.md.
        assert_eq!(cfg.model(), "glm-5.2");
        assert_eq!(cfg.get_str("model.base_url", "x"), "");
        assert_eq!(cfg.get_str("model.provider", "x"), "zai");
        assert_eq!(cfg.get_i64("agent.max_turns", 0), 90);
        assert!(cfg.get("agent.reasoning_effort").is_none(), "no reasoning_effort default");
        assert!(cfg.get_bool("display.show_reasoning", false));
        assert!(!cfg.get_bool("display.streaming", true), "config-layer streaming default is false");
        assert_eq!(cfg.get_i64("skills.creation_nudge_interval", 0), 10);
        assert_eq!(cfg.get_str("cron.provider", "x"), "");
        assert!(cfg.get("memory.flush_min_turns").is_none());
        assert_eq!(cfg.get_str("logging.level", ""), "INFO");
        assert_eq!(cfg.get_i64("logging.max_size_mb", 0), 5);
        assert_eq!(cfg.get_i64("logging.backup_count", 0), 3);
        assert_eq!(cfg.get_str("timezone", "x"), "");
        assert!(cfg.get_bool("security.redact_secrets", false));
        assert_eq!(cfg.get_str("approvals.mode", ""), "smart");
        assert!(cfg.get("agent.verbose").is_none(), "agent.verbose is cli-tree-only upstream");
    }

    #[test]
    fn dotenv_override_semantics() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "JOEY_TEST_DOTENV_A=file_value\n").unwrap();
        // Pre-set a stale shell value — the user .env must OVERRIDE it.
        std::env::set_var("JOEY_TEST_DOTENV_A", "shell_value");
        load_joey_dotenv(Some(dir.path()), None);
        assert_eq!(std::env::var("JOEY_TEST_DOTENV_A").unwrap(), "file_value");
        std::env::remove_var("JOEY_TEST_DOTENV_A");

        // Project env only fills missing values when the user env exists.
        let proj = dir.path().join("proj.env");
        std::fs::write(&proj, "JOEY_TEST_DOTENV_A=proj_value\nJOEY_TEST_DOTENV_B=proj_b\n").unwrap();
        std::env::remove_var("JOEY_TEST_DOTENV_B");
        load_joey_dotenv(Some(dir.path()), Some(&proj));
        assert_eq!(std::env::var("JOEY_TEST_DOTENV_A").unwrap(), "file_value");
        assert_eq!(std::env::var("JOEY_TEST_DOTENV_B").unwrap(), "proj_b");
        std::env::remove_var("JOEY_TEST_DOTENV_A");
        std::env::remove_var("JOEY_TEST_DOTENV_B");
    }

    #[test]
    fn dotenv_export_lines_and_credential_scrub() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".env"),
            "export JOEY_TEST_EXPORTED_TOKEN=abc\u{028B}def\n",
        )
        .unwrap();
        load_joey_dotenv(Some(dir.path()), None);
        // export prefix recognized AND non-ASCII scrubbed from *_TOKEN.
        assert_eq!(std::env::var("JOEY_TEST_EXPORTED_TOKEN").unwrap(), "abcdef");
        std::env::remove_var("JOEY_TEST_EXPORTED_TOKEN");
    }

    #[test]
    fn var_expansion_in_all_string_values() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("JOEY_TEST_EXPANSION_VAR", "expanded!");
        let cfg = cfg_from("model:\n  base_url: \"${JOEY_TEST_EXPANSION_VAR}\"\nnested:\n  deep:\n    - \"${JOEY_TEST_EXPANSION_VAR}/x\"\n");
        assert_eq!(cfg.get_str("model.base_url", ""), "expanded!");
        assert_eq!(cfg.get_str("nested.deep.0", ""), "expanded!/x");
        // Unresolved refs kept verbatim.
        let cfg = cfg_from("k: \"${JOEY_TEST_DOES_NOT_EXIST_XYZ}\"\n");
        assert_eq!(cfg.get_str("k", ""), "${JOEY_TEST_DOES_NOT_EXIST_XYZ}");
        std::env::remove_var("JOEY_TEST_EXPANSION_VAR");
    }

    #[test]
    fn save_writes_only_user_keys() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "display:\n  compact: true\n").unwrap();
        let mut cfg = Config::load_from(path.clone()).unwrap();
        cfg.set_and_save("agent.max_turns", "42").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let doc: Value = serde_yaml::from_str(&text).unwrap();
        let map = doc.as_mapping().unwrap();
        // Only the user's keys + _config_version — never the defaults tree.
        assert!(map.contains_key(skey("display")));
        assert!(map.contains_key(skey("agent")));
        assert!(map.contains_key(skey("_config_version")));
        assert_eq!(map.len(), 3, "no default contamination: {:?}", map);
        assert_eq!(doc["_config_version"].as_i64(), Some(33));
        assert_eq!(doc["agent"]["max_turns"].as_i64(), Some(42));

        // Round-trip: reload sees merged values.
        let cfg2 = Config::load_from(path).unwrap();
        assert_eq!(cfg2.get_i64("agent.max_turns", 0), 42);
        assert!(cfg2.get_bool("display.compact", false));
        assert_eq!(cfg2.get_i64("agent.api_max_retries", 0), 3, "defaults still merged for reads");
    }

    #[test]
    fn env_routing_predicate_table() {
        // Exact allowlist entries.
        assert!(is_env_config_key("OPENROUTER_API_KEY"));
        assert!(is_env_config_key("openrouter_api_key"));
        assert!(is_env_config_key("GITHUB_TOKEN"));
        assert!(is_env_config_key("SUDO_PASSWORD"));
        assert!(is_env_config_key("FAL_KEY"));
        // Suffix families.
        assert!(is_env_config_key("MY_CUSTOM_API_KEY"));
        assert!(is_env_config_key("SOMETHING_TOKEN"));
        // TERMINAL_SSH prefix family.
        assert!(is_env_config_key("TERMINAL_SSH_HOST"));
        assert!(is_env_config_key("terminal_ssh_anything"));
        // Dotted keys NEVER route to .env.
        assert!(!is_env_config_key("model.api_key"));
        assert!(!is_env_config_key("providers.custom.api_key"));
        assert!(!is_env_config_key("auxiliary.vision.api_key"));
        // Non-credential names stay in config.yaml.
        assert!(!is_env_config_key("SUDO_PASS"));
        assert!(!is_env_config_key("MY_KEY"));
        assert!(!is_env_config_key("terminal"));
        assert!(!is_env_config_key("PASSWORD"));
    }

    #[test]
    fn coercion_guards() {
        // String-typed schema default → never coerced.
        assert_eq!(coerce_set_value("approvals.mode", "off"), Value::String("off".into()));
        assert_eq!(coerce_set_value("approvals.mode", "true"), Value::String("true".into()));
        assert_eq!(coerce_set_value("logging.level", "DEBUG"), Value::String("DEBUG".into()));
        // Bool words for non-string keys.
        assert_eq!(coerce_set_value("compression.enabled", "off"), Value::Bool(false));
        assert_eq!(coerce_set_value("compression.enabled", "Yes"), Value::Bool(true));
        // isdigit ints: no negatives, no exponents.
        assert_eq!(coerce_set_value("agent.max_turns", "120"), Value::Number(120.into()));
        assert_eq!(coerce_set_value("agent.max_turns", "-3"), Value::String("-3".into()));
        assert_eq!(coerce_set_value("agent.max_turns", "1e3"), Value::String("1e3".into()));
        // Floats via single-dot removal.
        assert_eq!(
            coerce_set_value("compression.threshold", "0.75"),
            Value::Number(serde_yaml::Number::from(0.75))
        );
        // Unknown keys keep best-effort coercion.
        assert_eq!(coerce_set_value("some.unknown", "true"), Value::Bool(true));
        assert_eq!(coerce_set_value("some.unknown", "17"), Value::Number(17.into()));
    }

    #[test]
    fn null_section_override_is_ignored() {
        // `terminal:` with no value parses as null and must NOT wipe the
        // default section.
        let cfg = cfg_from("terminal:\n");
        assert_eq!(cfg.get_str("terminal.backend", "gone"), "local");
        // But a null override of a scalar replaces it.
        let cfg = cfg_from("timezone:\n");
        assert!(cfg.get("timezone").unwrap().is_null());
    }

    #[test]
    fn list_index_paths_get_and_set() {
        let mut doc: Value =
            serde_yaml::from_str("custom_providers:\n  - name: a\n    api_key: k1\n  - name: b\n").unwrap();
        // Get by index.
        assert_eq!(get_nested(&doc, "custom_providers.0.name").unwrap().as_str(), Some("a"));
        // Set by index does not destroy the list.
        set_nested(&mut doc, "custom_providers.1.name", Value::String("c".into())).unwrap();
        assert_eq!(get_nested(&doc, "custom_providers.1.name").unwrap().as_str(), Some("c"));
        assert!(doc["custom_providers"].is_sequence(), "list survives indexed set");
        assert_eq!(doc["custom_providers"].as_sequence().unwrap().len(), 2);
        // Non-numeric segment into a list errors.
        assert!(set_nested(&mut doc, "custom_providers.x.name", Value::Null).is_err());
        // Whole-index replacement.
        set_nested(&mut doc, "custom_providers.0", Value::String("plain".into())).unwrap();
        assert_eq!(get_nested(&doc, "custom_providers.0").unwrap().as_str(), Some("plain"));
    }

    #[test]
    fn unset_prunes_empty_dicts() {
        let mut doc: Value = serde_yaml::from_str("a:\n  b:\n    c: 1\nkeep: []\n").unwrap();
        assert!(unset_nested(&mut doc, "a.b.c"));
        // Empty dict containers pruned...
        assert!(get_nested(&doc, "a").is_none());
        // ...but user-authored empty lists preserved.
        assert!(get_nested(&doc, "keep").unwrap().is_sequence());
        assert!(!unset_nested(&mut doc, "nope.deep"));
    }

    #[test]
    fn root_model_normalization() {
        // Root provider/base_url move under model; api_base aliases.
        let cfg = cfg_from("provider: openrouter\napi_base: https://x.test/v1\nmodel: my/model\n");
        assert_eq!(cfg.get_str("model.provider", ""), "openrouter");
        assert_eq!(cfg.get_str("model.base_url", ""), "https://x.test/v1");
        assert_eq!(cfg.model(), "my/model");
        assert!(cfg.get("api_base").is_none());

        // model.name canonicalizes to model.default. Tested on the raw doc:
        // the baked-in local default model is truthy, so promotion is
        // (correctly) masked in the merged view when the user doc has no
        // explicit model.default.
        let doc: Value = serde_yaml::from_str("model:\n  name: some/model\n").unwrap();
        let norm = normalize_root_model_keys(doc);
        assert_eq!(
            get_nested(&norm, "model.default").and_then(|v| v.as_str()),
            Some("some/model")
        );
        assert!(get_nested(&norm, "model.name").is_none());

        // Root max_turns moves under agent.
        let cfg = cfg_from("max_turns: 33\n");
        assert_eq!(cfg.get_i64("agent.max_turns", 0), 33);
        assert!(cfg.get("max_turns").is_none());
    }

    #[test]
    fn corrupt_config_backs_up_and_serves_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "display: [unclosed\n").unwrap();
        let cfg = Config::load_from(path.clone()).unwrap();
        // Load never fails; defaults are served.
        assert_eq!(cfg.get_i64("agent.max_turns", 0), 90);
        // A timestamped corrupt backup exists.
        let baks: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with("config.yaml.corrupt.") && n.ends_with(".bak")
            })
            .collect();
        assert_eq!(baks.len(), 1, "corrupt backup created");
    }

    #[test]
    fn env_writer_quoting_and_export_updates() {
        let quoted = quote_env_value("has space");
        assert_eq!(quoted, "\"has space\"");
        assert_eq!(quote_env_value("plainvalue"), "plainvalue");
        assert_eq!(quote_env_value("with#hash"), "\"with#hash\"");
        assert_eq!(quote_env_value("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(quote_env_value(""), "");

        assert!(env_line_defines_key("export GITHUB_TOKEN=x", "GITHUB_TOKEN"));
        assert!(env_line_defines_key("GITHUB_TOKEN=x", "GITHUB_TOKEN"));
        assert!(!env_line_defines_key("MY_GITHUB_TOKEN=x", "GITHUB_TOKEN"));
    }

    #[test]
    fn env_writer_denylist_and_name_validation() {
        assert!(save_env_value("PATH", "x").is_err());
        assert!(save_env_value("LD_PRELOAD", "x").is_err());
        assert!(save_env_value("JOEY_HOME", "x").is_err());
        assert!(save_env_value("BAD NAME", "x").is_err());
        assert!(save_env_value("1STARTSWITHDIGIT", "x").is_err());
    }

    #[test]
    fn sanitize_splits_concatenated_known_keys() {
        let lines = vec!["ANTHROPIC_API_KEY=sk-ant-abc123OPENAI_API_KEY=sk-xyz789\n".to_string()];
        let out = sanitize_env_lines(&lines);
        assert_eq!(
            out,
            vec![
                "ANTHROPIC_API_KEY=sk-ant-abc123\n".to_string(),
                "OPENAI_API_KEY=sk-xyz789\n".to_string(),
            ]
        );
        // Structured values (URLs) are never split.
        let lines =
            vec!["WEBHOOK=https://h.test/x?OPENAI_API_KEY=embedded&other=1OPENAI_API_KEY=real\n".to_string()];
        let out = sanitize_env_lines(&lines);
        assert_eq!(out.len(), 1, "no split when first needle isn't at position 0 / structured");
    }

    #[test]
    fn ignore_user_config_exactly_one() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "agent:\n  max_turns: 7\n").unwrap();

        std::env::set_var("JOEY_IGNORE_USER_CONFIG", "true");
        let cfg = Config::load_from(path.clone()).unwrap();
        assert_eq!(cfg.get_i64("agent.max_turns", 0), 7, "\"true\" is not accepted");

        std::env::set_var("JOEY_IGNORE_USER_CONFIG", "1");
        let cfg = Config::load_from(path.clone()).unwrap();
        assert_eq!(cfg.get_i64("agent.max_turns", 0), 90, "exactly \"1\" ignores user config");
        std::env::remove_var("JOEY_IGNORE_USER_CONFIG");
    }
}
