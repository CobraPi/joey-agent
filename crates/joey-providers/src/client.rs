//! The provider client: maps a [`ProviderRequest`] onto the active provider's
//! wire protocol (OpenAI Chat Completions or Anthropic Messages), with SSE
//! streaming. Port of the `chat_completions` + `anthropic` transports and the
//! client-construction logic in `run_agent.py`.

use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::error::ProviderError;
use crate::profile::{ApiMode, ProviderProfile};
use crate::request::{ProviderRequest, ReasoningEffort};
use crate::types::{
    FinishReason, FunctionCall, NormalizedResponse, StreamEvent, ToolCall, Usage,
};

/// Default overall request timeout (upstream `HERMES_API_TIMEOUT=1800s`).
const DEFAULT_TIMEOUT_SECS: u64 = 1800;

/// A configured client bound to one provider + credentials.
pub struct ProviderClient {
    http: reqwest::Client,
    profile: ProviderProfile,
    base_url: String,
    api_key: Option<String>,
}

impl ProviderClient {
    /// Build a client for `profile`, resolving the API key from the environment
    /// unless `api_key` is supplied. `base_url` overrides the profile default
    /// when non-empty (custom endpoints).
    pub fn new(
        profile: ProviderProfile,
        base_url: Option<String>,
        api_key: Option<String>,
    ) -> Result<Self, ProviderError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs()))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(format!("{}/{}", joey_core::branding::CLI_NAME, joey_core::branding::VERSION))
            .build()
            .map_err(|e| ProviderError::Other(e.to_string()))?;

        let base = base_url
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| profile.base_url.to_string());
        let key = api_key.or_else(|| profile.resolve_api_key());

        Ok(Self {
            http,
            profile,
            base_url: base.trim_end_matches('/').to_string(),
            api_key: key,
        })
    }

    pub fn profile(&self) -> &ProviderProfile {
        &self.profile
    }

    pub fn has_credentials(&self) -> bool {
        self.api_key.is_some()
    }

    /// Non-streaming completion. Returns a fully-assembled response.
    pub async fn complete(&self, req: &ProviderRequest) -> Result<NormalizedResponse, ProviderError> {
        match self.profile.api_mode {
            ApiMode::ChatCompletions => self.chat_completions(req, None).await,
            ApiMode::AnthropicMessages => self.anthropic_messages(req, None).await,
        }
    }

    /// Streaming completion. Content/reasoning deltas are sent on `tx` as they
    /// arrive; the final assembled response is returned (and also emitted as
    /// [`StreamEvent::Done`]).
    pub async fn stream(
        &self,
        req: &ProviderRequest,
        tx: mpsc::UnboundedSender<StreamEvent>,
    ) -> Result<NormalizedResponse, ProviderError> {
        let streaming_req = ProviderRequest {
            stream: true,
            ..req.clone()
        };
        let result = match self.profile.api_mode {
            ApiMode::ChatCompletions => self.chat_completions(&streaming_req, Some(&tx)).await,
            ApiMode::AnthropicMessages => self.anthropic_messages(&streaming_req, Some(&tx)).await,
        };
        if let Ok(resp) = &result {
            let _ = tx.send(StreamEvent::Done(resp.clone()));
        }
        result
    }

    fn auth_header_openai(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut b = builder;
        if let Some(key) = &self.api_key {
            b = b.bearer_auth(key);
        }
        // OpenRouter attribution headers (harmless elsewhere).
        if self.profile.name == "openrouter" {
            b = b
                .header("HTTP-Referer", "https://github.com/joey/joey-agent")
                .header("X-Title", joey_core::branding::AGENT_NAME);
        }
        for (k, v) in self.profile.default_headers {
            b = b.header(*k, *v);
        }
        b
    }

    // ── OpenAI Chat Completions ──────────────────────────────────────────────

    async fn chat_completions(
        &self,
        req: &ProviderRequest,
        tx: Option<&mpsc::UnboundedSender<StreamEvent>>,
    ) -> Result<NormalizedResponse, ProviderError> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = self.build_openai_body(req);

        let resp = self
            .auth_header_openai(self.http.post(&url))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(status_error(resp).await);
        }

        if req.stream {
            self.parse_openai_stream(resp, tx).await
        } else {
            let v: Value = resp.json().await.map_err(|e| ProviderError::Parse(e.to_string()))?;
            parse_openai_response(&v)
        }
    }

    fn build_openai_body(&self, req: &ProviderRequest) -> Value {
        let wire_model = crate::profile::wire_model_name(&self.profile, &req.model);
        let mut messages: Vec<Value> = Vec::new();

        // System prompt as a leading system message.
        if let Some(sys) = &req.system {
            if !sys.is_empty() {
                messages.push(json!({"role": "system", "content": sys}));
            }
        }
        for m in &req.messages {
            messages.push(openai_message_json(m));
        }

        let mut body = json!({
            "model": wire_model,
            "messages": messages,
        });
        let obj = body.as_object_mut().unwrap();

        if !req.tools.is_empty() {
            obj.insert("tools".into(), serde_json::to_value(&req.tools).unwrap());
            obj.insert("tool_choice".into(), json!("auto"));
        }
        if let Some(mt) = req.max_tokens {
            obj.insert("max_tokens".into(), json!(mt));
        }
        if let Some(t) = req.temperature {
            obj.insert("temperature".into(), json!(t));
        }
        // Reasoning effort — OpenAI-style `reasoning_effort`, OpenRouter `reasoning`.
        match &req.reasoning {
            Some(ReasoningEffort::Level(level)) => {
                if self.profile.name == "openrouter" {
                    obj.insert("reasoning".into(), json!({"effort": level}));
                } else {
                    obj.insert("reasoning_effort".into(), json!(level));
                }
            }
            Some(ReasoningEffort::Disabled) => {
                if self.profile.name == "openrouter" {
                    obj.insert("reasoning".into(), json!({"enabled": false}));
                }
            }
            None => {}
        }
        if req.stream {
            obj.insert("stream".into(), json!(true));
            // Native Gemini's OpenAI shim rejects stream_options; omit there.
            if self.profile.name != "gemini" {
                obj.insert("stream_options".into(), json!({"include_usage": true}));
            }
        }
        body
    }

    async fn parse_openai_stream(
        &self,
        resp: reqwest::Response,
        tx: Option<&mpsc::UnboundedSender<StreamEvent>>,
    ) -> Result<NormalizedResponse, ProviderError> {
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut finish = FinishReason::Stop;
        let mut usage = Usage::default();
        let mut model: Option<String> = None;
        // tool_calls assembled by index.
        let mut tool_accum: Vec<ToolAccum> = Vec::new();

        let mut buf = String::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ProviderError::Connection(e.to_string()))?;
            buf.push_str(&String::from_utf8_lossy(&chunk));

            // Process complete SSE lines (data: ...\n).
            while let Some(nl) = buf.find('\n') {
                let line = buf[..nl].trim().to_string();
                buf.drain(..=nl);
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                if model.is_none() {
                    model = v.get("model").and_then(|m| m.as_str()).map(str::to_string);
                }
                if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                    usage = parse_usage(u);
                }
                let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
                    continue;
                };
                if let Some(fr) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                    finish = FinishReason::from_wire(fr);
                }
                let Some(delta) = choice.get("delta") else {
                    continue;
                };
                if let Some(c) = delta.get("content").and_then(|c| c.as_str()) {
                    if !c.is_empty() {
                        content.push_str(c);
                        if let Some(tx) = tx {
                            let _ = tx.send(StreamEvent::ContentDelta(c.to_string()));
                        }
                    }
                }
                // Reasoning deltas (OpenRouter `reasoning`, others `reasoning_content`).
                for key in ["reasoning", "reasoning_content"] {
                    if let Some(r) = delta.get(key).and_then(|r| r.as_str()) {
                        if !r.is_empty() {
                            reasoning.push_str(r);
                            if let Some(tx) = tx {
                                let _ = tx.send(StreamEvent::ReasoningDelta(r.to_string()));
                            }
                        }
                    }
                }
                if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                    accumulate_tool_calls(&mut tool_accum, tcs);
                }
            }
        }

        let tool_calls = finalize_tool_calls(tool_accum);
        if !tool_calls.is_empty() && finish == FinishReason::Stop {
            finish = FinishReason::ToolCalls;
        }
        Ok(NormalizedResponse {
            content,
            tool_calls,
            finish_reason: finish,
            reasoning: (!reasoning.is_empty()).then_some(reasoning),
            usage,
            model,
        })
    }

    // ── Anthropic Messages ───────────────────────────────────────────────────

    async fn anthropic_messages(
        &self,
        req: &ProviderRequest,
        tx: Option<&mpsc::UnboundedSender<StreamEvent>>,
    ) -> Result<NormalizedResponse, ProviderError> {
        let url = format!("{}/v1/messages", self.base_url);
        let body = self.build_anthropic_body(req);

        let mut builder = self
            .http
            .post(&url)
            .header("anthropic-version", "2023-06-01")
            .json(&body);
        if let Some(key) = &self.api_key {
            // sk-ant-api* keys use x-api-key; OAuth tokens use Bearer.
            if key.starts_with("sk-ant-oat") || key.starts_with("cc-") {
                builder = builder.bearer_auth(key);
            } else {
                builder = builder.header("x-api-key", key);
            }
        }

        let resp = builder.send().await?;
        if !resp.status().is_success() {
            return Err(status_error(resp).await);
        }

        if req.stream {
            self.parse_anthropic_stream(resp, tx).await
        } else {
            let v: Value = resp.json().await.map_err(|e| ProviderError::Parse(e.to_string()))?;
            parse_anthropic_response(&v)
        }
    }

    fn build_anthropic_body(&self, req: &ProviderRequest) -> Value {
        let wire_model = crate::profile::wire_model_name(&self.profile, &req.model);
        let messages: Vec<Value> = req.messages.iter().map(anthropic_message_json).collect();

        let mut body = json!({
            "model": wire_model,
            "messages": messages,
            "max_tokens": req.max_tokens.unwrap_or(self.profile.default_max_tokens),
        });
        let obj = body.as_object_mut().unwrap();
        if let Some(sys) = &req.system {
            if !sys.is_empty() {
                obj.insert("system".into(), json!(sys));
            }
        }
        if !req.tools.is_empty() {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.function.name,
                        "description": t.function.description,
                        "input_schema": t.function.parameters,
                    })
                })
                .collect();
            obj.insert("tools".into(), json!(tools));
        }
        // Extended thinking.
        if let Some(ReasoningEffort::Level(level)) = &req.reasoning {
            let budget = match level.as_str() {
                "xhigh" | "max" | "ultra" => 32000,
                "high" => 16000,
                "medium" => 8000,
                _ => 4000,
            };
            obj.insert("thinking".into(), json!({"type": "enabled", "budget_tokens": budget}));
            // Thinking requires temperature=1 on legacy models.
            obj.insert("temperature".into(), json!(1));
        }
        if req.stream {
            obj.insert("stream".into(), json!(true));
        }
        body
    }

    async fn parse_anthropic_stream(
        &self,
        resp: reqwest::Response,
        tx: Option<&mpsc::UnboundedSender<StreamEvent>>,
    ) -> Result<NormalizedResponse, ProviderError> {
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut finish = FinishReason::Stop;
        let mut usage = Usage::default();
        // Anthropic streams content blocks; tool_use args arrive as partial JSON.
        let mut blocks: Vec<AnthropicBlockAccum> = Vec::new();

        let mut buf = String::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ProviderError::Connection(e.to_string()))?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(nl) = buf.find('\n') {
                let line = buf[..nl].trim().to_string();
                buf.drain(..=nl);
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<Value>(data.trim()) else {
                    continue;
                };
                match v.get("type").and_then(|t| t.as_str()) {
                    Some("content_block_start") => {
                        let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                        let block = v.get("content_block");
                        let btype = block
                            .and_then(|b| b.get("type"))
                            .and_then(|t| t.as_str())
                            .unwrap_or("text");
                        ensure_block(&mut blocks, idx);
                        blocks[idx].block_type = btype.to_string();
                        if btype == "tool_use" {
                            blocks[idx].tool_id = block
                                .and_then(|b| b.get("id"))
                                .and_then(|s| s.as_str())
                                .unwrap_or("")
                                .to_string();
                            blocks[idx].tool_name = block
                                .and_then(|b| b.get("name"))
                                .and_then(|s| s.as_str())
                                .unwrap_or("")
                                .to_string();
                        }
                    }
                    Some("content_block_delta") => {
                        let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                        ensure_block(&mut blocks, idx);
                        let delta = v.get("delta");
                        if let Some(t) = delta.and_then(|d| d.get("text")).and_then(|t| t.as_str()) {
                            content.push_str(t);
                            if let Some(tx) = tx {
                                let _ = tx.send(StreamEvent::ContentDelta(t.to_string()));
                            }
                        }
                        if let Some(t) = delta.and_then(|d| d.get("thinking")).and_then(|t| t.as_str()) {
                            reasoning.push_str(t);
                            if let Some(tx) = tx {
                                let _ = tx.send(StreamEvent::ReasoningDelta(t.to_string()));
                            }
                        }
                        if let Some(pj) = delta.and_then(|d| d.get("partial_json")).and_then(|t| t.as_str()) {
                            blocks[idx].json_buf.push_str(pj);
                        }
                    }
                    Some("message_delta") => {
                        if let Some(sr) = v
                            .get("delta")
                            .and_then(|d| d.get("stop_reason"))
                            .and_then(|s| s.as_str())
                        {
                            finish = FinishReason::from_wire(sr);
                        }
                        if let Some(u) = v.get("usage") {
                            merge_anthropic_usage(&mut usage, u);
                        }
                    }
                    Some("message_start") => {
                        if let Some(u) = v.get("message").and_then(|m| m.get("usage")) {
                            merge_anthropic_usage(&mut usage, u);
                        }
                    }
                    _ => {}
                }
            }
        }

        let tool_calls: Vec<ToolCall> = blocks
            .into_iter()
            .filter(|b| b.block_type == "tool_use")
            .map(|b| ToolCall {
                id: b.tool_id,
                call_type: "function".to_string(),
                function: FunctionCall {
                    name: b.tool_name,
                    arguments: if b.json_buf.is_empty() { "{}".to_string() } else { b.json_buf },
                },
            })
            .collect();
        if !tool_calls.is_empty() {
            finish = FinishReason::ToolCalls;
        }
        Ok(NormalizedResponse {
            content,
            tool_calls,
            finish_reason: finish,
            reasoning: (!reasoning.is_empty()).then_some(reasoning),
            usage,
            model: None,
        })
    }
}

