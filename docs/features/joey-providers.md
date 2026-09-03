# joey-providers — LLM wire protocols, streaming, retries

`joey-providers` is the LLM provider layer of the joey-agent workspace: it maps a provider-neutral `ProviderRequest` onto the wire protocol of the active provider — OpenAI Chat Completions, Anthropic Messages, or the OpenAI Responses wire (Copilot only) — sends it with SSE streaming, and normalizes the result into a `NormalizedResponse` regardless of protocol. It is a port of upstream Hermes Agent's `providers/`, `agent/transports/` (chat + anthropic), `agent/anthropic_adapter.py`, `agent/error_classifier.py`, `agent/retry_utils.py`, the per-provider `plugins/model-providers/*` hooks, and the Copilot auth/catalog helpers in `hermes_cli/copilot_auth.py` / `models.py`. Ten provider profiles are registered (openrouter, anthropic, openai-api, copilot, ai-usage-hud, nous, deepseek, gemini, zai, xai), each with declarative metadata (base URL, env vars, aliases, fallback models) and per-provider reasoning wire shapes.

> See also: [../providers.md](../providers.md)

## Overview

The crate has three jobs:

1. **Profile resolution** (`profile.rs`, `lib.rs`) — pick the right provider from an explicit setting (`"auto"` allowed), the `base_url` hostname, the `vendor/model` prefix, or the bare model family name; then compute the wire model name (`wire_model_name`) and any base-URL override (`<ID>_BASE_URL` env var beats a caller-supplied URL; the app-wide OpenRouter default `https://openrouter.ai/api/v1` never hijacks a non-OpenRouter provider).
2. **Wire shaping** (`chat.rs`, `anthropic.rs`, `client.rs`) — build request bodies per protocol, including the per-provider reasoning/thinking shapes, the `max_tokens` vs `max_completion_tokens` switch, Anthropic history shaping (tool-result merging, orphan stripping, thinking-signature management), and prompt caching.
3. **Transport + normalization** (`client.rs`, `error.rs`) — reqwest-based HTTP with SSE parsing, tool-call delta accumulation, and a classified error taxonomy (`ProviderError`) plus jittered backoff helpers that the agent loop uses to decide retry / compress / failover.

The result surface is deliberately small: `ProviderClient::complete` / `ProviderClient::stream` return a `NormalizedResponse` (content, tool calls, finish reason, reasoning, usage, replay blocks), and `StreamEvent::ContentDelta` / `ReasoningDelta` / `Done` carry live deltas to the caller.

A typical request flow: the caller (joey-agent-core) builds a `ProviderRequest`, constructs a client via `build_client(provider_setting, base_url, model, api_key)`, and calls `stream(req, tx)` — the client resolves credentials per request (Copilot tokens are exchanged/cached here), picks the wire via `effective_api_mode` (pinned at build time for non-copilot profiles, re-derived per request for copilot-wire ones), and maps any non-2xx response through `ProviderError::from_status` with `Retry-After` parsing.

## Module map

| File | Purpose |
|---|---|
| `src/lib.rs` | Crate root; `build_client()` convenience wrapper, base-URL override resolution, `DEFAULT_AGGREGATOR_BASE_URL` |
| `src/request.rs` | `ProviderRequest` + `ReasoningEffort` (provider-neutral request types) |
| `src/types.rs` | `Message`, `ContentPart`, `ToolCall`, `ToolSchema`, `FinishReason`, `Usage`, `NormalizedResponse`, `StreamEvent` |
| `src/profile.rs` | `ProviderProfile` registry (10 profiles), aliases, `resolve_profile`, `get_profile`, `wire_model_name`, `is_copilot_wire`, `copilot_servable` |
| `src/chat.rs` | OpenAI Chat Completions body shaping: per-provider reasoning shapes, max-tokens kwarg switch, developer-role swap, vision stripping |
| `src/anthropic.rs` | Anthropic Messages adapter: message/tool conversion, extended-thinking contract, output limits, beta headers, prompt caching, response normalization |
| `src/client.rs` | `ProviderClient`: the three transports (chat / messages / responses), SSE parsers, tool-call accumulation, timeouts, error mapping |
| `src/copilot.rs` | Copilot auth (token exchange, device-code login), model catalog + 60s cache, model-id aliases, API-mode routing, custom-proxy endpoints |
| `src/zai.rs` | Z.AI endpoint probing (`ZAI_ENDPOINTS`, `detect_zai_endpoint`, `resolve_zai_base_url`, auth-store cache) |
| `src/error.rs` | `ProviderError` classification (status → variant), pattern tables, `jittered_backoff*`, `parse_retry_after` |
| `Cargo.toml` | Crate manifest (deps: `reqwest`, `tokio`, `serde_json`, `once_cell`, `sha2`, `ureq`, `rand`, `joey-core`) |

## Public API surface

### Request

| Item | Fields / variants | Notes |
|---|---|---|
| `ProviderRequest` | `model`, `messages: Vec<Message>`, `system: Option<String>`, `tools: Vec<ToolSchema>`, `max_tokens: Option<u32>`, `temperature: Option<f32>`, `reasoning: Option<ReasoningEffort>`, `stream: bool` | Builder methods: `new`, `with_system`, `with_tools`, `with_max_tokens`, `with_reasoning`, `streaming` |
| `ReasoningEffort` | `Disabled` \| `Level(String)` | Effort already resolved from config by the caller |

