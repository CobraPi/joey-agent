# AGENTS.md

Guidance for AI agents (and humans) working in this repository.

## What this project is

Joey Agent is a from-scratch **Rust rewrite** of Hermes Agent (Nous
Research, Python, MIT-licensed). It shares no code with upstream but
deliberately replicates its data formats, defaults, and wire behavior
byte-for-byte where possible (SQLite schema, cron `jobs.json`, `SKILL.md`
format, session-key grammar, provider payloads, even prompt wording). See
`PORTING.md` for the exhaustive parity tracker and `docs/README.md` for the
full documentation index — **read those before making non-trivial changes**,
they are kept accurate and are the primary source of truth for subsystem
design (turn loop, context compression, system prompt assembly, security
model, provider wire protocols, etc).

## Essential commands

```bash
cargo build --workspace              # build everything
cargo build -p <crate>               # build one crate (e.g. joey-cli)
cargo test --workspace               # run all tests (workspace currently ~520+ tests, must stay green)
cargo test -p <crate>                # run one crate's tests
cargo run -p joey-cli -- <args>      # run the joey CLI from source
cargo build --release                # release binary -> target/release/joey
```

There is no CI config in-repo (`.github/` absent) and no lint/format
config committed — run `cargo build --workspace` and `cargo test
--workspace` as the acceptance bar (this is mandated by
`.specify/memory/constitution.md`, not just convention).

## Workspace layout

Cargo workspace, 12 member crates under `crates/`, dependency graph is a
strict DAG (lower crates never depend on higher ones):

| Crate | Role |
|---|---|
| `joey-core` | branding/paths (`~/.joey`), layered YAML+env config, SQLite session store, logging, secret redaction |
| `joey-providers` | LLM provider wire protocols (OpenAI Chat Completions, OpenAI Responses, Anthropic Messages), SSE streaming, retries/backoff, error classification |
| `joey-tools` | `Tool` trait + registry, toolsets, JSON-schema sanitizer, fuzzy patch matcher, built-in tools |
| `joey-agent-core` | the turn loop (`Agent::run_turn`), system prompt assembly, context compression |
| `joey-cron` | self-contained scheduler (duration/interval/cron expressions), job store, ticker |
| `joey-mcp` | Model Context Protocol stdio JSON-RPC client |
| `joey-gateway` | platform-neutral messaging spine (session keys, message events, `PlatformAdapter` trait) |
| `joey-cli` | the `joey` binary: clap command tree + reedline REPL |
| `joey-tui` | terminal UI widgets/rendering (ratatui-based) |
| `joey-orchestration` | multi-agent/task orchestration primitives (newer, not yet in `docs/architecture.md`) |
| `joey-omo` | "oh-my-openagent" orchestration layer built on `joey-orchestration` — agents, goals, plan parsing, intent gating, team/notepad concepts |
| `joey-speckit-ui` | visual UI over spec-kit artifacts (`.specify/` specs/plans/tasks); reads/writes those files as the source of truth, never diverges into UI-only state |

`joey-orchestration` and `joey-omo` are newer additions layered on top of
`joey-agent-core`; `docs/architecture.md` and `docs/README.md` predate them
and only describe the original 8 crates — don't be surprised the diagrams
there are incomplete, cross-check against `Cargo.toml` workspace members
for the current full crate list.

Full docs index (read the relevant one before touching that subsystem):
`docs/architecture.md`, `docs/agent-turn-loop.md`, and (per `docs/README.md`)
`system-prompt.md`, `context-compression.md`, `tools.md`, `providers.md`,
`security.md`, `cron.md`, `mcp.md`, `gateway.md`, `cli.md`,
`state-and-config.md`, `events.md` — check `docs/` for which of these
actually exist before assuming a link resolves.

## Non-obvious conventions and gotchas

- **Upstream fidelity is a hard constraint, not a suggestion.** Guidance
  strings shown to the model (`guidance.rs` in `joey-agent-core`) are ported
  *verbatim* from the Python upstream because subtle wording changes
  measurably affect model behavior. Don't "clean up" prompt text without
  checking `PORTING.md`/upstream intent first. Source comments frequently
  cite the exact upstream Python file/line a piece of logic was ported from
  (e.g. `conversation_loop.py:4309`) — treat these as authoritative context,
  not decoration.
