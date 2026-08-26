//! The provider client: maps a [`ProviderRequest`] onto the active provider's
//! wire protocol (OpenAI Chat Completions or Anthropic Messages), with SSE
//! streaming. Port of the `chat_completions` + `anthropic` transports and the
//! client-construction logic in `run_agent.py`.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::anthropic;
use crate::chat;
use crate::copilot::{self, CopilotAuth};
use crate::error::{parse_retry_after, ProviderError};
use crate::profile::{ApiMode, ProviderProfile};
use crate::request::ProviderRequest;
use crate::types::{FinishReason, FunctionCall, NormalizedResponse, StreamEvent, ToolCall, Usage};

/// Default overall request timeout (upstream `HERMES_API_TIMEOUT=1800s`).
const DEFAULT_TIMEOUT_SECS: u64 = 1800;
/// Default per-read stall timeout for streaming (upstream
/// `HERMES_STREAM_READ_TIMEOUT`, chat_completion_helpers.py:2640-2657).
const DEFAULT_STREAM_READ_TIMEOUT_SECS: u64 = 120;

/// A configured client bound to one provider + credentials.
pub struct ProviderClient {
    http: reqwest::Client,
    profile: ProviderProfile,
    base_url: String,
    api_key: Option<String>,
    copilot_auth: Option<Arc<CopilotAuth>>,
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
        // xAI's upstream wire is codex_responses, which is not ported. Copilot
        // (and the ai-usage-hud proxy) use the Responses transport in this client.
        if profile.api_mode == ApiMode::CodexResponses
            && !crate::profile::is_copilot_wire(profile.name)
        {
            return Err(ProviderError::Other(format!(
                "provider '{}' requires the codex_responses wire mode, not yet ported",
                profile.name
            )));
        }

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs()))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(format!(
                "{}/{}",
                joey_core::branding::CLI_NAME,
                joey_core::branding::VERSION
            ))
            .build()
            .map_err(|e| ProviderError::Other(e.to_string()))?;

        let base = base_url
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| profile.base_url.to_string());
        let mut key = api_key.or_else(|| profile.resolve_api_key());
        let copilot_auth = if crate::profile::is_copilot_wire(profile.name) {
            let raw = if let Some(explicit) = key.take() {
                copilot::validate_copilot_token(&explicit).map_err(ProviderError::Auth)?;
                explicit
            } else {
                copilot::resolve_copilot_token()?.0
            };
            // A non-githubcopilot.com base-URL override on the copilot profile
            // pins the endpoint: the proxy accepts the raw GitHub credential,
            // so skip the exchange flow and serve all requests from it.
            let pinned = {
                let host =
                    joey_core::utils::base_url_hostname(&base).to_ascii_lowercase();
                !base.trim().is_empty()
                    && host != "api.githubcopilot.com"
                    && !host.ends_with(".githubcopilot.com")
                    && !host.is_empty()
            };
            (!raw.is_empty()).then(|| {
                if pinned {
                    Arc::new(copilot::CopilotAuth::with_endpoint(
                        raw,
                        base.trim_end_matches('/').to_string(),
                    ))
                } else {
                    Arc::new(copilot::CopilotAuth::new(raw))
                }
            })
        } else {
            None
        };

        Ok(Self {
            http,
            profile,
            base_url: base.trim_end_matches('/').to_string(),
            api_key: key,
            copilot_auth,
        })
    }

    pub fn profile(&self) -> &ProviderProfile {
        &self.profile
    }

    pub fn has_credentials(&self) -> bool {
        self.api_key.is_some()
            || self
                .copilot_auth
                .as_ref()
                .map(|a| a.has_raw_token())
                .unwrap_or(false)
    }

    /// Derive the effective API mode for a request. Non-copilot clients pin
    /// their wire at build time (profile.api_mode). Copilot-wire clients
    /// (copilot + ai-usage-hud) re-derive per-request because the SAME proxy
    /// serves models on different wires — gpt-5.x speaks /responses only,
    /// claude/gemini speak /chat/completions or /v1/messages — and a
    /// per-turn model substitution (fallback chain, NeuroCode tier,
    /// llm-selector allocator, image routing) must not ride the build-time
    /// wire of a different model (observed live 2026-08-18: chat-wire client
    /// + gpt-5.6-luna request → HTTP 400 "not accessible via the
    /// /chat/completions endpoint").
    fn effective_api_mode(&self, req: &ProviderRequest) -> ApiMode {
        if !crate::profile::is_copilot_wire(self.profile.name) {
            return self.profile.api_mode;
        }
        // Cache-only peek: `fetch_model_catalog` (called at client-build time
        // via build_client) keeps the cache warm; a cold cache degrades to
        // the same heuristic `build_client` uses. Never block the request
        // path on a synchronous catalog fetch.
        let normalized = copilot::normalize_model_id(&req.model);
        let catalog = copilot::peek_model_catalog();
        let entry = catalog.iter().find(|item| {
            item.get("id").and_then(Value::as_str) == Some(normalized.as_str())
        });
        copilot::model_api_mode(&req.model, entry)
    }

    /// Non-streaming completion. Returns a fully-assembled response.
    pub async fn complete(
        &self,
        req: &ProviderRequest,
    ) -> Result<NormalizedResponse, ProviderError> {
        match self.effective_api_mode(req) {
            ApiMode::ChatCompletions => self.chat_completions(req, None).await,
            ApiMode::AnthropicMessages => self.anthropic_messages(req, None).await,
            ApiMode::CodexResponses => self.responses(req, None).await,
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
        let result = match self.effective_api_mode(&streaming_req) {
            ApiMode::ChatCompletions => self.chat_completions(&streaming_req, Some(&tx)).await,
            ApiMode::AnthropicMessages => self.anthropic_messages(&streaming_req, Some(&tx)).await,
            ApiMode::CodexResponses => self.responses(&streaming_req, Some(&tx)).await,
        };
        if let Ok(resp) = &result {
            let _ = tx.send(StreamEvent::Done(Box::new(resp.clone())));
        }
        result
    }

    fn auth_header_openai(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut b = builder;
        if let Some(key) = &self.api_key {
            b = b.bearer_auth(key);
        }
        // OpenRouter attribution + categories headers (auxiliary_client.py:569-573).
        // Referer/X-Title are the correct rebrands per the branding policy. The
        // per-request x-anthropic-beta for Claude models is added in
        // chat_completions() where the model is available (agent_init.py:1107-1118).
        if self.profile.name == "openrouter" {
            b = b
                .header("HTTP-Referer", "https://github.com/joey/joey-agent")
                .header("X-Title", joey_core::branding::AGENT_NAME)
                .header("X-OpenRouter-Categories", "productivity,cli-agent");
        }
        for (k, v) in self.profile.default_headers {
            b = b.header(*k, *v);
        }
        b
    }

    async fn request_credentials(&self) -> Result<(String, Option<String>), ProviderError> {
        if let Some(auth) = &self.copilot_auth {
            let credentials = auth.credentials(&self.http).await?;
            return Ok((credentials.base_url, Some(credentials.token)));
        }
        Ok((self.base_url.clone(), self.api_key.clone()))
    }

    fn copilot_headers(
        &self,
        mut builder: reqwest::RequestBuilder,
        token: &str,
        user_initiated: bool,
        is_vision: bool,
    ) -> reqwest::RequestBuilder {
        builder = builder.bearer_auth(token);
        for (name, value) in copilot::request_headers(user_initiated, is_vision) {
            builder = builder.header(name, value);
        }
        builder
    }

    /// Retry once with a freshly exchanged Copilot API token after a 401.
    async fn send_with_auth_refresh(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ProviderError> {
        let retry = builder.try_clone();
        let response = builder.send().await?;
        if response.status().as_u16() != 401 || self.copilot_auth.is_none() {
            return Ok(response);
        }
        let Some(retry) = retry else {
            return Ok(response);
        };
        // SAFETY: the retry path that reaches here only runs when
        // copilot_auth was Some (checked in the retry decision above).
        let auth = self.copilot_auth.as_ref().expect("checked above");
        auth.invalidate();
        let credentials = auth.credentials(&self.http).await?;
        Ok(retry.bearer_auth(credentials.token).send().await?)
    }

    // ── OpenAI Chat Completions ──────────────────────────────────────────────

    async fn chat_completions(
        &self,
        req: &ProviderRequest,
        tx: Option<&mpsc::UnboundedSender<StreamEvent>>,
    ) -> Result<NormalizedResponse, ProviderError> {
        let (request_base, request_key) = self.request_credentials().await?;
        let url = format!("{}/chat/completions", request_base);
        let body = self.build_openai_body(req);

        let mut builder = if crate::profile::is_copilot_wire(self.profile.name) {
            let token = request_key.as_deref().ok_or_else(|| {
                ProviderError::Auth(
                    "No GitHub Copilot token found. Run `joey auth copilot login` or `joey model`."
                        .into(),
                )
            })?;
            self.copilot_headers(
                self.http.post(&url),
                token,
                request_is_user_initiated(req),
                request_has_images(req),
            )
        } else {
            self.auth_header_openai(self.http.post(&url))
        };
        // x-anthropic-beta for Claude models via OpenRouter (agent_init.py:1107-1118).
        if self.profile.name == "openrouter" && req.model.to_lowercase().contains("claude") {
            builder = builder.header("x-anthropic-beta", "fine-grained-tool-streaming-2025-05-14");
        }

        let resp = self.send_with_auth_refresh(builder.json(&body)).await?;

        if !resp.status().is_success() {
            return Err(status_error(resp).await);
        }

        if req.stream {
            self.parse_openai_stream(resp, tx).await
        } else {
            let v: Value = resp
                .json()
                .await
                .map_err(|e| ProviderError::Parse(e.to_string()))?;
            parse_openai_response(&v)
        }
    }

    fn build_openai_body(&self, req: &ProviderRequest) -> Value {
        let mut body = chat::build_openai_body(&self.profile, &self.base_url, req);
        if req.stream {
            // SAFETY: `body` comes from `chat::build_openai_body` which
            // always returns a `Value::Object`.
            let obj = body.as_object_mut().unwrap();
            obj.insert("stream".into(), json!(true));
            // stream_options.include_usage: omit ONLY for native-Gemini
            // endpoints (generativelanguage.googleapis.com WITHOUT /openai).
            // The port's gemini profile IS the /openai shim, so it keeps it
            // (chat_completion_helpers.py:2659-2666, M4).
            if !is_native_gemini_base_url(&self.base_url) {
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
        let mut finish: Option<FinishReason> = None;
        let mut saw_finish_string = false;
        let mut usage = Usage::default();
        let mut model: Option<String> = None;
        let mut saw_event = false;
        // tool_calls assembled by slot; Ollama index-reuse handled below.
        let mut tool_accum: Vec<ToolAccum> = Vec::new();
        let mut last_id_at_idx: std::collections::HashMap<u64, String> = Default::default();
        let mut active_slot_by_idx: std::collections::HashMap<u64, usize> = Default::default();

        let mut buf = String::new();
        let mut stream = resp.bytes_stream();
        let read_timeout = Duration::from_secs(stream_read_timeout_secs());
        loop {
            let next = tokio::time::timeout(read_timeout, stream.next()).await;
            let chunk = match next {
                Err(_) => {
                    return Err(ProviderError::Timeout(format!(
                        "stream stalled: no chunk within {}s",
                        read_timeout.as_secs()
                    )))
                }
                Ok(None) => break,
                Ok(Some(c)) => c.map_err(|e| ProviderError::Connection(e.to_string()))?,
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));

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
                saw_event = true;
                if model.is_none() {
                    model = v.get("model").and_then(|m| m.as_str()).map(str::to_string);
                }
                if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                    usage = parse_usage(u);
                }
                let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
                    continue;
                };
                // Integer finish_reason tolerance (chat_completions.py:667-671).
                if let Some(fr) = choice.get("finish_reason") {
                    if let Some(s) = fr.as_str() {
                        finish = Some(FinishReason::from_wire(s));
                        saw_finish_string = true;
                    } else if let Some(n) = fr.as_i64() {
                        finish = Some(FinishReason::from_wire(&n.to_string()));
                        saw_finish_string = true;
                    }
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
                // First-non-null of reasoning_content / reasoning, not both
                // appended (chat_completion_helpers.py:2813, M8). Joey
                // extension: copilot-wire claude models report thinking as
                // `reasoning_text` (verified live 2026-08-21, see the
                // thinking-param comment in chat.rs) — third fallback beyond
                // the upstream pair.
                let r = delta
                    .get("reasoning_content")
                    .and_then(|r| r.as_str())
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        delta
                            .get("reasoning")
                            .and_then(|r| r.as_str())
                            .filter(|s| !s.is_empty())
                    })
                    .or_else(|| {
                        delta
                            .get("reasoning_text")
                            .and_then(|r| r.as_str())
                            .filter(|s| !s.is_empty())
                    });
                if let Some(r) = r {
                    reasoning.push_str(r);
                    if let Some(tx) = tx {
                        let _ = tx.send(StreamEvent::ReasoningDelta(r.to_string()));
                    }
                }
                if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                    accumulate_tool_calls(
                        &mut tool_accum,
                        tcs,
                        &mut last_id_at_idx,
                        &mut active_slot_by_idx,
                    );
                }
            }
        }

        // Final-line flush: a stream whose last event lacks a trailing
        // newline leaves it in `buf` — usage/[DONE]/finish_reason would be
        // silently dropped. Re-run the line parser over the remainder once.
        if !buf.trim().is_empty() {
            let line = buf.trim().to_string();
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if data != "[DONE]" {
                    if let Ok(v) = serde_json::from_str::<Value>(data) {
                        saw_event = true;
                        if model.is_none() {
                            model = v.get("model").and_then(|m| m.as_str()).map(str::to_string);
                        }
                        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                            usage = parse_usage(u);
                        }
                        if let Some(choice) = v.get("choices").and_then(|c| c.get(0)) {
                            if let Some(fr) = choice.get("finish_reason") {
                                if let Some(s) = fr.as_str() {
                                    finish = Some(FinishReason::from_wire(s));
                                    saw_finish_string = true;
                                } else if let Some(n) = fr.as_i64() {
                                    finish = Some(FinishReason::from_wire(&n.to_string()));
                                    saw_finish_string = true;
                                }
                            }
                            if let Some(delta) = choice.get("delta") {
                                if let Some(c) = delta.get("content").and_then(|c| c.as_str()) {
                                    if !c.is_empty() {
                                        content.push_str(c);
                                    }
                                }
                                let r = delta
                                    .get("reasoning_content")
                                    .and_then(|r| r.as_str())
                                    .filter(|s| !s.is_empty())
                                    .or_else(|| {
                                        delta
                                            .get("reasoning")
                                            .and_then(|r| r.as_str())
                                            .filter(|s| !s.is_empty())
                                    });
                                if let Some(r) = r {
                                    reasoning.push_str(r);
                                }
                                if let Some(tcs) =
                                    delta.get("tool_calls").and_then(|t| t.as_array())
                                {
                                    accumulate_tool_calls(
                                        &mut tool_accum,
                                        tcs,
                                        &mut last_id_at_idx,
                                        &mut active_slot_by_idx,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        let tool_calls = finalize_tool_calls(tool_accum);

        // Zero-event guard: a stream that yielded nothing usable is an error,
        // not a legitimate empty completion (chat_completion_helpers.py:2968-2980).
        if finish.is_none()
            && content.is_empty()
            && reasoning.is_empty()
            && tool_calls.is_empty()
            && !saw_event
        {
            return Err(ProviderError::EmptyStream(
                "provider returned an empty stream with no finish_reason".into(),
            ));
        }

        // Partial-stream handling (chat_completion_helpers.py:2982-3044, M7).
        // A tool call whose accumulated args don't parse is truncated.
        let has_truncated_tool_args = tool_calls.iter().any(|tc| {
            let a = tc.function.arguments.trim();
            !a.is_empty() && a != "{}" && serde_json::from_str::<Value>(a).is_err()
        });
        let mut finish = finish.unwrap_or(FinishReason::Stop);
        if !saw_finish_string && has_truncated_tool_args {
            // Tool-call args dropped mid-stream with no finish_reason — flag so
            // the loop retries instead of executing a truncated call.
            finish = FinishReason::Length;
        } else if !saw_finish_string && !content.is_empty() && tool_calls.is_empty() {
            // Text-only drop: connection ended after text with no finish_reason.
            finish = FinishReason::Length;
        } else if has_truncated_tool_args {
            // finish_reason present but args truncated → genuine output-cap hit.
            finish = FinishReason::Length;
        } else if !tool_calls.is_empty() && finish == FinishReason::Stop {
            finish = FinishReason::ToolCalls;
        }

        Ok(NormalizedResponse {
            content,
            tool_calls,
            finish_reason: finish,
            reasoning: (!reasoning.is_empty()).then_some(reasoning),
            usage,
            model,
            reasoning_details: None,
            anthropic_content_blocks: None,
        })
    }

    // ── OpenAI Responses (Copilot GPT-5+/Codex) ─────────────────────────────

    fn build_responses_body(&self, req: &ProviderRequest) -> Value {
        let mut input = Vec::new();
        for message in &req.messages {
            if message.role == "tool" {
                if let Some(call_id) = &message.tool_call_id {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": message.text_content(),
                    }));
                }
                continue;
            }
            if let Some(parts) = &message.content_parts {
                let content: Vec<Value> = parts
                    .iter()
                    .map(|part| match part {
                        crate::types::ContentPart::Text { text } => {
                            json!({"type": "input_text", "text": text})
                        }
                        crate::types::ContentPart::ImageUrl { image_url } => {
                            json!({"type": "input_image", "image_url": image_url.url})
                        }
                    })
                    .collect();
                if !content.is_empty() {
                    // Spec shape: every input item needs "type":"message" —
                    // the Responses API (and the HUD proxy's translator)
                    // drops typeless items, silently losing the prompt.
                    input.push(json!({
                        "type": "message",
                        "role": message.role,
                        "content": content,
                    }));
                }
            } else if !message.text_content().trim().is_empty() {
                input.push(json!({
                    "type": "message",
                    "role": message.role,
                    "content": message.text_content(),
                }));
            }
            if message.role == "assistant" {
                for call in &message.tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.function.name,
                        "arguments": call.function.arguments,
                    }));
                }
            }
        }
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.function.name,
                    "description": tool.function.description,
                    "parameters": tool.function.parameters,
                    "strict": false,
                })
            })
            .collect();
        let mut body = json!({
            "model": copilot::normalize_model_id(&req.model),
            "input": input,
            "stream": req.stream,
            "store": false,
        });
        // SAFETY: `body` is constructed from `json!({ ... })` above.
        let obj = body.as_object_mut().unwrap();
        if let Some(system) = req.system.as_ref().filter(|s| !s.trim().is_empty()) {
            obj.insert("instructions".into(), json!(system));
        }
        if !tools.is_empty() {
            obj.insert("tools".into(), Value::Array(tools));
            obj.insert("tool_choice".into(), json!("auto"));
            obj.insert("parallel_tool_calls".into(), json!(true));
        }
        if let Some(max_tokens) = req.max_tokens {
            obj.insert("max_output_tokens".into(), json!(max_tokens));
        }
        // Reasoning: clamp/map the effort onto the model's valid set before
        // serialization. `xhigh` is a valid joey effort (joey-core
        // VALID_EFFORTS) but historically not a /responses enum member for
        // most models — the server (or HUD proxy translator) rejects or
        // misroutes it. Valid efforts pass through verbatim; anything above
        // the model's max (e.g. xhigh on a max=high gpt-5.x) clamps down.
        if let Some(crate::request::ReasoningEffort::Level(effort)) = &req.reasoning {
            let e = effort.trim().to_lowercase();
            if !e.is_empty() && e != "none" {
                let normalized = copilot::normalize_model_id(&req.model);
                let catalog_entry = copilot::peek_model_catalog().into_iter().find(|item| {
                    item.get("id").and_then(Value::as_str) == Some(normalized.as_str())
                });
                let valid = copilot::model_reasoning_efforts(&req.model, catalog_entry.as_ref());
                let mapped = clamp_effort(&e, &valid);
                obj.insert(
                    "reasoning".into(),
                    json!({"effort": mapped, "summary": "auto"}),
                );
                // Mirror what real OpenAI /responses clients send with
                // store:false: ask for the encrypted reasoning payload so
                // reasoning items survive the (stateless) round trip. The
                // server returns reasoning items regardless (observed on the
                // live copilot wire 2026-08-21); summary:"auto" is what makes
                // it actually emit parsable summary text.
                obj.insert(
                    "include".into(),
                    json!(["reasoning.encrypted_content"]),
                );
            }
        }
        body
    }

    async fn responses(
        &self,
        req: &ProviderRequest,
        tx: Option<&mpsc::UnboundedSender<StreamEvent>>,
    ) -> Result<NormalizedResponse, ProviderError> {
        if !crate::profile::is_copilot_wire(self.profile.name) {
            return Err(ProviderError::Other(
                "codex_responses wire mode is only implemented for Copilot".into(),
            ));
        }
        let (request_base, request_key) = self.request_credentials().await?;
        let token = request_key.as_deref().ok_or_else(|| {
            ProviderError::Auth(
                "No GitHub Copilot token found. Run `joey auth copilot login` or `joey model`."
                    .into(),
            )
        })?;
        let url = format!("{}/responses", request_base.trim_end_matches('/'));
        let body = self.build_responses_body(req);
        tracing::debug!(target: "joey_providers::copilot", body = %body, "Copilot Responses request");
        let builder = self.copilot_headers(
            self.http.post(url).json(&body),
            token,
            request_is_user_initiated(req),
            request_has_images(req),
        );
        let response = self.send_with_auth_refresh(builder).await?;
        if !response.status().is_success() {
            return Err(status_error(response).await);
        }
        if req.stream {
            self.parse_responses_stream(response, tx).await
        } else {
            let value: Value = response
                .json()
                .await
                .map_err(|e| ProviderError::Parse(e.to_string()))?;
            parse_responses_response(&value)
        }
    }

    async fn parse_responses_stream(
        &self,
        response: reqwest::Response,
        tx: Option<&mpsc::UnboundedSender<StreamEvent>>,
    ) -> Result<NormalizedResponse, ProviderError> {
        let mut content = String::new();
        let mut reasoning = String::new();
        // Output-item accumulation for tool calls. Two join strategies are
        // maintained because real Copilot /responses streams (via the
        // ai-usage-hud proxy, observed 2026-08-18) obfuscate `item_id` on
        // `function_call_arguments` events — every delta carries a DIFFERENT
        // opaque item_id that matches neither `output_item.added` nor its
        // siblings — so keying deltas by item_id shreds the arguments into
        // one-entry-per-fragment garbage. `output_index` IS stable across
        // all events for one call, so it is the primary join key.
        //
        // `response.output_item.done` carries the COMPLETE item (clean
        // call_id, name, full arguments) and is treated as authoritative
        // when seen; deltas only fill in when `done` events are absent.
        // slot = output_index -> (wire call_id, function name, accumulated args, authoritative?)
        let mut calls: Vec<(Option<u64>, String, String, String, bool)> = Vec::new();
        let mut completed: Option<Value> = None;
        let mut buffer = String::new();
        let mut stream = response.bytes_stream();
        let read_timeout = Duration::from_secs(stream_read_timeout_secs());
        while let Some(chunk) = tokio::time::timeout(read_timeout, stream.next())
            .await
            .map_err(|_| ProviderError::Timeout("Responses stream stalled".into()))?
        {
            let chunk = chunk.map_err(|e| ProviderError::Connection(e.to_string()))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(newline) = buffer.find('\n') {
                let line = buffer[..newline].trim().to_string();
                buffer.drain(..=newline);
                let Some(raw) = line.strip_prefix("data:") else {
                    continue;
                };
                if raw.trim() == "[DONE]" {
                    continue;
                }
                let Ok(event) = serde_json::from_str::<Value>(raw.trim()) else {
                    continue;
                };
                let output_index = event.get("output_index").and_then(Value::as_u64);
                match event.get("type").and_then(Value::as_str).unwrap_or("") {
                    "response.output_text.delta" => {
                        if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                            content.push_str(delta);
                            if let Some(tx) = tx {
                                let _ = tx.send(StreamEvent::ContentDelta(delta.into()));
                            }
                        }
                    }
                    "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                        if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                            reasoning.push_str(delta);
                            if let Some(tx) = tx {
                                let _ = tx.send(StreamEvent::ReasoningDelta(delta.into()));
                            }
                        }
                    }
                    "response.output_item.added" => {
                        let item = event.get("item").unwrap_or(&Value::Null);
                        if item.get("type").and_then(Value::as_str) == Some("function_call") {
                            let item_id = item
                                .get("id")
                                .or_else(|| item.get("call_id"))
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let call_id = item
                                .get("call_id")
                                .and_then(Value::as_str)
                                .unwrap_or(&item_id)
                                .to_string();
                            let name = item
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            if !call_id.is_empty() || !name.is_empty() {
                                match output_index {
                                    Some(idx) => {
                                        let slot = ensure_call_slot(&mut calls, idx);
                                        if !call_id.is_empty() {
                                            slot.1 = call_id;
                                        }
                                        if !name.is_empty() {
                                            slot.2 = name;
                                        }
                                    }
                                    None => {
                                        calls.push((None, call_id, name, String::new(), false))
                                    }
                                }
                            }
                        }
                    }
                    "response.function_call_arguments.delta" => {
                        let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
                        // Join by output_index when present (the stable key on
                        // obfuscated streams); fall back to item_id only when
                        // the event carries no output_index at all.
                        if let Some(idx) = output_index {
                            if let Some(slot) = calls.iter_mut().find(|(i, ..)| *i == Some(idx)) {
                                slot.3.push_str(delta);
                            } else {
                                calls.push((Some(idx), String::new(), String::new(), delta.to_string(), false));
                            }
                        }
                    }
                    "response.function_call_arguments.done" => {
                        // Full accumulated arguments for the call — authoritative.
                        let args = event
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if let Some(idx) = output_index {
                            let slot = ensure_call_slot(&mut calls, idx);
                            if slot.3.len() < args.len() {
                                slot.3 = args.to_string();
                            }
                            slot.4 = true;
                        }
                    }
                    "response.output_item.done" => {
                        // Complete output item — authoritative call identity.
                        let item = event.get("item").unwrap_or(&Value::Null);
                        if item.get("type").and_then(Value::as_str) == Some("function_call") {
                            let call_id = item
                                .get("call_id")
                                .or_else(|| item.get("id"))
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                            let args = item
                                .get("arguments")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            if let Some(idx) = output_index {
                                let slot = ensure_call_slot(&mut calls, idx);
                                if !call_id.is_empty() {
                                    slot.1 = call_id;
                                }
                                if !name.is_empty() {
                                    slot.2 = name.to_string();
                                }
                                if slot.3.len() < args.len() {
                                    slot.3 = args.to_string();
                                }
                                slot.4 = true;
                            }
                        }
                    }
                    "response.completed" => completed = event.get("response").cloned(),
                    "error" | "response.failed" => {
                        return Err(ProviderError::ServerError(event.to_string()));
                    }
                    _ => {}
                }
            }
        }
        if let Some(value) = completed {
            let parsed = parse_responses_response(&value)?;
            if !parsed.content.is_empty() || !parsed.tool_calls.is_empty() {
                return Ok(parsed);
            }
        }
        // Slots: drop empty-name/empty-arg fragments (deltas that never joined
        // a real item), order by slot index.
        calls.sort_by_key(|(idx, ..)| idx.unwrap_or(u64::MAX));
        let tool_calls = calls
            .into_iter()
            .filter(|(_, call_id, name, args, authoritative)| {
                *authoritative
                    || (!name.is_empty() && !args.trim().is_empty())
                    || (!call_id.is_empty() && !args.trim().is_empty())
            })
            .map(|(idx, call_id, name, arguments, _authoritative)| ToolCall {
                // Obfuscated streams may never reveal a clean call_id; fall
                // back to a deterministic synthetic id so tool results can
                // still reference the call.
                id: if call_id.is_empty() {
                    format!("call_resp_{}", idx.unwrap_or(0))
                } else {
                    call_id
                },
                call_type: "function".into(),
                function: FunctionCall { name, arguments },
            })
            .collect::<Vec<_>>();
        if content.is_empty() && reasoning.is_empty() && tool_calls.is_empty() {
            return Err(ProviderError::EmptyStream(
                "Copilot Responses stream returned no output".into(),
            ));
        }
        Ok(NormalizedResponse {
            content,
            finish_reason: if tool_calls.is_empty() {
                FinishReason::Stop
            } else {
                FinishReason::ToolCalls
            },
            tool_calls,
            reasoning: (!reasoning.is_empty()).then_some(reasoning),
            usage: Usage::default(),
            model: None,
            reasoning_details: None,
            anthropic_content_blocks: None,
        })
    }

    // ── Anthropic Messages ───────────────────────────────────────────────────

    async fn anthropic_messages(
        &self,
        req: &ProviderRequest,
        tx: Option<&mpsc::UnboundedSender<StreamEvent>>,
    ) -> Result<NormalizedResponse, ProviderError> {
        // Strip a trailing /v1 before appending /v1/messages (L5,
        // anthropic_adapter.py:780-783).
        let (request_base, request_key) = self.request_credentials().await?;
        let base = strip_trailing_v1(&request_base);
        let url = format!("{}/v1/messages", base);
        let mut body = anthropic::build_anthropic_body(req, &request_base);
        if crate::profile::is_copilot_wire(self.profile.name) {
            body["model"] = json!(copilot::normalize_model_id(&req.model));
        }
        if req.stream {
            // SAFETY: `body` was just built as a json!({...}) object.
            body.as_object_mut()
                .unwrap()
                .insert("stream".into(), json!(true));
        }

        let mut builder = self
            .http
            .post(&url)
            .header("anthropic-version", "2023-06-01")
            .json(&body);
        if crate::profile::is_copilot_wire(self.profile.name) {
            let token = request_key.as_deref().ok_or_else(|| {
                ProviderError::Auth(
                    "No GitHub Copilot token found. Run `joey auth copilot login` or `joey model`."
                        .into(),
                )
            })?;
            builder = self.copilot_headers(
                builder,
                token,
                request_is_user_initiated(req),
                request_has_images(req),
            );
        } else if let Some(key) = &self.api_key {
            // OAuth-shaped tokens use Bearer; Console keys use x-api-key
            // (anthropic_adapter.py:395-420). See module note: only the honest
            // token-detection layer is replicated, not the identity spoofing.
            if anthropic::is_oauth_token(key) {
                builder = builder.bearer_auth(key);
            } else {
                builder = builder.header("x-api-key", key);
            }
        }
        // Beta headers on native requests (anthropic_adapter.py:326-333, M11).
        if let Some(betas) = anthropic::anthropic_beta_header(&self.base_url) {
            builder = builder.header("anthropic-beta", betas);
        }

        let resp = self.send_with_auth_refresh(builder).await?;
        if !resp.status().is_success() {
            return Err(status_error(resp).await);
        }

        if req.stream {
            self.parse_anthropic_stream(resp, tx).await
        } else {
            let v: Value = resp
                .json()
                .await
                .map_err(|e| ProviderError::Parse(e.to_string()))?;
            anthropic::parse_anthropic_response(&v)
        }
    }

    async fn parse_anthropic_stream(
        &self,
        resp: reqwest::Response,
        tx: Option<&mpsc::UnboundedSender<StreamEvent>>,
    ) -> Result<NormalizedResponse, ProviderError> {
        let mut usage = Usage::default();
        let mut finish = FinishReason::Stop;
        let mut saw_event = false;
        // Content blocks, assembled by index.
        let mut blocks: Vec<AnthropicBlockAccum> = Vec::new();
        let mut model: Option<String> = None;

        let mut buf = String::new();
        let mut stream = resp.bytes_stream();
        let read_timeout = Duration::from_secs(stream_read_timeout_secs());
        let mut stream_done = false;
        loop {
            if stream_done {
                // Flush any final line that lacked a trailing newline
                // (message_delta/usage) through the normal parser, then exit.
                if !buf.trim().is_empty() && !buf.ends_with('\n') {
                    buf.push('\n');
                }
            } else {
                let next = tokio::time::timeout(read_timeout, stream.next()).await;
                match next {
                    Err(_) => {
                        return Err(ProviderError::Timeout(format!(
                            "stream stalled: no chunk within {}s",
                            read_timeout.as_secs()
                        )))
                    }
                    Ok(None) => {
                        stream_done = true;
                        continue;
                    }
                    Ok(Some(c)) => {
                        let chunk =
                            c.map_err(|e| ProviderError::Connection(e.to_string()))?;
                        buf.push_str(&String::from_utf8_lossy(&chunk));
                    }
                }
            }
            while let Some(nl) = buf.find('\n') {
                let line = buf[..nl].trim().to_string();
                buf.drain(..=nl);
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<Value>(data.trim()) else {
                    continue;
                };
                saw_event = true;
                match v.get("type").and_then(|t| t.as_str()) {
                    // Error events → classified error, not silent success (M16).
                    Some("error") => {
                        let err = v.get("error");
                        let etype = err
                            .and_then(|e| e.get("type"))
                            .and_then(|t| t.as_str())
                            .unwrap_or("");
                        let emsg = err
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                            .unwrap_or("anthropic stream error");
                        return Err(anthropic_stream_error(etype, emsg));
                    }
                    Some("content_block_start") => {
                        let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                        let block = v.get("content_block");
                        let btype = block
                            .and_then(|b| b.get("type"))
                            .and_then(|t| t.as_str())
                            .unwrap_or("text");
                        if !ensure_block(&mut blocks, idx) {
                            return Err(ProviderError::Parse(format!(
                                "anthropic stream index {} exceeds cap; dropping stream",
                                idx
                            )));
                        }
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
                        if btype == "redacted_thinking" {
                            blocks[idx].data = block
                                .and_then(|b| b.get("data"))
                                .and_then(|s| s.as_str())
                                .unwrap_or("")
                                .to_string();
                        }
                    }
                    Some("content_block_delta") => {
                        let idx = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                        if !ensure_block(&mut blocks, idx) {
                            continue;
                        }
                        let delta = v.get("delta");
                        if let Some(t) = delta.and_then(|d| d.get("text")).and_then(|t| t.as_str())
                        {
                            blocks[idx].text.push_str(t);
                            if let Some(tx) = tx {
                                let _ = tx.send(StreamEvent::ContentDelta(t.to_string()));
                            }
                        }
                        if let Some(t) = delta
                            .and_then(|d| d.get("thinking"))
                            .and_then(|t| t.as_str())
                        {
                            blocks[idx].thinking.push_str(t);
                            if let Some(tx) = tx {
                                let _ = tx.send(StreamEvent::ReasoningDelta(t.to_string()));
                            }
                        }
                        // Signed thinking: signature_delta carries `signature`.
                        if let Some(sig) = delta
                            .and_then(|d| d.get("signature"))
                            .and_then(|s| s.as_str())
                        {
                            blocks[idx].signature.push_str(sig);
                        }
                        if let Some(pj) = delta
                            .and_then(|d| d.get("partial_json"))
                            .and_then(|t| t.as_str())
                        {
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
                            anthropic::merge_anthropic_usage(&mut usage, u);
                        }
                    }
                    Some("message_start") => {
                        if let Some(msg) = v.get("message") {
                            if model.is_none() {
                                model = msg
                                    .get("model")
                                    .and_then(|m| m.as_str())
                                    .map(str::to_string);
                            }
                            if let Some(u) = msg.get("usage") {
                                anthropic::merge_anthropic_usage(&mut usage, u);
                            }
                        }
                    }
                    _ => {}
                }
            }
            // Stream exhausted and buffer drained — done (the flush above
            // already ran the final unterminated line through the parser).
            if stream_done && buf.trim().is_empty() {
                break;
            }
        }

        // Zero-event guard, parity with the chat path (M7).
        if !saw_event && blocks.is_empty() {
            return Err(ProviderError::EmptyStream(
                "anthropic stream delivered no events".into(),
            ));
        }
        // Rebuild the ordered block list + parallel channels from accumulators.
        let mut text_parts: Vec<String> = Vec::new();
        let mut reasoning_parts: Vec<String> = Vec::new();
        let mut reasoning_details: Vec<Value> = Vec::new();
        let mut ordered_blocks: Vec<Value> = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        for b in &blocks {
            let raw = b.to_block_value();
            if let Some(clean) = anthropic::sanitize_replay_block(&raw) {
                ordered_blocks.push(clean.clone());
                match b.block_type.as_str() {
                    "text" => text_parts.push(b.text.clone()),
                    "thinking" => {
                        reasoning_parts.push(b.thinking.clone());
                        reasoning_details.push(clean);
                    }
                    "redacted_thinking" => reasoning_details.push(clean),
                    "tool_use" => tool_calls.push(ToolCall {
                        id: b.tool_id.clone(),
                        call_type: "function".to_string(),
                        function: FunctionCall {
                            name: b.tool_name.clone(),
                            arguments: if b.json_buf.is_empty() {
                                "{}".to_string()
                            } else {
                                b.json_buf.clone()
                            },
                        },
                    }),
                    _ => {}
                }
            } else if b.block_type == "text" {
                text_parts.push(b.text.clone());
            }
        }

        Ok(anthropic::finalize_anthropic_response(
            text_parts,
            reasoning_parts,
            reasoning_details,
            ordered_blocks,
            tool_calls,
            finish,
            usage,
            model,
        ))
    }
}

