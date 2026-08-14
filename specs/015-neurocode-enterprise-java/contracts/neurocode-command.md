# Contract: /neurocode Slash Command

**Spec**: [spec.md](../spec.md) | **Plan**: [plan.md](../plan.md) | **Constitution**: II (CLI/TUI Parity)

The `/neurocode` command is the control surface for the feature — reachable
as a chat slash command AND from the CLI text surface (Constitution II).

## Subcommands

```
/neurocode                           # status: enabled?, indexed?, artifact count, tier config,
                                     #          pega version, pattern counts, domain sources
/neurocode tier [economical|frontier|auto]   # show or set tier for next task / session
/neurocode tier pin <tier>           # pin a tier for the session
/neurocode tier unpin                # revert to automatic
/neurocode index [--force]           # trigger indexing (async)
/neurocode query <type> <symbol>     # direct graph query
/neurocode ingest <category> <path> [--version <v>] [--provenance <p>]
/neurocode patterns                  # list learned patterns
/neurocode anti-patterns             # list learned anti-patterns
/neurocode domain list               # list ingested domain knowledge
/neurocode domain remove <id>        # remove a domain source (conflict resolution)
/neurocode --help                    # exit code 0, usage text
```

## Output format

All output is plain text (Constitution II — text in/out). Example status:

```
NeuroCode: enabled
Index: 1,234 artifacts (last indexed 2026-08-13 14:22)
Pega: Infinity '24 (auto-detected from Gradle BOM)
Tiers: economical=qwen2.5-coder-7b, frontier=claude-3.5-sonnet
Patterns: 12 successful, 3 anti-patterns active
Domain: 4 sources (2 framework docs, 1 entity catalog, 1 postmortem)
```

## Config keys (config.yaml, dotted, additive)

```yaml
neurocode:
  enabled: false                  # default-off (FR-003)
  tier:
    economical:
      model: ""                   # model id for economical tier (Mode 2)
    frontier:
      model: ""                   # model id for frontier tier (Mode 2)
    ambiguous_default: economical # which tier AmbiguousDefault resolves to
  verify:
    steps: []                     # verification step configs (FR-010)
    max_fix_iterations: 3
  classifier:
    scope_fanout_frontier_threshold: 4   # artifacts referenced to lean Frontier
  pega:
    version: ""                   # explicit override (empty = auto-detect, Q4)
```

All keys are additive (`neurocode.*` namespace) and default to safe values
so the feature is useful with zero configuration when enabled. No existing
config key is modified (Constitution VII).

## CLI parity

Every subcommand is also reachable from the CLI (`joey neurocode ...`), with
identical output. The slash-command and CLI paths share the same handler in
`crates/joey-cli/src/commands/neurocode.rs`.
