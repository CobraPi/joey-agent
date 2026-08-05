# Research: Dynamic LLM Model Selector

**Feature**: 011-dynamic-llm-selector | **Phase**: 0 | **Date**: 2026-08-04

This document resolves every NEEDS CLARIFICATION and grounds every technical
decision in real code (file:line citations are to the current working tree).
All findings were produced by reading the actual source, not from assumptions.

---

## 1. The `auto` cost-scorer the spec assumes — DOES NOT EXIST in the Rust port

**This is the most consequential finding.** The spec (FR-007, User Story 5,
Assumptions line 187) repeatedly references "the existing provider `auto`
cost-routing", "the existing provider feasibility+cost scorer", and "fall back
to the existing cost-only `auto` routing". That scorer exists in **upstream
Python** (`agent/model_metadata.py`, `model_cost_guard.py`) but was
**deliberately NOT ported** to Joey:

- `crates/joey-cli/src/model_catalog.rs:7-11` comment: "Upstream's live probes
  … are deliberately not ported".
- `PORTING.md:392-400` confirms the cost/feasibility probes are deferred.
- The only `auto` handling anywhere is `resolve_profile`
  (`crates/joey-providers/src/profile.rs:313-315`), where `auto` is a
  **provider** sentinel ("auto-detect the provider from the model
  string/base_url"), NOT a model sentinel.
- The main turn reads `cfg.model()` (`crates/joey-core/src/config.rs:256`)
  literally and passes it straight to the wire —
  `Agent::build_request()` → `ProviderRequest::new(self.config.model.clone(), …)`
  at `crates/joey-agent-core/src/agent.rs:831-841`.
- The only model-level `auto` is in compression: `AuxSummaryBackend::from_config`
  (`crates/joey-agent-core/src/compression/summary.rs:180`) treats
  `auxiliary.compression.model == "auto"` as "inherit the main model" — a
  passthrough, not a cost pick.

**Decision**: Build a self-contained capability+cost scorer inside
`joey-llm-selector`. It does NOT layer on a scorer that doesn't exist. The
cold-start default (FR-007) uses this scorer directly. The disable path
(`/llm-selector` disabling dynamic allocation, or selecting a concrete model)
falls back to the **literal configured model** (`cfg.model()`) — which is the
correct, honest fallback since there is no cost-router to fall back to.

**Rationale**: A from-scratch scorer is unavoidable; pretending to "reuse" a
nonexistent one would be a fabrication. The scorer we build is small (capability
gates + a cost tie-break — see §6 of data-model.md) and is the single source of
truth for both cold-start and the diagnoser's baseline.

**Alternatives considered**:
- *Port upstream's `model_metadata.py`*: rejected — upstream is GitHub-Copilot
  -specific and tightly coupled to its Python runtime; a clean-room Rust scorer
  is leaner and multi-provider (Constitution VIII).
- *Fall back to `cfg.model()` for cold-start too*: rejected — it would mean the
  selector assigns the SAME model to every module on turn 1, defeating the
  paper's per-module diversity (SC-002) before the diagnoser has learned
  anything.

---

## 2. The "compound system" module inventory — only 2 real LLM call sites + subagent dispatch today

The spec (FR-004, Key Entities line 163) names seven auxiliary modules:
history compression, vision analysis, title generation, web extraction,
session search, curator review. Ground truth from the code:

| Spec-named module | Reality | Evidence |
|---|---|---|
| **Main reasoning turn** | REAL LLM call site | `agent.rs:831-841` `build_request()` → `client.complete()` at `agent.rs:881`. Model id = `AgentConfig.model` (`agent.rs:82`). **Intercept point #1.** |
| **History compression** | REAL LLM call site | `crates/joey-agent-core/src/compression/summary.rs:242-259` `AuxSummaryBackend::generate()` → `client.complete(&req)` at `summary.rs:257`. Model resolved in `from_config` (`summary.rs:169-237`). **Intercept point #2.** |
| **Subagent goal dispatch** | REAL model-resolution site | `crates/joey-orchestration/src/subagent.rs:19-30` `resolve_model()`: `task_model > req_model > default_model > parent_model`. Dispatched at `manager.rs:162`/`185`. **Intercept point #3** (resolve at the `model` string, `manager.rs:196-201`). |
| Vision analysis | NOT a separate LLM call | `vision_analyze` is a **built-in tool** (`crates/joey-tools/.../toolsets.rs:38,209`). Vision runs through the main turn. joey-tools has ZERO `ProviderRequest` usage. |
| Title generation | DOES NOT EXIST | No `title_gen`/`auto_title` anywhere. Titles are user-set via `/title` (`repl.rs:1148`). |
| Web extraction | NOT a separate LLM call | `web_extract` is a **tool** (`crates/joey-tools/.../web_tools.rs`). No LLM call. |
| Session search | NOT a separate LLM call | `session_search` is a **tool** (`session_search_tool.rs`). Uses SQLite FTS5, no LLM. |
| Curator review | DOES NOT EXIST | `/curator` is a slash-command **stub** (`slash.rs:95`). No LLM curator module. |

**Decision**: The `ModuleId` enum is seeded with the **three real intercept
points** (`MainTurn`, `Compression`, `Subagent`) plus a `Custom(String)`
variant so new call sites (vision-as-a-module, title gen, curator, future
side-LLMs) can be added **additively** without breaking the on-disk schema
(Constitution VII). The enum is `#[non_exhaustive]` where it appears in the
public trait surface.

**Rationale**: Enumerating non-existent modules would be fabrication; the
selector's real surface today is small and clean (3 single-string swaps). The
extensible enum means FR-004's "the module set can grow as new call sites are
added" is honored without a schema bump.

**Alternatives considered**:
- *Spin up the missing modules (title gen, curator) as part of this feature*:
  rejected — out of scope; the spec says modules are "the agent's current
  distinct LLM call sites", and those don't exist. Would violate Constitution V
  (reviewable increments) by bundling unrelated work.
- *Hardcode a fixed 7-module enum*: rejected — would fabricate 4 non-existent
  modules and require a schema bump when reality changes.

---

## 3. On-disk format for the global allocation map — single JSON, atomic write

**Decision**: `~/.joey/llm-selector/allocations.json`, a single versioned JSON
file written atomically via `joey_core::utils::atomic_json_write`
(`crates/joey-core/src/utils.rs:156`; tmp-in-dir + fsync + rename, symlink-safe,
EXDEV fallback). Resolved under `process_joey_home()`
(`crates/joey-core/src/constants.rs:135`) so the map is genuinely shared across
profiles (FR-014) — `process_joey_home()` deliberately ignores the per-profile
override, unlike `joey_home()`.

**Rationale**:
- `atomic_json_write` already exists and is the primitive `auth_store`
  (`crates/joey-core/src/auth_store.rs:134`) builds on — the spec's "match
  `auth_store`'s pattern" is satisfied by reusing the underlying primitive, no
  bespoke code.
- The map is small (one entry per module, ~3-10 entries) and read once per
  turn + rewritten occasionally. SQLite is unjustified weight (Constitution
  VIII): a new `rusqlite`-backed store for a flat map would add binary size +
  compile time for no measurable benefit over a single JSON read.
- Human-readable JSON aids `/llm-selector` debugging (FR-011, FR-018).
- The schema carries a `schema_version` field from day one, so it is a
  versioned public on-disk format (Constitution VII) — future breaking changes
  require a MAJOR bump + migration, but the initial format is new (nothing to
  break).

**Alternatives considered**:
- *SQLite table*: rejected (Constitution VIII — unjustified weight; the store
  is tiny and flat; atomic rename already gives crash safety).
- *TOML*: rejected — `serde_json` is already workspace-standard and JSON
  round-trips nested maps more naturally; TOML offers no advantage here.
- *Per-profile storage under `~/.joey/profiles/<n>/`*: rejected — violates
  FR-014's "global, shared across profiles" requirement and the spec's explicit
  "maximize compound-system improvement by transferring learning between
  profiles" rationale.

---

## 4. DAG / crate placement — acyclic, narrow trait

**Decision**: New crate `crates/joey-llm-selector` added to `[workspace]
members` (root `Cargo.toml:3-16`) between `joey-providers` and `joey-tools`,
mirroring its layer. It depends only on `joey-core` + `joey-providers`
(`[dependencies]` mirrors `crates/joey-mcp/Cargo.toml:7-27` shape).
`joey-agent-core/Cargo.toml:7-29` and `joey-cli/Cargo.toml` each gain
`joey-llm-selector.workspace = true`. `joey-agent-core` consumes ONLY the
`ModelAllocator` trait (the narrow public surface); `joey-cli` consumes the
query API for `/llm-selector`.

**DAG verification** (acyclic — no back-edges):
```
joey-core (leaf)
  └─ joey-providers
       └─ joey-llm-selector   ← NEW (same layer as a consumer of joey-providers)
            ↑ consumed by
       joey-tools
            └─ joey-agent-core  ← consumes only ModelAllocator trait
                 └─ joey-orchestration / joey-cli / joey-tui / joey-omo / joey-speckit-ui
```
`joey-llm-selector` touches no crate above its own layer. A change in its
engine internals never forces edits to `joey-agent-core` beyond the trait
boundary (Constitution VI).

**Alternatives considered**:
- *Put the selector inside `joey-agent-core`*: rejected — violates Constitution
  VI (threads new logic through shared core paths) and makes the engine
  untestable in isolation.
- *Put it in `joey-providers`*: rejected — `joey-providers` is the wire-protocol
  layer (chat completions, SSE, retries); allocation policy is a higher
  concern. Would also force every consumer of `joey-providers` to compile the
  selector.
- *Callback/threading model (agent-core calls back into selector)*: rejected —
  explicitly called out in the spec's clarification (Session 2026-08-03 Joey
  adaptation) as violating Constitution VI.