### Types (`types.rs`)

| Type | Shape |
|---|---|
| `Message` | OpenAI-wire-shaped chat message; `content` or `content_parts` (multimodal), `tool_calls`, `tool_call_id`, `name`, `reasoning`, `reasoning_details`, `anthropic_content_blocks`, plus `#[serde(skip)]` in-process markers `compressed_summary` / `synthetic`. Constructors: `system`/`user`/`assistant`, `tool_result`, `assistant_with_tools`; `text_content()` best-effort plain text |
| `ContentPart` | `Text { text }` \| `ImageUrl { image_url: ImageUrl { url } }` (serde tag `type`: `text` / `image_url`) |
| `ToolCall` / `FunctionCall` | `{ id, call_type ("function"), function: { name, arguments } }`; `parsed_args()` parses the JSON-string arguments (`{}` on failure) |
| `ToolSchema` / `FunctionSchema` | OpenAI function-tool shape `{ type, function: { name, description, parameters } }` + optional Anthropic `cache_control` marker |
| `FinishReason` | `Stop` \| `ToolCalls` \| `Length` \| `ContentFilter`; `from_wire` maps `tool_calls`/`tool_use`→`ToolCalls`, `length`/`max_tokens`/`model_context_window_exceeded`→`Length`, `content_filter`/`refusal`→`ContentFilter`, unknown→`Stop` |
| `Usage` | `prompt_tokens`, `completion_tokens`, `total_tokens`, `cache_read_tokens`, `cache_write_tokens`, `reasoning_tokens` |
| `NormalizedResponse` | `content`, `tool_calls`, `finish_reason`, `reasoning`, `usage`, `model`, `reasoning_details` (Anthropic signed thinking blocks / chat `reasoning_details` for replay), `anthropic_content_blocks` (ordered blocks, only when a turn interleaved signed thinking with `tool_use`) |
| `StreamEvent` | `ContentDelta(String)` \| `ReasoningDelta(String)` \| `Done(Box<NormalizedResponse>)` |

### Profile enums

| Type | Variants | Values |
|---|---|---|
| `ApiMode` | `ChatCompletions` \| `AnthropicMessages` \| `CodexResponses` | `as_str()`: `"chat_completions"`, `"anthropic_messages"`, `"codex_responses"` |
| `AuthType` | `ApiKey` \| `OAuth` | Every ported profile registers `ApiKey` |

### `ProviderError` variants

| Variant | Retryable | Compress | Failover | Meaning |
|---|---|---|---|---|
| `Auth(String)` | no | no | yes | 401 / auth failures |
| `RateLimit { message, retry_after }` | yes | no | yes | 429 (non-overload body), transient 402, 400 rate-limit bodies; carries `retry_after` |
| `Billing(String)` | no | no | yes | Credit/quota exhaustion (402, 403 billing bodies) |
| `Overloaded(String)` | yes | no | no | 429/503/529 with overload body — back off on the same key, don't rotate |
| `PayloadTooLarge(String)` | yes | **yes** | no | 413 |
| `ContextOverflow(String)` | yes | **yes** | no | Context-window overflow — retry after compression |
| `ModelNotFound(String)` | no | no | yes | Model-not-found bodies on 400/404 |
| `FormatError(String)` | no | no | yes | Malformed request / validation / policy block — deterministic, never retried |
| `Timeout(String)` | yes | no | no | 408, reqwest timeouts, stream stalls |
| `Connection(String)` | yes | no | no | Connection errors |
| `EmptyStream(String)` | yes | no | no | Stream yielded zero events (upstream `EmptyStreamError`) |
| `ServerError(String)` | yes | no | no | 5xx and equivalents |
| `Status { status, message }` | 404 or 5xx only | no | no | Unclassified status; bare 404 stays retryable |
| `Parse(String)` | no | no | no | Response parse failure |
| `Other(String)` | no | no | no | Misc (e.g. unported wire mode) |

`From<reqwest::Error>` maps timeout→`Timeout`, connect errors→`Connection`, everything else→`Other`.

### Free functions

| Function | Purpose |
|---|---|
| `build_client(provider_setting, base_url, model, api_key)` | `resolve_profile` + copilot catalog consult (api-mode routing) + Z.AI probe → `ProviderClient::new` |
| `resolve_profile(provider_setting, base_url, model)` | Explicit name/alias → HUD/custom-copilot magnets → hostname → `vendor/` prefix → bare family name → OpenRouter default |
| `get_profile(name)` | Registry lookup by canonical name or alias |
| `wire_model_name(profile, model)` | OpenRouter keeps the full slug; copilot-wire uses alias normalization; Anthropic wire uses `normalize_model_name`; others strip known vendor prefixes |
| `anthropic::normalize_model_name(model)` | Strip `anthropic/`, dots→hyphens for `claude-*`, preserve Bedrock IDs |
| `zai::detect_zai_endpoint` / `_async` | Probe the four Z.AI endpoints, return the first that accepts the key |
| `zai::resolve_zai_base_url(api_key, default_url, env_override)` | Env override → cached detection → probe → default |
| `copilot::resolve_copilot_token`, `validate_copilot_token`, `device_code_login`, `CopilotAuth`, `normalize_model_id`, `model_api_mode`, `fetch_model_catalog`, `peek_model_catalog`, `fallback_models`, `model_reasoning_efforts`, `catalog_context_window`, `request_headers`, `custom_endpoint`, `hud_endpoint`, `hud_health_check` | Copilot auth/catalog/routing surface (see below) |
| `error::jittered_backoff`, `jittered_backoff_api`, `jittered_backoff_with`, `parse_retry_after` | Backoff + Retry-After parsing (see Errors & retries) |

