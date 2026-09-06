# Contract: Configuration Keys

New additive section in config.yaml (defaults merged from DEFAULT_CONFIG_YAML; CONFIG_VERSION bump, additive-only).

| Key | Type | Default | Effect |
|---|---|---|---|
| speckit.enabled | bool | true | Master switch. false → every new code path short-circuits; behavior identical to pre-feature (FR-013) |
| speckit.lifecycle_context | bool | true | Session-start lifecycle detection + one-time context injection (D5) |
| speckit.hooks | bool | true | extensions.yml discovery + hook execution (20 points) |

Invariants:
1. Keys are plain dotted config paths (no secrets; nothing routes to .env).
2. All three are independent; enabled=false overrides sub-toggles.
3. Absence of the section behaves exactly as defaults (older config files unaffected — additive merge).
4. Detection no-ops (one stat of .specify/) on non-spec-kit repositories; no per-turn cost regardless of setting.