---

## 5. Dependency justification — NO new external dependency

**Decision**: `joey-llm-selector` introduces **zero** new external dependencies.
It uses only workspace-standard crates already in `[workspace.dependencies]`:
`joey-core`, `joey-providers`, `serde`, `serde_json`, `tokio`, `tracing`,
`anyhow`, `thiserror`. Per Constitution VIII, each is justified:

| Dep | Why needed | Already used by |
|---|---|---|
| `joey-core` | `process_joey_home()`, `Config` access, `atomic_json_write` | every crate |
| `joey-providers` | `ProviderProfile`, `ApiMode`, Copilot catalog fetch, backoff helpers, chat client for the diagnoser | joey-agent-core, joey-cli |
| `serde`/`serde_json` | (de)serialize the allocation map + catalog entries | every crate |
| `tokio` | detached `tokio::spawn` for the async diagnoser (FR-009) | joey-agent-core, joey-mcp |
| `tracing` | structured logs for fallback/reallocation events (FR-015, FR-018) | every crate |
| `anyhow`/`thiserror` | error types | every crate |

**Alternatives considered and rejected**:
- *`reqwest` for catalog fetch*: rejected — Copilot catalog fetch already uses
  `ureq` (`copilot.rs:518`); OpenRouter/models.dev fetch lives in `joey-cli`
  (`model_catalog.rs`). The selector reuses these existing paths via
  `joey-providers`, adding no HTTP client.