`ProviderClient` methods: `new(profile, base_url, api_key)`, `complete(&req)`, `stream(&req, tx)`, `profile()`, `has_credentials()`.

## Provider profiles

| Profile | Aliases | Base URL | ApiMode | AuthType | Notes |
|---|---|---|---|---|---|
| `openrouter` | `or` | `https://openrouter.ai/api/v1` | `ChatCompletions` | `ApiKey` | Aggregator default; keeps full `vendor/model` slugs; extra headers below; `OPENROUTER_API_KEY` |
| `anthropic` | `claude`, `claude-oauth`, `claude-code` | `https://api.anthropic.com` | `AnthropicMessages` | `ApiKey` | `ANTHROPIC_API_KEY` / `ANTHROPIC_TOKEN` / `CLAUDE_CODE_OAUTH_TOKEN`; override `ANTHROPIC_BASE_URL`; aux model `claude-haiku-4-5-20251001` |
| `openai-api` | `openai` | `https://api.openai.com/v1` | `ChatCompletions` | `ApiKey` | `OPENAI_API_KEY`; override `OPENAI_BASE_URL`; sends no reasoning params |
| `copilot` | `github-copilot`, `github-models`, `github-model`, `github` | `https://api.githubcopilot.com` | `ChatCompletions` (per-request reroute) | `ApiKey` | `COPILOT_GITHUB_TOKEN` / `GH_TOKEN` / `GITHUB_TOKEN`; override `COPILOT_API_BASE_URL` |
| `ai-usage-hud` | `usage-hud`, `ai-usage` | `http://127.0.0.1:8317` | `ChatCompletions` (per-request reroute) | `ApiKey` | Joey-specific local Copilot reverse proxy; override `AI_USAGE_HUD_BASE_URL`; same wire/credentials as copilot (`is_copilot_wire` covers both) |
| `nous` | `nous-portal`, `nousresearch` | `https://inference-api.nousresearch.com/v1` | `ChatCompletions` | `ApiKey` | `NOUS_API_KEY`; upstream uses device-code OAuth — not ported, stays ApiKey (deliberate adaptation) |
| `deepseek` | `deepseek-chat` | `https://api.deepseek.com/v1` | `ChatCompletions` | `ApiKey` | `DEEPSEEK_API_KEY`; override `DEEPSEEK_BASE_URL`; aux model `deepseek-chat` |
| `gemini` | `google`, `google-gemini`, `google-ai-studio` | `https://generativelanguage.googleapis.com/v1beta/openai` | `ChatCompletions` | `ApiKey` | `GOOGLE_API_KEY` then `GEMINI_API_KEY`; uses Google's OpenAI-compat shim (native REST adapter unported); aux model `gemini-3.5-flash` |
| `zai` | `glm`, `z-ai`, `z.ai`, `zhipu` | `https://api.z.ai/api/paas/v4` | `ChatCompletions` | `ApiKey` | `GLM_API_KEY` / `ZAI_API_KEY` / `Z_AI_API_KEY`; override `GLM_BASE_URL`; endpoint probing below; aux model `glm-4.5-flash`; fallbacks `glm-5.2`, `glm-5`, `glm-4-9b` |
| `xai` | `grok`, `x-ai`, `x.ai` | `https://api.x.ai/v1` | `CodexResponses` | `ApiKey` | `XAI_API_KEY`; override `XAI_BASE_URL`; **client construction is refused** — `ProviderClient::new` returns an error because the codex_responses wire is only implemented for copilot-wire profiles |

**Wire model name examples** (`wire_model_name`):

| Profile | Input | Wire model |
|---|---|---|
| `openrouter` | `anthropic/claude-opus-4.6` | `anthropic/claude-opus-4.6` (slug kept) |
| `anthropic` | `anthropic/claude-opus-4.6` | `claude-opus-4-6` |
| copilot-wire | `github-copilot/gpt-5.4` / `openai/o3` | `gpt-5.4` / `gpt-5.3-codex` (alias map) |
| other native | `deepseek/deepseek-chat` | `deepseek-chat` (known prefix stripped) |

All profiles have `default_max_tokens = None` (the Anthropic-family model table is the only fallback). Hostname detection uses exact/suffix matching for short ambiguous domains (`x.ai`, `z.ai`) so hosts like `max.ai` don't misroute. Two magnet rules override auto-detection: an off-githubcopilot `COPILOT_API_BASE_URL` routes EVERYTHING to `copilot` (routing contract: no request escapes the proxy), while the HUD env var magnetizes only models the Copilot backend can serve (`copilot_servable`: live catalog, else `gpt-`/`claude-`/`gemini-`/`o1`/`o3`/`o4`/`mai-code-` prefixes) — vendor families Copilot never carries (`glm-`, `deepseek-`, `grok-`, …) fall through to their native providers so the proxy can't silently substitute its default model.

