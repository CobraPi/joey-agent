# Providers & Model Routing (`joey-providers`, `joey-llm-selector`)

`joey-providers` maps a provider-neutral `ProviderRequest` onto a concrete
wire protocol (OpenAI Chat Completions, Anthropic Messages, or OpenAI
Responses), with SSE streaming, and normalizes every result into a
`NormalizedResponse`. `joey-llm-selector` is the dynamic per-module model
allocator that engages when the configured model is `auto`. This doc covers
both, plus how `model.provider` / `model.base_url` / API keys resolve.

## 1. Provider registry (`profile.rs`)

Every provider is a declarative `ProviderProfile`: canonical name, aliases,
`ApiMode` (wire protocol), default base URL, env vars holding the API key
(in priority order), optional `<X>_BASE_URL` override var, auth type,
default aux model, display metadata (label/TUI description/signup URL), and
curated `fallback_models` (agentic tool-calling models used when a live
catalog fetch fails).

Wire protocols (`ApiMode`):
- `ChatCompletions` — OpenAI `/chat/completions` (default)
- `AnthropicMessages` — Anthropic `/v1/messages`
- `CodexResponses` — OpenAI `/responses` (implemented for Copilot-wire only)

Registered providers:

| Name | Aliases | Wire | Base URL | Key env vars (priority order) | Base URL env var | Notes |
|---|---|---|---|---|---|---|
| `openrouter` | `or` | chat | `https://openrouter.ai/api/v1` | `OPENROUTER_API_KEY` | — | Aggregator; app-wide default; keeps full `vendor/model` slug; sends `HTTP-Referer`/`X-Title`/`X-OpenRouter-Categories` headers; adds `x-anthropic-beta: fine-grained-tool-streaming-2025-05-14` for Claude models |
| `anthropic` | `claude`, `claude-oauth`, `claude-code` | anthropic | `https://api.anthropic.com` | `ANTHROPIC_API_KEY`, `ANTHROPIC_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN` | `ANTHROPIC_BASE_URL` | x-api-key vs Bearer chosen by token shape; aux model `claude-haiku-4-5-20251001` |
| `openai-api` | `openai` | chat | `https://api.openai.com/v1` | `OPENAI_API_KEY` | `OPENAI_BASE_URL` | never sends `tool_choice` or reasoning params |
| `copilot` | `github-copilot`, `github-models`, `github-model`, `github` | chat/responses/anthropic (per model) | `https://api.githubcopilot.com` | `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN` | `COPILOT_API_BASE_URL` | GitHub device-code OAuth + token exchange; Copilot editor headers; model-id alias normalization; GPT-5+ → Responses wire; `/v1/messages`-only models → Anthropic wire (see §6) |
| `ai-usage-hud` | `usage-hud`, `ai-usage` | chat (Copilot wire) | `http://127.0.0.1:8317` | same as copilot | `AI_USAGE_HUD_BASE_URL` | joey-specific: local Copilot-compatible reverse proxy with usage capture; skipping exchange flow, raw GitHub credential accepted. Setting `AI_USAGE_HUD_BASE_URL` (off-host) magnetizes ALL `auto` resolution through the proxy |
| `nous` | `nous-portal`, `nousresearch` | chat | `https://inference-api.nousresearch.com/v1` | `NOUS_API_KEY` | — | full `reasoning_config` dict; fallbacks `hermes-3-405b`/`hermes-3-70b` |
| `deepseek` | `deepseek-chat` | chat | `https://api.deepseek.com/v1` | `DEEPSEEK_API_KEY` | `DEEPSEEK_BASE_URL` | `extra_body.thinking` + `reasoning_effort` for R1-class; aux `deepseek-chat` |
| `gemini` | `google`, `google-gemini`, `google-ai-studio` | chat | `https://generativelanguage.googleapis.com/v1beta/openai` | `GOOGLE_API_KEY`, `GEMINI_API_KEY` | `GEMINI_BASE_URL` | uses Google's OpenAI-compat shim (native REST adapter unported); aux `gemini-3.5-flash` |
| `zai` | `glm`, `z-ai`, `z.ai`, `zhipu` | chat | `https://api.z.ai/api/paas/v4` | `GLM_API_KEY`, `ZAI_API_KEY`, `Z_AI_API_KEY` | `GLM_BASE_URL` | 4-endpoint probing (see §5); aux `glm-4.5-flash`; fallbacks glm-5.2/glm-5/glm-4-9b |
| `xai` | `grok`, `x-ai`, `x.ai` | codex_responses | `https://api.x.ai/v1` | `XAI_API_KEY` | `XAI_BASE_URL` | **the codex_responses wire is NOT ported for xai** — constructing a client for it returns an error |

