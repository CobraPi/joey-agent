//! GitHub Copilot authentication, catalog, and model-routing support.
//!
//! Faithful Rust port of Hermes Agent's `hermes_cli/copilot_auth.py` and the
//! Copilot-specific helpers in `hermes_cli/models.py`: token resolution follows
//! COPILOT_GITHUB_TOKEN -> GH_TOKEN -> GITHUB_TOKEN -> `gh auth token`, raw
//! GitHub credentials are exchanged for short-lived Copilot API tokens, the
//! account-specific Enterprise endpoint is honored, and device-code login is
//! available to the setup/CLI surfaces.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::error::ProviderError;
use crate::profile::ApiMode;

pub const COPILOT_BASE_URL: &str = "https://api.githubcopilot.com";
pub const COPILOT_MODELS_URL: &str = "https://api.githubcopilot.com/models";
pub const COPILOT_OAUTH_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
pub const COPILOT_EDITOR_VERSION: &str = "vscode/1.104.1";
pub const COPILOT_ENV_VARS: &[&str] = &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"];
const TOKEN_EXCHANGE_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const EXCHANGE_USER_AGENT: &str = "GitHubCopilotChat/0.26.7";
const REFRESH_MARGIN_SECS: u64 = 120;

#[derive(Debug, Clone, Default)]
pub struct CopilotCredentials {
    pub token: String,
    pub base_url: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Default)]
struct CachedToken {
    token: String,
    base_url: String,
    expires_at: u64,
}

impl CachedToken {
    fn is_valid(&self) -> bool {
        !self.token.is_empty() && self.expires_at.saturating_sub(REFRESH_MARGIN_SECS) > now_epoch()
    }
}

#[derive(Debug)]
pub struct CopilotAuth {
    raw_token: String,
    cached: Mutex<CachedToken>,
}

impl CopilotAuth {
    pub fn new(raw_token: String) -> Self {
        Self {
            raw_token,
            cached: Mutex::new(CachedToken::default()),
        }
    }

    pub fn from_environment() -> Result<Option<Self>, ProviderError> {
        let (token, _) = resolve_copilot_token()?;
        Ok((!token.is_empty()).then(|| Self::new(token)))
    }

    pub fn has_raw_token(&self) -> bool {
        !self.raw_token.is_empty()
    }

    pub fn invalidate(&self) {
        if let Ok(mut guard) = self.cached.lock() {
            *guard = CachedToken::default();
        }
    }

    /// Match Hermes's deliberate raw-token fallback when exchange fails.
    fn raw_credentials(&self) -> CopilotCredentials {
        CopilotCredentials {
            token: self.raw_token.clone(),
            base_url: derive_base_url_from_proxy_ep(&self.raw_token)
                .or_else(|| {
                    std::env::var("COPILOT_API_BASE_URL")
                        .ok()
                        .filter(|v| !v.trim().is_empty())
                })
                .unwrap_or_else(|| COPILOT_BASE_URL.to_string()),
            expires_at: now_epoch() + 300,
        }
    }

    pub async fn credentials(
        &self,
        http: &reqwest::Client,
    ) -> Result<CopilotCredentials, ProviderError> {
        if let Ok(guard) = self.cached.lock() {
            if guard.is_valid() {
                return Ok(CopilotCredentials {
                    token: guard.token.clone(),
                    base_url: guard.base_url.clone(),
                    expires_at: guard.expires_at,
                });
            }
        }

        let token_url = std::env::var("COPILOT_TOKEN_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| TOKEN_EXCHANGE_URL.to_string());
        let response = match http
            .get(token_url)
            .header("Authorization", format!("token {}", self.raw_token))
            .header("User-Agent", EXCHANGE_USER_AGENT)
            .header("Accept", "application/json")
            .header("Editor-Version", COPILOT_EDITOR_VERSION)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::debug!("Copilot token exchange failed; using raw token: {error}");
                return Ok(self.raw_credentials());
            }
        };
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            tracing::debug!(
                status = status.as_u16(),
                "Copilot token exchange failed; using raw token"
            );
            return Ok(self.raw_credentials());
        }
        let data: Value = match serde_json::from_str(&text) {
            Ok(data) => data,
            Err(error) => {
                tracing::debug!(
                    "Invalid Copilot token exchange response; using raw token: {error}"
                );
                return Ok(self.raw_credentials());
            }
        };
        let api_token = data
            .get("token")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if api_token.is_empty() {
            tracing::debug!("Copilot token exchange returned an empty token; using raw token");
            return Ok(self.raw_credentials());
        }
        let expires_at = data
            .get("expires_at")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| now_epoch() + 1800);
        let resolved = exchange_base_url(&data, &api_token)
            .or_else(|| {
                std::env::var("COPILOT_API_BASE_URL")
                    .ok()
                    .filter(|v| !v.trim().is_empty())
            })
            .unwrap_or_else(|| COPILOT_BASE_URL.to_string())
            .trim_end_matches('/')
            .to_string();
        let cached = CachedToken {
            token: api_token.clone(),
            base_url: resolved.clone(),
            expires_at,
        };
        if let Ok(mut guard) = self.cached.lock() {
            *guard = cached;
        }
        Ok(CopilotCredentials {
            token: api_token,
            base_url: resolved,
            expires_at,
        })
    }
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn validate_copilot_token(token: &str) -> Result<(), String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("Empty token".into());
    }
    if token.starts_with("ghp_") {
        return Err(
            "Classic Personal Access Tokens (ghp_*) are not supported by the Copilot API. Use OAuth device login, a fine-grained github_pat_* with Copilot Requests permission, or a ghu_* GitHub App token."
                .into(),
        );
    }
    Ok(())
}

