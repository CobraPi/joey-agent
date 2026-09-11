# Contract: neurocode.memory.* config keys

Additive key specs in the `RAG_CONFIG_KEYS` pattern (`crates/joey-neurocode-rag/src/config.rs`), loaded by a `MemoryConfig::load(&Config)` following `RagConfig::load` (clamped ints, warned-and-fallback strings, env-first only for secrets — none here).

| Key | Kind | Default | Validation / clamp |
|-----|------|---------|--------------------|
| neurocode.memory.enabled | Bool | false | gates every capture/injection/distill path; false = byte-identical existing behavior |
| neurocode.memory.top_k | Int | 5 | 1..=20; applies per section (preferences and episodes each return up to top_k) |
| neurocode.memory.injection_char_limit | Int | 2048 | 256..=8192 |
| neurocode.memory.max_episodes | Int | 500 | 50..=10000 (FIFO eviction) |
| neurocode.memory.distill_model | Str | "" | empty = resolve via NeuroCode economical tier |

Rules: keys read with the documented defaults when absent; unknown `neurocode.memory.*` keys are ignored as today; no key is auto-routed to `.env` (none are secrets); all keys are optional and additive — existing configs are valid unchanged.
