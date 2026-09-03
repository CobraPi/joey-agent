# Joey Agent — Feature Reference (per-crate)

This is the companion to the top-level docs: one deep-dive page per
workspace crate, generated from a full source scan (September 2026). Each
page covers its crate's public APIs, behaviors, configuration, defaults,
security, and testing. For workflow-level guides see the top-level pages;
for exhaustive per-crate detail, read these.

| Page | Crate | What it covers |
|---|---|---|
| [joey-core.md](joey-core.md) | joey-core | branding, home & profiles, layered config, SQLite session store (schema 22), logging, secret redaction, theming, full default-config reference |
| [joey-providers.md](joey-providers.md) | joey-providers | 10 provider profiles, Chat Completions / Anthropic Messages / Responses wire protocols, SSE streaming, error taxonomy & retry/backoff, Copilot auth, Z.AI probing |
| [joey-tools.md](joey-tools.md) | joey-tools | Tool trait & registry, all toolsets, every built-in tool with full parameter tables, schema sanitizer, fuzzy patch matcher, security layers, limits |
| [joey-agent-core.md](joey-agent-core.md) | joey-agent-core | the turn loop, tool dispatch (parallel/sequential), system-prompt assembly, verbatim guidance strings, context compression, loop detection, hooks, threat scan |
| [joey-llm-selector.md](joey-llm-selector.md) | joey-llm-selector | dynamic LLM model selection (feature 011): diagnosis, budget, allocator integration, `model.selector.*` config |
| [joey-cron.md](joey-cron.md) | joey-cron | schedule grammar (durations/cron/ISO), croniter semantics, jobs.json format, ticker & at-most-once delivery, output retention |
| [joey-mcp.md](joey-mcp.md) | joey-mcp | MCP server config & merging, safe env, JSON-RPC lifecycle, pagination, tool namespacing, schema normalization, security validation |
| [joey-gateway.md](joey-gateway.md) | joey-gateway | session-key grammar, SessionSource, MessageEvent, SendResult & error classification, PlatformAdapter trait & capabilities |
| [joey-cli.md](joey-cli.md) | joey-cli | full clap command tree & exit codes, profiles, REPL & slash-command catalog, TUI selection, agent wiring |
| [joey-tui.md](joey-tui.md) | joey-tui | layout & panels, subagent rail/panes, overlays, keybindings, NeuroCode explorer, rendering states |
| [joey-orchestration.md](joey-orchestration.md) | joey-orchestration | SubagentManager & config, child lifecycle, concurrency & grant-back, dispatch API, plan→worktree→evaluate→join pipeline, teams |
| [joey-omo.md](joey-omo.md) | joey-omo | 11-agent roster, categories & model resolution, intent gating, goals, plan parsing & start-work, wisdom/notepad, team mode |
| [joey-speckit-ui.md](joey-speckit-ui.md) | joey-speckit-ui | spec-kit artifact model, CST & meaning layers, patch engine, workflow runner, REST/WS API |
| [joey-browser.md](joey-browser.md) | joey-browser | CDP driver, attach vs managed launch, element refs & actions, snapshots/overlays/SoM vision, URL safety |
| [joey-neurocode.md](joey-neurocode.md) | joey-neurocode | code graph store & schema, ingest & grammars, tiers/classifier, context assembly, Pega support, verify loop, auto-index |
| [joey-neurocode-rag.md](joey-neurocode-rag.md) | joey-neurocode-rag | 18 config keys, 5 embedding backends & profiles, consent model, hybrid RRF search, vector quantization |
| [joey-copilot.md](joey-copilot.md) | joey-copilot | .github Copilot bundle discovery (instructions/prompts/skills/mcp), plugin manifest |
| [skills.md](skills.md) | repo skills/ | the skills system (SKILL.md format, dirs, tooling) and the bundled skill catalog |

## Reading order

Suggested order: joey-core → joey-providers → joey-tools →
joey-agent-core → then the specialty pages as needed.

## Conventions

- Values are verbatim from source (backticked).
- `~/.joey` means the JOEY_HOME-resolved home.
- Upstream = Hermes Agent Python original.