fn timeout_secs() -> u64 {
    std::env::var("JOEY_API_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
}

async fn status_error(resp: reqwest::Response) -> ProviderError {
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs);
    let body = resp.text().await.unwrap_or_default();
    ProviderError::from_status(status, &body, retry_after)
}

// ── OpenAI message/response helpers ──────────────────────────────────────────

fn openai_message_json(m: &crate::types::Message) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("role".into(), json!(m.role));

    if let Some(parts) = &m.content_parts {
        obj.insert("content".into(), serde_json::to_value(parts).unwrap());
    } else {
        obj.insert("content".into(), json!(m.content.clone().unwrap_or_default()));
    }
    if !m.tool_calls.is_empty() {
        obj.insert("tool_calls".into(), serde_json::to_value(&m.tool_calls).unwrap());
    }
    if let Some(id) = &m.tool_call_id {
        obj.insert("tool_call_id".into(), json!(id));
    }
    if m.role == "tool" {
        if let Some(name) = &m.name {
            obj.insert("name".into(), json!(name));
        }
    }
    Value::Object(obj)
}

fn parse_openai_response(v: &Value) -> Result<NormalizedResponse, ProviderError> {
    let choice = v
        .get("choices")
        .and_then(|c| c.get(0))
        .ok_or_else(|| ProviderError::Parse("no choices in response".into()))?;
    let msg = choice.get("message").unwrap_or(&Value::Null);
    let content = msg
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    let reasoning = msg
        .get("reasoning")
        .or_else(|| msg.get("reasoning_content"))
        .and_then(|r| r.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let mut tool_calls = Vec::new();
    if let Some(tcs) = msg.get("tool_calls").and_then(|t| t.as_array()) {
        for tc in tcs {
            let id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
            let f = tc.get("function").unwrap_or(&Value::Null);
            let name = f.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let args = f.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}").to_string();
            tool_calls.push(ToolCall::new(id, name, args));
        }
    }

    let finish = choice
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .map(FinishReason::from_wire)
        .unwrap_or(FinishReason::Stop);
    let usage = v.get("usage").map(parse_usage).unwrap_or_default();
    let model = v.get("model").and_then(|m| m.as_str()).map(str::to_string);

    Ok(NormalizedResponse {
        content,
        tool_calls,
        finish_reason: finish,
        reasoning,
        usage,
        model,
    })
}