## OpenAI Chat Completions wire

- **Endpoint**: `POST {base}/chat/completions`; auth `Authorization: Bearer <key>`.
- **OpenRouter extra headers**: `HTTP-Referer: https://github.com/joey/joey-agent`, `X-Title: Joey Agent` (branding `AGENT_NAME`), `X-OpenRouter-Categories: productivity,cli-agent`.
- **Claude via OpenRouter**: `x-anthropic-beta: fine-grained-tool-streaming-2025-05-14` added when the model name contains `claude`.
- **System role**: leading system message uses role `developer` (instead of `system`) when the model contains `gpt-5` or `codex` (`DEVELOPER_ROLE_MODELS`).
- **Max tokens**: key is `max_completion_tokens` when the host is `api.openai.com` / `*.openai.azure.com` or the model starts with `gpt-4o`, `gpt-4.1`, `gpt-5`, `o1`, `o3`, `o4`; otherwise `max_tokens`. Resolution: caller value > profile default (always `None`) > Anthropic-family output-table fallback (e.g. `anthropic/claude-opus-4.6` → `128000`).
- **tool_choice**: never sent on the OpenAI wire, with or without tools.
- **Streaming additions**: `stream: true` plus `stream_options: {"include_usage": true}` — omitted only for native Gemini endpoints (`generativelanguage.googleapis.com` without `/openai`; the port's gemini profile IS the shim, so it keeps it).
- **Vision**: image content parts are serialized for vision-capable model families and stripped to concatenated text for non-vision models.

### Per-provider reasoning shapes

| Profile | Wire shape |
|---|---|
| `openrouter` | Only for allowlisted models (`deepseek/`, `anthropic/`, `openai/`, `x-ai/`, `google/gemini-2`, `google/gemma-4`, `qwen/qwen3`, `tencent/hy3`, `xiaomi/`; Nous hosts always qualify; `api.mistral.ai` excluded): `extra_body.reasoning = {"enabled": true, "effort": E}`; `Disabled` → `{"enabled": false}`; unset → `{"enabled": true, "effort": "medium"}`. Reasoning-mandatory Claude models (Claude 4.6+ — everything not in the legacy optional list) get NO `reasoning` field; the effort routes to top-level `verbosity: E` instead |
| `nous` | `extra_body.reasoning = {"enabled": true, "effort": E}`; omitted entirely when `Disabled`; unset → `"medium"` |
| `deepseek` | Thinking-capable models only (V4+: `deepseek-v*` except `deepseek-v3`; or exactly `deepseek-reasoner`): `extra_body.thinking = {"type": "enabled"\|"disabled"}` + top-level `reasoning_effort` (`xhigh`/`max`/`ultra` → `max`; `low`/`medium`/`high` verbatim). `deepseek-chat` (V3) gets nothing |
| `zai` | `glm-` ≥ 4.5 (parsed `^glm-(\d+)(\.(\d+))?`) or GLM-5.2 aliases (`glm-5.2`/`glm-5-2`/`glm-5p2`): `extra_body.thinking = {"type": "enabled"\|"disabled"}` (unset → omitted, server default). GLM-5.2 additionally sends top-level `reasoning_effort` with minimum `high`: `xhigh`/`max`/`ultra` → `max`, every other level → `high` |
| `gemini` | `extra_body.extra_body.google.thinking_config = {"include_thoughts": bool, "thinking_level": str}` (snake_cased for the shim). `gemini-2.5-*` gets `include_thoughts` only; `gemini-3`/`gemini-3.1` flash maps `minimal`/`low`→`low`, `high`+→`high`, else `medium`; pro maps `high`+→`high`, else `low`; invalid levels → `medium`; `Disabled`/`none` → `{"include_thoughts": false}` |
| copilot-wire (`copilot`, `ai-usage-hud`) | Claude models (not Haiku) via `/chat/completions`: top-level `thinking = {"type": "enabled", "budget_tokens": B}` with `xhigh`/`max`/`ultra` → `32000`, `high` → `16000`, `medium`/unknown → `8000`, `low` → `4000`. `Disabled` omits the parameter; Haiku, Gemini, and unknown families get nothing |
| `openai-api` (and any unlisted) | Sends nothing reasoning-related — no `thinking`, no `extra_body`, no `reasoning_effort` |

## Anthropic Messages wire

- **URL**: `{base minus trailing /v1}/v1/messages` (a base ending in `/v1` is stripped first, avoiding `/v1/v1/messages`).
- **Headers**: `anthropic-version: 2023-06-01` always. Auth per token shape: `sk-ant-api*` (Console keys) → `x-api-key`; other `sk-ant-*`, `eyJ*` (JWTs), `cc-*` (Claude Code OAuth) → `Authorization: Bearer`. `anthropic-beta` is a comma-joined list: `interleaved-thinking-2025-05-14` + `fine-grained-tool-streaming-2025-05-14` always; `context-1m-2025-08-07` added only on Azure hosts; MiniMax Anthropic endpoints (`api.minimax.io/anthropic`, `api.minimaxi.com/anthropic`) have the streaming and 1M betas stripped. OAuth-only betas (`claude-code-*`, `oauth-*`) are deliberately never sent — the upstream identity-spoofing layer is not replicated.
- **System**: top-level `system` field (string, or a block list when carrying `cache_control` markers).
- **Tools**: `{"name", "description", "input_schema"}` with duplicate-name dedup (second occurrence dropped with a warning), nullable-union collapsing (`anyOf`/`oneOf` with one non-null branch → that branch), top-level `oneOf`/`allOf`/`anyOf` removal, and an `{"type":"object","properties":{}}` floor. When tools are present, `tool_choice: {"type": "auto"}` is sent.
- **Tool results**: consecutive tool-result messages are merged into ONE user message containing all `tool_result` blocks (parallel tool calls produce a single user turn); empty content becomes `"(no output)"`. Tool IDs are sanitized to `[a-zA-Z0-9_-]`.
- **History shaping**: orphaned `tool_use`/`tool_result` blocks are stripped (adjacency-checked), consecutive same-role messages are merged to enforce alternation, thinking signatures are managed per endpoint (third-party endpoints strip ALL thinking blocks; direct Anthropic keeps signed thinking only on the latest assistant turn; Kimi endpoints replay as-is; DeepSeek `/anthropic` strips signed, keeps unsigned), and only the 3 most recent computer-use screenshots are kept.
- **Extended thinking** (models without `haiku` in the name, when an effort level is set):
  - Adaptive models (Claude 4.6+, unknown Claudes, Kimi family): `thinking = {"type": "adaptive", "display": "summarized"}` + `output_config = {"effort": E}` where E maps `ultra`/`max`→`max`, `xhigh`→`xhigh`, `high`→`high`, `medium`→`medium`, `low`/`minimal`→`low`, unknown→`medium`. The 4.6 family rejects `xhigh`, so it is downgraded to `max` there.
  - Legacy models (claude-3, 4.0/4.1, date-stamped 4.0, 4.5 families, haiku-4.5): `thinking = {"type": "enabled", "budget_tokens": B}` + forced `temperature: 1` + `max_tokens` raised to `max(effective, B + 4096)`. Budget table: `xhigh` 32000, `high` 16000, `medium` 8000, `low` 4000, unknown 8000.
  - Models 4.7+ (adaptive, not in the 4.6 or legacy lists) strip `temperature`/`top_p`/`top_k` entirely — they 400 on any non-default sampling params.
- **`max_tokens`** is mandatory: caller value when positive, else the per-model output ceiling (longest-substring match in `ANTHROPIC_OUTPUT_LIMITS`; e.g. `claude-opus-4-6`/`claude-sonnet-5` 128000, `claude-sonnet-4-6`/`claude-*-4-5` 64000, `claude-opus-4` 32000, `claude-3-5-*` 8192, `claude-3-*` 4096, `minimax` 131072, `qwen3` 65536; default 128000).
- **Prompt caching**: default-on for Claude models — the upstream `system_and_3` strategy places 4 ephemeral breakpoints (`{"type": "ephemeral"}`, 5-minute TTL): the system prompt plus the last 3 non-system messages. The 1-hour TTL tier is not ported.
- **Model name**: `normalize_model_name` strips `anthropic/`, converts dots→hyphens for `claude-*` (`claude-opus-4.6` → `claude-opus-4-6`), preserves Bedrock IDs (`anthropic.claude-*`, `us.anthropic.*`), leaves non-Anthropic models untouched.

## Responses API wire (Copilot only)

The `CodexResponses` mode is implemented solely for copilot-wire profiles (`responses()` errors otherwise). GPT-5+ Copilot models (major ≥ 5, except `gpt-5-mini`) are routed here per-request by `model_api_mode`; Claude/Gemini/older models ride `/chat/completions` or `/v1/messages`.

- **Endpoint**: `POST {base}/responses` with Copilot headers + Bearer token.
- **Body shape**:

```json
{
  "model": "<normalized id>",
  "input": [ {"type": "message", "role": "...", "content": ...},
             {"type": "function_call", "call_id": "...", "name": "...", "arguments": "..."},
             {"type": "function_call_output", "call_id": "...", "output": "..."} ],
  "stream": true,
  "store": false,
  "instructions": "<system prompt>",
  "tools": [ {"type": "function", "name": "...", "description": "...", "parameters": {...}, "strict": false} ],
  "tool_choice": "auto",
  "parallel_tool_calls": true,
  "max_output_tokens": 4096,
  "reasoning": {"effort": "high", "summary": "auto"},
  "include": ["reasoning.encrypted_content"]
}
```

  `instructions`, `tools`, `tool_choice`, `parallel_tool_calls`, and `max_output_tokens` are present only when non-empty; `reasoning` + `include` only when an effort level is set. Every input message item carries `"type": "message"` — typeless items are silently dropped by the API (a past regression). Multimodal parts map to `input_text` / `input_image`.
- **Effort clamping**: the effort is clamped onto the model's valid set (from the catalog entry's `capabilities.supports.reasoning_effort`, falling back to `minimal`/`low`/`medium`/`high` for gpt-5.x and `low`/`medium`/`high` for o-series). Effort already valid → verbatim; above the model's max → highest valid ≤ it (e.g. `xhigh` on a `minimal..high` model → `high`); below the min → the min; cold catalog → `minimal..high` verbatim, `xhigh`/`max`/`ultra` → `high`, unknown → `medium`.

