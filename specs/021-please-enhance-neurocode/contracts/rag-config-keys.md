# Contract: Configuration Keys (neurocode.rag.*)

**Spec**: [spec.md](../spec.md) | **Plan**: [plan.md](../plan.md) | **Data model**: [data-model.md](../data-model.md)

All keys are NEW and namespaced under `neurocode.rag.*`; none rename or
shadow an existing key — the change is backward compatible (constitution
Principle VII).

## Key table

18 keys total:

| Key | Type | Default | Meaning |
|---|---|---|---|
| `neurocode.rag.enabled` | bool | `false` | Master switch; `false` = byte-identical parity with pre-enhancement behavior (FR-009/SC-005). |
| `neurocode.rag.backend` | enum `auto \| local_onnx \| openai_compat \| ollama` | `auto` | Backend selection; `auto` resolves `local_onnx` when `neurocode.rag.local.model_dir` artifacts verify, else keyword-only degradation with indication until a backend is explicitly configured — never an implicit network call. |
| `neurocode.rag.base_url` | string | `http://localhost:11434` | Embedding service base for the SECONDARY HTTP backends; the loopback default counts as fully local. Unused by `local_onnx`. |
| `neurocode.rag.model` | string | `nomic-embed-text-v1.5` | Model **profile name** (R2); `CodeRankEmbed` also supported. See [embedding-backend.md](./embedding-backend.md) § Model Profiles. |
| `neurocode.rag.api_key` | string | empty | Bearer token for OpenAiCompat. **NOTE:** this key ends in `_KEY`, so the existing config layer auto-routes it to `.env` (never `config.yaml`) per repo convention — document this in user-facing docs; reads use the same dotted-path getter either way. |
| `neurocode.rag.local.model_dir` | string | `~/.joey/neurocode/models/<profile>/` | Directory holding `model.onnx` + `tokenizer.json` (+ license files) for `local_onnx`; `~` expanded at load time. Layout + integrity rules: [embedding-backend.md](./embedding-backend.md) § Local Model Artifacts. |
| `neurocode.rag.local.mirror_url` | string | empty | Mirror for `/neurocode model fetch`. EMPTY = fetch disabled — joey never downloads models. Only ever pointed at project-controlled mirrors; artifacts SHA-256-verified against project-recorded hashes (R8). |
| `neurocode.rag.local.ort_dylib_path` | string | empty | Path to the ONNX Runtime dylib for `ort` `load-dynamic`. Empty = `ORT_DYLIB_PATH` env var or system lookup. |
| `neurocode.rag.batch_size` | int | `64` | Embedding batch size; validated range **16–128** (invalid → config-load warning + fallback to default, not a crash). |
| `neurocode.rag.top_k` | int | `10` | Default result limit for hybrid search. |
| `neurocode.rag.context_window_lines` | int | `20` | ± context lines per result; clamped 0–200. |
| `neurocode.rag.relation_max_depth` | int | `2` | Max BFS depth for relation expansion; clamped 0–2. |
| `neurocode.rag.include_fallback_chunks` | bool | `true` | FallbackCoarse chunks participate in ranking (FR-014). |
| `neurocode.rag.quantize_threshold` | int | `100000` | Chunk count above which int8 quantization is applied. |
| `neurocode.rag.prefetch.enabled` | bool | `false` | Proactive pre-fetch into agent context (FR-015). **Hard runtime gate:** requires a fully-local backend (`local_onnx` in-process, or a loopback `base_url`) — if the backend is not local at runtime the gate errors the pre-fetch OFF, never on. |
| `neurocode.rag.refresh.max_files_per_turn` | int | `50` | Refresh budget: max files processed per turn (FR-004). |
| `neurocode.rag.refresh.max_bytes_per_turn` | int | `52428800` | Refresh budget: max bytes read per turn (50 MiB) (FR-004). |
| `neurocode.rag.timeout_secs` | int | `30` | HTTP timeout for embedding calls (secondary HTTP backends) and model-fetch downloads. |

## Rules

- **Unknown keys**: unknown `neurocode.rag.*` keys follow the existing
  config-layer behavior verbatim — no new strictness, no new errors.
- **Readability**: every key is readable via the existing dotted-path
  getters (`config.get("neurocode.rag.top_k")` etc.); no bespoke
  accessor API is introduced.
- **Effective time**: changes take effect at the next refresh/turn;
  there is no hot-reload contract within a running turn.
- All defaults above are the safe zero-config posture: disabled, local,
  bounded, and (for models) non-downloading — `mirror_url` empty means
  the default configuration never contacts a network host.

## Example

```yaml
neurocode:
  rag:
    enabled: true
    backend: auto                  # resolves local_onnx when artifacts verify
    base_url: "http://localhost:11434"  # secondary HTTP backends only
    model: "nomic-embed-text-v1.5"      # profile name (CodeRankEmbed supported)
    local:
      model_dir: "~/.joey/neurocode/models/nomic-embed-text-v1.5"
      mirror_url: ""               # empty = /neurocode model fetch disabled
      ort_dylib_path: ""           # empty = ORT_DYLIB_PATH env / system lookup
    batch_size: 64
    top_k: 10
    context_window_lines: 20
    relation_max_depth: 2
    include_fallback_chunks: true
    quantize_threshold: 100000
    refresh:
      max_files_per_turn: 50
      max_bytes_per_turn: 52428800
    timeout_secs: 30
# neurocode.rag.api_key lives in .env (auto-routed by the _KEY rule), e.g.:
#   JOEY_NEUROCODE_RAG_API_KEY=***
```

## Test obligations

- Defaults test: fresh config yields exactly the table's defaults.
- Validation/clamping test: out-of-range ints clamp/replace per rule above.
- `.env` routing test: setting `neurocode.rag.api_key` persists to `.env`,
  not `config.yaml`.
- `~` expansion test: `local.model_dir` is expanded at load time.

---

Regression coverage: required by constitution Principle VII — tasks MUST
include a **config contract test enumerating the full key table (18 keys)
with exact defaults**, pinning this contract against silent additions,
renames, or default drift.
