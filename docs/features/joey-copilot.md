# joey-copilot — .github Copilot extension bundles

`joey-copilot` is a small parsing crate that reads a project's `.github/`
directory — the same artifacts GitHub Copilot reads — and turns them into
typed Rust values Joey can feed into its system prompt, skills index,
slash-command prompt fallback, and MCP server surface. The parsers are
**total**: missing, unreadable, oversized, or malformed files yield empty
results rather than errors, so callers can run `discover` unconditionally
on any project. It also owns the installed-plugin manifest.

> See also: [../copilot.md](../copilot.md)

## Overview

- Two files total: `src/lib.rs` (692 lines) and `Cargo.toml` — the whole
  crate is one module with no submodules.
- Parses five artifact kinds under `.github/`:
  `copilot-instructions.md`, `instructions/*.instructions.md`,
  `prompts/*.prompt.md`, `skills/*/SKILL.md`, `mcp.json`.
- **Not auth**: GitHub Copilot *authentication* — the
  `COPILOT_GITHUB_TOKEN → GH_TOKEN → GITHUB_TOKEN → gh auth token`
  chain, exchange of GitHub credentials for short-lived Copilot API
  tokens, the account-specific Enterprise endpoint, and device-code
  login — lives in `joey-providers`
  (`crates/joey-providers/src/copilot.rs`: `CopilotCredentials`,
  `CopilotAuth`, `validate_copilot_token`, `resolve_copilot_token`).
  See [joey-providers.md](joey-providers.md). Copilot-served
  *embeddings* (Metis, default model `metis-1024-I16-Binary`) live in
  `joey-neurocode-rag` — see
  [joey-neurocode-rag.md](joey-neurocode-rag.md). This crate touches
  none of that; it is purely repo-extension parsing plus the plugin
  manifest.
- A Joey extension with no upstream Hermes counterpart (Hermes has no
  `.github` parsing) — tracked in `PORTING.md`.

## Public API

Constants and paths:

| Item | Value | Notes |
|---|---|---|
| `COPILOT_DIR` | `".github"` | directory (relative to a project root) holding extension files |
| `MAX_READ_BYTES` | `65536` (64 KiB, private) | cap on any single file read |
| `MAX_NAME_LENGTH` | `100` (private) | skill-name cap, matches `joey-tools` `skills_tool.rs` |
| `MAX_DESCRIPTION_LENGTH` | `500` (private) | skill-description cap, matches `skills_tool.rs` |
| `SKIP_DIRS` | `references`, `templates`, `assets`, `scripts` (private) | skill-directory names holding support files, not skills |
| `plugins_dir()` | `~/.joey/copilot/plugins` | where installed plugins live |
| `plugins_manifest_path()` | `~/.joey/copilot/plugins.json` | on-disk plugin manifest |

Functions:

| Function | Signature | Notes |
|---|---|---|
| `discover` | `(cwd: &Path) -> CopilotBundle` | run all five parsers |
| `parse_instructions` | `(cwd: &Path) -> Option<String>` | `.github/copilot-instructions.md`, raw (no frontmatter expected) |
| `parse_instruction_files` | `(cwd: &Path) -> Vec<InstructionFile>` | non-recursive, sorted by path |
| `parse_prompts` | `(cwd: &Path) -> Vec<CopilotPrompt>` | non-recursive, sorted by name |
| `parse_skills` | `(cwd: &Path) -> Vec<CopilotSkill>` | depth-2 scan, sorted by name |
| `parse_mcp_servers` | `(cwd: &Path) -> Option<Value>` | the `servers` object only |
| `load_manifest` | `() -> PluginManifest` | missing/corrupt → empty (never errors) |
| `save_manifest` | `(&PluginManifest) -> std::io::Result<()>` | pretty JSON, creates parent dirs |

Data types:

| Type | Fields |
|---|---|
| `CopilotBundle` | `instructions: Option<String>`, `instruction_files: Vec<InstructionFile>`, `prompts: Vec<CopilotPrompt>`, `skills: Vec<CopilotSkill>`, `mcp_servers: Option<Value>` |
| `InstructionFile` | `path: PathBuf`, `apply_to: Option<String>` (frontmatter `applyTo` glob), `body: String` |
| `CopilotPrompt` | `name: String` (file stem, e.g. `deploy` from `deploy.prompt.md`), `description: String`, `mode: Option<String>` (`chat`/`agent`/`ask`/`edit`), `body: String`, `path: PathBuf` |
| `CopilotSkill` | `name: String`, `description: String`, `path: PathBuf` (the `SKILL.md` itself) |
| `PluginRecord` | `name` (slug / repo dir name), `source` (original git URL / owner-repo / local path), `installed_at` (RFC3339 UTC), `commit: Option<String>` (`git rev-parse HEAD` at install time), `skills: Vec<String>`, `prompts: Vec<String>` |
| `PluginManifest` | `plugins: Vec<PluginRecord>` |

Parser behavior:

| Parser | Behavior |
|---|---|
| `parse_instructions` | verbatim capped read; `None` if missing/unreadable |
| `parse_instruction_files` | `*.instructions.md` only; YAML frontmatter may carry `applyTo` (glob string, e.g. `"**/*.ts"` per the crate's own test); body is everything after frontmatter |
| `parse_prompts` | `*.prompt.md` only; `description` = frontmatter `description`, else the first non-heading body line, else `""`; `body` trimmed |
| `parse_skills` | `SKILL.md` under `.github/skills/<dir>/`; `name` = frontmatter `name` else directory name, truncated to 100 chars; `description` = frontmatter `description` else first body line; >500 chars → 497 kept + `"..."` |
| `parse_mcp_servers` | `.github/mcp.json` as JSON; returns the `servers` value **only if** it is an object; `None` on missing file, invalid JSON, or absent/non-object `servers` |

Frontmatter splitting matches `joey-tools`
`skills_tool.rs::parse_frontmatter` semantics: text must start (after
leading whitespace) with `---`; frontmatter ends at the next `\n---`;
the body follows; unterminated frontmatter means no frontmatter (the
whole text is the body). `first_body_line` picks the first non-empty,
non-`#` line. Reads are UTF-8 lossy (`String::from_utf8_lossy`).

## The five artifacts

All optional; `discover` on a project with no `.github/` (or none of
these files) returns an all-empty bundle.

| Path | Parsed by | Consumed by |
|---|---|---|
| `.github/copilot-instructions.md` | `parse_instructions` | system-prompt context tier (`Copilot instructions` block) |
| `.github/instructions/*.instructions.md` | `parse_instruction_files` | same context-tier block, concatenated; `applyTo` respected |
| `.github/prompts/<name>.prompt.md` | `parse_prompts` | `/<name>` slash-command prompt fallback |
| `.github/skills/<dir>/SKILL.md` | `parse_skills` | skills index (category `copilot`), `skill_view` |
| `.github/mcp.json` | `parse_mcp_servers` | MCP server-config merge (`joey-mcp`) |

Shapes (as exercised by the crate's own tests):

```markdown
<!-- .github/copilot-instructions.md — plain markdown, no frontmatter -->
# Repo instructions
Always use tabs. Never commit binaries.
```

```markdown
<!-- .github/instructions/ts.instructions.md -->
---
applyTo: "**/*.ts"
---
Use strict mode.
```

```markdown
<!-- .github/prompts/deploy.prompt.md -->
---
description: Deploys the app
mode: agent
---
Ship it now.
```

```markdown
<!-- .github/skills/pdf/SKILL.md — same grammar as ~/.joey/skills/ -->
---
name: pdf-tools
description: Work with PDFs
---
# PDF
```

```json
// .github/mcp.json — servers object only
{
  "servers": {
    "fetch": { "command": "uvx", "args": ["mcp-server-fetch"] },
    "remote": { "type": "http", "url": "https://example.com/mcp" }
  }
}
```

Skill-directory layout notes: `pdf/references/note.md` inside a skill
folder is supporting material, not a skill (depth-2 scan finds only
direct `<dir>/SKILL.md`); a top-level `scripts/SKILL.md` would be
skipped outright.

## Discovery & install flow

Project-side discovery (`discover`) is what the CLI and REPL surfaces
consume; plugin management is layered on top in `joey-cli`
(`crates/joey-cli/src/copilot_cmd.rs`):

| Command | Effect |
|---|---|
| `joey copilot install <source> [--ref REF]` | classify the source, clone (`git clone --depth 1`) or copy into `~/.joey/copilot/plugins/<name>`, copy shipped skill folders to `~/.joey/skills/copilot/<plugin>/<skill>/` so regular skills discovery picks them up, and record a `PluginRecord` in the manifest |
| `joey copilot list` | installed plugins + per-plugin skills/prompts |
| `joey copilot remove <name>` | delete the plugin dir, its installed skills, and the manifest record (user skills untouched) |
| `joey copilot update [name]` | re-pull all plugins, or one |
| `joey copilot status` | render what the current project's `.github/` provides |

Install-source classification (`classify_source` in `copilot_cmd.rs`):

| Source shape | Resolution |
|---|---|
| contains `://` | `Git(url)` — used as-is |
| `owner/repo`, each side `[A-Za-z0-9_.-]+` | resolved to `https://github.com/<owner>/<repo>.git` |
| existing local directory | copied |
| anything else | unsupported (refused) |

Plugin names are validated by `sanitize_name` (non-empty,
`[A-Za-z0-9._-`]` only) and derived from the URL's last path segment
sans `.git` (`plugin_name_from_url`). `copilot_cmd.rs` mirrors the
crate's `SKIP_DIRS` when copying skills.

Slash surfaces (see [joey-cli.md](joey-cli.md)):

- `/copilot [status|list|install|remove|update]` — bare `/copilot` and
  `/copilot status` render `copilot_status_text`; git-performing
  subcommands point at the CLI commands. Registered in both REPL slash
  dispatch and the TUI.
- **Prompt fallback**: an otherwise-unknown `/name` resolves via
  `find_prompt_body` — first `<cwd>/.github/prompts/<name>.prompt.md`
  (through `parse_prompts`), then any installed plugin's
  `<name>.prompt.md` under `~/.joey/copilot/plugins/` (depth-4 walk,
  hand-split frontmatter for `mode`). The prompt body plus any user
  args are submitted as one agent turn; project prompts win on clash.

MCP merge: `parse_mcp_servers` output feeds
`joey_mcp::merge_project_server_configs(base, project)` — user config
wins on name clash; merged entries pass the same exfiltration filter and
`${ENV}` interpolation as user-configured servers; no auto-connect
([joey-mcp.md](joey-mcp.md), [../copilot.md](../copilot.md)). The merge
is gated in `joey-cli` on `copilot.enabled` (default `true`) and is
skipped in safe mode (`JOEY_SAFE_MODE`).

## Limits & guarantees

| Property | Guarantee |
|---|---|
| Totality | no public parser returns `Result`; missing/unreadable/invalid/hostile input → empty or `None`, never an error |
| Read cap | every file read ≤ `64 * 1024` bytes (`read_capped` via `Read::take`, UTF-8 lossy) |
| Skill name | ≤ 100 chars (char-counted), matching `joey-tools` `skills_tool.rs` |
| Skill description | ≤ 500 chars; overlong → 497 kept + `"..."` |
| Scan depth | `instructions/` and `prompts/` at `max_depth(1)`; `skills/` at `max_depth(2)` |
| Skip dirs | `references`, `templates`, `assets`, `scripts` never become skills |
| `mcp.json` | only a JSON object with an object-valued `servers` key parses; everything else → `None` |
| Manifest | corrupt/missing → empty manifest; save creates parent dirs, writes pretty JSON |
| Timestamps | std-only `civil_from_days` RFC3339 UTC; no datetime-crate dependency |
| Env | `JOEY_HOME` relocates both plugin paths (pinned by test) |

Additional detail:

- **Parsers are total** — a hostile or half-written `.github/` tree can
  never fail `discover`; the worst case is a bundle with fewer entries.
- `frontmatter_edge_cases` pins that unterminated frontmatter means *no*
  frontmatter (the whole text becomes the body) — a malformed file
  degrades to "instructions as body", not an error.
- The `applyTo` glob is parsed and carried but evaluated by the consumer
  (context-tier injection), not by this crate.
- `PluginRecord.commit` is `Option<String>`: `Some` for git installs
  (`git rev-parse HEAD`), `None` for local-path copies.

## Who consumes what (wiring map)

Where the crate's outputs land (implementation in `joey-cli`, spec-level
detail in [../copilot.md](../copilot.md)):

| Output | Consumer | Where |
|---|---|---|
| `instructions` + `instruction_files` | `copilot_status_text`; system-prompt `Copilot instructions` context block | `joey-cli` `copilot_cmd.rs`, `joey-agent-core` prompt assembly |
| `prompts` | `find_prompt_body` `/<name>` fallback (project first, then plugins) | `joey-cli` `repl.rs` |
| `skills` | skills index category `copilot`; installed plugin skills copied to `~/.joey/skills/copilot/` | `joey-tools` skills machinery |
| `mcp_servers` | `merge_project_server_configs` (gated on `copilot.enabled`, skipped in `JOEY_SAFE_MODE`) | `joey-cli` `slash_extra.rs`, `joey-mcp` |
| `PluginManifest` | `joey copilot install/list/remove/update/status` | `joey-cli` `copilot_cmd.rs` |

## Testing

12 inline `#[cfg(test)]` tests in `src/lib.rs`:

| Test | Asserts |
|---|---|
| `discover_empty_dir` | empty project yields an all-empty bundle |
| `instructions_verbatim` | `copilot-instructions.md` round-trips byte-for-byte through both `parse_instructions` and `discover` |
| `instruction_files_apply_to` | `applyTo` glob parsed; no-frontmatter file gets `None`; sorted by path |
| `prompts_sorted_and_frontmatter` | prompts sorted by name; description/mode from frontmatter; body-line description fallback |
| `skills_names_and_skip_dirs` | frontmatter name wins; dir-name fallback; `scripts/` skipped; nested subdirs not skills |
| `skill_description_truncated` | 600-char description → exactly 500 chars ending `"..."` |
| `mcp_servers_parsing` | valid two-server file parses (`fetch` stdio + `remote` http); invalid JSON and missing `servers` → `None` |
| `frontmatter_edge_cases` | no frontmatter / unterminated frontmatter / >100-char name truncated to 100 |
| `rfc3339_shape` | timestamp is 20 chars with digits/separators in place and sane ranges |
| `civil_from_days_known_values` | 1970-01-01, 2024-01-01, 2026-08-31, 2026-09-01, 1969-12-31 |
| `manifest_round_trip` | save + load under a `JOEY_HOME` temp override; both paths under the override |
| `manifest_corrupt_loads_empty` | corrupt and missing manifest files both load empty |

`JOEY_HOME`-touching tests serialize on a mutex (env vars are
process-global) and restore the previous value even when assertions fire.

## See also

- [../copilot.md](../copilot.md) — user-facing Copilot integration guide
- [joey-providers.md](joey-providers.md) — Copilot auth, device-code login, token exchange, catalog/model routing (`copilot.rs`)
- [joey-neurocode-rag.md](joey-neurocode-rag.md) — Metis Copilot embeddings backend
- [joey-cli.md](joey-cli.md) — `joey copilot` commands, `/copilot` and `/<prompt>` slash fallback
- [joey-mcp.md](joey-mcp.md) — project server merge semantics
- [joey-tools.md](joey-tools.md) — the SKILL.md grammar these parsers mirror
- [README.md](README.md) — the features index