- *A scheduling/cron crate for the diagnoser*: rejected — `tokio::spawn` is
  sufficient for a detached best-effort task; `joey-cron` exists but is for
  user-facing scheduled jobs, not internal background optimization.
- *A traits/helper crate like `async-trait`*: the `ModelAllocator` trait is
  synchronous at the resolve site (the per-turn cache makes the hot path
  non-async); only the diagnoser is async and it lives inside the crate.

---

## 6. Capability metadata consolidation — typed CandidateModel, with a vision-detection gap

**Finding**: Model capability metadata today is scattered across three sources
and returns **untyped `serde_json::Value`**; there is no unified
per-model capability struct, and **vision support is not recorded anywhere**.

| Source | Location | Fields available |
|---|---|---|
| Copilot catalog | `copilot.rs:518` `fetch_model_catalog` → `Vec<Value>` | context window (`catalog_context_window` → `capabilities.limits.max_prompt_tokens`, `copilot.rs:602`); reasoning efforts (`copilot.rs:611`); tool/API mode inferred from `supported_endpoints` (`copilot.rs:584-594`, `model_api_mode` `copilot.rs:463`); type filter `capabilities.type == "chat"` (`copilot.rs:576-583`) |
| OpenRouter | `model_catalog.rs:617` `fetch_openrouter_models`; `model_catalog.rs:696` `fetch_models_with_pricing` | tool-calling from `supported_parameters` contains `"tools"` (`openrouter_model_supports_tools` `model_catalog.rs:587`); free-tier from `pricing.{prompt,completion}==0` (`model_catalog.rs:599`); cost from pricing |
| models.dev registry | `model_catalog.rs:281` (1h TTL, `MODELS_DEV_CACHE_TTL` `model_catalog.rs:273`) | `tool_call: bool` (`model_catalog.rs:435`); `limit.context` (`model_catalog.rs:470`); `cost.{input,output}` (`model_catalog.rs:449`) |
| Hardcoded | `crates/joey-agent-core/src/compression/catalog.rs:83` `DEFAULT_CONTEXT_LENGTHS` (substring match) | context length only |