/// Resolve a raw GitHub credential exactly in Hermes/Copilot CLI priority order.
pub fn resolve_copilot_token() -> Result<(String, String), ProviderError> {
    for env_var in COPILOT_ENV_VARS {
        let value = std::env::var(env_var)
            .unwrap_or_default()
            .trim()
            .to_string();
        if value.is_empty() {
            continue;
        }
        if let Err(message) = validate_copilot_token(&value) {
            tracing::warn!(
                source = *env_var,
                "unsupported Copilot credential: {}",
                message
            );
            continue;
        }
        return Ok((value, (*env_var).to_string()));
    }
    if let Some(token) = try_gh_cli_token() {
        validate_copilot_token(&token).map_err(ProviderError::Auth)?;
        return Ok((token, "gh auth token".into()));
    }
    Ok((String::new(), String::new()))
}

fn gh_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(path) = which::which("gh") {
        candidates.push(path);
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/bin/gh"));
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/gh"));
    candidates.push(PathBuf::from("/usr/local/bin/gh"));
    candidates.sort();
    candidates.dedup();
    candidates
}

fn try_gh_cli_token() -> Option<String> {
    let hostname = std::env::var("COPILOT_GH_HOST").unwrap_or_default();
    for executable in gh_candidates() {
        if !executable.is_file() {
            continue;
        }
        let mut cmd = Command::new(&executable);
        cmd.args(["auth", "token"]);
        if !hostname.trim().is_empty() {
            cmd.args(["--hostname", hostname.trim()]);
        }
        cmd.env_remove("GITHUB_TOKEN").env_remove("GH_TOKEN");
        let Ok(output) = cmd.output() else { continue };
        if output.status.success() {
            let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !token.is_empty() {
                return Some(token);
            }
        }
    }
    None
}