Notes:
- "copilot wire" (`is_copilot_wire`) = `copilot` + `ai-usage-hud`: Copilot
  headers, credential resolution, model normalization, Responses routing,
  401 auth-refresh retry.
- Custom/base_url: any OpenAI-compatible endpoint works by leaving the
  provider setting or pointing base_url at it; unrecognized hosts fall
  through to the `openrouter` profile with the custom base URL as override.
- `ollama` and `groq` are NOT registered (tests pin this) — an Ollama
  endpoint is used via generic base_url override on the chat wire.

## 2. Resolution: `model.provider` / `base_url` / `api_key`

`resolve_profile(provider_setting, base_url, model)` order:
1. Explicit non-empty, non-`auto` provider setting (name or alias) wins.
2. `AI_USAGE_HUD_BASE_URL` or `COPILOT_API_BASE_URL` pointing off
   githubcopilot.com → magnetize: everything resolves to that copilot-wire
   profile so no request escapes the proxy (explicit provider still wins).
3. base_url hostname detection: openrouter.ai / api.anthropic.com /
   api.openai.com / *.githubcopilot.com / nousresearch.com / x.ai / z.ai /
   deepseek.com / googleapis.com.
4. `vendor/model` prefix detection (`anthropic/…`, `google/…`, etc.).
5. Bare model-family detection: `glm-*`→zai, `claude-*`→anthropic,
   `gpt-*`→openai-api, `gemini-*`→gemini, `deepseek-*`→deepseek,
   `grok-*`→xai (matters for OMO switching with bare model IDs).
6. Fallback: `openrouter` (aggregator default).

Base-URL override (`resolve_base_override`): `<ID>_BASE_URL` env var first;
then a caller-supplied `model.base_url` that differs from the profile
default. The app-wide default aggregator URL
(`https://openrouter.ai/api/v1`) never overrides a non-OpenRouter
provider's native endpoint.

API key: explicit argument → profile env vars in priority order. For
copilot-wire, the key is validated/validated and run through the GitHub
token exchange; a non-githubcopilot.com pinned endpoint skips the exchange.

`wire_model_name`: OpenRouter keeps the full slug; copilot-wire normalizes
aliases (e.g. `openai/o3`→`gpt-5.3-codex`); Anthropic wire applies
`normalize_model_name` (strip `anthropic/`, dots→hyphens for claude);
others strip a known vendor prefix.

## 3. SSE streaming behavior

All streaming is SSE parsed by hand over `respwest::bytes_stream()`:
- Stream request: `"stream": true`; chat wire adds
  `stream_options.include_usage` (omitted only for native Gemini
  endpoints). Copilot Responses wire streams typed events
  (`response.output_text.delta`, `response.reasoning_summary_text.delta`,
  `response.function_call_arguments.delta`,
  `response.output_item.added`→function_call, `response.completed`).
- Deltas are emitted to the caller as `StreamEvent::ContentDelta` /
  `ReasoningDelta`, and the final assembled `NormalizedResponse` is both
  returned and emitted as `StreamEvent::Done`.
- Per-read stall timeout: 120 s (`DEFAULT_STREAM_READ_TIMEOUT_SECS`,
  `HERMES_STREAM_READ_TIMEOUT` upstream) — a stalled stream becomes
  `ProviderError::Timeout`. Overall request timeout 1800 s, connect 10 s.
- Tool-call deltas are accumulated by index (Chat wire) or item_id
  (Responses wire); Ollama-style index reuse is handled.
- Zero-event guard: a stream that yields nothing usable is
  `EmptyStream` (retryable), not an empty completion.
- Partial-stream handling: tool-call args that don't parse as JSON with no
  finish_reason → finish becomes `Length` so the loop retries instead of
  executing a truncated call; text-only drops likewise become `Length`.