fn timeout_secs() -> u64 {
    std::env::var("JOEY_API_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
}

fn stream_read_timeout_secs() -> u64 {
    std::env::var("JOEY_STREAM_READ_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_STREAM_READ_TIMEOUT_SECS)
}

/// Strip a trailing `/v1` (or `/v1/`) — the Anthropic path is `/v1/messages`
/// and doubling it produces `/v1/v1/messages` (anthropic_adapter.py:780-783).
fn strip_trailing_v1(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    trimmed.strip_suffix("/v1").unwrap_or(trimmed).to_string()
}

/// True for the native Gemini REST endpoint (generativelanguage.googleapis.com
/// WITHOUT the `/openai` shim path). The port's gemini profile IS the /openai
/// shim, so this is false for it (chat_completion_helpers.py:2659-2666).
fn is_native_gemini_base_url(base_url: &str) -> bool {
    let n = base_url.trim().trim_end_matches('/').to_lowercase();
    n.contains("generativelanguage.googleapis.com") && !n.ends_with("/openai")
}

/// Map an Anthropic SSE error event to a classified error (M16). `overloaded_error`
/// and `api_error` are retryable; others fall through to a generic status.
fn anthropic_stream_error(etype: &str, message: &str) -> ProviderError {
    match etype {
        "overloaded_error" => ProviderError::Overloaded(message.to_string()),
        "rate_limit_error" => ProviderError::RateLimit {
            message: message.to_string(),
            retry_after: None,
        },
        "api_error" | "timeout_error" => ProviderError::ServerError(message.to_string()),
        "authentication_error" | "permission_error" => ProviderError::Auth(message.to_string()),
        "invalid_request_error" => ProviderError::FormatError(message.to_string()),
        _ => ProviderError::Status {
            status: 0,
            message: format!("{etype}: {message}"),
        },
    }
}

async fn status_error(resp: reqwest::Response) -> ProviderError {
    let status = resp.status().as_u16();
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_retry_after);
    let body = resp.text().await.unwrap_or_default();
    ProviderError::from_status(status, &body, retry_after)
}

// ── OpenAI response parsing ──────────────────────────────────────────────────

fn request_is_user_initiated(req: &ProviderRequest) -> bool {
    req.messages
        .last()
        .map(|message| message.role == "user")
        .unwrap_or(true)
}

fn request_has_images(req: &ProviderRequest) -> bool {
    req.messages.iter().any(|message| {
        message
            .content_parts
            .as_ref()
            .map(|parts| {
                parts
                    .iter()
                    .any(|part| matches!(part, crate::types::ContentPart::ImageUrl { .. }))
            })
            .unwrap_or(false)
    })
}

/// Get-or-create the accumulator slot for output item index `idx` in a
/// Responses stream. Slots are `(output_index, call_id, name, args,
/// authoritative)` tuples; see [`ProviderClient::parse_responses_stream`]
/// for why `output_index` (not `item_id`) is the join key.
fn ensure_call_slot<'a>(
    calls: &'a mut Vec<(Option<u64>, String, String, String, bool)>,
    idx: u64,
) -> &'a mut (Option<u64>, String, String, String, bool) {
    // Slots are kept sorted by index in practice (events arrive in order);
    // scan from the back for the common append case, else linear search.
    if let Some(pos) = calls.iter().rposition(|(i, ..)| *i == Some(idx)) {
        return &mut calls[pos];
    }
    calls.push((Some(idx), String::new(), String::new(), String::new(), false));
    calls.last_mut().expect("just pushed")
}

