# Skills system & bundled catalog

Skills are Joey Agent's reusable knowledge units: directories containing a `SKILL.md` (YAML frontmatter + markdown body) plus optional `references/`, `templates/`, `assets/`, and `scripts/` payloads. Discovered skills are indexed into the system prompt as a mandatory consult list, and the agent loads full skill content on demand through the `skills_list` / `skill_view` tools. Skills come from several roots — the user's `~/.joey/skills/`, a bundled directory shipped with the repo, configured external dirs, and project Copilot skills (`.github/skills`) — merged with first-name-wins precedence and filtered by `skills.disabled`. The repo ships a ~75-skill catalog under `skills/` covering Apple integrations, coding workflows, creative pipelines, MLOps, research, and more.

> See also: [README.md](README.md), [joey-tools.md](joey-tools.md), [joey-agent-core.md](joey-agent-core.md), [joey-copilot.md](joey-copilot.md)

## Overview

The skills subsystem spans four crates:

- `joey-core` (`constants.rs`) — path resolution: `skills_dir()`, `optional_skills_dir()`, `bundled_skills_dir()`, and the `JOEY_OPTIONAL_SKILLS` / `JOEY_BUNDLED_SKILLS` env overrides.
- `joey-tools` (`tools/skills_tool.rs`) — discovery (`discover_with`) and the `skills_list` / `skill_view` tools, registered by hand in `builtins.rs`.
- `joey-agent-core` (`prompt.rs`, `guidance.rs`) — the `## Skills (mandatory)` index section injected into the system prompt, plus skills-related guidance strings.
- `joey-cli` (`skills_cmd.rs`, `repl.rs`, `tui.rs`, `slash_extra.rs`) — the `joey skills` subcommand tree and the `/skills`, `/reload-skills`, `/learn`, and `/curator` slash commands.

A third tool, `skill_manage`, is declared in the `skills` toolset (`toolsets.rs`) and referenced throughout guidance text (e.g. `skill_manage(action='patch')`), but no `SkillManage` implementation is registered in this port — `register_all` registers only `SkillsList` and `SkillView`.

## How skills work

### Loading roots and precedence

Discovery (`discover_with` in `skills_tool.rs`; `build_skills_system_prompt` in `prompt.rs` uses the same ordering) walks these roots in order:

1. **User skills** — `skills_dir()` = `~/.joey/skills/` (per-profile under `~/.joey/profiles/<name>`).
2. **Bundled skills** — `bundled_skills_dir(None)`, resolved as: `JOEY_BUNDLED_SKILLS` env var → exe-adjacent `<prefix>/share/joey-agent/skills` → caller default → `~/.joey/skills/`.
3. **External dirs** — `skills.external_dirs` config list; entries are tilde-expanded and must exist.
4. **Project Copilot skills** — `<cwd>/.github/skills`, a Joey extension gated on `copilot.enabled` (default `true`); entries are forced into the `copilot` category (see [joey-copilot.md](joey-copilot.md)).

Dedup is first-name-wins, so precedence is user skills > bundled > external > project `.github`. Any skill whose frontmatter `name` appears in `skills.disabled` is dropped everywhere (tool listings and prompt index alike).

A separate **optional-skills** root, `optional_skills_dir()` (`JOEY_OPTIONAL_SKILLS` env → `<prefix>/share/joey-agent/optional-skills` → default → `~/.joey/optional-skills/`), is not part of tool discovery; it backs packaged extras such as the spec-kit skill resolver in `joey-cli` (`speckit_slash.rs` checks `~/.joey/skills/<skill>/SKILL.md` first, then `~/.joey/optional-skills/<skill>/SKILL.md`).

### Walk limits and category derivation