## Streaming

All three transports parse SSE manually over `resp.bytes_stream()`:

- Lines starting with `data:` are JSON-parsed; `data: [DONE]` terminates; non-`data:` lines are skipped; a final line lacking a trailing newline is flushed through the parser once so usage/finish/`[DONE]` are never dropped.
- **Per-chunk timeout**: each `stream.next()` await is wrapped in `tokio::time::timeout` — a stall beyond it fails with `ProviderError::Timeout("stream stalled: no chunk within Ns")`. Default 120s, overridable via `JOEY_STREAM_READ_TIMEOUT`.

**Chat Completions deltas**: `choices[0].delta.content` → content; reasoning is first-non-null of `reasoning_content` → `reasoning` → `reasoning_text` (the third is a Joey extension for copilot-wire Claude, verified live 2026-08-21); the fields are never appended together. `finish_reason` tolerates integer values (mapped via their string form). Usage is taken from the last non-null `usage` chunk.

**Tool-call accumulation**: `delta.tool_calls` entries are accumulated by their `index` into slots — args concatenate, names are assigned (not concatenated) so providers that resend the full name per chunk survive. Ollama fix: a delta reusing an index with a *different* id opens a fresh slot. `MAX_STREAM_INDEX = 1024` caps indices defensively (a hostile `{"index": 1e18}` would otherwise OOM the process; out-of-range deltas are dropped). Empty-name slots are dropped at finalize; missing ids get `call_{i}`; missing args default to `{}`.

