# Joey Agent — Feature Gap Analysis

Comparative analysis of crush, hermes-agent, and pi against joey-agent's current state.
Generated from source-level investigation of all four codebases.

---

## CURRENT JOEY-AGENT CAPABILITIES (baseline)

### Core
- Agent turn loop with tool calling, retries, jittered backoff, fallback provider chains
- Context compression (8 modules: anchors, breakdown, catalog, compressor, engine, estimator, feedback, orchestrator, summary)
- Iteration budget with summary finalizer
- Untrusted tool result wrapping
- Parallel-safe tool detection
- Threat scanning (threat_scan.rs)

### Tools (12 built-in)
read_file, write_file, patch, search_files, terminal, todo, memory, web_search, web_extract, skills_list, skill_view, process

### Infrastructure
- Session store (SQLite, FTS5 search)
- MCP client (stdio transport)
- Cron scheduler
- Gateway (base + WhatsApp identity)
- Orchestration (subagent spawning, delegation tool)
- TUI (ratatui: anim, theme, state, input, widgets, app)
- Provider layer (OpenAI + Anthropic compatible, streaming)
- Checkpoints/rollback
- Slash commands (60+ registered, ~25 implemented)
- Clarify tool (ask user questions)
- joey-omo (OMO agent prompt system)

---

## GAPS BY SOURCE CODEBASE

### FROM CRUSH (charmbracelet/crush — Go)

| Feature | Impact | Effort | Description |
|---------|--------|--------|-------------|
| **LSP Integration** | CRITICAL | High | Full Language Server Protocol: diagnostics, go-to-definition, references, call hierarchy, document symbols, rename, replace_symbol. Auto-starts LSP servers per file type via config. Uses powernap library. This is THE differentiator for coding agents. |
| **File Change Tracking** | HIGH | Medium | Tracks file reads per session in SQLite. Records when files were read, enables diff display and "what changed" views. |
| **Diff Detection** | HIGH | Low | Detects unified diffs in tool output (scans for @@ hunk markers, ---/+++ headers, diff --git). Enables inline diff rendering. |
| **Diff Generation** | HIGH | Low | Generates unified diffs between before/after file content using go-udiff. |
| **Loop Detection** | HIGH | Low | SHA-256 signature of tool name + input + output. Window of 10 steps, max 5 repeats. Prevents agent from getting stuck repeating the same tool calls. |
| **Hooks System** | HIGH | Medium | PreToolUse hooks — user-defined shell commands that fire before tool execution. Can allow, deny, halt (stop whole turn), or rewrite tool input. Regex matchers per tool name. Exit codes: 2=block tool, 49=halt turn. |
| **Local Model Discovery** | MEDIUM | Medium | Auto-discovers local LLM servers: ollama (11434), lmstudio (1234), llamacpp, litellm, omlx. Enriches model catalog with local options. |
| **Session Auto-Summarization** | MEDIUM | Medium | Threshold-based: large context (>200k) gets 20k buffer, small context uses 20% ratio. Auto-generates titles. |
| **Multi-Client Workspace** | MEDIUM | High | Client/server architecture — multiple clients can connect to a backend server, share sessions. |
| **Completions/Autocomplete** | MEDIUM | Medium | Slash command autocomplete, file path completion in TUI. |
| **Sourcegraph Integration** | LOW | Low | Optional Sourcegraph tool for code search. |

### FROM HERMES-AGENT (Nous Research — Python)

| Feature | Impact | Effort | Description |
|---------|--------|--------|-------------|
| **Autonomous Skill Creation (Curator)** | HIGH | Medium | Background skill maintenance orchestrator. Reviews agent-created skills, auto-transitions lifecycle states (draft→active→pinned→archived), consolidates related skills. Inactivity-triggered (no cron daemon). |
| **Learning Graph** | MEDIUM | Medium | "Learning made visible" — skill/memory relationship graph. Derives connections between learned skills and memory chunks. |
| **Multi-backend Memory** | MEDIUM | High | Pluggable memory backends: Honcho (dialectic user modeling), holographic (vector store), hindsight, retaindb, openviking. Query rewrite. |
| **Provider Adapters** | MEDIUM | Medium | Native adapters for Bedrock, Azure Identity, Gemini native, Codex runtime. |
| **Prompt Caching** | MEDIUM | Low | Anthropic cache_control headers for prompt caching (cost reduction). |
| **Error Classifier** | MEDIUM | Medium | FailoverReason classification: rate_limit, auth, auth_permanent, billing, connection, format, parse, model_not_found. Drives retry/fallback decisions. |
| **Trajectory Compression** | LOW | Medium | Compresses agent trajectories for training data generation. |
| **ACP Adapter** | LOW | Medium | Agent Communication Protocol — inter-agent messaging. |
| **Billing/Usage Tracking** | LOW | Medium | Credits tracker, account usage, billing view. |
| **Image/Video Generation** | MEDIUM | Medium | FAL-based image + video generation tools. |
| **Browser Automation** | HIGH | High | Camofox (stealth browser), CDP, dialog handling, browser supervisor. |
| **Voice Mode** | MEDIUM | High | TTS (NeuTTS), voice memo transcription, voice mode toggle. |
| **Kanban Multi-Agent** | MEDIUM | High | Task board for multi-profile coordination: tasks, links, comments, blocking. |

