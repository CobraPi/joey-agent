//! Local model server discovery (port of crush's `internal/discover/`).
//!
//! Auto-detects local LLM inference servers running on the machine:
//!   - **Ollama** (default port 11434) — `/api/tags`
//!   - **LM Studio** (default port 1234) — `/v1/models`
//!   - **llama.cpp** server (default port 8080) — `/v1/models`
//!   - **LiteLLM** (default port 4000) — `/v1/models`
//!   - **MLX** (default port 8000) — `/v1/models`
//!
//! Each discoverer probes the server's model-listing endpoint with a short
//! timeout and returns the list of available models. Results are merged into
//! the model catalog so users can pick local models from `joey model`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Default timeout for discovery probes (2 seconds — crush uses a similar
/// short window since local servers respond instantly when running).
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);

/// A discovered local model server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredServer {
    /// Server type identifier (e.g. "ollama", "lmstudio").
    pub server_type: ServerType,
    /// Base URL (e.g. "http://localhost:11434/v1").
    pub base_url: String,
    /// Discovered models.
    pub models: Vec<DiscoveredModel>,
}

/// Type of local model server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServerType {
    Ollama,
    LmStudio,
    LlamaCpp,
    LiteLlm,
    Mlx,
}

impl ServerType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ServerType::Ollama => "ollama",
            ServerType::LmStudio => "lmstudio",
            ServerType::LlamaCpp => "llamacpp",
            ServerType::LiteLlm => "litellm",
            ServerType::Mlx => "mlx",
        }
    }
}

/// A single discovered model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredModel {
    /// Model ID/name as reported by the server.
    pub id: String,
    /// Display label for the model picker.
    pub label: String,
    /// The server type that hosts this model.
    pub server_type: ServerType,
    /// Base URL for API calls.
    pub base_url: String,
}

#[allow(dead_code)]
impl DiscoveredModel {
    /// The provider string for config (e.g. "openai" for OpenAI-compatible).
    pub fn provider(&self) -> &'static str {
        match self.server_type {
            ServerType::Ollama => "openai", // Ollama has an OpenAI-compatible endpoint
            _ => "openai",
        }
    }

    /// The model string to use in config (id + base_url hint).
    pub fn model_id(&self) -> &str {
        &self.id
    }
}

/// Discover all local model servers.
///
/// Probes each known server type concurrently and returns the union of all
/// discovered models. Servers that don't respond within the timeout are
/// silently skipped.
pub async fn discover_all() -> Vec<DiscoveredServer> {
    let (ollama, lmstudio, llamacpp, litellm, mlx) = tokio::join!(
        discover_ollama(),
        discover_lmstudio(),
        discover_llamacpp(),
        discover_litellm(),
        discover_mlx(),
    );
    [ollama, lmstudio, llamacpp, litellm, mlx]
        .into_iter()
        .flatten()
        .collect()
}

/// Discover all local models (flattened).
#[allow(dead_code)]
pub async fn discover_models() -> Vec<DiscoveredModel> {
    discover_all()
        .await
        .into_iter()
        .flat_map(|s| s.models)
        .collect()
}

/// Probe Ollama (default: http://localhost:11434).
pub async fn discover_ollama() -> Option<DiscoveredServer> {
    let base = base_url_or_env("OLLAMA_BASE_URL", "http://localhost:11434");
    let api_url = format!("{}/api/tags", base);
    let resp = probe_url(&api_url).await?;

    // Ollama returns { "models": [{ "name": "...", ... }] }
    #[derive(Deserialize)]
    struct OllamaResponse {
        #[serde(default)]
        models: Vec<OllamaModel>,
    }
    #[derive(Deserialize)]
    struct OllamaModel {
        name: String,
    }

    let parsed: OllamaResponse = serde_json::from_slice(&resp).ok()?;
    let models = parsed
        .models
        .into_iter()
        .map(|m| DiscoveredModel {
            label: format!("🦙 {} (Ollama)", m.name),
            id: m.name,
            server_type: ServerType::Ollama,
            base_url: format!("{}/v1", base),
        })
        .collect();

    Some(DiscoveredServer {
        server_type: ServerType::Ollama,
        base_url: format!("{}/v1", base),
        models,
    })
}

/// Probe LM Studio (default: http://localhost:1234).
pub async fn discover_lmstudio() -> Option<DiscoveredServer> {
    let base = base_url_or_env("LMSTUDIO_BASE_URL", "http://localhost:1234");
    discover_openai_compatible(&base, ServerType::LmStudio, "🏠").await
}

/// Probe llama.cpp server (default: http://localhost:8080).
pub async fn discover_llamacpp() -> Option<DiscoveredServer> {
    let base = base_url_or_env("LLAMACPP_BASE_URL", "http://localhost:8080");
    discover_openai_compatible(&base, ServerType::LlamaCpp, "🦙").await
}

/// Probe LiteLLM proxy (default: http://localhost:4000).
pub async fn discover_litellm() -> Option<DiscoveredServer> {
    let base = base_url_or_env("LITELLM_BASE_URL", "http://localhost:4000");
    discover_openai_compatible(&base, ServerType::LiteLlm, "🔗").await
}