**Usage parsing** (`parse_usage`, chat wire): `prompt_tokens` / `completion_tokens` / `total_tokens` verbatim; cache read from `prompt_tokens_details.cached_tokens` with DeepSeek's top-level `prompt_cache_hit_tokens` as fallback; cache write from `prompt_tokens_details.cache_write_tokens`; reasoning tokens from `completion_tokens_details.reasoning_tokens`. The Anthropic wire maps `input_tokens`/`output_tokens`/`cache_read_input_tokens`/`cache_creation_input_tokens` and sums `total_tokens` itself; the Responses wire reads `input_tokens`/`output_tokens`/`total_tokens` plus `output_tokens_details.reasoning_tokens`.

**Partial-stream truncation**: if the stream ends with no `finish_reason` and either (a) accumulated tool args don't parse as JSON or (b) text arrived with no tool calls, `FinishReason::Length` is set so the loop retries instead of executing a truncated call; truncated args alongside a real finish also produce `Length`. A stream with zero events and no content/args is an `EmptyStream` error.

**Responses events**: `response.output_text.delta` (content), `response.reasoning_summary_text.delta` / `response.reasoning_text.delta` (reasoning), `response.output_item.added` / `response.output_item.done` and `response.function_call_arguments.delta` / `.done` (tool-call accumulation — keyed by the stable `output_index`, since real proxy streams obfuscate `item_id`; the `done` payloads are authoritative), `response.completed` (authoritative full response), `error` / `response.failed` → `ServerError`.

**Anthropic events**: `message_start` (model + initial usage), `content_block_start` (block type / tool id+name / redacted data), `content_block_delta` (`text`, `thinking`, `signature`, `partial_json` deltas), `message_delta` (stop reason + final usage), and `error` events which map to classified errors (`overloaded_error`→`Overloaded`, `rate_limit_error`→`RateLimit`, `api_error`/`timeout_error`→`ServerError`, `authentication_error`/`permission_error`→`Auth`, `invalid_request_error`→`FormatError`, else `Status`). Blocks are assembled by index (same `MAX_STREAM_INDEX` cap) and re-run through the replay-block sanitizer before normalization.

## Errors & retries

`ProviderError::from_status(status, body, retry_after)` classifies by status with body-pattern disambiguation (pattern tables are ported verbatim: billing, rate-limit, overload, context-overflow — including the load-bearing bare `max_tokens` — model-not-found, request-validation, policy-blocked, empty-response):

| Status | Classification |
|---|---|
| 401 | `Auth` |
| 402 | `RateLimit` when usage-limit + transient signals (`try again`, `resets at`, …) co-occur; else `Billing` |
| 403 | `Billing` for `key limit exceeded` / `spending limit` / billing bodies; else `Auth` |
| 404 | `Billing` / policy-block `FormatError` / `ModelNotFound` by body; bare 404 → `Status` (retryable — proxy glitch) |
| 408 | `Timeout` |
| 413 | `PayloadTooLarge` (retryable + compress) |
| 429 | `Overloaded` when the body matches overload patterns (back off same key); else `RateLimit` with `retry_after` |
| 400 | Bucket order: request-validation `FormatError` (checked BEFORE overflow; bare `invalid_request_error` excluded) → empty-response `ServerError` → `ContextOverflow` → policy `FormatError` → `ModelNotFound` → `RateLimit` → `Billing` → `FormatError` |
| 500/502 | validation bodies → `FormatError` (fail fast); empty-response → `ServerError`; overflow → `ContextOverflow`; else `ServerError` |
| 503/529 | empty-response → `ServerError`; overflow → `ContextOverflow`; else `Overloaded` |
| other 4xx/5xx | `FormatError` / `ServerError`; anything else → `Status` |