fn exchange_base_url(data: &Value, token: &str) -> Option<String> {
    let endpoint = data
        .get("endpoints")
        .and_then(Value::as_object)
        .and_then(|v| v.get("api"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .trim_end_matches('/');
    if !endpoint.is_empty() {
        return Some(endpoint.to_string());
    }
    derive_base_url_from_proxy_ep(token)
}

pub fn derive_base_url_from_proxy_ep(token: &str) -> Option<String> {
    let raw = token
        .split(';')
        .find_map(|part| part.trim().strip_prefix("proxy-ep="))?
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    if raw.is_empty() {
        return None;
    }
    let host = raw
        .strip_prefix("proxy.")
        .map(|rest| format!("api.{rest}"))
        .unwrap_or_else(|| raw.to_string());
    Some(format!("https://{host}"))
}

pub fn request_headers(is_agent_turn: bool, is_vision: bool) -> Vec<(&'static str, String)> {
    let mut headers = vec![
        ("Editor-Version", COPILOT_EDITOR_VERSION.into()),
        ("User-Agent", "JoeyAgent/1.0".into()),
        ("Copilot-Integration-Id", "vscode-chat".into()),
        ("Openai-Intent", "conversation-edits".into()),
        (
            "x-initiator",
            if is_agent_turn {
                "agent".into()
            } else {
                "user".into()
            },
        ),
    ];
    if is_vision {
        headers.push(("Copilot-Vision-Request", "true".into()));
    }
    headers
}

/// Synchronous device flow used by `joey auth copilot` and `joey model`.
pub fn device_code_login(timeout: Duration) -> Result<String, ProviderError> {
    let host = std::env::var("COPILOT_GH_HOST").unwrap_or_else(|_| "github.com".into());
    let device_url = format!("https://{}/login/device/code", host.trim_end_matches('/'));
    let token_url = format!(
        "https://{}/login/oauth/access_token",
        host.trim_end_matches('/')
    );
    let response = ureq::post(&device_url)
        .set("Accept", "application/json")
        .set("Content-Type", "application/x-www-form-urlencoded")
        .set("User-Agent", "JoeyAgent/1.0")
        .send_form(&[
            ("client_id", COPILOT_OAUTH_CLIENT_ID),
            ("scope", "read:user"),
        ])
        .map_err(|e| {
            ProviderError::Connection(format!(
                "failed to initiate Copilot device authorization: {e}"
            ))
        })?;
    let data: Value = response
        .into_json()
        .map_err(|e| ProviderError::Parse(e.to_string()))?;
    let device_code = data
        .get("device_code")
        .and_then(Value::as_str)
        .unwrap_or("");
    let user_code = data.get("user_code").and_then(Value::as_str).unwrap_or("");
    let verification_uri = data
        .get("verification_uri")
        .and_then(Value::as_str)
        .unwrap_or("https://github.com/login/device");
    if device_code.is_empty() || user_code.is_empty() {
        return Err(ProviderError::Auth(
            "GitHub did not return a device code".into(),
        ));
    }
    println!();
    println!("  Open this URL in your browser: {verification_uri}");
    println!("  Enter this code: {user_code}");
    println!();
    println!("  Waiting for authorization...");

    let mut interval = data
        .get("interval")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .max(1);
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs(interval + 3));
        let poll = ureq::post(&token_url)
            .set("Accept", "application/json")
            .set("Content-Type", "application/x-www-form-urlencoded")
            .set("User-Agent", "JoeyAgent/1.0")
            .send_form(&[
                ("client_id", COPILOT_OAUTH_CLIENT_ID),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ]);
        let Ok(response) = poll else { continue };
        let result: Value = response.into_json().unwrap_or_else(|_| json!({}));
        if let Some(token) = result.get("access_token").and_then(Value::as_str) {
            return Ok(token.to_string());
        }
        match result.get("error").and_then(Value::as_str).unwrap_or("") {
            "authorization_pending" => continue,
            "slow_down" => {
                interval = result
                    .get("interval")
                    .and_then(Value::as_u64)
                    .unwrap_or(interval + 5);
            }
            "expired_token" => {
                return Err(ProviderError::Auth("Copilot device code expired".into()))
            }
            "access_denied" => {
                return Err(ProviderError::Auth(
                    "Copilot authorization was denied".into(),
                ))
            }
            other if !other.is_empty() => {
                return Err(ProviderError::Auth(format!(
                    "Copilot authorization failed: {other}"
                )))
            }
            _ => {}
        }
    }
    Err(ProviderError::Timeout(
        "timed out waiting for Copilot authorization".into(),
    ))
}