fn parse_responses_response(v: &Value) -> Result<NormalizedResponse, ProviderError> {
    if let Some(error) = v.get("error").filter(|e| !e.is_null()) {
        return Err(ProviderError::ServerError(error.to_string()));
    }
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
    for item in v
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match item.get("type").and_then(Value::as_str).unwrap_or("") {
            "message" => {
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    match part.get("type").and_then(Value::as_str).unwrap_or("") {
                        "output_text" | "text" => {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                content.push_str(text);
                            }
                        }
                        "refusal" => {
                            if let Some(text) = part
                                .get("refusal")
                                .or_else(|| part.get("text"))
                                .and_then(Value::as_str)
                            {
                                content.push_str(text);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "function_call" => {
                let id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                tool_calls.push(ToolCall::new(id, name, arguments));
            }
            "reasoning" => {
                for summary in item
                    .get("summary")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(text) = summary.get("text").and_then(Value::as_str) {
                        if !reasoning.is_empty() {
                            reasoning.push('\n');
                        }
                        reasoning.push_str(text);
                    }
                }
            }
            _ => {}
        }
    }
    let usage_value = v.get("usage").unwrap_or(&Value::Null);
    let input_tokens = usage_value
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage_value
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let usage = Usage {
        prompt_tokens: input_tokens,
        completion_tokens: output_tokens,
        total_tokens: usage_value
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(input_tokens + output_tokens),
        reasoning_tokens: usage_value
            .get("output_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        ..Usage::default()
    };
    if content.is_empty() && tool_calls.is_empty() && reasoning.is_empty() {
        return Err(ProviderError::Parse(
            "Copilot Responses payload contained no output".into(),
        ));
    }
    Ok(NormalizedResponse {
        content,
        finish_reason: if tool_calls.is_empty() {
            FinishReason::Stop
        } else {
            FinishReason::ToolCalls
        },
        tool_calls,
        reasoning: (!reasoning.is_empty()).then_some(reasoning),
        usage,
        model: v.get("model").and_then(Value::as_str).map(str::to_string),
        reasoning_details: None,
        anthropic_content_blocks: None,
    })
}

fn parse_openai_response(v: &Value) -> Result<NormalizedResponse, ProviderError> {
    let choice = v
        .get("choices")
        .and_then(|c| c.get(0))
        .ok_or_else(|| ProviderError::Parse("no choices in response".into()))?;
    let msg = choice.get("message").unwrap_or(&Value::Null);
    let mut content = msg
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string();
    // First-non-null of reasoning / reasoning_content
    // (chat_completions.py:714). Joey extension: copilot-wire claude
    // models report thinking as `reasoning_text` (verified live
    // 2026-08-21, see the thinking-param comment in chat.rs) — third
    // fallback beyond the upstream pair.
    let reasoning = msg
        .get("reasoning")
        .or_else(|| msg.get("reasoning_content"))
        .or_else(|| msg.get("reasoning_text"))
        .and_then(|r| r.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let mut tool_calls = Vec::new();
    if let Some(tcs) = msg.get("tool_calls").and_then(|t| t.as_array()) {
        for tc in tcs {
            let id = tc
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or("")
                .to_string();
            let f = tc.get("function").unwrap_or(&Value::Null);
            let name = f
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let args = f
                .get("arguments")
                .and_then(|a| a.as_str())
                .unwrap_or("{}")
                .to_string();
            tool_calls.push(ToolCall::new(id, name, args));
        }
    }

    // Integer finish_reason tolerance (chat_completions.py:667-671).
    let mut finish = match choice.get("finish_reason") {
        Some(Value::String(s)) => FinishReason::from_wire(s),
        Some(Value::Number(n)) => FinishReason::from_wire(&n.to_string()),
        _ => FinishReason::Stop,
    };

    // Structured refusal → content + content_filter finish, but only when it
    // is the sole payload (chat_completions.py:739-760, M9).
    let refusal = msg
        .get("refusal")
        .and_then(|r| r.as_str())
        .filter(|s| !s.trim().is_empty());
    if let Some(refusal) = refusal {
        if content.trim().is_empty() && tool_calls.is_empty() {
            content = refusal.to_string();
            if matches!(finish, FinishReason::Stop) {
                finish = FinishReason::ContentFilter;
            }
        }
    }

    let usage = v.get("usage").map(parse_usage).unwrap_or_default();
    let model = v.get("model").and_then(|m| m.as_str()).map(str::to_string);
    // Keep reasoning_details (OpenRouter unified format) for downstream replay (M9).
    let reasoning_details = msg
        .get("reasoning_details")
        .filter(|v| !v.is_null())
        .cloned();

    Ok(NormalizedResponse {
        content,
        tool_calls,
        finish_reason: finish,
        reasoning,
        usage,
        model,
        reasoning_details,
        anthropic_content_blocks: None,
    })
}

/// Parse OpenAI-shaped usage incl. cache stats (M9). Cache write comes from
/// `prompt_tokens_details.cache_write_tokens`; DeepSeek's native shape uses the
/// top-level `prompt_cache_hit_tokens` fallback (chat_completions.py:781-796).
fn parse_usage(u: &Value) -> Usage {
    let get = |k: &str| u.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    let details = u.get("prompt_tokens_details");
    let mut cache_read = details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if cache_read == 0 {
        cache_read = u
            .get("prompt_cache_hit_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
    }
    let cache_write = details
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    Usage {
        prompt_tokens: get("prompt_tokens"),
        completion_tokens: get("completion_tokens"),
        total_tokens: get("total_tokens"),
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
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

/// Accumulate OpenAI tool-call deltas by slot. Handles the Ollama fix
/// (chat_completion_helpers.py:2745-2916): a new tool call reusing the same raw
/// index with a *different* id gets a fresh slot; names are assigned (not
/// concatenated) to survive providers that resend the full name each chunk.
fn accumulate_tool_calls(
    accum: &mut Vec<ToolAccum>,
    tcs: &[Value],
    last_id_at_idx: &mut std::collections::HashMap<u64, String>,
    active_slot_by_idx: &mut std::collections::HashMap<u64, usize>,
) {
    for tc in tcs {
        let raw_idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
        if raw_idx > MAX_STREAM_INDEX {
            // Hostile/buggy provider — a huge index would otherwise allocate
            // ~10^18 accumulator slots. Drop the delta defensively.
            continue;
        }
        let delta_id = tc
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string();

        active_slot_by_idx
            .entry(raw_idx)
            .or_insert(raw_idx as usize);
        if !delta_id.is_empty() {
            if let Some(prev) = last_id_at_idx.get(&raw_idx) {
                if *prev != delta_id {
                    let new_slot = accum.len();
                    active_slot_by_idx.insert(raw_idx, new_slot);
                }
            }
            last_id_at_idx.insert(raw_idx, delta_id.clone());
        }
        // SAFETY: `active_slot_by_idx` was populated for `raw_idx` during
        // the first pass through the tool-call accumulation loop.
        let slot = *active_slot_by_idx.get(&raw_idx).unwrap();
        while accum.len() <= slot {
            accum.push(ToolAccum::default());
        }
        if !delta_id.is_empty() {
            accum[slot].id = delta_id;
        }
        if let Some(f) = tc.get("function") {
            if let Some(name) = f.get("name").and_then(|n| n.as_str()) {
                if !name.is_empty() {
                    accum[slot].name = name.to_string();
                }
            }
            if let Some(args) = f.get("arguments").and_then(|a| a.as_str()) {
                accum[slot].args.push_str(args);
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
            let id = if a.id.is_empty() {
                format!("call_{}", i)
            } else {
                a.id
            };
            let args = if a.args.is_empty() {
                "{}".to_string()
            } else {
                a.args
            };
            ToolCall::new(id, a.name, args)
        })
        .collect()
}

// ── Anthropic streaming block accumulator ────────────────────────────────────

#[derive(Default)]
struct AnthropicBlockAccum {
    block_type: String,
    text: String,
    thinking: String,
    signature: String,
    data: String,
    tool_id: String,
    tool_name: String,
    json_buf: String,
}

impl AnthropicBlockAccum {
    /// Reconstruct the raw block Value for sanitize_replay_block.
    fn to_block_value(&self) -> Value {
        match self.block_type.as_str() {
            "text" => json!({"type": "text", "text": self.text}),
            "thinking" => {
                let mut b = json!({"type": "thinking", "thinking": self.thinking});
                if !self.signature.is_empty() {
                    b["signature"] = json!(self.signature);
                }
                b
            }
            "redacted_thinking" => json!({"type": "redacted_thinking", "data": self.data}),
            "tool_use" => {
                let input: Value = if self.json_buf.is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(&self.json_buf).unwrap_or(json!({}))
                };
                json!({"type": "tool_use", "id": self.tool_id, "name": self.tool_name, "input": input})
            }
            other => json!({"type": other}),
        }
    }
}

/// Cap on block/tool-call slot indices accepted from SSE deltas.
///
/// Legitimate streams carry at most a handful of blocks per response; a
/// hostile or buggy provider (or MITM proxy) sending `{"index": 1e18}`
/// would otherwise allocate ~10^18 accumulator slots and OOM the process.
/// Events referencing a slot beyond this cap are dropped defensively.
const MAX_STREAM_INDEX: u64 = 1024;

fn ensure_block(blocks: &mut Vec<AnthropicBlockAccum>, idx: usize) -> bool {
    if idx > MAX_STREAM_INDEX as usize {
        return false;
    }
    while blocks.len() <= idx {
        blocks.push(AnthropicBlockAccum::default());
    }
    true
}

/// Clamp/map a reasoning effort onto a model's valid effort set for the
/// /responses wire (2026-08-21, copilot-wire reasoning fix).
///
/// - Effort already in `valid` → verbatim.
/// - Effort above the model's max → the highest valid entry not exceeding it
///   (e.g. xhigh on a `minimal..high` gpt-5.x → high).
/// - Effort below the model's min → the model's minimum.
/// - Empty `valid` (unknown model, cold catalog) → generic /responses clamp:
///   minimal/low/medium/high verbatim, xhigh/max/ultra → high, unknown →
///   medium (matching the anthropic/gemini adapters' unknown-effort fallback).
///
/// Ranking follows joey-core `VALID_EFFORTS` (ascending capability); valid
/// entries outside that order (e.g. a catalog's "none") are ignored.
fn clamp_effort(effort: &str, valid: &[String]) -> String {
    let rank = |e: &str| joey_core::reasoning::VALID_EFFORTS.iter().position(|r| *r == e);
    if valid.iter().any(|v| v == effort) {
        return effort.to_string();
    }
    if valid.is_empty() {
        return match effort {
            "minimal" | "low" | "medium" | "high" => effort.to_string(),
            "xhigh" | "max" | "ultra" => "high".to_string(),
            _ => "medium".to_string(),
        };
    }
    let Some(effort_rank) = rank(effort) else {
        return "medium".to_string();
    };
    let ranked: Vec<(usize, &str)> = valid
        .iter()
        .filter_map(|v| rank(v).map(|r| (r, v.as_str())))
        .collect();
    // Highest valid entry at or below the effort.
    if let Some((_, v)) = ranked.iter().filter(|(r, _)| *r <= effort_rank).max() {
        return (*v).to_string();
    }
    // Effort below every valid entry → clamp up to the model's minimum.
    ranked
        .iter()
        .min_by_key(|(r, _)| *r)
        .map(|(_, v)| (*v).to_string())
        .unwrap_or_else(|| "medium".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_usage_hud_client_pins_proxy_and_skips_exchange() {
        // Building a client from the ai-usage-hud profile must attach a
        // CopilotAuth pinned to the proxy endpoint: credentials() then
        // returns the raw GitHub token + the proxy base URL WITHOUT
        // contacting GitHub's exchange endpoint.
        let profile = crate::profile::get_profile("ai-usage-hud").unwrap();
        let client = ProviderClient::new(profile, None, Some("ghu_test".into())).unwrap();
        assert!(client.copilot_auth.is_some());
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (base, token) = rt
            .block_on(async { client.request_credentials().await })
            .unwrap();
        assert_eq!(base, "http://127.0.0.1:8317");
        assert_eq!(token.as_deref(), Some("ghu_test"));
    }

    #[test]
    fn ai_usage_hud_client_honors_env_base_override() {
        let _guard = crate::copilot::TEST_ENV_LOCK.lock().unwrap();
        // Seed the copilot catalog cache so build_client's catalog consult
        // (copilot-wire api-mode routing) is served in-process and never
        // reaches the network. The fixture doesn't affect what this test
        // asserts: with a pinned custom endpoint, request_credentials returns
        // the raw token + pinned base without consulting api_mode.
        let _catalog = crate::copilot::CatalogCacheGuard::seed("gpt-4o");
        std::env::set_var("AI_USAGE_HUD_BASE_URL", "http://10.0.0.5:9000/");
        let profile = crate::profile::get_profile("ai-usage-hud").unwrap();
        // build_client applies the env override through resolve_base_override.
        let client = crate::build_client("ai-usage-hud", "", "gpt-4o", Some("ghu_test".into()))
            .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (base, _) = rt
            .block_on(async { client.request_credentials().await })
            .unwrap();
        assert_eq!(base, "http://10.0.0.5:9000");
        std::env::remove_var("AI_USAGE_HUD_BASE_URL");
    }

    #[test]
    fn copilot_initiator_tracks_user_vs_tool_loop() {
        let user_request = ProviderRequest::new(
            "gpt-4.1",
            vec![crate::types::Message::user("hello")],
        );
        assert!(request_is_user_initiated(&user_request));

        let tool_request = ProviderRequest::new(
            "gpt-4.1",
            vec![
                crate::types::Message::user("read a file"),
                crate::types::Message::tool_result("call_1", "read_file", "contents"),
            ],
        );
        assert!(!request_is_user_initiated(&tool_request));
    }

    #[test]
    fn copilot_responses_body_preserves_multimodal_content() {
        let profile = crate::profile::get_profile("copilot").unwrap();
        let client = ProviderClient::new(profile, None, Some("ghu_test".into())).unwrap();
        let mut message = crate::types::Message::user("");
        message.content = None;
        message.content_parts = Some(vec![
            crate::types::ContentPart::Text { text: "inspect".into() },
            crate::types::ContentPart::ImageUrl {
                image_url: crate::types::ImageUrl { url: "data:image/png;base64,AA==".into() },
            },
        ]);
        let request = ProviderRequest::new("gpt-5.4", vec![message]);
        let body = client.build_responses_body(&request);
        assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
        assert_eq!(body["input"][0]["content"][1]["image_url"], "data:image/png;base64,AA==");
        assert!(request_has_images(&request));
    }

    #[test]
    fn copilot_responses_parser_normalizes_text_tools_reasoning_and_usage() {
        let response = parse_responses_response(&json!({
            "id": "resp_1",
            "output": [
                {"type": "reasoning", "summary": [{"type": "summary_text", "text": "checked"}]},
                {"type": "message", "content": [{"type": "output_text", "text": "done"}]},
                {"type": "function_call", "call_id": "call_1", "name": "read_file", "arguments": "{\"path\":\"README.md\"}"}
            ],
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "total_tokens": 15,
                "output_tokens_details": {"reasoning_tokens": 2}
            }
        }))
        .unwrap();
        assert_eq!(response.content, "done");
        assert_eq!(response.reasoning.as_deref(), Some("checked"));
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].function.name, "read_file");
        assert_eq!(response.usage.prompt_tokens, 10);
        assert_eq!(response.usage.reasoning_tokens, 2);
    }

    /// Regression (2026-08-18): the live ai-usage-hud proxy obfuscates
    /// `item_id` on function_call_arguments events — every delta carries a
    /// DIFFERENT opaque id matching neither output_item.added nor its
    /// siblings. Keying by item_id shredded tool args into one garbage entry
    /// per fragment. The stream must accumulate by `output_index` and prefer
    /// the authoritative `output_item.done` / `function_call_arguments.done`
    /// / `response.completed` payloads.
    #[test]
    fn copilot_responses_stream_survives_obfuscated_item_ids() {
        let sse = [
            r#"{"type":"response.created","response":{"id":"resp_1"}}"#,
            r#"{"type":"response.in_progress"}"#,
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"OBFC_ADD_111","call_id":"call_REAL_1","name":"calculate","arguments":""}}"#,
            r#"{"type":"response.function_call_arguments.delta","output_index":0,"item_id":"OBFC_A","obfuscation":1,"delta":"{\"e""#,
            r#"{"type":"response.function_call_arguments.delta","output_index":0,"item_id":"OBFC_B","obfuscation":1,"delta":"xpr\":"}"#,
            r#"{"type":"response.function_call_arguments.delta","output_index":0,"item_id":"OBFC_C","obfuscation":1,"delta":"\"6*7\"}"}"#,
            r#"{"type":"response.function_call_arguments.done","output_index":0,"item_id":"OBFC_DONE","arguments":"{\"expr\":\"6*7\"}"}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","id":"OBFC_ITEM_DONE","call_id":"call_REAL_1","name":"calculate","arguments":"{\"expr\":\"6*7\"}","status":"completed"}}"#,
            r#"{"type":"response.completed","response":{"id":"resp_1","status":"completed","output":[{"type":"message","content":[]},{"type":"function_call","call_id":"call_REAL_1","name":"calculate","arguments":"{\"expr\":\"6*7\"}"}],"usage":{"input_tokens":10,"output_tokens":5}}}"#,
            "[DONE]",
        ];
        let body = {
            let mut b = String::from("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n");
            for line in &sse {
                b.push_str("data: ");
                b.push_str(line);
                b.push_str("\n\n");
            }
            b
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            use std::io::{Read, Write};
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf); // drain the request
            sock.write_all(body.as_bytes()).unwrap();
            sock.flush().unwrap();
        });
        let profile = crate::profile::get_profile("copilot").unwrap();
        let base = format!("http://{}", addr);
        let client = ProviderClient::new(profile, Some(base), Some("ghu_test".into())).unwrap();
        let req = ProviderRequest::new(
            "gpt-5.4",
            vec![crate::types::Message::user("compute 6*7")],
        )
        .streaming(true);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let resp = rt.block_on(client.stream(&req, tx)).expect("stream ok");
        // Exactly ONE tool call, cleanly assembled.
        assert_eq!(resp.tool_calls.len(), 1, "deltas must not shred into fragments");
        assert_eq!(resp.tool_calls[0].id, "call_REAL_1");
        assert_eq!(resp.tool_calls[0].function.name, "calculate");
        assert_eq!(resp.tool_calls[0].function.arguments, "{\"expr\":\"6*7\"}");
        assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
        while let Ok(_) = rx.try_recv() {}
    }

    /// Same obfuscated stream but WITHOUT `response.completed` — the delta
    /// accumulator alone must still produce one clean call.
    #[test]
    fn copilot_responses_stream_obfuscated_without_completed() {
        let sse = [
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"OBFC_ADD","call_id":"call_X","name":"calculate","arguments":""}}"#,
            r#"{"type":"response.function_call_arguments.delta","output_index":0,"item_id":"OBFC_1","delta":"{\"expr"}"#,
            r#"{"type":"response.function_call_arguments.delta","output_index":0,"item_id":"OBFC_2","delta":"\":\"9+1\"}"}"#,
            r#"{"type":"response.function_call_arguments.done","output_index":0,"item_id":"OBFC_D","arguments":"{\"expr\":\"9+1\"}"}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"call_X","name":"calculate","arguments":"{\"expr\":\"9+1\"}","status":"completed"}}"#,
            "data: [DONE]",
        ];
        let body = {
            let mut b = String::from("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n");
            for line in &sse {
                b.push_str("data: ");
                b.push_str(line);
                b.push_str("\n\n");
            }
            b
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            use std::io::{Read, Write};
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf);
            sock.write_all(body.as_bytes()).unwrap();
            sock.flush().unwrap();
        });
        let profile = crate::profile::get_profile("copilot").unwrap();
        let base = format!("http://{}", addr);
        let client = ProviderClient::new(profile, Some(base), Some("ghu_test".into())).unwrap();
        let req = ProviderRequest::new(
            "gpt-5.4",
            vec![crate::types::Message::user("compute 9+1")],
        )
        .streaming(true);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let resp = rt.block_on(client.stream(&req, tx)).expect("stream ok");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].id, "call_X");
        assert_eq!(resp.tool_calls[0].function.arguments, "{\"expr\":\"9+1\"}");
    }

    /// Copilot-wire claude models ride /chat/completions and report
    /// thinking via a `reasoning_text` delta field (Joey extension,
    /// verified live 2026-08-21 — see the thinking-param comment in
    /// chat.rs). The stream parser must emit ReasoningDelta for those and
    /// stay silent for deltas without reasoning fields.
    #[test]
    fn copilot_chat_stream_emits_reasoning_text_deltas() {
        let sse = [
            r#"{"choices":[{"index":0,"delta":{"role":"assistant","reasoning_text":"Let me "},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"reasoning_text":"think this through."},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":"The answer "},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":"is 42."},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            r#"{"choices":[{"index":0,"delta":{"content":"final"},"finish_reason":null}]}"#,
            "[DONE]",
        ];
        let body = {
            let mut b = String::from("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n");
            for line in &sse {
                b.push_str("data: ");
                b.push_str(line);
                b.push_str("\n\n");
            }
            b
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            use std::io::{Read, Write};
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf); // drain the request
            sock.write_all(body.as_bytes()).unwrap();
            sock.flush().unwrap();
        });
        let profile = crate::profile::get_profile("copilot").unwrap();
        let base = format!("http://{}", addr);
        let client = ProviderClient::new(profile, Some(base), Some("ghu_test".into())).unwrap();
        // claude-* on the copilot wire routes to ChatCompletions.
        let req = ProviderRequest::new(
            "claude-opus-5",
            vec![crate::types::Message::user("meaning of life")],
        )
        .streaming(true);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let resp = rt.block_on(client.stream(&req, tx)).expect("stream ok");
        // Collect the streamed events.
        let mut reasoning_deltas = Vec::new();
        let mut content_deltas = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            match ev {
                StreamEvent::ReasoningDelta(s) => reasoning_deltas.push(s),
                StreamEvent::ContentDelta(s) => content_deltas.push(s),
                StreamEvent::Done(_) => {}
            }
        }
        assert_eq!(
            reasoning_deltas,
            vec!["Let me ".to_string(), "think this through.".to_string()],
            "reasoning_text deltas must surface as ReasoningDelta"
        );
        assert_eq!(content_deltas, vec!["The answer ".to_string(), "is 42.".to_string(), "final".to_string()]);
        // Assembled response carries the joined reasoning and content.
        assert_eq!(resp.reasoning.as_deref(), Some("Let me think this through."));
        assert_eq!(resp.content, "The answer is 42.final");
    }

    /// Same wire, precedence: reasoning_content (or reasoning) must win
    /// over reasoning_text when both are present — first-non-null order is
    /// preserved, never appended together.
    #[test]
    fn copilot_chat_stream_reasoning_first_non_null_beats_reasoning_text() {
        let sse = [
            r#"{"choices":[{"index":0,"delta":{"reasoning_content":"upstream ","reasoning_text":"copilot "},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"reasoning":"pair ","reasoning_text":"again "},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"reasoning_text":"only "},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ];
        let body = {
            let mut b = String::from("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n");
            for line in &sse {
                b.push_str("data: ");
                b.push_str(line);
                b.push_str("\n\n");
            }
            b
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            use std::io::{Read, Write};
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf);
            sock.write_all(body.as_bytes()).unwrap();
            sock.flush().unwrap();
        });
        let profile = crate::profile::get_profile("copilot").unwrap();
        let base = format!("http://{}", addr);
        let client = ProviderClient::new(profile, Some(base), Some("ghu_test".into())).unwrap();
        let req = ProviderRequest::new(
            "claude-opus-5",
            vec![crate::types::Message::user("hi")],
        )
        .streaming(true);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let resp = rt.block_on(client.stream(&req, tx)).expect("stream ok");
        let mut reasoning_deltas = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let StreamEvent::ReasoningDelta(s) = ev {
                reasoning_deltas.push(s);
            }
        }
        // Higher-priority fields win per delta; reasoning_text is used only
        // when neither sibling is present.
        assert_eq!(
            reasoning_deltas,
            vec![
                "upstream ".to_string(),
                "pair ".to_string(),
                "only ".to_string()
            ]
        );
        assert_eq!(
            resp.reasoning.as_deref(),
            Some("upstream pair only ")
        );
    }

    /// Regression (2026-08-18): a copilot-wire client built for a chat-wire
    /// model must re-derive the wire per request — gpt-5.x rides /responses
    /// even when profile.api_mode was pinned to chat at build time.
    #[test]
    fn copilot_client_reroutes_wire_per_request_model() {
        let profile = crate::profile::get_profile("copilot").unwrap();
        // Chat wire at build time (gpt-4.1 → ChatCompletions heuristically).
        let client = ProviderClient::new(profile, None, Some("ghu_test".into())).unwrap();
        let gpt5_req = ProviderRequest::new("gpt-5.4", vec![crate::types::Message::user("hi")]);
        assert_eq!(client.effective_api_mode(&gpt5_req), ApiMode::CodexResponses);
        let claude_req = ProviderRequest::new(
            "claude-sonnet-5",
            vec![crate::types::Message::user("hi")],
        );
        assert_eq!(
            client.effective_api_mode(&claude_req),
            ApiMode::ChatCompletions
        );
    }

    #[test]
    fn ollama_index_reuse_gets_fresh_slot() {
        // M7: a new tool call reusing index 0 with a NEW id → fresh slot.
        let mut accum = Vec::new();
        let mut last = Default::default();
        let mut active = Default::default();
        accumulate_tool_calls(
            &mut accum,
            &[json!({"index": 0, "id": "a", "function": {"name": "f", "arguments": "{}"}})],
            &mut last,
            &mut active,
        );
        accumulate_tool_calls(
            &mut accum,
            &[json!({"index": 0, "id": "b", "function": {"name": "g", "arguments": "{}"}})],
            &mut last,
            &mut active,
        );
        let calls = finalize_tool_calls(accum);
        assert_eq!(
            calls.len(),
            2,
            "reused index with a new id gets its own slot"
        );
        assert_eq!(calls[0].id, "a");
        assert_eq!(calls[1].id, "b");
    }

    #[test]
    fn tool_name_assigned_not_concatenated() {
        // M8/Ollama: providers resending the full name each chunk must not
        // produce "read_fileread_file".
        let mut accum = Vec::new();
        let mut last = Default::default();
        let mut active = Default::default();
        for _ in 0..2 {
            accumulate_tool_calls(
                &mut accum,
                &[
                    json!({"index": 0, "id": "a", "function": {"name": "read_file", "arguments": "{\"x\":1"}}),
                ],
                &mut last,
                &mut active,
            );
        }
        let calls = finalize_tool_calls(accum);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(
            calls[0].function.arguments, r#"{"x":1{"x":1"#,
            "args concatenate"
        );
    }

    #[test]
    fn openai_response_refusal_promotes_to_content_filter() {
        // M9: a sole refusal → content + content_filter.
        let v = json!({
            "choices": [{"message": {"content": "", "refusal": "I can't help with that."}, "finish_reason": "stop"}],
            "model": "gpt-4.1"
        });
        let n = parse_openai_response(&v).unwrap();
        assert_eq!(n.content, "I can't help with that.");
        assert_eq!(n.finish_reason, FinishReason::ContentFilter);
        // But a refusal alongside real content does NOT hijack the turn.
        let v = json!({
            "choices": [{"message": {"content": "here you go", "refusal": "note"}, "finish_reason": "stop"}]
        });
        let n = parse_openai_response(&v).unwrap();
        assert_eq!(n.content, "here you go");
        assert_eq!(n.finish_reason, FinishReason::Stop);
    }

    #[test]
    fn openai_response_integer_finish_and_reasoning_first_non_null() {
        // Poolside integer finish_reason tolerance.
        let v = json!({"choices": [{"message": {"content": "x", "reasoning_content": "rc"}, "finish_reason": 24}]});
        let n = parse_openai_response(&v).unwrap();
        assert_eq!(n.finish_reason, FinishReason::Stop);
        // reasoning_content wins as first-non-null over absent reasoning.
        assert_eq!(n.reasoning.as_deref(), Some("rc"));
    }

    #[test]
    fn openai_response_reasoning_text_copilot_extension() {
        // Copilot-wire claude thinking: `reasoning_text` alone is captured
        // (Joey extension, verified live 2026-08-21 — see chat.rs).
        let v = json!({"choices": [{"message": {"content": "x", "reasoning_text": "rt"}, "finish_reason": "stop"}]});
        let n = parse_openai_response(&v).unwrap();
        assert_eq!(n.reasoning.as_deref(), Some("rt"));
        // Precedence preserved: reasoning beats reasoning_text...
        let v = json!({"choices": [{"message": {"content": "x", "reasoning": "r", "reasoning_text": "rt"}, "finish_reason": "stop"}]});
        let n = parse_openai_response(&v).unwrap();
        assert_eq!(n.reasoning.as_deref(), Some("r"));
        // ...and reasoning_content beats reasoning_text.
        let v = json!({"choices": [{"message": {"content": "x", "reasoning_content": "rc", "reasoning_text": "rt"}, "finish_reason": "stop"}]});
        let n = parse_openai_response(&v).unwrap();
        assert_eq!(n.reasoning.as_deref(), Some("rc"));
    }

    #[test]
    fn usage_cache_stats() {
        // M9: DeepSeek prompt_cache_hit_tokens fallback + cache_write_tokens.
        let u = json!({
            "prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110,
            "prompt_cache_hit_tokens": 40
        });
        let usage = parse_usage(&u);
        assert_eq!(usage.cache_read_tokens, 40);
        let u = json!({
            "prompt_tokens": 100,
            "prompt_tokens_details": {"cached_tokens": 30, "cache_write_tokens": 20}
        });
        let usage = parse_usage(&u);
        assert_eq!(usage.cache_read_tokens, 30);
        assert_eq!(usage.cache_write_tokens, 20);
    }

    #[test]
    fn base_url_helpers() {
        assert_eq!(
            strip_trailing_v1("https://api.anthropic.com/v1"),
            "https://api.anthropic.com"
        );
        assert_eq!(
            strip_trailing_v1("https://api.anthropic.com"),
            "https://api.anthropic.com"
        );
        assert!(is_native_gemini_base_url(
            "https://generativelanguage.googleapis.com/v1beta"
        ));
        assert!(!is_native_gemini_base_url(
            "https://generativelanguage.googleapis.com/v1beta/openai"
        ));
    }

    #[test]
    fn anthropic_stream_error_classification() {
        assert!(matches!(
            anthropic_stream_error("overloaded_error", "busy"),
            ProviderError::Overloaded(_)
        ));
        assert!(anthropic_stream_error("overloaded_error", "busy").is_retryable());
        assert!(matches!(
            anthropic_stream_error("api_error", "boom"),
            ProviderError::ServerError(_)
        ));
        assert!(matches!(
            anthropic_stream_error("invalid_request_error", "bad"),
            ProviderError::FormatError(_)
        ));
    }

    #[test]
    fn xai_codex_mode_refused() {
        let xai = crate::profile::get_profile("xai").unwrap();
        match ProviderClient::new(xai, None, Some("k".into())) {
            Err(e) => assert!(e.to_string().contains("codex_responses")),
            Ok(_) => panic!("xai codex mode should be refused"),
        }
    }

    #[test]
    fn parse_sse_delta_malformed_does_not_panic() {
        // FR-006/SC-005: accumulate_tool_calls (SAFETY comment at client.rs:1308)
        // processes tool_calls delta objects from SSE events. The unwrap on
        // active_slot_by_idx is guarded by the entry().or_insert() above.
        // Feed malformed delta objects to prove no panic.

        // Missing index field (defaults to 0 via unwrap_or).
        let mut accum = Vec::new();
        let mut last: std::collections::HashMap<u64, String> = Default::default();
        let mut active: std::collections::HashMap<u64, usize> = Default::default();
        accumulate_tool_calls(
            &mut accum,
            &[json!({"function": {"name": "f", "arguments": "{}"}})],
            &mut last,
            &mut active,
        );

        // Missing function field entirely.
        accumulate_tool_calls(
            &mut accum,
            &[json!({"index": 0, "id": "x"})],
            &mut last,
            &mut active,
        );

        // Non-object delta (string element in the array).
        accumulate_tool_calls(
            &mut accum,
            &[json!("not an object")],
            &mut last,
            &mut active,
        );

        // Null tool call element.
        accumulate_tool_calls(
            &mut accum,
            &[Value::Null],
            &mut last,
            &mut active,
        );

        // Index as a non-numeric type.
        accumulate_tool_calls(
            &mut accum,
            &[json!({"index": "zero", "function": {"name": "g"}})],
            &mut last,
            &mut active,
        );

        // Large index value.
        accumulate_tool_calls(
            &mut accum,
            &[json!({"index": 999, "id": "big", "function": {"name": "h", "arguments": "{}"}})],
            &mut last,
            &mut active,
        );

        // The function processed all malformed deltas without panicking.
        // Names at shared slots get overwritten; just verify no panic and
        // that the finalization produced valid output.
        let calls = finalize_tool_calls(accum);
        // Slot 0 was overwritten by "g" (both share raw_idx=0); slot 999 has "h".
        assert!(calls.iter().any(|c| c.function.name == "h"));
        assert!(calls.iter().all(|c| !c.function.name.is_empty()));
    }
}

