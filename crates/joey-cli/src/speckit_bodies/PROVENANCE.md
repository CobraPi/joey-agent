# Vendored spec-kit workflow bodies — provenance

The ten `.md` files in this directory are the upstream spec-kit workflow
command bodies, vendored **byte-verbatim** (frontmatter included).

- Upstream repo: `github/spec-kit`
- Local checkout: `/Users/jo110366/Development/spec-kit`
- Upstream commit: `e3e6a3c87ba4f7b6138856b483becf8e69cc9610`
- Source directory: `templates/commands/`
- Date vendored: 2026-09-03

## File list (byte sizes)

| File | Bytes |
|---|---|
| `analyze.md` | 11351 |
| `checklist.md` | 21970 |
| `clarify.md` | 19022 |
| `constitution.md` | 9778 |
| `converge.md` | 12383 |
| `implement.md` | 12409 |
| `plan.md` | 7675 |
| `specify.md` | 18083 |
| `tasks.md` | 10822 |
| `taskstoissues.md` | 7439 |

## Why the upstream checkout is the source (not `~/.joey/skills`)

The copies installed under `~/.joey/skills/speckit-<name>/SKILL.md` have
their YAML frontmatter stripped (that is how skills are materialized for
the agent), so they cannot serve as a byte-faithful source. The upstream
spec-kit checkout retains the complete files — frontmatter, handoffs,
scripts, tools — and is therefore the canonical source for vendoring.

## Refresh procedure

1. Re-copy from the upstream checkout:
   ```bash
   for f in specify clarify plan constitution checklist tasks analyze implement converge taskstoissues; do \
     cp /Users/jo110366/Development/spec-kit/templates/commands/$f.md \
        crates/joey-cli/src/speckit_bodies/$f.md; done
   ```
2. Verify byte-identity:
   ```bash
   for f in specify clarify plan constitution checklist tasks analyze implement converge taskstoissues; do \
     cmp /Users/jo110366/Development/spec-kit/templates/commands/$f.md \
        crates/joey-cli/src/speckit_bodies/$f.md || echo MISMATCH-$f; done
   ```
   The loop must print nothing.
3. Update the commit hash and date in this file.
4. Run `cargo test -p joey-cli` — the bundled-body tests assert frontmatter
   shape (handoffs/tools/scripts) and will surface upstream drift.