### FROM PI (earendil-works — TypeScript)

| Feature | Impact | Effort | Description |
|---------|--------|--------|-------------|
| **Differential Rendering TUI** | HIGH | High | Custom differential rendering engine — only redraws changed cells. Kitty keyboard protocol, terminal image support (Kitty graphics), terminal color detection, IME cursor positioning. |
| **Session Tree/Branching** | HIGH | Medium | Tree-structured sessions with branching. Navigate between branches. Branch summarization generates context summaries when switching branches. |
| **Project Trust Model** | HIGH | Low | Security model: prompts user before loading project resources (.pi/ settings, extensions). Prevents malicious project files from auto-executing. |
| **Extensions System** | HIGH | Medium | Self-extensible: .pi/extensions/ directory for custom tool definitions. Extensions can hook into project trust, system prompts, UI. |
| **RPC/Server Mode** | MEDIUM | Medium | JSONL RPC protocol, IPC, supervisor process. Enables editor integrations (VSCode, etc.) and multi-process architectures. |
| **Parallel Tool Execution** | MEDIUM | Low | Configurable: "sequential" or "parallel" tool execution within a single assistant message. |
| **beforeToolCall/afterToolCall** | MEDIUM | Low | Tool call interception hooks: block, rewrite results, terminate early. |
| **File Operation Tracking in Compaction** | MEDIUM | Low | Tracks read/modified files through compaction. Preserves file lists in compaction summaries. |
| **Queue Mode** | MEDIUM | Low | "all" or "one-at-a-time" message queue draining. |
| **Provider Composer** | MEDIUM | Medium | Compose multiple providers, provider attribution. |
| **30+ Provider Integrations** | MEDIUM | High | Massive provider catalog: OpenAI, Anthropic, Google, Bedrock, Azure, DeepSeek, Groq, Cerebras, Fireworks, Together, Mistral, xAI, Kimi, MiniMax, Moonshot, NVIDIA, Qwen, Xiaomi, ZAI, Cloudflare, Vercel, HuggingFace, GitHub Copilot, OpenRouter, etc. |

---

## IMPLEMENTATION PRIORITY (ranked by impact × achievability)

### TIER 1 — Core Capability Multipliers (implement now)
1. **Loop Detection** (crush) — Low effort, prevents stuck agents, immediate UX win
2. **File Change Tracking + Diff** (crush) — Track what changed, show diffs
3. **LSP Integration** (crush) — THE coding agent feature: diagnostics, go-to-def, references
4. **Hooks System** (crush) — PreToolUse safety hooks
5. **Local Model Discovery** (crush) — Auto-detect ollama/lmstudio/llamacpp

### TIER 2 — UX Multipliers
6. **Project Trust Model** (pi) — Security prompt for untrusted projects
7. **Parallel Tool Execution Mode** (pi) — Configurable sequential/parallel
8. **beforeToolCall/afterToolCall hooks** (pi) — Tool interception
9. **Session Branching** (pi) — Tree-structured sessions
10. **Autocomplete in TUI** (crush) — Slash commands + file paths

### TIER 3 — Self-Improvement & Advanced
11. **Autonomous Skill Curator** (hermes) — Background skill maintenance
12. **Prompt Caching** (hermes) — Anthropic cache_control
13. **Extensions System** (pi) — Custom tool definitions
14. **RPC Mode** (pi) — Editor integrations
15. **Error Classifier** (hermes) — Better retry/fallback decisions