Semantics consumed by the agent loop: `is_retryable()` (retry same request), `should_compress()` (compress conversation first — `PayloadTooLarge` + `ContextOverflow` only), `should_failover()` (switch model/credential — `Auth`, `Billing`, `ModelNotFound`, `FormatError`, `RateLimit`), `retry_after()` (server-advised delay, `RateLimit` only).

**Backoff** (port of `retry_utils.py`):

| Function | Base | Max | Used for |
|---|---|---|---|
| `jittered_backoff(attempt)` | 5s | 120s | Upstream default signature |
| `jittered_backoff_api(attempt)` | 2s | 60s | API-error retry path (conversation loop) |
| `jittered_backoff_with(attempt, base, max)` | — | — | Parameterized core |

Formula: `min(base · 2^(attempt−1), max) + U[0, 0.5·delay)` with a real RNG; `attempt` is 1-based. `parse_retry_after(raw)` parses float seconds from a `Retry-After` header, ignores non-numeric/negative values, and caps at **600s**. `From<reqwest::Error>` maps timeouts→`Timeout`, connect errors→`Connection`, else `Other`.

## Copilot integration

- **Credential resolution** (`resolve_copilot_token`): `COPILOT_GITHUB_TOKEN` → `GH_TOKEN` → `GITHUB_TOKEN` → `gh auth token` (searching `which gh`, `~/.local/bin/gh`, `/opt/homebrew/bin/gh`, `/usr/local/bin/gh`; honors `COPILOT_GH_HOST`; strips `GITHUB_TOKEN`/`GH_TOKEN` from the child env). Classic PATs (`ghp_*`) are rejected — fine-grained `github_pat_*`, `ghu_*` app tokens, or OAuth are required.
- **Endpoints**: `COPILOT_BASE_URL = https://api.githubcopilot.com`; catalog at `https://api.githubcopilot.com/models`. Token exchange: `GET https://api.github.com/copilot_internal/v2/token` (overridable via `COPILOT_TOKEN_URL`) with `User-Agent: GitHubCopilotChat/0.26.7` and `Editor-Version: vscode/1.104.1`; the response's `endpoints.api` (or a `proxy-ep=` token claim) derives an Enterprise base URL. Exchange failure falls back to the raw credential (Hermes parity).
- **Token caching/refresh**: exchanged tokens cached in-process; considered valid until `expires_at − 120s` (`REFRESH_MARGIN_SECS`). A 401 on any request triggers one retry with a freshly exchanged token (`send_with_auth_refresh`).
- **Device-code login** (`device_code_login`, used by `joey auth copilot` / `joey model`): `POST https://github.com/login/device/code` with `client_id = Iv1.b507a08c87ecfe98`, `scope = read:user`; polls `/login/oauth/access_token` at the advised interval (+3s) until authorized/expired/denied.
- **Request headers** (`request_headers`): `Editor-Version: vscode/1.104.1`, `User-Agent: JoeyAgent/1.0`, `Copilot-Integration-Id: vscode-chat`, `Openai-Intent: conversation-edits`, `x-initiator: agent|user` (tracks whether the last message is a user turn), plus `Copilot-Vision-Request: true` for image-bearing requests.
- **Model catalog**: `fetch_model_catalog` fetches `/models` (exchange path or custom proxy), filters to usable chat models (non-empty id, picker-enabled, `capabilities.type == "chat"`, a usable endpoint among `/chat/completions`, `/responses`, `/v1/messages`, deduplicated), and caches in-process for **60s**. `peek_model_catalog` reads the cache without fetching (used on the per-request routing path).
- **Custom proxy / AI Usage HUD**: `COPILOT_API_BASE_URL` or `AI_USAGE_HUD_BASE_URL` pointing at an off-githubcopilot host activates custom-endpoint mode — all traffic goes to the proxy with the RAW GitHub credential (no exchange; the proxy owns upstream auth/refresh/usage capture). The HUD profile pins `http://127.0.0.1:8317` and offers `hud_health_check` against `/api/health`.
- **Routing** (`model_api_mode`): normalized `gpt-5`+ (except `gpt-5-mini`) → `CodexResponses`; a catalog entry exposing only `/v1/messages` → `AnthropicMessages`; else `ChatCompletions`. Copilot-wire clients re-derive the wire per request so a per-turn model substitution never rides the wrong wire.
- **Fallback models** (`fallback_models`, when the live catalog is unreachable): `gpt-5.4`, `gpt-5.4-mini`, `gpt-5-mini`, `gpt-5.3-codex`, `gpt-5.2-codex`, `gpt-4.1`, `gpt-4o`, `gpt-4o-mini`, `claude-sonnet-4.6`, `claude-sonnet-5`, `claude-sonnet-4`, `claude-sonnet-4.5`, `claude-haiku-4.5`, `gemini-3.1-pro-preview`, `gemini-3-pro-preview`, `gemini-3-flash-preview`, `gemini-2.5-pro`.
- **Model-id aliases** (`normalize_model_id`): e.g. `openai/o3` → `gpt-5.3-codex`, `openai/o1` → `gpt-5.2`, `openai/gpt-5-nano` → `gpt-5-mini`, `claude-sonnet-4-6` → `claude-sonnet-4.6`; otherwise the `vendor/` prefix is stripped.