fn parse_usage(u: &Value) -> Usage {
    let get = |k: &str| u.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    let details = u.get("prompt_tokens_details");
    let cache_read = details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    Usage {
        prompt_tokens: get("prompt_tokens"),
        completion_tokens: get("completion_tokens"),
        total_tokens: get("total_tokens"),
        cache_read_tokens: cache_read,
        cache_write_tokens: 0,
        reasoning_tokens: u
            .get("completion_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    }
}

// ── Streaming tool-call accumulation (OpenAI) ────────────────────────────────

#[derive(Default)]
struct ToolAccum {
    id: String,
    name: String,
    args: String,
}

fn accumulate_tool_calls(accum: &mut Vec<ToolAccum>, tcs: &[Value]) {
    for tc in tcs {
        let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
        while accum.len() <= idx {
            accum.push(ToolAccum::default());
        }
        if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
            if !id.is_empty() {
                accum[idx].id = id.to_string();
            }
        }
        if let Some(f) = tc.get("function") {
            if let Some(name) = f.get("name").and_then(|n| n.as_str()) {
                if !name.is_empty() {
                    accum[idx].name = name.to_string();
                }
            }
            if let Some(args) = f.get("arguments").and_then(|a| a.as_str()) {
                accum[idx].args.push_str(args);
            }
        }
    }
}

fn finalize_tool_calls(accum: Vec<ToolAccum>) -> Vec<ToolCall> {
    accum
        .into_iter()
        .filter(|a| !a.name.is_empty())
        .enumerate()
        .map(|(i, a)| {
            let id = if a.id.is_empty() { format!("call_{}", i) } else { a.id };
            let args = if a.args.is_empty() { "{}".to_string() } else { a.args };
            ToolCall::new(id, a.name, args)
        })
        .collect()
}

// ── Anthropic message/response helpers ───────────────────────────────────────

fn anthropic_message_json(m: &crate::types::Message) -> Value {
    // Anthropic has no "tool" role — tool results are user messages with a
    // tool_result content block; assistant tool calls are tool_use blocks.
    match m.role.as_str() {
        "tool" => json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                "content": m.content.clone().unwrap_or_default(),
            }],
        }),
        "assistant" if !m.tool_calls.is_empty() => {
            let mut blocks: Vec<Value> = Vec::new();
            if let Some(text) = &m.content {
                if !text.is_empty() {
                    blocks.push(json!({"type": "text", "text": text}));
                }
            }
            for tc in &m.tool_calls {
                blocks.push(json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.function.name,
                    "input": tc.parsed_args(),
                }));
            }
            json!({"role": "assistant", "content": blocks})
        }
        role => {
            if let Some(parts) = &m.content_parts {
                let blocks: Vec<Value> = parts.iter().map(anthropic_content_part).collect();
                json!({"role": role, "content": blocks})
            } else {
                json!({"role": role, "content": m.content.clone().unwrap_or_default()})
            }
        }
    }
}