- Each root is walked with `max_depth(6)` looking for `SKILL.md` files.
- Any `SKILL.md` inside a `references/`, `templates/`, `assets/`, `scripts/`, or `.hub/` subtree is skipped (those are support dirs, not nested skills).
- Frontmatter is parsed from the first 4000 characters of the file.
- `name` falls back to the skill's directory name; names are truncated at 100 chars (`MAX_NAME_LENGTH`).
- `description` falls back to the first non-empty, non-heading body line; descriptions are truncated at 500 chars (`MAX_DESCRIPTION_LENGTH`, kept 497 + `...`).
- **Category** is derived from the path relative to the root: in tool discovery a top-level skill (e.g. `skills/dogfood/SKILL.md`) has no category, while `mlops/inference/vllm` gets category `mlops/inference`. In the system-prompt index a top-level skill uses its own directory name as its category (upstream's rule), so `dogfood` shows under `dogfood:`.
- Copilot-root skills always get category `copilot`.

### SKILL.md format

```
skills/<category>/<skill-name>/
├── SKILL.md          # required: frontmatter + body
├── references/       # optional: linked .md docs (depth-1 in listings)
├── templates/        # optional: linked template files (recursive)
├── assets/           # optional: linked asset files (recursive)
└── scripts/          # optional: linked scripts (.py/.sh/.bash/.js/.ts/.rb, top level)
```

Frontmatter sits between a leading `---` and a closing `\n---` and must parse as a YAML mapping:

| Key | Required | Notes |
|---|---|---|
| `name` | yes | Falls back to directory name; ≤100 chars in discovery. Lowercase-hyphen convention. |
| `description` | expected | Falls back to first body line; 500-char cap in tool output, 60-char cap in the prompt index. |
| `version` | no | Peer convention among bundled skills (e.g. `computer-use` is `2.1.0`). |
| `author` | no | Peer convention. |
| `license` | no | Peer convention. |
| `metadata.joey.tags` | no | Tags; a YAML list or comma-separated string. Takes precedence over top-level `tags`. |
| `metadata.joey.related_skills` | no | Related skill names; same list-or-string parsing, precedence over top-level `related_skills`. |
| `tags`, `related_skills` | no | Top-level fallbacks for the two `metadata.joey.*` keys. |

The body is free markdown after the closing `---`. A category can also ship a `DESCRIPTION.md` (with a frontmatter `description`) next to its skill dirs; the prompt index appends it to the category line, first root to define one wins.

A minimal working skill:

```yaml
---
name: deploy-checklist
description: Use when deploying the API. Runs smoke tests, checks rollback plan.
metadata:
  joey:
    tags: [deploy, ops]
    related_skills: [plan]
---

# Deploy checklist

## When to Use
- User asks to deploy or promote a build

## Steps
1. Run `scripts/verify.sh` ... (completion criterion: exit 0)
```

Note the validator limits quoted by the bundled authoring skill differ from the discovery-time caps above: the upstream `_validate_frontmatter` enforced a 1024-char description cap and ~100k-char `SKILL.md` cap at write time, while discovery truncates names at 100 chars and descriptions at 500.

### Linked files

`skill_view`'s first call returns `linked_files` buckets: `references/*.md` (depth 1), all files under `templates/` and `assets/` (recursive), and top-level `scripts/` files with code extensions. A second call with `file_path` reads one of them. When a requested file is missing, the error enumerates available files bucketed as `references` / `templates` / `assets` / `scripts` / `other` (the `other` bucket covers `.md/.py/.yaml/.yml/.json/.tex/.sh` files outside the standard subdirs).

## System-prompt injection

`build_skills_system_prompt` (`prompt.rs`) is called **once per session** during prompt assembly (snapshot semantics — no per-turn rebuild, keeping provider prompt-prefix caches warm). When any of `skills_list` / `skill_view` / `skill_manage` is in the active toolset and at least one skill is discovered, it emits:

```
## Skills (mandatory)
<SKILLS_INDEX_PREAMBLE text>
<available_skills>
  category: <category description from DESCRIPTION.md, if any>
    - name: description
    ...
</available_skills>

Only proceed without loading a skill if genuinely none are relevant to the task.
```

Verified mechanics:

- `SKILLS_INDEX_PREAMBLE` (ported from `prompt_builder.py:1725-1745`, branded) instructs the model to scan the list before replying, load anything matching or partially relevant via `skill_view(name)`, err on the side of loading, load the `joey-agent` skill first for any Joey-configuration task, fix broken skills with `skill_manage(action='patch')`, offer to save skills after hard tasks, and update skills that proved incomplete.
- `SKILLS_INDEX_FOOTER` is the single trailing line: "Only proceed without loading a skill if genuinely none are relevant to the task."
- Categories are sorted (BTreeMap), names sorted and deduped within each; descriptions are quote-stripped and capped at 60 chars (57 + `...`).
- A top-level skill (no category subdir) appears under its own directory name as the category; nested paths join all parent dirs (e.g. `mlops/inference/vllm/SKILL.md` → category `mlops/inference`, name `vllm`).
- Both the frontmatter `name` and the directory name are checked against `skills.disabled`.
- The section is placed **before** the environment hints, matching upstream `system_prompt.py` ordering.
- An empty index (no roots exist, or every skill disabled) suppresses the section entirely — the prompt has no skills block.
- `SKILLS_GUIDANCE` (verbatim from upstream) is appended to the tool-guidance block when `skill_manage` is in the active set: after a complex task (5+ tool calls), a tricky fix, or a non-trivial workflow, save the approach as a skill; patch stale skills immediately rather than waiting to be asked.
- The context-breakdown report (`compression/breakdown.rs`) carves the `<available_skills>` block out of the system prompt and accounts for it as a separate "Skills" token category; the compressor summarizes `skills_list`/`skill_view`/`skill_manage` results as `[tool] name=... (N chars)`.

## Tools & CLI

### `skills_list`

Lists discovered skills (name + description + category).

| Parameter | Type | Required | Notes |
|---|---|---|---|
| `category` | string | no | Exact category filter (e.g. `mlops`, `copilot`). |

Returns `{success, skills: [{name, description, category}], categories, count, hint}`. If `~/.joey/skills/` doesn't exist yet it is created and a "No skills found" message is returned.

### `skill_view`

Loads one skill's full `SKILL.md` (first call) or a linked file (with `file_path`).

| Parameter | Type | Required | Notes |
|---|---|---|---|
| `name` | string | yes | Frontmatter name or directory name; plugin skills use `plugin:skill`. Absolute paths and `..` are rejected up front. |
| `file_path` | string | no | Relative path inside the skill dir (e.g. `references/api.md`). Omit for the main `SKILL.md`. |

Behavior verified from source:

- Multiple name matches across roots → error refusing to guess, listing the candidate paths; disambiguate with `category/skill-name`.
- Unknown name → error plus the first 20 available skill names.
- Main mode returns `{success, name, description, tags, related_skills, content, path, skill_dir, linked_files, usage_hint}`.
- `file_path` mode canonicalizes and requires the target to stay inside the skill dir; binary files return a `[Binary file: ...]` placeholder instead of content.

### `skill_manage`

Declared in the `skills` toolset and named in guidance, but not implemented/registered in this port (see [joey-tools.md](joey-tools.md) for the toolset table). Guidance text still tells the model to use `skill_manage(action='patch')` for skill repair.

### `joey skills` CLI

From `crates/joey-cli/src/skills_cmd.rs`:

| Command | Effect |
|---|---|
| `joey skills` | Prints the usage line. |
| `joey skills list [--enabled-only]` | Name/Category/Source/Status table; Source is `local` (under `~/.joey/skills/`) or `builtin`; prints enabled/disabled counts. |
| `joey skills inspect <name>` | Category, description, path, and the full `SKILL.md` body. |
| `joey skills enable <name>` / `joey skills disable <name>` | Edits the `skills.disabled` config list (saved comma-joined); takes effect in new sessions or after `/reload-skills`. |
| `joey skills config` | Shows local/bundled dirs and manual-install instructions (`git clone <repo> ~/.joey/skills/<name>`). |
| `browse` / `search` / `install` / `publish` / `repair-official` / `tap` | Recognized but deferred (need the marketplace service); exit 1 with a manual-install hint. |

### Slash commands

- `/skills` — lists installed skills (name + description) in the REPL and TUI.
- `/reload-skills` — rescans the skill directories and reports the new count.
- `/learn <description>` — asks the agent to draft a `SKILL.md` from your description and install it under `~/.joey/skills/<kebab-name>/`.
- `/curator [dedupe|refresh]` — background skill maintenance: propose dedupe of near-duplicates, or refresh frontmatter descriptions.

## Authoring skills

The in-repo guide is the bundled skill itself: `skills/software-development/joey-agent-skill-authoring/SKILL.md` — read it with `skill_view(name="joey-agent-skill-authoring")`. Quick how-to, grounded in the format above:

1. Pick the closest existing category under `skills/` (or `~/.joey/skills/` for personal skills); don't invent new top-level categories casually.
2. Create `<category>/<kebab-name>/SKILL.md` with `name` and `description` frontmatter (description should lead with when-to-use triggers — the model pays for it every turn), then a concise operational body. Peer skills run 8–14k chars; push bulk material into `references/` and point to it.
3. Optionally add `references/`, `templates/`, `assets/`, `scripts/` and reference them from the body — they surface as `linked_files` in `skill_view`.
4. Re-scan with `/reload-skills` (or restart) and verify with `joey skills inspect <name>`.

The authoring skill also prescribes a peer-matched body structure — `Overview`, `When to Use` (with counter-triggers), topic sections with quick-reference tables and exact commands, `Common Pitfalls`, and a `Verification Checklist` — and a set of writing-quality principles (optimize for process predictability, prune no-op prose, end steps with completion criteria, watch for premature completion).

## Bundled catalog

The repo-root `skills/` directory contains 75 `SKILL.md` files across these categories:

| Category | Skills |
|---|---|
| `apple/` | `apple-notes`, `apple-reminders`, `findmy`, `imessage` |
| `autonomous-ai-agents/` | `claude-code`, `codex`, `joey-agent` (with `references/native-mcp.md`, `references/webhooks.md`), `opencode` |
| `computer-use/` | `computer-use` (v2.1.0 — drives `joey-browser` tools plus native desktop apps; see [joey-browser.md](joey-browser.md)) |
| `creative/` | `architecture-diagram`, `ascii-art`, `ascii-video`, `baoyu-infographic`, `claude-design`, `comfyui`, `design-md`, `excalidraw`, `humanizer`, `manim-video`, `p5js`, `popular-web-designs`, `pretext`, `sketch`, `songwriting-and-ai-music`, `touchdesigner-mcp` |
| `data-science/` | `jupyter-live-kernel` |
| `dogfood/` | `dogfood` |
| `email/` | `himalaya` |
| `github/` | `codebase-inspection`, `github-auth`, `github-code-review`, `github-issues`, `github-pr-workflow`, `github-repo-management` |
| `joey-desktop-plugins/` | `joey-desktop-plugins` |
| `media/` | `gif-search`, `heartmula`, `songsee`, `youtube-content` |
| `mlops/` | `evaluating-llms-harness` (dir `evaluation/lm-evaluation-harness`), `weights-and-biases` (dir `evaluation/weights-and-biases`), `huggingface-hub`, `llama-cpp` (dir `inference/llama-cpp`), `serving-llms-vllm` (dir `inference/vllm`), `audiocraft-audio-generation` (dir `models/audiocraft`), `segment-anything-model` (dir `models/segment-anything`) |
| `note-taking/` | `obsidian` |
| `productivity/` | `airtable`, `google-workspace`, `maps`, `nano-pdf`, `notion`, `ocr-and-documents`, `petdex`, `powerpoint`, `teams-meeting-pipeline` |
| `research/` | `arxiv`, `blogwatcher`, `llm-wiki`, `polymarket`, `research-paper-writing` |
| `smart-home/` | `openhue` |
| `social-media/` | `xurl` |
| `software-development/` | `joey-agent-skill-authoring`, `node-inspect-debugger`, `plan`, `python-debugpy`, `requesting-code-review`, `rust-review`, `simplify-code`, `spike`, `systematic-debugging`, `test-driven-development` |
| `ulw-plan/` | `ulw-plan` (adversarial planning workflow; `references/` holds intent-clear, intent-unclear, and full-workflow docs) |
| `yuanbao/` | `yuanbao` |
| `index-cache/` | No `SKILL.md` — four JSON caches: `anthropics_skills`, `claude_marketplace`, `lobehub_index`, `openai_skills` |

### Notable bundled assets

- `creative/comfyui` — 12 scripts, 9 workflows, plus tests.
- `productivity/powerpoint` — `scripts/office` tooling and ECMA/ISO XSD schemas for OOXML validation.
- `creative/p5js` and `creative/popular-web-designs` — HTML/JS templates.
- `creative/baoyu-infographic` — 21 layouts and 21 styles.
- `research/research-paper-writing` — LaTeX templates for NeurIPS 2025, ICML 2026, ICLR 2026, COLM 2025, ACL, and AAAI 2026.
- `creative/excalidraw`, `productivity/maps`, `research/arxiv`, `research/polymarket`, and `media/youtube-content` — ship helper scripts.
- `productivity/google-workspace` — `scripts/*.py` helpers.

## Configuration

| Key / env | Default | Effect |
|---|---|---|
| `skills.external_dirs` | `[]` | Extra skill roots (tilde-expanded, must exist); lowest precedence above Copilot skills. |
| `skills.disabled` | `[]` | Skill names hidden from the agent (tool listings and prompt index); managed by `joey skills enable/disable`. |
| `skills.creation_nudge_interval` | `10` | How aggressively the agent is nudged to save new skills after complex tasks (the repo config example sets `15`). |
| `skills.bundles.<name>` | — | Named skill lists loadable in bulk (e.g. `joey config set skills.bundles.review "review-pr,test-driven-development"`). |
| `copilot.enabled` | `true` | Gates discovery of project skills under `<cwd>/.github/skills`. |
| `JOEY_BUNDLED_SKILLS` | — | Overrides the bundled-skills directory (highest priority, ahead of the exe-adjacent `share/joey-agent/skills` and `~/.joey/skills/` fallbacks). |
| `JOEY_OPTIONAL_SKILLS` | — | Overrides the optional-skills directory (same resolution chain, `~/.joey/optional-skills/` fallback); used by packaged extras such as the spec-kit skill resolver, not by tool discovery. |

Paths resolve per-profile via `JOEY_HOME` / `-p <profile>` — see [joey-core.md](joey-core.md) for home resolution and [joey-cli.md](joey-cli.md) for the CLI surface.

Example config fragment:

```yaml
skills:
  creation_nudge_interval: 15
  external_dirs:
    - ~/code/my-skills
  disabled:
    - petdex
```

## See also

- [README.md](README.md) — features index.
- [joey-tools.md](joey-tools.md) — tool registry, toolsets (including `skills`), and traversal guards.
- [joey-agent-core.md](joey-agent-core.md) — system-prompt assembly and context compression.
- [joey-copilot.md](joey-copilot.md) — `.github/skills` project skills and `SKILL.md` parsing from Copilot bundles.
- [joey-browser.md](joey-browser.md) — the browser tools driven by the `computer-use` skill.
- [joey-core.md](joey-core.md) — `~/.joey` layout, config layers, env overrides.
- [joey-cli.md](joey-cli.md) — `joey skills` in the command tree, REPL/TUI slash commands.