pub fn normalize_model_id(model: &str) -> String {
    let raw = model.trim();
    let aliases: HashMap<&str, &str> = HashMap::from([
        ("openai/gpt-5", "gpt-5-mini"),
        ("openai/gpt-5-chat", "gpt-5-mini"),
        ("openai/gpt-5-mini", "gpt-5-mini"),
        ("openai/gpt-5-nano", "gpt-5-mini"),
        ("openai/gpt-4.1", "gpt-4.1"),
        ("openai/gpt-4.1-mini", "gpt-4.1"),
        ("openai/gpt-4.1-nano", "gpt-4.1"),
        ("openai/gpt-4o", "gpt-4o"),
        ("openai/gpt-4o-mini", "gpt-4o-mini"),
        ("openai/o1", "gpt-5.2"),
        ("openai/o1-mini", "gpt-5-mini"),
        ("openai/o1-preview", "gpt-5.2"),
        ("openai/o3", "gpt-5.3-codex"),
        ("openai/o3-mini", "gpt-5-mini"),
        ("openai/o4-mini", "gpt-5-mini"),
        ("anthropic/claude-opus-4.6", "claude-opus-4.6"),
        ("anthropic/claude-sonnet-5", "claude-sonnet-5"),
        ("anthropic/claude-sonnet-4.6", "claude-sonnet-4.6"),
        ("anthropic/claude-sonnet-4", "claude-sonnet-4"),
        ("anthropic/claude-sonnet-4.5", "claude-sonnet-4.5"),
        ("anthropic/claude-haiku-4.5", "claude-haiku-4.5"),
        ("claude-sonnet-5", "claude-sonnet-5"),
        ("claude-opus-4-6", "claude-opus-4.6"),
        ("claude-sonnet-4-6", "claude-sonnet-4.6"),
        ("claude-sonnet-4-0", "claude-sonnet-4"),
        ("claude-sonnet-4-5", "claude-sonnet-4.5"),
        ("claude-haiku-4-5", "claude-haiku-4.5"),
        ("anthropic/claude-opus-4-6", "claude-opus-4.6"),
        ("anthropic/claude-sonnet-4-6", "claude-sonnet-4.6"),
        ("anthropic/claude-sonnet-4-0", "claude-sonnet-4"),
        ("anthropic/claude-sonnet-4-5", "claude-sonnet-4.5"),
        ("anthropic/claude-haiku-4-5", "claude-haiku-4.5"),
    ]);
    if let Some(value) = aliases.get(raw) {
        return (*value).to_string();
    }
    raw.split_once('/')
        .map(|(_, tail)| tail.trim().to_string())
        .unwrap_or_else(|| raw.to_string())
}

pub fn model_api_mode(model: &str, catalog_entry: Option<&Value>) -> ApiMode {
    let normalized = normalize_model_id(model).to_lowercase();
    if let Some(rest) = normalized.strip_prefix("gpt-") {
        let major = rest
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0);
        if major >= 5 && !normalized.starts_with("gpt-5-mini") {
            return ApiMode::CodexResponses;
        }
    }
    if let Some(entry) = catalog_entry {
        let endpoints: Vec<&str> = entry
            .get("supported_endpoints")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if endpoints.contains(&"/v1/messages") && !endpoints.contains(&"/chat/completions") {
            return ApiMode::AnthropicMessages;
        }
    }
    ApiMode::ChatCompletions
}

