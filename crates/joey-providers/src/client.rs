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
        // uses the Responses transport implemented in this client.
        if profile.api_mode == ApiMode::CodexResponses && profile.name != "copilot" {
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
        let copilot_auth = if profile.name == "copilot" {
            let raw = if let Some(explicit) = key.take() {
                copilot::validate_copilot_token(&explicit).map_err(ProviderError::Auth)?;
                explicit
            } else {
                copilot::resolve_copilot_token()?.0
            };
            (!raw.is_empty()).then(|| Arc::new(CopilotAuth::new(raw)))
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

    /// Non-streaming completion. Returns a fully-assembled response.
    pub async fn complete(
        &self,
        req: &ProviderRequest,
    ) -> Result<NormalizedResponse, ProviderError> {
        match self.profile.api_mode {
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
        let result = match self.profile.api_mode {
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

        let mut builder = if self.profile.name == "copilot" {
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
                // appended (chat_completion_helpers.py:2813, M8).
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
                    input.push(json!({"role": message.role, "content": content}));
                }
            } else if !message.text_content().trim().is_empty() {
                input.push(json!({"role": message.role, "content": message.text_content()}));
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
        if let Some(crate::request::ReasoningEffort::Level(effort)) = &req.reasoning {
            obj.insert("reasoning".into(), json!({"effort": effort}));
        }
        body
    }

    async fn responses(
        &self,
        req: &ProviderRequest,
        tx: Option<&mpsc::UnboundedSender<StreamEvent>>,
    ) -> Result<NormalizedResponse, ProviderError> {
        if self.profile.name != "copilot" {
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
        // item_id -> (wire call_id, function name, accumulated arguments)
        let mut calls: std::collections::HashMap<String, (String, String, String)> =
            Default::default();
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
                            if !item_id.is_empty() {
                                calls
                                    .entry(item_id)
                                    .or_insert((call_id, name, String::new()));
                            }
                        }
                    }
                    "response.function_call_arguments.delta" => {
                        let item_id = event
                            .get("item_id")
                            .or_else(|| event.get("call_id"))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
                        if !item_id.is_empty() {
                            calls
                                .entry(item_id.clone())
                                .or_insert_with(|| (item_id, String::new(), String::new()))
                                .2
                                .push_str(delta);
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
        let tool_calls = calls
            .into_iter()
            .map(|(_, (call_id, name, arguments))| ToolCall {
                id: call_id,
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
        if self.profile.name == "copilot" {
            body["model"] = json!(copilot::normalize_model_id(&req.model));
        }
        if req.stream {
            body.as_object_mut()
                .unwrap()
                .insert("stream".into(), json!(true));
        }

        let mut builder = self
            .http
            .post(&url)
            .header("anthropic-version", "2023-06-01")
            .json(&body);
        if self.profile.name == "copilot" {
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
                        ensure_block(&mut blocks, idx);
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
    // First-non-null of reasoning / reasoning_content (chat_completions.py:714).
    let reasoning = msg
        .get("reasoning")
        .or_else(|| msg.get("reasoning_content"))
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

fn ensure_block(blocks: &mut Vec<AnthropicBlockAccum>, idx: usize) {
    while blocks.len() <= idx {
        blocks.push(AnthropicBlockAccum::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