- Anthropic stream: content blocks (text / tool_use with
  `input_json_delta` / thinking / redacted_thinking) accumulated by index;
  usage from `message_delta`; interleaved signed thinking + tool_use is
  captured verbatim (`anthropic_content_blocks`) for signature-safe replay.

## 4. Retries, backoff, error classification (`error.rs`)

`ProviderError` classification (`from_status`, port of upstream
`error_classifier`), with body-pattern tables (billing, rate-limit,
overload, usage-limit, context-overflow — including Chinese-language
patterns — model-not-found, request-validation, OpenRouter policy blocks,
empty-provider-response):

- 401→Auth; 403→Billing only for "key limit exceeded"/spending-limit/billing
  patterns, else Auth; 402→RateLimit if usage-limit text has transient
  signals ("try again", "resets at", …) else Billing; 404→Billing/policy/
  ModelNotFound patterns, else retryable `Status{404}`; 408→Timeout;
  413→PayloadTooLarge; 429→Overloaded if overload text (Z.AI-style; no
  key rotation) else RateLimit (carries `retry_after`); 400 buckets in
  order: validation (excluding bare `invalid_request_error`) →
  empty-response → context-overflow → policy → model-not-found →
  rate-limit → billing → FormatError; 500/502→validation→FormatError else
  ServerError (context-overflow patterns recognized); 503/529→Overloaded;
  other 4xx→FormatError, other 5xx→ServerError.

Decisions:
- `is_retryable`: RateLimit, Overloaded, PayloadTooLarge, ContextOverflow,
  Timeout, Connection, EmptyStream, ServerError, generic 404/5xx.
- `should_compress`: PayloadTooLarge + ContextOverflow (compress then retry).
- `should_failover`: Auth, Billing, ModelNotFound, FormatError, RateLimit.

Backoff (upstream `retry_utils`): `min(base·2^(attempt-1), max) +
U[0, 0.5·delay)` with real RNG. Conversation loop uses
`jittered_backoff_api` (base 2 s, max 60 s); default variant base 5 s,
max 120 s. `Retry-After` header parsed as float seconds, capped at 600 s.
Copilot-wire requests additionally retry once after a 401 by re-exchanging
the Copilot API token (`send_with_auth_refresh`).

## 5. Z.AI endpoint probing (`zai.rs`)

Four official endpoints probed in order: global, cn
(open.bigmodel.cn), coding-global, coding-cn (probe models glm-5 /
glm-5.2, glm-5.1, glm-5v-turbo, glm-4.7) — separate billing per plan/region
means a key may only work on one. On client construction with no explicit
`GLM_BASE_URL` override and no cached detection, joey probes each endpoint
(8 s timeout, 1-token ping), then caches the winner in auth.json provider
state keyed by sha256(api_key)[..16]. Empty key skips probing entirely.

## 6. Reasoning / thinking support

Chat wire (`ReasoningEffort`: `Disabled` or `Level(string)`):
- OpenRouter: allowlisted models only. Mandatory-reasoning Claude models
  send no `reasoning` field and route effort onto `verbosity`; others send
  `extra_body.reasoning {enabled, effort}`; Disabled sends `{enabled:false}`.
- Nous: full `reasoning_config` dict, omitted when disabled.
- DeepSeek: `extra_body.thinking {type: enabled|disabled}` for
  thinking-capable families; `reasoning_effort` ("max" for high).
- Z.AI: `extra_body.thinking` for glm-4.5+; GLM-5.2 adds native
  `reasoning_effort` (high/max only — low clamps up).
- Gemini shim: `extra_body.extra_body.google.thinking_config`
  (`include_thoughts`, `thinking_level`/`thinking_budget`).
- OpenAI direct: sends nothing reasoning-related.