pub fn fallback_models() -> Vec<String> {
    [
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5-mini",
        "gpt-5.3-codex",
        "gpt-5.2-codex",
        "gpt-4.1",
        "gpt-4o",
        "gpt-4o-mini",
        "claude-sonnet-4.6",
        "claude-sonnet-5",
        "claude-sonnet-4",
        "claude-sonnet-4.5",
        "claude-haiku-4.5",
        "gemini-3.1-pro-preview",
        "gemini-3-pro-preview",
        "gemini-3-flash-preview",
        "gemini-2.5-pro",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Fetch the authenticated Copilot model catalog for setup/model selection.
/// This is intentionally synchronous because the numbered setup wizard is a
/// synchronous TTY flow, matching Hermes' urllib implementation.
pub fn fetch_model_catalog(timeout: Duration) -> Result<Vec<Value>, ProviderError> {
    let (raw_token, _) = resolve_copilot_token()?;
    if raw_token.is_empty() {
        return Ok(Vec::new());
    }
    let token_url = std::env::var("COPILOT_TOKEN_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| TOKEN_EXCHANGE_URL.to_string());
    // Match Hermes `get_copilot_api_token`: prefer exchange, but fall back to
    // the raw credential (some callers already provide a Copilot API token).
    let exchange_data: Value = ureq::get(&token_url)
        .set("Authorization", &format!("token {raw_token}"))
        .set("User-Agent", EXCHANGE_USER_AGENT)
        .set("Accept", "application/json")
        .set("Editor-Version", COPILOT_EDITOR_VERSION)
        .timeout(timeout)
        .call()
        .ok()
        .and_then(|response| response.into_json().ok())
        .unwrap_or_else(|| json!({}));
    let api_token = exchange_data
        .get("token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .unwrap_or(&raw_token);
    let base_url = exchange_base_url(&exchange_data, api_token)
        .or_else(|| {
            std::env::var("COPILOT_API_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
        .unwrap_or_else(|| COPILOT_BASE_URL.to_string());
    let mut request = ureq::get(&format!("{}/models", base_url.trim_end_matches('/')))
        .set("Authorization", &format!("Bearer {api_token}"));
    for (name, value) in request_headers(true, false) {
        request = request.set(name, &value);
    }
    let response = request
        .timeout(timeout)
        .call()
        .map_err(|e| ProviderError::Connection(format!("Copilot model catalog failed: {e}")))?;
    let payload: Value = response
        .into_json()
        .map_err(|e| ProviderError::Parse(e.to_string()))?;
    let items = payload
        .as_array()
        .cloned()
        .or_else(|| payload.get("data").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for item in items {
        let id = item.get("id").and_then(Value::as_str).unwrap_or("").trim();
        if id.is_empty() || item.get("model_picker_enabled").and_then(Value::as_bool) == Some(false)
        {
            continue;
        }
        let model_type = item
            .get("capabilities")
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !model_type.is_empty() && model_type != "chat" {
            continue;
        }
        if let Some(endpoints) = item.get("supported_endpoints").and_then(Value::as_array) {
            let usable = endpoints.iter().filter_map(Value::as_str).any(|endpoint| {
                matches!(
                    endpoint.trim(),
                    "/chat/completions" | "/responses" | "/v1/messages"
                )
            });
            if !endpoints.is_empty() && !usable {
                continue;
            }
        }
        if seen.insert(id.to_string()) {
            result.push(item);
        }
    }
    Ok(result)
}

pub fn catalog_context_window(entry: &Value) -> Option<u64> {
    entry
        .get("capabilities")?
        .get("limits")?
        .get("max_prompt_tokens")?
        .as_u64()
        .filter(|value| *value > 0)
}

pub fn catalog_reasoning_efforts(entry: &Value) -> Vec<String> {
    entry
        .get("capabilities")
        .and_then(|v| v.get("supports"))
        .and_then(|v| v.get("reasoning_effort"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn model_reasoning_efforts(model: &str, catalog_entry: Option<&Value>) -> Vec<String> {
    if let Some(entry) = catalog_entry {
        let efforts = catalog_reasoning_efforts(entry);
        if !efforts.is_empty() {
            return efforts;
        }
    }
    let raw = model.trim().to_lowercase();
    if ["openai/o1", "openai/o3", "openai/o4", "o1", "o3", "o4"]
        .iter()
        .any(|prefix| raw.starts_with(prefix))
    {
        return ["low", "medium", "high"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    }
    if normalize_model_id(model)
        .to_lowercase()
        .starts_with("gpt-5")
    {
        return ["minimal", "low", "medium", "high"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_classic_pat() {
        assert!(validate_copilot_token("ghp_deadbeef").is_err());
        assert!(validate_copilot_token("ghu_deadbeef").is_ok());
    }

    #[test]
    fn derives_enterprise_api_host() {
        assert_eq!(
            derive_base_url_from_proxy_ep(
                "tid=x;proxy-ep=proxy.enterprise.githubcopilot.com;exp=1"
            ),
            Some("https://api.enterprise.githubcopilot.com".into())
        );
    }

    #[test]
    fn normalizes_hermes_copilot_aliases() {
        assert_eq!(normalize_model_id("openai/o3"), "gpt-5.3-codex");
        assert_eq!(normalize_model_id("claude-sonnet-4-6"), "claude-sonnet-4.6");
        assert_eq!(
            normalize_model_id("anthropic/claude-sonnet-4-0"),
            "claude-sonnet-4"
        );
        assert_eq!(normalize_model_id("copilot/gpt-5.4"), "gpt-5.4");
    }

    #[test]
    fn derives_catalog_context_and_reasoning() {
        let entry = json!({"capabilities":{"limits":{"max_prompt_tokens":128000},"supports":{"reasoning_effort":["low","high"]}}});
        assert_eq!(catalog_context_window(&entry), Some(128000));
        assert_eq!(
            model_reasoning_efforts("gpt-5.4", Some(&entry)),
            vec!["low", "high"]
        );
        assert_eq!(
            model_reasoning_efforts("gpt-5.4", None),
            vec!["minimal", "low", "medium", "high"]
        );
    }

    #[test]
    fn routes_models_like_hermes() {
        assert_eq!(model_api_mode("gpt-5.4", None), ApiMode::CodexResponses);
        assert_eq!(model_api_mode("gpt-5-mini", None), ApiMode::ChatCompletions);
        assert_eq!(
            model_api_mode("claude-opus-4.6", None),
            ApiMode::ChatCompletions
        );
        let entry = json!({"supported_endpoints":["/v1/messages"]});
        assert_eq!(
            model_api_mode("claude-opus-4.6", Some(&entry)),
            ApiMode::AnthropicMessages
        );
    }
}