- **The system prompt is built once per session and never re-rendered** —
  this is intentional (`Agent::new`/`build_system_prompt`) to keep
  provider prompt-prefix caches warm. Don't refactor this into a
  per-turn rebuild.
- **Explicit tool registration, not reflection.** All built-in tools are
  registered by hand in `joey_tools::builtins::register_all`. If you add a
  tool, wire it there — there's no auto-discovery mechanism to rely on.
  Tools are grouped into toolsets (`file`, `terminal`, `web`, `coding`,
  `joey-cli`, …) with recursive `includes`, resolved the same way upstream
  resolves them.
  - Read-only tools are dispatched **concurrently** ("parallel-safe");
    everything else runs sequentially with `tool_delay` spacing.
- **On-disk formats must stay Hermes-compatible.** SQLite schema is
  `SCHEMA_VERSION = 22`; a `~/.hermes` home directory renamed to `~/.joey`
  must open and work unchanged. Don't bump schema/format versions casually
  — check `PORTING.md` for what's pinned.
- **Untrusted content passes through sanitization/threat-scan layers**
  before reaching the model or shell: context files, `web_search`/
  `web_extract`/`browser_*`/`mcp_*` tool results, and MCP server configs
  each have their own layer (see `docs/security.md` if present). Don't
  bypass these when adding new tools that ingest external content.
- **One deliberate divergence from upstream:** upstream's Anthropic-OAuth
  path impersonates Claude Code (spoofed client identity/headers) to make
  subscription billing accept its traffic. Joey does **not** do this — it's
  considered a ToS violation and is intentionally omitted. Don't
  re-introduce it. Use an Anthropic API key instead.
- **Config/state lives under `~/.joey/`** (override via `JOEY_HOME`;
  per-profile under `~/.joey/profiles/<name>` with `-p/--profile`). Config
  keys use dotted paths (`agent.max_turns`, `model.provider`, …); any key
  ending in `_KEY`/`_TOKEN`/`_SECRET`/`_PASSWORD` is auto-routed to `.env`
  instead of `config.yaml` — don't hardcode secrets into `config.yaml`
  logic paths.
- **Rust toolchain:** stable channel only (`rust-toolchain.toml`), edition
  2021. Bundled/`rusqlite` with `bundled` feature — no system SQLite
  dependency required.
- **spec-kit workflow present** (`.specify/`, `specs/001…003-*`): this repo
  uses the spec-kit lifecycle (`/speckit-specify` → `/speckit-clarify` →
  `/speckit-plan` → `/speckit-tasks` → `/speckit-implement`) for larger
  features, governed by `.specify/memory/constitution.md`. If asked to plan
  a nontrivial feature, check whether spec-kit artifacts are expected
  rather than jumping straight to code — the constitution treats
  `.specify/` files as **the** source of truth for spec-kit-managed
  features (never let a UI, e.g. `joey-speckit-ui`, hold state that
  diverges from those files).
- **Constitution highlights that affect how you should implement changes**
  (`.specify/memory/constitution.md`, currently v1.1.0):
  - Every crate must be independently buildable/testable
    (`cargo build -p <crate>` / `cargo test -p <crate>`).
  - Public surfaces (APIs, CLI flags/exit codes, config keys, on-disk
    formats, traits) are backward-compatible contracts; breaking one
    requires a MAJOR bump + documented migration, and regression tests are
    mandatory for anything touching a public surface.
  - New crates/modules add tests alongside implementation, not after.
  - Avoid speculative abstractions/dependencies; justify any new
    dependency's binary-size/compile-time cost.
- **`PORTING.md` is a living audit document** — when you complete or
  change upstream-parity work, that file is expected to be updated
  (it tracks Complete / Partial / Deferred / Deliberate-deviation status
  per subsystem with dates).

## Testing approach

- Tests live per-crate under `crates/<crate>/tests/` (integration-style)
  plus inline `#[cfg(test)]` unit tests in source files — present in
  `joey-agent-core`, `joey-orchestration`, `joey-speckit-ui`, `joey-tools`,
  `joey-omo`, `joey-tui`.
- Tests assert exact schemas, wire envelopes, grammars, and even prompt
  text in places — this is deliberate given the fidelity-to-upstream goal,
  don't loosen an assertion just because it looks overly strict without
  checking whether it's pinning upstream-parity behavior.
- Run the full suite (`cargo test --workspace`) after any change touching
  more than one crate; run the scoped `-p <crate>` suite for isolated work.
