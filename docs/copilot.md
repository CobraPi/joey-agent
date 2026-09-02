# joey-copilot — Native GitHub Copilot Integration

Parses a project's `.github/` directory (the same artifacts GitHub Copilot
reads) and feeds them into Joey's prompt, skills, prompt-expansion, and MCP
surfaces. This is a **Joey extension with no upstream Hermes counterpart**
(Hermes has no `.github` parsing) — see PORTING.md.

## Overview

Five artifacts are recognized, all optional:

| Path | Effect |
|---|---|
| `.github/copilot-instructions.md` | Injected into the system prompt context tier as a `Copilot instructions` block |
| `.github/instructions/*.instructions.md` | Same `Copilot instructions` block (concatenated; `applyTo` frontmatter respected) |
| `.github/skills/<name>/SKILL.md` | Listed in the `## Skills (mandatory)` index under category `copilot`; viewable via `skill_view` |
| `.github/prompts/<name>.prompt.md` | Resolvable by the `/<name>` slash-command prompt fallback |
| `.github/mcp.json` | Servers merged into the MCP server config surface (no auto-connect) |

Parsing lives in the `joey-copilot` crate; the crate also owns the plugin
manifest `~/.joey/copilot/plugins.json` and the plugin directory
`~/.joey/copilot/plugins/`.

## Configuration

Everything is gated on one key, default **true** (nothing to configure for
the common case — drop files into `.github/` and they take effect):

```yaml
copilot:
  enabled: true   # kill-switch: set false to ignore .github/ entirely
```

With `copilot.enabled: false` the prompt block, skills-index entries,
prompt fallback, and `mcp.json` merge are all skipped.

## The `.github` directory reference

**`.github/copilot-instructions.md`** — plain markdown, no frontmatter.
Treated like AGENTS.md: untrusted content, threat-scanned and truncated
into the context tier.

```markdown
# Project guidance
Always run tests before committing. Prefer small, targeted diffs.
```

**`.github/instructions/*.instructions.md`** — optional `applyTo`
frontmatter (glob); the body is appended to the same block.

```markdown
---
applyTo: "**/*.rs"
---
Use `cargo fmt` semantics when editing Rust files in this repo.
```

**`.github/prompts/<name>.prompt.md`** — frontmatter `description` and
`mode` (e.g. `agent` vs `edit`); the body is the prompt template.

```markdown
---
description: Explain why a test is failing
mode: agent
---
Read the failing test output and the code under test, then explain
the root cause.
```

**`.github/skills/<name>/SKILL.md`** — the agentskills.io format; the same
SKILL.md grammar Joey already uses for `~/.joey/skills/`.

**`.github/mcp.json`** — a `servers` object, same shape as `mcp_servers:`:

```json
{
  "servers": {
    "local-dev": {
      "command": "npx",
      "args": ["-y", "some-mcp-server"],
      "env": { "API_TOKEN": "${MY_API_TOKEN}" }
    },
    "remote": {
      "url": "https://mcp.example.com/sse",
      "headers": { "Authorization": "Bearer ${MCP_TOKEN}" }
    }
  }
}
```

## Installing plugins (`joey copilot ...`)

- `joey copilot install <git-url|owner/repo|local-path>` — clones
  (`git clone --depth 1`) or copies the source into
  `~/.joey/copilot/plugins/<name>`, copies any skill folders into
  `~/.joey/skills/copilot/<plugin>/<skill>/SKILL.md`, and records a
  `PluginRecord {name, source, installed_at, commit, skills, prompts}` in
  `~/.joey/copilot/plugins.json`.
- `joey copilot list` — installed plugins and their skills/prompts.
- `joey copilot update [name]` — re-pull (all plugins, or one).
- `joey copilot remove <name>` — delete the plugin dir, its skills, and
  its manifest record.
- `joey copilot status` — parser state for the current project
  (what was found in `.github/`).

Installed skills are picked up by `skills_list`/`skill_view` like any
other skill; installed `*.prompt.md` files join the `/<name>` fallback
below. **Only install sources you trust** — install runs `git clone` of
whatever you point it at, and plugin skills/prompts are instructions the
agent will read.

## Slash commands

- `/copilot [status|list|install|remove|update]` — REPL and TUI mirror of
  the CLI subcommands; bare `/copilot` shows status.
- **Prompt fallback**: an unknown `/name` resolves against
  `.github/prompts/<name>.prompt.md` first, then installed plugins'
  `*.prompt.md` files (project wins on name clash). The prompt body plus
  any user args are submitted as one agent turn.

## MCP merge semantics

`joey-mcp::merge_project_server_configs(base, project)` merges
`.github/mcp.json` servers into the user config surface:

- **User config wins** on name clash — a project server never shadows an
  explicit `mcp_servers.<name>` entry.
- Merged entries pass the same **exfiltration filter** and **`${ENV}`
  interpolation** as user-configured servers.
- **No auto-connect**: Joey's MCP runtime is config-only today
  (`joey mcp test` connects on demand). Merged project servers show up in
  `joey mcp list`, `/reload-mcp` output, and oneshot `--toolsets`
  validation.

## NeuroCode & HyperCode

Both `/neurocode` and `/hypercode` build their agents through the same
`joey-agent-core` system-prompt assembly and `joey-tools` skill machinery,
so copilot skills and instructions are visible to hypercode children and
neurocode turns automatically — no wiring exists specifically for them.

## Security notes

- Instruction content (`.github/copilot-instructions.md`,
  `instructions/*.instructions.md`) is untrusted: it flows through the
  same sanitization/threat-scan context-tier handling as AGENTS.md.
- Skills are read through `skill_view`'s existing path checks — a project
  skill cannot escape its directory.
- `.github/mcp.json` entries pass the exfiltration filter before merge;
  suspicious entries are refused, not silently used.
- Plugin install runs `git clone` — treat install sources as code you
  would be willing to check out and read.