**Decision**: Define a typed `CandidateModel` in `joey-llm-selector/src/candidate.rs`
that consolidates these sources, and a `CatalogConsolidator` that normalizes
provider-specific JSON into it. Fields: `id`, `provider`, `context_window`,
`supports_tools: bool`, `supports_vision: bool`, `tier: CapabilityTier`,
`cost: Option<Cost>`.

**Vision gap**: vision support is the one capability with no reliable signal.
**Decision**: derive `supports_vision` from a small curated id-prefix table
(e.g. `gpt-4o*`, `gpt-4.1*`, `claude-3-7*`, `gemini-*`, `grok-vision*`) PLUS
any provider capability hint when present (e.g. Copilot's
`capabilities.supports.vision` if/when exposed, OpenRouter
`architecture.input_modalities` contains `"image"`). The table lives in
`candidate.rs` and is additive — adding a prefix never breaks the schema.

**Rationale**: FR-005's hard capability gates (vision/tools/context window)
cannot be satisfied from existing metadata alone; the typed struct + table is
the minimum needed. Keeping it in one crate avoids scattering.

**Alternatives considered**:
- *Require the provider to expose a vision field and skip models that don't*:
  rejected — would silently drop capable models and under-fill the candidate
  pool (violates SC-005).
- *Skip vision gating entirely*: rejected — violates FR-005 (a vision turn must
  never be assigned a text-only model).

---

## 7. Diagnoser dispatch — detached tokio::spawn, failure-triggered only

**Decision**: The diagnoser runs as a detached `tokio::spawn` inside
`joey-llm-selector`, calling the provider chat client directly via
`joey-providers` (reusing auth/retry/backoff — no second implementation).
It is triggered ONLY by observable failure (FR-009): a turn error/exception,
an auxiliary call failure, an empty/null response, or a retry fired by the
existing retry loop at `crates/joey-agent-core/src/agent.rs:909`
`call_with_retries` (which emits `AgentEvent::RetryAttempt` at `agent.rs:996`
and classifies via `ProviderError::is_retryable()` at
`crates/joey-providers/src/error.rs:60`). Its result writes to the allocation
map atomically on completion. It is bounded by the learning budget and never
blocks the hot path.

**Integration point**: the agent-core side of the trait surface exposes a
`record_observation(...)` method that `call_with_retries` and the compression
backend call on the failure signals above; the implementation forwards to a
channel consumed by the detached diagnoser task. The main-turn model resolution
(`build_request`) calls only the synchronous `resolve(module)` — never the
diagnoser.

**Rationale** (per spec clarification, Session 2026-08-03 Joey adaptation):
keeps the diagnoser self-contained in the new crate, reuses the hardened
chat path, and detaches from the turn lifetime. A CLI subprocess would
duplicate the chat client and add IPC; an agent-core callback would thread
selector logic back through shared core paths (Constitution VI).

**Alternatives considered**:
- *Synchronous diagnoser on the hot path*: rejected — violates FR-009 and
  Constitution VIII (blocks interactive turns).
- *Run diagnoser on every turn*: rejected — the paper and the spec
  (clarification on "observable failure") scope it to failures; running on
  success would blow the budget and add latency for no gain.

---

## 8. Fallback / graceful degradation surface

**Finding**: two distinct "fallback" concepts exist today:
- **(a) Static `ProviderProfile.fallback_models`**
  (`crates/joey-providers/src/profile.rs:71-73`, `&'static [&'static str]`)
  — curated per-provider display lists for the picker. This is what FR-015's
  "provider's curated `fallback_models` list" refers to.