Anthropic wire: legacy Claude 3.x/4.0/4.5 use manual
`thinking.budget_tokens` by effort (xhigh 32000, high 16000, medium 8000,
low 4000); Claude 4.6+ / unknown Claude / Kimi-family use adaptive
`output_config.effort` (xhigh only on 4.7+; 4.6 caps at high).
Beta headers: `interleaved-thinking-2025-05-14` +
`fine-grained-tool-streaming-2025-05-14` always; `context-1m-2025-08-07`
on Azure endpoints; MiniMax-anthropic endpoints drop tool-streaming/1M.
Signed thinking/redacted_thinking blocks are replayed on subsequent turns
(preserved order when interleaved with tool_use). The OAuth client-identity
spoofing layer from upstream is deliberately NOT ported.

Copilot wire: `model_api_mode` routes GPT-5+ (except gpt-5-mini) to the
Responses wire, which maps reasoning to `reasoning: {effort}`; catalog
entries exposing only `/v1/messages` route to the Anthropic wire.

## 7. Tool-call wire formats

- Neutral: `ToolSchema` (OpenAI function shape, optional Anthropic
  `cache_control`) in, `ToolCall {id, function{name, arguments-as-JSON-string}}` out.
- Chat wire: OpenAI `tools`/`tool_calls` deltas (args accumulated by index);
  never sends `tool_choice` on the openai-api wire.
- Anthropic wire: `tools` → `{name, description, input_schema, [cache_control]}`;
  tool_use blocks with `input_json_delta` streaming; tool results converted
  to `tool_result` blocks merged into a single user message; consecutive
  tool messages coalesced; tool ids sanitized.
- Responses wire (Copilot): tools as `{type:"function", name, description,
  parameters, strict:false}` + `tool_choice:"auto"` +
  `parallel_tool_calls:true`; history as `function_call` /
  `function_call_output` items; args accumulated by item_id.

## 8. `joey-llm-selector` — dynamic model allocation

Engages when `model = "auto"` (activation sentinel) or
`model.selector.enabled = true` AND the active provider exposes a live
model catalog. Treats the agent as a compound system; each LLM call site
is a `ModuleId`: `main_turn`, `compression`, `subagent`, extensible via
`custom:<name>` (regex `^[a-z][a-z0-9_]{0,31}$`).

Mechanics:
- Allocation map persisted at `~/.joey/llm-selector/allocations.json`
  (entries, pins, diagnostics, learning budget, diagnoser model).
- Candidate pool: Copilot-wire providers use the Copilot `/models` catalog
  (60 s in-process cache, chat-type filter, context window from
  `max_context_window_tokens`→`max_prompt_tokens`→table); all other
  providers use the models.dev registry. Generic OpenAI-compat `/models`
  probes are also supported. Empty pool → auto-disable with notice.
- Tiers (`CapabilityTier`, from model id): Frontier (gpt-5, claude-opus-4,
  gemini-2.5-pro, grok-4, o3/o4), Flash (haiku/flash/mini/nano/micro),
  Versatile (gpt-4.1/4o/4.5, claude-sonnet-4, gemini-2.5, glm-4.6/5, …),
  Standard (rest). Used for cost tie-breaks.
- Cold-start scoring (`ColdStartScorer`): hard capability gates first
  (tools, vision-if-images, min context window), then cheapest capable
  (tier cost weight, then $/Mtok when known).
- Learning loop (detached `tokio` task, never blocks the turn): the agent
  forwards observations (`FailureSignal`: TurnError, AuxCallFailure,
  EmptyResponse, RetryTriggered); an LLM judge (diagnoser model on the
  active provider) scores outputs p∈[0,1], falling back to a signal-driven
  heuristic; reallocation happens within the learning budget
  (default 8/cycle).
- `resolve()` is O(1) off a per-turn cache, honors pinned/implicit pins,
  re-resolves stale ids, and on dead models walks provider
  `fallback_models` (DegradedFallback) before the literal configured model.
  `report_permanent_error` prevents re-resolving to a dead model.
- Control surface: `/llm-selector status|pool|allocations|diagnostics|pin|
  unpin|budget|diagnoser|enable|disable|refresh|help`. Config:
  `model.selector.enabled`, `model.selector.budget`,
  `model.selector.diagnoser_model` (versatile-tier only).

NeuroCode (`joey-neurocode`, wired via `joey-cli`): separate subsystem, off
by default (`neurocode.enabled`); has its own frontier/economical tier
models (`neurocode.tier.providers.<id>`), resolved through the same
provider profile machinery — not part of llm-selector's allocation map.