#[cfg(test)]
mod responses_body_tests {
    use super::*;
    use crate::request::ProviderRequest;
    use crate::types::{ContentPart, ImageUrl, Message};

    fn client() -> ProviderClient {
        let profile = crate::profile::get_profile("copilot").unwrap();
        let mut profile = profile.clone();
        profile.api_mode = ApiMode::CodexResponses;
        ProviderClient::new(profile, None, Some("gho_test_1234567890".into())).unwrap()
    }

    #[test]
    fn responses_input_items_have_message_type() {
        // Regression: input items without "type":"message" are silently
        // dropped by the Responses API (and the AI Usage HUD proxy's
        // translator), losing the whole conversation — every prompt got the
        // same generic greeting.
        let c = client();
        let req = ProviderRequest::new(
            "gpt-5.4",
            vec![Message::user("Reply with exactly: APPLE")],
        );
        let body = c.build_responses_body(&req);
        let input = body["input"].as_array().unwrap();
        assert!(!input.is_empty(), "user message must be present");
        for item in input {
            assert_eq!(
                item["type"], "message",
                "every message input item needs type=message (got {item})"
            );
            assert!(item["role"].is_string(), "role preserved");
        }
    }

    #[test]
    fn responses_system_goes_to_instructions_not_input() {
        let c = client();
        let req = ProviderRequest::new("gpt-5.4", vec![Message::user("hi")])
            .with_system(Some("You are joey.".to_string()));
        let body = c.build_responses_body(&req);
        assert_eq!(body["instructions"], "You are joey.");
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 1, "only the user message in input");
        assert_eq!(input[0]["role"], "user");
    }

    #[test]
    fn responses_tool_roundtrip_shapes() {
        let c = client();
        let mut assistant = Message::assistant("calling");
        assistant.tool_calls = vec![crate::types::ToolCall {
            id: "call_1".into(),
            call_type: "function".into(),
            function: crate::types::FunctionCall {
                name: "read_file".into(),
                arguments: "{\"path\":\"/x\"}".into(),
            },
        }];
        let mut tool_msg = crate::types::Message::tool_result("call_1", "read_file", "contents");
        tool_msg.tool_call_id = Some("call_1".into());
        let req = ProviderRequest::new(
            "gpt-5.4",
            vec![Message::user("read it"), assistant, tool_msg],
        );
        let body = c.build_responses_body(&req);
        let input = body["input"].as_array().unwrap();
        let types: Vec<&str> = input.iter().map(|i| i["type"].as_str().unwrap_or("")).collect();
        assert!(types.contains(&"message"));
        assert!(types.contains(&"function_call"));
        assert!(types.contains(&"function_call_output"));
        // tools flatten to the Responses shape
        let _ = req.tools; // (none here; shape covered by translateTools on the proxy)
    }

    #[test]
    fn responses_multimodal_parts() {
        let c = client();
        let mut msg = Message::user("look");
        msg.content_parts = Some(vec![
            ContentPart::Text { text: "look".into() },
            ContentPart::ImageUrl {
                image_url: ImageUrl { url: "https://x/y.png".into() },
            },
        ]);
        let req = ProviderRequest::new("gpt-5.4", vec![msg]);
        let body = c.build_responses_body(&req);
        let item = &body["input"][0];
        assert_eq!(item["type"], "message");
        let content = item["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[1]["type"], "input_image");
    }

    // ── Reasoning elicitation on the copilot-wire /responses body ──────────

    /// xhigh on a gpt-5.x model (cold catalog → minimal/low/medium/high set)
    /// MUST clamp to high and request reasoning summaries.
    #[test]
    fn responses_effort_xhigh_clamps_to_high_and_requests_summary() {
        // Serialize against catalog-seeding tests (CatalogCacheGuard users):
        // this test asserts the COLD-catalog clamp, so it must not observe a
        // concurrently-seeded fixture for the same model.
        let _guard = crate::copilot::TEST_ENV_LOCK.lock().unwrap();
        let c = client();
        let req = ProviderRequest::new("gpt-5.4", vec![Message::user("hi")])
            .with_reasoning(Some(crate::request::ReasoningEffort::Level("xhigh".into())));
        let body = c.build_responses_body(&req);
        assert_eq!(body["reasoning"]["effort"], json!("high"));
        assert_eq!(body["reasoning"]["summary"], json!("auto"));
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        // store/stream behavior untouched.
        assert_eq!(body["store"], json!(false));
        assert_eq!(body["stream"], json!(false));
    }

    /// Catalog-provided valid sets win over the cold-cache heuristic: a model
    /// whose catalog entry allows xhigh keeps xhigh verbatim; one capped at
    /// high clamps xhigh→high.
    #[test]
    fn responses_effort_uses_catalog_valid_set() {
        let _guard = crate::copilot::TEST_ENV_LOCK.lock().unwrap();
        // Entry advertising xhigh — no clamp.
        let _xhigh = crate::copilot::CatalogCacheGuard::seed_with(
            "gpt-5.4",
            json!({"reasoning_effort": ["none", "low", "medium", "high", "xhigh"]}),
        );
        let c = client();
        let req = ProviderRequest::new("gpt-5.4", vec![Message::user("hi")])
            .with_reasoning(Some(crate::request::ReasoningEffort::Level("xhigh".into())));
        let body = c.build_responses_body(&req);
        assert_eq!(body["reasoning"]["effort"], json!("xhigh"));

        // Entry capped at high — xhigh clamps down.
        let _capped = crate::copilot::CatalogCacheGuard::seed_with(
            "gpt-5.4",
            json!({"reasoning_effort": ["low", "medium", "high"]}),
        );
        let body = c.build_responses_body(&req);
        assert_eq!(body["reasoning"]["effort"], json!("high"));

        // Effort below the model minimum clamps up (low on a min=high set).
        let _minhigh = crate::copilot::CatalogCacheGuard::seed_with(
            "gpt-5.4",
            json!({"reasoning_effort": ["high", "max"]}),
        );
        let low_req = ProviderRequest::new("gpt-5.4", vec![Message::user("hi")])
            .with_reasoning(Some(crate::request::ReasoningEffort::Level("low".into())));
        let body = c.build_responses_body(&low_req);
        assert_eq!(body["reasoning"]["effort"], json!("high"));
    }

    /// Valid efforts pass through unchanged; `none`/empty/Disabled omit the
    /// reasoning object entirely (pre-fix behavior for those cases).
    #[test]
    fn responses_valid_efforts_pass_through_and_none_omits() {
        // Serialize against catalog-seeding tests (CatalogCacheGuard users):
        // this test asserts pass-through against the COLD catalog, so it must
        // not observe a concurrently-seeded valid-set for the same model.
        let _guard = crate::copilot::TEST_ENV_LOCK.lock().unwrap();
        let c = client();
        for effort in ["minimal", "low", "medium", "high"] {
            let req = ProviderRequest::new("gpt-5.4", vec![Message::user("hi")])
                .with_reasoning(Some(crate::request::ReasoningEffort::Level(effort.into())));
            let body = c.build_responses_body(&req);
            assert_eq!(
                body["reasoning"]["effort"],
                json!(effort),
                "{effort} must pass through verbatim"
            );
            assert_eq!(body["reasoning"]["summary"], json!("auto"));
            assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        }
        // ReasoningEffort::Level("none") → omitted entirely.
        let req = ProviderRequest::new("gpt-5.4", vec![Message::user("hi")])
            .with_reasoning(Some(crate::request::ReasoningEffort::Level("none".into())));
        let body = c.build_responses_body(&req);
        assert!(body.get("reasoning").is_none());
        assert!(body.get("include").is_none());
        // Disabled → omitted entirely.
        let req = ProviderRequest::new("gpt-5.4", vec![Message::user("hi")])
            .with_reasoning(Some(crate::request::ReasoningEffort::Disabled));
        let body = c.build_responses_body(&req);
        assert!(body.get("reasoning").is_none());
        assert!(body.get("include").is_none());
        // No reasoning config at all → omitted entirely.
        let req = ProviderRequest::new("gpt-5.4", vec![Message::user("hi")]);
        let body = c.build_responses_body(&req);
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn responses_effort_clamp_table() {
        // Cold-catalog generic clamp table.
        let valid: Vec<String> = vec![];
        assert_eq!(clamp_effort("low", &valid), "low");
        assert_eq!(clamp_effort("xhigh", &valid), "high");
        assert_eq!(clamp_effort("max", &valid), "high");
        assert_eq!(clamp_effort("ultra", &valid), "high");
        assert_eq!(clamp_effort("bogus", &valid), "medium");
        // Catalog sets.
        let gpt5: Vec<String> = ["minimal", "low", "medium", "high"]
            .iter().map(|s| s.to_string()).collect();
        assert_eq!(clamp_effort("medium", &gpt5), "medium");
        assert_eq!(clamp_effort("xhigh", &gpt5), "high");
        let o_series: Vec<String> = ["low", "medium", "high"]
            .iter().map(|s| s.to_string()).collect();
        assert_eq!(clamp_effort("minimal", &o_series), "low");
        // Unrankable catalog entries ("none") don't break the clamp.
        let with_none: Vec<String> = ["none", "low", "high"]
            .iter().map(|s| s.to_string()).collect();
        assert_eq!(clamp_effort("xhigh", &with_none), "high");
    }
}