- **(b) Runtime fallback chain**
  (`crates/joey-agent-core/src/agent.rs:146-153` `FallbackEntry`, walked by
  `try_activate_fallback` at `agent.rs:682-702`, invoked from
  `call_with_retries` at `agent.rs:956/966`) — config-driven
  (`model.fallback_providers`).

**Decision**: The selector is a smarter layer invoked at **resolve time**
(before any call), not a replacement for (b)'s runtime failover. On catalog
unreachable / model removed, the selector: (1) falls back to the last-known-good
allocation from the map, (2) if that's also stale, to the active profile's
`fallback_models` (a), (3) if none, to `cfg.model()`. The existing runtime
fallback chain (b) remains intact and runs if a selected model still errors at
call time — defense in depth.

**Rationale**: Two independent failure surfaces (resolve-time vs call-time)
shouldn't be collapsed; keeping both maximizes robustness (SC-007) without
duplicating logic.

---

## 9. `/llm-selector` command wiring — slash + CLI parity

**Finding**: Slash commands are a `static REGISTRY: &[CommandDef]` built via a
`cmd!` macro at `crates/joey-cli/src/slash.rs:34-117` (the `/model` entry is
`slash.rs:69`); dispatch is a `match def.name` arm at `repl.rs:825`
(`"model" => model_slash(st, args)`). Prefix abbreviation (`resolve()` at
`slash.rs:153`) handles `/llm-s` automatically.

**Decision**: Add one `cmd!("llm-selector", ...)` line to `REGISTRY` (near
`slash.rs:69`) and one `"llm-selector" => llm_selector_slash(st, args)` arm at
`repl.rs:825-826`; the handler lives in a new
`crates/joey-cli/src/commands/llm_selector.rs`. All capabilities (inspect
state/pool/allocations/diagnostics, pin/unpin, set budget, enable/disable) are
text in/out — satisfying Constitution II (CLI/TUI parity) with no UI-only
affordance.

**Alternatives considered**:
- *A `joey llm-selector` subcommand instead of a slash command*: rejected —
  the spec (FR-001) requires a slash command with CLI reachability; we provide
  both by making the handler callable from the CLI text surface too.

---

## Open questions resolved

All spec NEEDS CLARIFICATION items were resolved during `/speckit-clarify`
(see spec.md Clarifications). No new NEEDS CLARIFICATION remain after this
research phase. The two scope corrections above (no pre-existing scorer; only
2 real LLM modules + subagent dispatch) are facts about the codebase, not
ambiguities — they are reflected in the plan's Technical Context and the
data-model/contracts.

---

## 10. Performance validation results (T056, Constitution VIII)

The plan sets explicit performance budgets for the two hot paths. Measured on
a debug build with a realistic 24-model pool (spanning all four tiers) and a
populated per-turn cache, using self-contained `std::time::Instant` benchmarks
(no criterion dependency — Constitution VIII):

| Path | Budget | Measured | Headroom | Verdict |
|---|---|---|---|---|
| `resolve` (cache hit) | < 50µs | **0.460µs/call** (10 000 calls) | ~108× | PASS |
| `refresh_at_turn_start` | < 1ms | **290.582µs/call** (1000 calls) | ~3.4× | PASS |
| diagnoser never blocks `resolve` | (asserted) | the diagnoser runs in a detached `tokio::spawn` task; `resolve` never awaits it (asserted by `test_record_observation_noop_without_diagnoser`) | — | PASS |

Both hot paths land well within budget. `resolve` is a HashMap lookup behind a
Mutex lock (O(1), sub-microsecond). `refresh_at_turn_start` is dominated by the
single atomic file read + JSON deserialize + cache rebuild; the ~290µs is
comfortably under the 1ms ceiling and is one-shot per turn (never on the
per-call hot path). Release builds are faster still.

Benchmark source: `test_perf_resolve_within_50us_from_cache` and
`test_perf_refresh_at_turn_start_within_1ms` in
`crates/joey-llm-selector/src/allocator.rs` (run with `--nocapture` to see
numbers). The assertions gate against order-of-magnitude regressions.
