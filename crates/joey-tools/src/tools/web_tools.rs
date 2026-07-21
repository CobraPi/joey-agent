//! Web tools: web_search + web_extract (port of `tools/web_tools.py`).
//!
//! Providers are pluggable upstream; this port ships Tavily (JSON search API)
//! and a direct HTML fetch+extract path. `web_search` is gated on a provider
//! key being present.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::context::ToolContext;
use crate::registry::{tool_error, Tool, ToolResult};
use crate::truncate;

fn tavily_key() -> Option<String> {
    std::env::var("TAVILY_API_KEY").ok().filter(|s| !s.trim().is_empty())
}

pub struct WebSearch;

#[async_trait]
impl Tool for WebSearch {
    fn name(&self) -> &str {
        "web_search"
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn description(&self) -> &str {
        "Search the web and return titles, URLs, and snippets. Requires a search \
         provider key (TAVILY_API_KEY)."
    }
    fn emoji(&self) -> &str {
        "🔍"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "limit": {"type": "integer", "default": 5, "minimum": 1, "maximum": 20}
            },
            "required": ["query"]
        })
    }
    fn check(&self, _ctx: &ToolContext) -> bool {
        tavily_key().is_some()
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let Some(query) = args.get("query").and_then(|v| v.as_str()) else {
            return tool_error("missing required parameter: query");
        };
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5).clamp(1, 20);
        let Some(key) = tavily_key() else {
            return tool_error("web_search requires TAVILY_API_KEY to be set");
        };

        let client = reqwest::Client::new();
        let resp = client
            .post("https://api.tavily.com/search")
            .json(&json!({
                "api_key": key,
                "query": query,
                "max_results": limit,
                "search_depth": "basic"
            }))
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => return tool_error(format!("search request failed: {}", e)),
        };
        if !resp.status().is_success() {
            return tool_error(format!("search provider returned {}", resp.status()));
        }
        let body: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => return tool_error(format!("bad search response: {}", e)),
        };

        let mut out = String::new();
        if let Some(answer) = body.get("answer").and_then(|a| a.as_str()) {
            if !answer.is_empty() {
                out.push_str(&format!("Answer: {}\n\n", answer));
            }
        }
        if let Some(results) = body.get("results").and_then(|r| r.as_array()) {
            for (i, r) in results.iter().enumerate() {
                let title = r.get("title").and_then(|t| t.as_str()).unwrap_or("");
                let url = r.get("url").and_then(|u| u.as_str()).unwrap_or("");
                let content = r.get("content").and_then(|c| c.as_str()).unwrap_or("");
                out.push_str(&format!("{}. {}\n   {}\n   {}\n\n", i + 1, title, url, content));
            }
        }
        if out.is_empty() {
            out.push_str("No results.");
        }
        ToolResult::Text(out)
    }
}

pub struct WebExtract;

#[async_trait]
impl Tool for WebExtract {
    fn name(&self) -> &str {
        "web_extract"
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn description(&self) -> &str {
        "Fetch one or more URLs and return their main text content as markdown."
    }
    fn emoji(&self) -> &str {
        "📄"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "urls": {"type": "array", "items": {"type": "string"}, "description": "Up to 5 URLs."},
                "char_limit": {"type": "integer", "default": 15000, "minimum": 2000}
            },
            "required": ["urls"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let Some(urls) = args.get("urls").and_then(|v| v.as_array()) else {
            return tool_error("missing required parameter: urls");
        };
        let char_limit = args
            .get("char_limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(15000)
            .max(2000) as usize;

        let client = reqwest::Client::builder()
            .user_agent("joey-agent web_extract")
            .build()
            .unwrap();

        let mut out = String::new();
        for url_val in urls.iter().take(5) {
            let Some(url) = url_val.as_str() else {
                continue;
            };
            // SSRF guard: block obvious private/metadata targets.
            if is_blocked_url(url) {
                out.push_str(&format!("## {}\n[blocked: private/metadata address]\n\n", url));
                continue;
            }
            match client.get(url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    let html = resp.text().await.unwrap_or_default();
                    let text = html2text::from_read(html.as_bytes(), 100)
                        .unwrap_or_else(|_| html.clone());
                    let clipped = truncate::bounded_head_tail(&text, char_limit);
                    out.push_str(&format!("## {}\n{}\n\n", url, clipped));
                }
                Ok(resp) => out.push_str(&format!("## {}\n[HTTP {}]\n\n", url, resp.status())),
                Err(e) => out.push_str(&format!("## {}\n[fetch failed: {}]\n\n", url, e)),
            }
        }
        ToolResult::Text(out)
    }
}

/// Minimal SSRF guard (port of the load-bearing checks in `tools/url_safety.py`):
/// blocks localhost, private ranges, and the cloud metadata IP.
fn is_blocked_url(url: &str) -> bool {
    let host = joey_core::utils::base_url_hostname(url);
    if host.is_empty() {
        return true;
    }
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    if host == "169.254.169.254" {
        return true;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1])
            }
            std::net::IpAddr::V6(v6) => v6.is_loopback(),
        };
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_metadata_ip() {
        assert!(is_blocked_url("http://169.254.169.254/latest/meta-data/"));
        assert!(is_blocked_url("http://localhost:8080/"));
        assert!(is_blocked_url("http://127.0.0.1/"));
        assert!(!is_blocked_url("https://example.com/page"));
    }
}