## Z.AI endpoint probing

Z.AI bills general and coding plans separately on global and China endpoints; a key valid on one may return "Insufficient balance" on another. `ZAI_ENDPOINTS` (probe order):

| ID | Base URL | Probe models | Label |
|---|---|---|---|
| `global` | `https://api.z.ai/api/paas/v4` | `glm-5` | Global |
| `cn` | `https://open.bigmodel.cn/api/paas/v4` | `glm-5` | China |
| `coding-global` | `https://api.z.ai/api/coding/paas/v4` | `glm-5.2`, `glm-5.1`, `glm-5v-turbo`, `glm-4.7` | Global (Coding Plan) |
| `coding-cn` | `https://open.bigmodel.cn/api/coding/paas/v4` | `glm-5.2`, `glm-5.1`, `glm-5v-turbo`, `glm-4.7` | China (Coding Plan) |

`resolve_zai_base_url(api_key, default, env_override)`: an explicit `GLM_BASE_URL` always wins; an empty key skips probing entirely (pure latency otherwise); a cached detection is used when its `key_hash` matches; otherwise each endpoint × probe model is tried with a `{"model", "stream": false, "max_tokens": 1, "messages": [{"role":"user","content":"ping"}]}` request under an **8s timeout**, and the first HTTP 200 wins. Results are cached in the auth store (`auth.json`) under provider `zai` → `detected_endpoint` keyed on the first 16 hex chars of `sha256(api_key)` — written with `set_active=false` so caching never flips the user's active provider. `build_client` runs this automatically for the `zai` profile when no explicit override exists.

## Defaults & limits

| Setting | Value | Source |
|---|---|---|
| Overall request timeout | 1800s | `JOEY_API_TIMEOUT` env override; upstream `HERMES_API_TIMEOUT` |
| Connect timeout | 10s | Fixed in `ProviderClient::new` |
| Stream per-chunk read timeout | 120s | `JOEY_STREAM_READ_TIMEOUT` env override |
| Copilot catalog cache TTL | 60s | `CATALOG_CACHE_TTL` |
| Copilot token refresh margin | 120s before `expires_at` | `REFRESH_MARGIN_SECS` |
| Retry-After cap | 600s | `parse_retry_after` |
| Backoff (default / API) | 5s–120s / 2s–60s | `jittered_backoff` / `jittered_backoff_api` |
| Z.AI probe timeout | 8s per endpoint | `detect_zai_endpoint(api_key, 8.0)` |
| Stream index cap | 1024 | `MAX_STREAM_INDEX` |
| Anthropic default output ceiling | 128000 | `ANTHROPIC_DEFAULT_OUTPUT_LIMIT` |

## Testing

The crate carries ~106 inline `#[cfg(test)]` tests (anthropic 23, client 28, chat 18, profile 17, copilot 13, zai 4, error 3) asserting exact wire JSON, error classification, and routing behavior:

- **Exact wire bodies**: per-provider reasoning shapes (OpenRouter verbosity-vs-reasoning, DeepSeek/Z.AI `thinking` + `reasoning_effort`, Gemini snake-cased `thinking_config`, copilot-wire Claude budgets, openai-api sending nothing), max-token kwarg switching, developer-role swap, Anthropic adaptive/legacy thinking contracts (4.6 `xhigh`→`max`, 4.7 sampling-param stripping, legacy `temperature=1` + `max_tokens ≥ budget+4096`), tool-result merging, prompt-cache breakpoints, beta-header composition, and the full `/responses` body shape (effort clamping against cold and seeded catalogs).
- **Error table + backoff bounds**: the status→variant classification matrix (including 400 bucket ordering and 429 overload-vs-ratelimit), backoff window assertions over 50 random draws per case, and `Retry-After` parsing/capping.
- **Endpoint resolution**: hostname/prefix/bare-family detection, alias maps, copilot/HUD magnetization (including the GLM-must-not-ride-the-proxy rule), Z.AI endpoint table + cache short-circuit + sha256 key-hash, and refusal of xai client construction.
- **Live-TCP SSE regressions**: real `TcpListener`-served SSE streams exercise the streaming parsers end-to-end — obfuscated `item_id` Responses streams (with and without `response.completed`), copilot-wire `reasoning_text` deltas and first-non-null precedence, plus tool-call accumulation edge cases (Ollama index reuse, name re-assignment, malformed deltas, hostile indices).

Run them scoped: `cargo test -p joey-providers`. Tests that mutate environment variables or the catalog cache serialize on a shared `TEST_ENV_LOCK` (with save/restore guards) so a developer's real exported `AI_USAGE_HUD_BASE_URL` can't leak into assertions.

## See also

- [../providers.md](../providers.md) — user-facing provider configuration docs
- [joey-core.md](joey-core.md) — branding, auth store (`auth.json`), reasoning-effort parsing (`VALID_EFFORTS`)
- [joey-agent-core.md](joey-agent-core.md) — the turn loop that consumes `is_retryable`/`should_compress`/`should_failover`
- [joey-cli.md](joey-cli.md), [joey-copilot.md](joey-copilot.md) — `joey auth copilot`, model picker, and Copilot setup flows
