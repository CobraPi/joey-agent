//! Web tools: web_search + web_extract — port of `tools/web_tools.py` with
//! the Tavily provider (`plugins/web/tavily/provider.py`).
//!
//! * `web_search` → Tavily `/search`, result envelope
//!   `{"success": true, "data": {"web": [{title,url,description,position}]}}`
//!   serialized with indent=2.
//! * `web_extract` → secret-in-URL + credential-query-param blocking, per-URL
//!   SSRF filtering, Tavily `/extract`, base64-image placeholders, 75/25
//!   head-tail truncation with the stored-full-text footer, and results in
//!   input order.

use async_trait::async_trait;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::context::ToolContext;
use crate::pyjson::{commas, dumps, dumps_indent2};
use crate::registry::{Tool, ToolResult};
use crate::url_safety;

pub const DEFAULT_EXTRACT_CHAR_LIMIT: usize = 15000;
pub const MAX_STORED_TEXT_CHARS: usize = 2_000_000;

fn tavily_key() -> Option<String> {
    std::env::var("TAVILY_API_KEY").ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn tavily_base_url() -> String {
    std::env::var("TAVILY_BASE_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://api.tavily.com".to_string())
}

async fn tavily_request(endpoint: &str, mut payload: Map<String, Value>) -> Result<Value, String> {
    let Some(api_key) = tavily_key() else {
        return Err(
            "TAVILY_API_KEY environment variable not set. Get your API key at https://app.tavily.com/home"
                .to_string(),
        );
    };
    payload.insert("api_key".into(), json!(api_key));
    let url = format!("{}/{}", tavily_base_url(), endpoint.trim_start_matches('/'));
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .json(&Value::Object(payload))
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let resp = resp.error_for_status().map_err(|e| e.to_string())?;
    resp.json::<Value>().await.map_err(|e| e.to_string())
}

/// Availability gate for BOTH web tools (`check_web_api_key` analog — this
/// port ships the Tavily backend).
fn check_web_api_key() -> bool {
    tavily_key().is_some()
}

// ---------------------------------------------------------------------------
// web_search
// ---------------------------------------------------------------------------

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
        "Search the web for information. Returns up to 5 results by default with titles, URLs, and descriptions. The query is passed through to the configured backend, so operators such as site:domain, filetype:pdf, intitle:word, -term, and \"exact phrase\" may work when the backend supports them."
    }
    fn emoji(&self) -> &str {
        "🔍"
    }
    fn max_result_chars(&self) -> Option<usize> {
        Some(100_000)
    }
    fn check(&self, _ctx: &ToolContext) -> bool {
        check_web_api_key()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query to look up on the web. You may include backend-supported operators such as site:example.com, filetype:pdf, intitle:word, -term, or \"exact phrase\"."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return. Defaults to 5.",
                    "minimum": 1,
                    "maximum": 100,
                    "default": 5
                }
            },
            "required": ["query"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let limit = match args.get("limit") {
            Some(Value::Number(n)) => n.as_i64().unwrap_or(5),
            Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(5),
            _ => 5,
        }
        .clamp(1, 100);

        let response_data = if !check_web_api_key() {
            json!({
                "success": false,
                "error": "No web search provider configured. Run `joey tools` to set one up.",
            })
        } else {
            let mut payload = Map::new();
            payload.insert("query".into(), json!(query));
            payload.insert("max_results".into(), json!(limit.min(20)));
            payload.insert("include_raw_content".into(), json!(false));
            payload.insert("include_images".into(), json!(false));
            match tavily_request("search", payload).await {
                Ok(raw) => {
                    // `_normalize_tavily_search_results`.
                    let mut web_results: Vec<Value> = Vec::new();
                    if let Some(results) = raw.get("results").and_then(|r| r.as_array()) {
                        for (i, result) in results.iter().enumerate() {
                            web_results.push(json!({
                                "title": result.get("title").and_then(|t| t.as_str()).unwrap_or(""),
                                "url": result.get("url").and_then(|u| u.as_str()).unwrap_or(""),
                                "description": result.get("content").and_then(|c| c.as_str()).unwrap_or(""),
                                "position": i + 1,
                            }));
                        }
                    }
                    json!({"success": true, "data": {"web": web_results}})
                }
                Err(e) => {
                    if e.contains("TAVILY_API_KEY") {
                        json!({"success": false, "error": e})
                    } else {
                        json!({"success": false, "error": format!("Tavily search failed: {}", e)})
                    }
                }
            }
        };
        ToolResult::Text(dumps_indent2(&response_data))
    }
}

// ---------------------------------------------------------------------------
// web_extract
// ---------------------------------------------------------------------------

