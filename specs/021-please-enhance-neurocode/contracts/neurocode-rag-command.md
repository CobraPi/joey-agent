# Contract: /neurocode Slash Command (RAG additions)

**Spec**: [spec.md](../spec.md) | **Plan**: [plan.md](../plan.md) | **Constitution**: II (CLI/TUI parity)

Additions to the existing `/neurocode` command. Existing subcommands
(`tier`, `index`, `query`, `ingest`, `patterns`, `anti-patterns`,
`domain`, `--help`) are untouched.

## Grammar additions

```
/neurocode search <query...> [--path <glob>] [--limit <n>] [--expand-lines <n>] [--relations <0-2>] [--json]
/neurocode consent show|ack|revoke
/neurocode model fetch [<profile>] [--dylib]
```

- `search` renders ranked results in CLI plain text by default; `--json`
  emits the tool-shaped payload (see
  [neurocode-rag-tools.md](./neurocode-rag-tools.md)) — identical shape,
  Principle II parity.
- The **TUI renders the same payload** through its result view; no
  TUI-only state or divergent formatting (constitution II, III).
- `--path` maps to `file_filter`; `--limit` to `limit`; `--expand-lines`
  to `expand_lines`; `--relations` to `relation_depth` (validated 0–2).

## Consent subcommand (FR-012)

`consent show` prints the current state, the target backend base_url,
and the model — e.g.:

```
Consent: never_acknowledged
Backend: https://api.voyageai.com (remote)
Model:   voyage-code-3
```

`consent ack`:
- Requires **explicit confirmation** (interactive prompt restating that
  this repository's code will be sent to the remote service); a declined
  or non-interactive confirmation aborts with no state change.
- Writes `consent.json` with `state: "Acknowledged"` plus the audit
  fields (see [data-model.md](../data-model.md) §8).

`consent revoke`:
- Sets `state: "Revoked"` (writes `revoked_at`).
- **Immediate effect**: subsequent remote embedding calls for this
  project stop — refreshes and searches use local/keyword-only mode
  (FR-012, edge case "consent revoked mid-operation").

**Re-ack after revoke is allowed** (state machine: Revoked →
Acknowledged via the same explicit ack flow).

Absent `consent.json` displays as `never_acknowledged`.

## Model fetch subcommand

`/neurocode model fetch [<profile>] [--dylib]` — pure file management
(no embedding, no index writes beyond the `rag_model_artifacts` row):

- **Without `--dylib`**: fetches the named (or configured-default)
  model profile's artifacts — `model.onnx` + `tokenizer.json` +
  license files — into `neurocode.rag.local.model_dir` from the
  project mirror root (`neurocode.rag.local.mirror_url`), with every
  download **SHA-256-verified against the project-recorded hashes**;
  any mismatch is REFUSED (nothing lands in `model_dir`). The
  `rag_model_artifacts` row is written from the expected hashes at
  fetch time — stricter than manual-placement self-registration (see
  [embedding-backend.md](./embedding-backend.md) § Local Model
  Artifacts & Distribution).
- **With `--dylib`**: fetches the **platform-appropriate ONNX Runtime
  dylib** (platform/arch auto-detected; ~10–31 MB depending on
  platform) to the fetched-copy location, likewise SHA-256-verified
  per-platform against project-recorded hashes. Reuses
  `neurocode.rag.local.mirror_url` — **no new config key**.
- **Empty `mirror_url`** = fetch disabled: a clear message, no
  download, and **never any huggingface.co contact** (R8).
- Error rendering follows the existing neurocode command conventions.

**Parity (FR-009)**: the `model` subcommand is available whenever
neurocode is active (harmless — file management only); with RAG
disabled, all RAG behavior stays byte-identical.

## Status extension

`/neurocode status` output gains a RAG section **only when**
`neurocode.rag.enabled == true` (index state, chunk/vector counts,
model/dim/quantization, last_refresh_at, backend health, consent state,
degradation flags — mirroring the `neurocode_status` tool's rag
section). When disabled, status output is byte-identical to today
(FR-009 parity).

## Index extension

`/neurocode index --force` additionally rebuilds the semantic index
when RAG is enabled. If the configured backend is remote and consent is
not `Acknowledged`, the consent flow is re-prompted before any content
egress; declined consent limits the rebuild to local/keyword-only.

## Conventions preserved

- Exit codes and error rendering follow the existing neurocode command
  conventions.
- Unknown-subcommand behavior is unchanged (existing usage/error path).

## Test obligations

1. **Grammar parse tests** — every flag combination above parses to the
   documented SearchRequest mapping; invalid `--relations 3` and missing
   `<query...>` produce the standard argument-error rendering.
2. **Parity when disabled** — with RAG disabled, `/neurocode status`
   output and all pre-existing subcommand outputs are byte-identical;
   `search`/`consent` follow the defined unknown/disabled behavior.
3. **Consent state-machine CLI test** — show → ack (confirmed) → show →
   revoke → show → re-ack → show, asserting `consent.json` contents and
   that a mocked remote embed attempt is refused while not
   `Acknowledged`.
4. **Model-fetch grammar parse tests** — `model fetch`, `model fetch
   <profile>`, `model fetch --dylib`, and flag/profile combinations
   parse per the grammar; invalid usage produces the standard
   argument-error rendering.
5. **Hash-refusal test** — a mirror artifact whose SHA-256 differs
   from the project-recorded hash is refused; `model_dir` and
   `rag_model_artifacts` stay untouched.
6. **Empty-mirror message test** — `model fetch` with empty
   `neurocode.rag.local.mirror_url` prints the fetch-disabled message
   and performs no network call.
7. **Disabled-path harmlessness** — with `neurocode.rag.enabled=false`,
   the `model` subcommand remains usable for file management while
   agent session output and RAG behavior stay byte-identical (FR-009
   parity scope).

---
Regression coverage: required by constitution Principle VII — tasks MUST include tests pinning this contract.