fn anthropic_content_part(part: &crate::types::ContentPart) -> Value {
    match part {
        crate::types::ContentPart::Text { text } => json!({"type": "text", "text": text}),
        crate::types::ContentPart::ImageUrl { image_url } => {
            let url = &image_url.url;
            if let Some(rest) = url.strip_prefix("data:") {
                // data:<media_type>;base64,<data>
                if let Some((meta, data)) = rest.split_once(",") {
                    let media_type = meta.split(';').next().unwrap_or("image/png");
                    return json!({
                        "type": "image",
                        "source": {"type": "base64", "media_type": media_type, "data": data},
                    });
                }
            }
            json!({"type": "image", "source": {"type": "url", "url": url}})
        }
    }
}

fn parse_anthropic_response(v: &Value) -> Result<NormalizedResponse, ProviderError> {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();

    if let Some(blocks) = v.get("content").and_then(|c| c.as_array()) {
        for b in blocks {
            match b.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                        content.push_str(t);
                    }
                }
                Some("thinking") => {
                    if let Some(t) = b.get("thinking").and_then(|t| t.as_str()) {
                        reasoning.push_str(t);
                    }
                }
                Some("tool_use") => {
                    let id = b.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                    let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    let input = b.get("input").cloned().unwrap_or(json!({}));
                    tool_calls.push(ToolCall::new(id, name, input.to_string()));
                }
                _ => {}
            }
        }
    }

    let finish = v
        .get("stop_reason")
        .and_then(|s| s.as_str())
        .map(FinishReason::from_wire)
        .unwrap_or(FinishReason::Stop);
    let mut usage = Usage::default();
    if let Some(u) = v.get("usage") {
        merge_anthropic_usage(&mut usage, u);
    }
    let model = v.get("model").and_then(|m| m.as_str()).map(str::to_string);

    Ok(NormalizedResponse {
        content,
        tool_calls,
        finish_reason: finish,
        reasoning: (!reasoning.is_empty()).then_some(reasoning),
        usage,
        model,
    })
}

fn merge_anthropic_usage(usage: &mut Usage, u: &Value) {
    let get = |k: &str| u.get(k).and_then(|v| v.as_u64());
    if let Some(i) = get("input_tokens") {
        usage.prompt_tokens = i;
    }
    if let Some(o) = get("output_tokens") {
        usage.completion_tokens = o;
    }
    if let Some(c) = get("cache_read_input_tokens") {
        usage.cache_read_tokens = c;
    }
    if let Some(c) = get("cache_creation_input_tokens") {
        usage.cache_write_tokens = c;
    }
    usage.total_tokens = usage.prompt_tokens + usage.completion_tokens;
}

#[derive(Default)]
struct AnthropicBlockAccum {
    block_type: String,
    tool_id: String,
    tool_name: String,
    json_buf: String,
}

fn ensure_block(blocks: &mut Vec<AnthropicBlockAccum>, idx: usize) {
    while blocks.len() <= idx {
        blocks.push(AnthropicBlockAccum::default());
    }
}