/// Vendor secret prefixes (subset of `agent/redact._PREFIX_RE`) used to block
/// secrets embedded in URLs before they reach a third-party extract backend.
static SECRET_PREFIX_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?:sk-ant-[A-Za-z0-9_\-]{8,}|sk-or-[A-Za-z0-9_\-]{8,}|sk-[A-Za-z0-9_\-]{16,}|gh[pousr]_[A-Za-z0-9]{16,}|xox[baprs]-[A-Za-z0-9\-]{10,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_\-]{16,})",
    )
    .unwrap()
});

fn percent_decode(url: &str) -> String {
    percent_encoding::percent_decode_str(url).decode_utf8_lossy().into_owned()
}

/// Port of `_web_extract_url` — accept URL strings or search-result objects.
fn web_extract_url(value: &Value) -> Option<String> {
    let v = match value {
        Value::Object(o) => o.get("url").or_else(|| o.get("href"))?.clone(),
        other => other.clone(),
    };
    match v {
        Value::String(s) => {
            let t = s.trim().to_string();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        }
        _ => None,
    }
}

/// Port of `convert_base64_images_to_links`.
pub fn convert_base64_images_to_links(text: &str) -> String {
    static MD_B64: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"!\[(?P<alt>[^\]]*)\]\(\s*data:image/[^;]+;base64,[A-Za-z0-9+/=\s]+\)").unwrap()
    });
    static PAREN_B64: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"\(\s*data:image/[^;]+;base64,[A-Za-z0-9+/=\s]+\)").unwrap());
    static BARE_B64: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"data:image/[^;]+;base64,[A-Za-z0-9+/=]+").unwrap());

    let out = MD_B64.replace_all(text, |caps: &regex::Captures| {
        let alt = caps.name("alt").map(|m| m.as_str().trim()).unwrap_or("");
        if alt.is_empty() {
            "[IMAGE]".to_string()
        } else {
            format!("[IMAGE: {}]", alt)
        }
    });
    let out = PAREN_B64.replace_all(&out, "[IMAGE]");
    BARE_B64.replace_all(&out, "[IMAGE]").into_owned()
}