/// Probe MLX server (default: http://localhost:8000).
pub async fn discover_mlx() -> Option<DiscoveredServer> {
    let base = base_url_or_env("MLX_BASE_URL", "http://localhost:8000");
    discover_openai_compatible(&base, ServerType::Mlx, "🍎").await
}

/// Generic OpenAI-compatible model discovery (`GET /v1/models`).
async fn discover_openai_compatible(
    base: &str,
    server_type: ServerType,
    emoji: &str,
) -> Option<DiscoveredServer> {
    let url = format!("{}/v1/models", base);
    let resp = probe_url(&url).await?;

    // Standard OpenAI models response: { "data": [{ "id": "..." }] }
    #[derive(Deserialize)]
    struct ModelsResponse {
        #[serde(default)]
        data: Vec<ModelEntry>,
    }
    #[derive(Deserialize)]
    struct ModelEntry {
        id: String,
    }

    let parsed: ModelsResponse = serde_json::from_slice(&resp).ok()?;
    let type_name = server_type.as_str();
    let models = parsed
        .data
        .into_iter()
        .map(|m| DiscoveredModel {
            label: format!("{} {} ({})", emoji, m.id, type_name),
            id: m.id,
            server_type: server_type.clone(),
            base_url: format!("{}/v1", base),
        })
        .collect();

    Some(DiscoveredServer {
        server_type,
        base_url: format!("{}/v1", base),
        models,
    })
}

/// Fetch a URL with a short timeout. Returns the body bytes on success.
async fn probe_url(url: &str) -> Option<Vec<u8>> {
    let client = reqwest::Client::builder()
        .timeout(DISCOVERY_TIMEOUT)
        .build()
        .ok()?;

    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().await.ok()?;
    Some(bytes.to_vec())
}

/// Read a base URL from an env var, falling back to a default.
fn base_url_or_env(env_var: &str, default: &str) -> String {
    std::env::var(env_var)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

/// Run model discovery and print results to stdout.
pub async fn run_discover() {
    use nu_ansi_term::Color;

    println!();
    println!("{}", Color::Cyan.bold().paint("⚕ Scanning for local model servers..."));
    println!();

    let servers = discover_all().await;

    if servers.is_empty() {
        println!("{}", Color::Yellow.paint("No local model servers found."));
        println!();
        println!("{}", Color::DarkGray.paint("Supported servers:"));
        println!("  {} Ollama      (port 11434) — https://ollama.com", Color::DarkGray.paint("•"));
        println!("  {} LM Studio   (port 1234)  — https://lmstudio.ai", Color::DarkGray.paint("•"));
        println!("  {} llama.cpp   (port 8080)  — https://github.com/ggerganov/llama.cpp", Color::DarkGray.paint("•"));
        println!("  {} LiteLLM     (port 4000)  — https://github.com/BerriAI/litellm", Color::DarkGray.paint("•"));
        println!("  {} MLX         (port 8000)  — https://github.com/ml-explore/mlx", Color::DarkGray.paint("•"));
        println!();
        return;
    }

    let mut total_models = 0;
    for server in &servers {
        let icon = match server.server_type {
            ServerType::Ollama => "🦙",
            ServerType::LmStudio => "🏠",
            ServerType::LlamaCpp => "🔧",
            ServerType::LiteLlm => "🔗",
            ServerType::Mlx => "🍎",
        };
        println!(
            "{} {} {} — {} model(s)",
            icon,
            Color::Green.bold().paint(server.server_type.as_str()),
            Color::DarkGray.paint(&server.base_url),
            server.models.len()
        );
        total_models += server.models.len();
        for model in &server.models {
            println!("  {} {}", Color::Cyan.paint("›"), model.id);
        }
        println!();
    }

    println!(
        "{} server(s), {} model(s) discovered",
        servers.len(),
        total_models
    );
    println!();
    println!("{}", Color::DarkGray.paint("To use a local model:"));
    println!(
        "{}",
        Color::DarkGray.paint("  joey config set model <model-id> --provider openai")
    );
    println!(
        "{}",
        Color::DarkGray.paint("  joey config set model.base_url <base-url>")
    );
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_type_strings() {
        assert_eq!(ServerType::Ollama.as_str(), "ollama");
        assert_eq!(ServerType::LmStudio.as_str(), "lmstudio");
        assert_eq!(ServerType::LlamaCpp.as_str(), "llamacpp");
    }

    #[test]
    fn base_url_env_fallback() {
        // Without env var set, returns default.
        let url = base_url_or_env("NONEXISTENT_VAR_XYZ", "http://localhost:1234");
        assert_eq!(url, "http://localhost:1234");
    }

    #[tokio::test]
    async fn discover_all_doesnt_crash_without_servers() {
        // No local servers running — should return empty, not panic.
        let servers = discover_all().await;
        // Might find something if the user runs local servers, but won't crash.
        let _ = servers;
    }

    #[test]
    fn discovered_model_provider() {
        let m = DiscoveredModel {
            id: "llama3".into(),
            label: "test".into(),
            server_type: ServerType::Ollama,
            base_url: "http://localhost:11434/v1".into(),
        };
        assert_eq!(m.provider(), "openai");
    }
}
