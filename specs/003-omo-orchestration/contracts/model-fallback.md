# Contract: Model Fallback Chains

**Feature**: 003-omo-orchestration

## Resolution Algorithm

```
resolve_model(requirement, available_models) -> Option<(model, variant)>:
  for entry in requirement.fallback_chain:
    1. Exact match: if entry.model is in available_models → return (entry.model, entry.variant)
    2. Family match: detect ModelFamily of entry.model;
       if any model in that family is in available_models → return (that_model, entry.variant)
  return None  (agent skipped)
```

## ModelFamily Detection

| Family | Prefixes / Patterns |
|--------|-------------------|
| Anthropic | `claude-*` (opus, sonnet, haiku, fable) |
| Gpt | `gpt-*` (5.4, 5.5, 5.6-sol, 5.6-terra, 5.6-luna, codex) |
| Kimi | `kimi-*` (k2, k2.6, k2.7, k3) |
| Glm | `glm-*` (4.6v, 5, 5.1, 5.2) |
| Gemini | `gemini-*` (3-flash, 3.1-pro) |
| Minimax | `minimax-*` (m2.7, m3), `MiniMax-*` |

## Agent Fallback Chains (1-to-1 with OMO source)

### sisyphus (requiresAnyModel: true)
1. claude-opus-4-8 (variant: max) — Anthropic
2. kimi-k3 — Kimi
3. gpt-5.6-sol (variant: medium) — Gpt
4. glm-5 — Glm
5. big-pickle — (fallback)

### hephaestus (requiresProvider: [openai, github-copilot, opencode, vercel])
1. gpt-5.6-sol (variant: medium) — Gpt

### oracle
1. gpt-5.6-sol (variant: xhigh) → 2. gpt-5.6-sol (variant: high, copilot) →
3. gemini-3.1-pro (variant: high) → 4. claude-opus-4-8 (variant: max) →
5. glm-5.2

### librarian
1. gpt-5.4-mini-fast → 2. qwen3.5-plus → 3. minimax-m2.7-highspeed →
4. minimax-m3 → 5. MiniMax-M3 → 6. minimax-m2.7 → 7. claude-haiku-4-5 →
8. gpt-5.4-nano

### explore
(Same chain as librarian)

### multimodal-looker
1. gpt-5.6-sol (variant: low) → 2. kimi-k3 → 3. glm-4.6v → 4. gpt-5-nano

### prometheus
1. claude-opus-4-8 (variant: max) → 2. gpt-5.6-sol (variant: high) →
3. glm-5.2 → 4. gemini-3.1-pro

### metis
1. claude-sonnet-4-6 → 2. claude-opus-4-8 (variant: max) →
3. gpt-5.6-sol (variant: medium) → 4. glm-5.2 → 5. kimi-k3

### momus
1. gpt-5.6-terra (variant: high) → 2. gpt-5.6-terra (variant: high, copilot) →
3. gpt-5.6-sol (variant: xhigh) → 4. gpt-5.6-sol (variant: high, copilot) →
5. claude-opus-4-8 (variant: max) → 6. gemini-3.1-pro (variant: high) →
7. glm-5.2

### atlas
1. claude-sonnet-4-6 → 2. kimi-k3 → 3. gpt-5.6-sol (variant: medium) →
4. minimax-m3 → 5. MiniMax-M3 → 6. minimax-m2.7

### sisyphus-junior
1. claude-sonnet-4-6 → 2. kimi-k3 → 3. gpt-5.6-sol (variant: medium) →
4. minimax-m3 → 5. MiniMax-M3 → 6. minimax-m2.7 → 7. big-pickle

## Category Fallback Chains (1-to-1 with OMO source)

| Category | Chain (abbreviated) |
|----------|-------------------|
| visual-engineering | gemini-3.1-pro (high) → glm-5 → claude-opus-4-8 (max) → glm-5.2 → kimi-k3 |
| ultrabrain | gpt-5.6-sol (xhigh) → gpt-5.6-sol (high, copilot) → gemini-3.1-pro (high) → claude-opus-4-8 (max) → glm-5.2 |
| deep | gpt-5.6-terra (xhigh) → gpt-5.6-terra (high, copilot) → gpt-5.6-sol (high) → gpt-5.6-sol (medium) → claude-opus-4-8 (max) → gemini-3.1-pro (high) → kimi-k3 → glm-5.2 |
| artistry | gemini-3.1-pro (high) → claude-opus-4-8 (max) → gpt-5.6-sol (high) → kimi-k3 → glm-5.2 |
| quick | gpt-5.4-mini → claude-haiku-4-5 → gemini-3-flash → minimax-m3 → MiniMax-M3 → minimax-m2.7 → gpt-5-nano |
| unspecified-low | gpt-5.6-luna (xhigh) → gpt-5.6-luna (high, copilot) → claude-sonnet-4-6 → gpt-5.6-sol (medium) → kimi-k3 → gemini-3-flash → minimax-m3 → MiniMax-M3 → minimax-m2.7 |
| unspecified-high | claude-opus-4-8 (max) → gpt-5.6-sol (high) → glm-5 → kimi-k3 → glm-5.2 → kimi-k3 |
| writing | gemini-3-flash → kimi-k3 → claude-sonnet-4-6 → minimax-m3 → MiniMax-M3 → minimax-m2.7 |

## Behavioral Contracts

- **BC-006**: Resolution MUST try chain entries in declared order.
- **BC-007**: Exact model ID match takes priority within an entry; family-
  level matching is the fallback.
- **BC-008**: If no entry resolves, the agent/category is skipped (returns
  None).
- **BC-009**: User-configured model overrides bypass the chain entirely.
- **BC-010**: requiresProvider constraint (Hephaestus) is checked before
  chain resolution; if unmet, the agent is skipped without walking the chain.