/// Port of `_store_full_text` — write the full page to `<joey>/cache/web`.
fn store_full_text(url: &str, content: &str) -> Option<String> {
    let cache_dir = joey_core::constants::joey_dir("cache/web", "web_cache");
    std::fs::create_dir_all(&cache_dir).ok()?;
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_else(|| "page".to_string())
        .replace(':', "_");
    static SLUG_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[^A-Za-z0-9._-]").unwrap());
    let slug_full = SLUG_RE.replace_all(&host, "-").into_owned();
    let slug_cut: String = slug_full.chars().take(60).collect();
    let slug = slug_cut.trim_matches('-');
    let slug = if slug.is_empty() { "page" } else { slug };
    let digest = hex::encode(Sha256::digest(url.as_bytes()));
    let path = cache_dir.join(format!("{}-{}.md", slug, &digest[..10]));
    let mut content = content.to_string();
    if content.len() > MAX_STORED_TEXT_CHARS {
        let cut = crate::truncate::floor_char_boundary(&content, MAX_STORED_TEXT_CHARS);
        content = format!(
            "{}\n\n[... stored copy truncated at {} chars of {}; re-extract a more specific URL for the rest ...]",
            &content[..cut],
            commas(MAX_STORED_TEXT_CHARS as u64),
            commas(content.len() as u64)
        );
    }
    std::fs::write(&path, content).ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// Port of `_truncate_with_footer` — (model_text, was_truncated).
fn truncate_with_footer(content: &str, url: &str, char_limit: usize) -> (String, bool) {
    if content.len() <= char_limit {
        return (content.to_string(), false);
    }
    let head_budget = (char_limit as f64 * 0.75) as usize;
    let tail_budget = char_limit - head_budget;

    let head_end = crate::truncate::floor_char_boundary(content, head_budget);
    let mut head = &content[..head_end];
    let tail_start = crate::truncate::ceil_char_boundary(content, content.len() - tail_budget);
    let mut tail = &content[tail_start..];

    // Snap the head cut back to the last newline.
    if let Some(nl) = head.rfind('\n') {
        if nl as f64 > head_budget as f64 * 0.5 {
            head = &head[..nl];
        }
    }
    // Snap the tail cut forward to the next newline.
    if let Some(nl) = tail.find('\n') {
        if (nl as f64) < tail_budget as f64 * 0.5 {
            tail = &tail[nl + 1..];
        }
    }

    let total = content.len();
    let stored_path = store_full_text(url, content);

    let mut footer_lines: Vec<String> = vec![
        String::new(),
        format!("{} [TRUNCATED] {}", "─".repeat(8), "─".repeat(8)),
        format!(
            "Showing {} chars (head) + {} chars (tail) of {} total clean characters.",
            commas(head.len() as u64),
            commas(tail.len() as u64),
            commas(total as u64)
        ),
    ];
    match &stored_path {
        Some(path) => {
            let middle_start_line = head.matches('\n').count() + 2;
            footer_lines.push(format!("Full text saved to: {}", path));
            footer_lines.push(format!(
                "To read the omitted middle: read_file path=\"{}\" offset={} limit=200  (the file is the complete page; raise/lower offset to page through it).",
                path, middle_start_line
            ));
        }
        None => {
            footer_lines.push(
                "Full text could not be stored; re-run web_extract on a more specific URL or use browser_navigate for the complete page."
                    .to_string(),
            );
        }
    }
    footer_lines.push("─".repeat(29));

    let mut model_text =
        format!("{}\n\n[... middle omitted — see footer ...]\n\n{}", head, tail);
    model_text.push('\n');
    model_text.push_str(&footer_lines.join("\n"));
    (model_text, true)
}

fn extract_char_limit(ctx: &ToolContext, arg: Option<i64>) -> usize {
    let configured = match arg {
        Some(v) => v,
        None => ctx
            .config()
            .get_i64("web.extract_char_limit", DEFAULT_EXTRACT_CHAR_LIMIT as i64),
    };
    (configured.max(2000) as usize).min(500_000)
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
        "Extract content from web page URLs. Returns clean page content in markdown/text (no LLM summarization — fast). Also works with PDF URLs (arxiv papers, documents) — pass the PDF link directly. Pages within the char budget (default 15000) return whole; larger pages return a head+tail window with a footer telling you the full text's saved file path and the read_file call to page through the omitted middle. Inline images appear as [IMAGE: alt] placeholders; real image URLs are kept as links. If a URL fails or times out, use the browser tool instead."
    }
    fn emoji(&self) -> &str {
        "📄"
    }
    fn max_result_chars(&self) -> Option<usize> {
        Some(100_000)
    }
    fn check(&self, _ctx: &ToolContext) -> bool {
        check_web_api_key()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "urls": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "List of URLs to extract content from (max 5 URLs per call)",
                    "maxItems": 5
                },
                "char_limit": {
                    "type": "integer",
                    "description": "Optional per-page character budget sent back (default 15000). Pages larger than this are head+tail truncated with the full text stored to disk. Raise it when you need more of a long page inline.",
                    "minimum": 2000
                }
            },
            "required": ["urls"]
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let urls: Vec<Value> = match args.get("urls") {
            Some(Value::Array(a)) => a.iter().take(5).cloned().collect(),
            _ => Vec::new(),
        };
        let char_limit_arg = args.get("char_limit").and_then(|v| v.as_i64());

        // ── Normalize + secret blocking (exfiltration prevention) ─────
        let mut normalized: Vec<(usize, String)> = Vec::new();
        let mut invalid: Map<String, Value> = Map::new(); // index → entry
        for (index, item) in urls.iter().enumerate() {
            let Some(u) = web_extract_url(item) else {
                invalid.insert(
                    index.to_string(),
                    json!({
                        "url": "",
                        "title": "",
                        "content": "",
                        "error": format!(
                            "Invalid URL item at index {}: expected a URL string or an object with a string 'url' or 'href' field",
                            index
                        ),
                    }),
                );
                continue;
            };
            if SECRET_PREFIX_RE.is_match(&u) || SECRET_PREFIX_RE.is_match(&percent_decode(&u)) {
                return ToolResult::Text(dumps(&json!({
                    "success": false,
                    "error": "Blocked: URL contains what appears to be an API key or token. Secrets must not be sent in URLs.",
                })));
            }
            if let Some(key) = url_safety::sensitive_query_param_name(&u) {
                return ToolResult::Text(dumps(&json!({
                    "success": false,
                    "error": format!(
                        "Blocked: URL contains a credential-like query parameter ({}). Web extract backends are third-party readers; remove the sensitive query parameter or use a local browser session when this access is explicitly required.",
                        key
                    ),
                })));
            }
            normalized.push((index, u));
        }

        // ── SSRF protection — filter private/internal URLs per-URL ────
        let mut safe: Vec<(usize, String)> = Vec::new();
        let mut ssrf_blocked: Map<String, Value> = Map::new();
        for (index, u) in normalized {
            let cfg = ctx.config().clone();
            let u2 = u.clone();
            let ok = tokio::task::spawn_blocking(move || url_safety::is_safe_url(&u2, &cfg))
                .await
                .unwrap_or(false);
            if !ok {
                ssrf_blocked.insert(
                    index.to_string(),
                    json!({
                        "url": u, "title": "", "content": "",
                        "error": "Blocked: URL targets a private or internal network address",
                    }),
                );
            } else {
                safe.push((index, u));
            }
        }

        // ── Dispatch safe URLs to Tavily /extract ─────────────────────
        let mut provider_results: Vec<Value> = Vec::new();
        if !safe.is_empty() {
            let safe_urls: Vec<String> = safe.iter().map(|(_, u)| u.clone()).collect();
            let mut payload = Map::new();
            payload.insert("urls".into(), json!(safe_urls));
            payload.insert("include_images".into(), json!(false));
            match tavily_request("extract", payload).await {
                Ok(raw) => {
                    // `_normalize_tavily_documents`: successes then failures.
                    if let Some(results) = raw.get("results").and_then(|r| r.as_array()) {
                        for result in results {
                            let url = result
                                .get("url")
                                .and_then(|u| u.as_str())
                                .unwrap_or_else(|| safe.first().map(|(_, u)| u.as_str()).unwrap_or(""));
                            let rawc = result
                                .get("raw_content")
                                .and_then(|c| c.as_str())
                                .filter(|c| !c.is_empty())
                                .or_else(|| result.get("content").and_then(|c| c.as_str()))
                                .unwrap_or("");
                            provider_results.push(json!({
                                "url": url,
                                "title": result.get("title").and_then(|t| t.as_str()).unwrap_or(""),
                                "content": rawc,
                                "raw_content": rawc,
                            }));
                        }
                    }
                    if let Some(fails) = raw.get("failed_results").and_then(|r| r.as_array()) {
                        for fail in fails {
                            provider_results.push(json!({
                                "url": fail.get("url").and_then(|u| u.as_str()).unwrap_or(""),
                                "title": "",
                                "content": "",
                                "raw_content": "",
                                "error": fail.get("error").and_then(|e| e.as_str()).unwrap_or("extraction failed"),
                            }));
                        }
                    }
                    if let Some(fails) = raw.get("failed_urls").and_then(|r| r.as_array()) {
                        for fail_url in fails {
                            let u = fail_url.as_str().map(str::to_string).unwrap_or_else(|| fail_url.to_string());
                            provider_results.push(json!({
                                "url": u, "title": "", "content": "", "raw_content": "",
                                "error": "extraction failed",
                            }));
                        }
                    }
                }
                Err(e) => {
                    for (_, u) in &safe {
                        provider_results.push(json!({
                            "url": u, "title": "", "content": "",
                            "error": if e.contains("TAVILY_API_KEY") { e.clone() } else { format!("Tavily extract failed: {}", e) },
                        }));
                    }
                }
            }
        }

        // ── Reconstruct original input order ──────────────────────────
        // Provider results are matched to safe URLs by list order (providers
        // preserve request order); missing entries get the no-result error.
        let mut by_url: std::collections::HashMap<String, Vec<Value>> =
            std::collections::HashMap::new();
        for r in provider_results {
            let u = r.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string();
            by_url.entry(u).or_default().push(r);
        }
        let mut ordered: Vec<Value> = Vec::new();
        for index in 0..urls.len() {
            let key = index.to_string();
            if let Some(entry) = invalid.get(&key) {
                ordered.push(entry.clone());
                continue;
            }
            if let Some(entry) = ssrf_blocked.get(&key) {
                ordered.push(entry.clone());
                continue;
            }
            if let Some((_, u)) = safe.iter().find(|(i, _)| *i == index) {
                let entry = by_url.get_mut(u).and_then(|v| {
                    if v.is_empty() {
                        None
                    } else {
                        Some(v.remove(0))
                    }
                });
                match entry {
                    Some(e) => ordered.push(e),
                    None => ordered.push(json!({
                        "url": u, "title": "", "content": "",
                        "error": "Extract backend returned no result for this URL",
                    })),
                }
            }
        }

        if ordered.is_empty() {
            return ToolResult::Text(dumps(
                &json!({"error": "Content was inaccessible or not found"}),
            ));
        }

        // ── Truncate-and-store per result ─────────────────────────────
        let effective_char_limit = extract_char_limit(ctx, char_limit_arg);
        let mut trimmed_results: Vec<Value> = Vec::new();
        for result in &ordered {
            let error = result.get("error").cloned().unwrap_or(Value::Null);
            let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
            let title = result.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
            let mut content =
                result.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
            if error.is_null() {
                let raw_content = result
                    .get("raw_content")
                    .and_then(|c| c.as_str())
                    .filter(|c| !c.is_empty())
                    .unwrap_or(&content)
                    .to_string();
                if !raw_content.is_empty() {
                    let clean = convert_base64_images_to_links(&raw_content);
                    let (model_text, _truncated) =
                        truncate_with_footer(&clean, &url, effective_char_limit);
                    content = model_text;
                }
            }
            trimmed_results.push(json!({
                "url": url,
                "title": title,
                "content": content,
                "error": error,
            }));
        }

        let result_json = dumps_indent2(&json!({ "results": trimmed_results }));
        // Belt-and-suspenders sweep over the serialized JSON.
        ToolResult::Text(convert_base64_images_to_links(&result_json))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use joey_core::Config;

    fn ctx() -> ToolContext {
        ToolContext::new(std::env::temp_dir(), Config::defaults(), "w")
    }

    fn parse(r: &ToolResult) -> Value {
        serde_json::from_str(&r.to_content_string()).unwrap()
    }

    #[tokio::test]
    async fn extract_blocks_secret_urls() {
        let c = ctx();
        let r = parse(
            &WebExtract
                .execute(json!({"urls": ["https://evil.test/?k=sk-ant-abcdefghijklmnop"]}), &c)
                .await,
        );
        assert_eq!(r["success"], false);
        assert_eq!(
            r["error"],
            "Blocked: URL contains what appears to be an API key or token. Secrets must not be sent in URLs."
        );
    }

    #[tokio::test]
    async fn extract_blocks_credential_query_params() {
        let c = ctx();
        let r = parse(
            &WebExtract
                .execute(json!({"urls": ["https://site.test/cb?token=abc123"]}), &c)
                .await,
        );
        assert_eq!(r["success"], false);
        assert!(r["error"]
            .as_str()
            .unwrap()
            .starts_with("Blocked: URL contains a credential-like query parameter (token)."));
    }

    #[tokio::test]
    async fn extract_ssrf_blocked_entries_in_order() {
        let c = ctx();
        let r = parse(
            &WebExtract.execute(json!({"urls": ["http://169.254.169.254/latest"]}), &c).await,
        );
        let results = r["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0]["error"],
            "Blocked: URL targets a private or internal network address"
        );
    }

    #[tokio::test]
    async fn extract_invalid_item_entry() {
        let c = ctx();
        let r = parse(&WebExtract.execute(json!({"urls": [123]}), &c).await);
        let results = r["results"].as_array().unwrap();
        assert!(results[0]["error"]
            .as_str()
            .unwrap()
            .starts_with("Invalid URL item at index 0:"));
    }

    #[test]
    fn base64_placeholders() {
        let text = "before ![diagram](data:image/png;base64,AAAA====) after data:image/jpeg;base64,BBBB end";
        let out = convert_base64_images_to_links(text);
        assert!(out.contains("[IMAGE: diagram]"));
        assert!(out.contains("[IMAGE]"));
        assert!(!out.contains("base64,AAAA"));
        // Real image links untouched.
        let keep = convert_base64_images_to_links("![x](https://a.test/i.png)");
        assert_eq!(keep, "![x](https://a.test/i.png)");
    }

    #[test]
    fn footer_truncation_stores_full_text() {
        let _lock = crate::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let _guard = joey_core::constants::HomeOverrideGuard::new(dir.path().to_path_buf());
        let long: String = (0..2000).map(|i| format!("line number {}\n", i)).collect();
        let (text, truncated) = truncate_with_footer(&long, "https://example.com/page", 2000);
        assert!(truncated);
        assert!(text.contains("[TRUNCATED]"));
        assert!(text.contains("[... middle omitted — see footer ...]"));
        assert!(text.contains("Full text saved to: "));
        assert!(text.contains("To read the omitted middle: read_file path="));
        // Stored file holds the complete page.
        let path_line = text.lines().find(|l| l.starts_with("Full text saved to: ")).unwrap();
        let path = path_line.trim_start_matches("Full text saved to: ");
        assert_eq!(std::fs::read_to_string(path).unwrap(), long);
        // Short pages come back whole.
        let (whole, t2) = truncate_with_footer("short", "https://example.com", 2000);
        assert_eq!(whole, "short");
        assert!(!t2);
    }

    #[test]
    fn char_limit_clamps() {
        let c = ctx();
        assert_eq!(extract_char_limit(&c, None), 15000);
        assert_eq!(extract_char_limit(&c, Some(100)), 2000);
        assert_eq!(extract_char_limit(&c, Some(1_000_000)), 500_000);
    }
}
