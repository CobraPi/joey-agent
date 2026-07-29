# Data Model: Code-Hygiene Bug Sweep & Panic Hardening

**Feature**: `006-bug-sweep-clippy-panic-hardening`
**Date**: 2026-07-28

This feature introduces **no new persisted data entities** and **no schema
change**. It is a code-hygiene/hardening feature: the data model is the
existing one, frozen and unchanged (Constitution VII, FR-004). This document
records the *logical* entities the feature operates on, the invariants that
govern them, and the classifications that drive the implementation.

---

## E1 — Kimi Prompt Variant (P1)

The prompt-selection function `for_model(model: &str) -> &'static str`
maps a model id to a static guidance-prompt string. The Kimi family branch
is the entity under correction.

### Fields

| Field | Type | Notes |
|---|---|---|
| `model_id` | `&str` (input) | Lowercased internally; matched against `k2.6`/`k2-6`, `k2.7`/`k2-7` substrings. |
| `ModelFamily` | enum (`Gpt`, `Gemini`, `Glm`, `Kimi`, ...) | Detected via `ModelFamily::detect(model)` (existing). |
| resolved prompt | `&'static str` | One of `kimi_k2_6()`, `kimi_k2_7()`, `kimi_k3()`, or the non-Kimi defaults. |

### State transition (the bug → the fix)

```
            BEFORE (buggy)                          AFTER (fixed)
model id ────────────────────────────── model id ──────────────────────────────
"kimi-k2.6"  ──► for_model() ──► kimi_k2_7()  ❌   ──► for_model() ──► kimi_k2_6()  ✅
"kimi-k2-6"  ──► for_model() ──► kimi_k2_7()  ❌   ──► for_model() ──► kimi_k2_6()  ✅
"kimi-k2.7"  ──► for_model() ──► kimi_k2_7()  ✅   ──► for_model() ──► kimi_k2_7()  ✅ (unchanged)
"kimi-k3"    ──► for_model() ──► kimi_k3()    ✅   ──► for_model() ──► kimi_k3()    ✅ (unchanged)
other Kimi   ──► for_model() ──► kimi_k3()    ✅   ──► for_model() ──► kimi_k3()    ✅ (unchanged)
non-Kimi     ──► for_model() ──► <family>()  ✅   ──► for_model() ──► <family>()  ✅ (unchanged)
```

### Validation rules (from spec FR-001, FR-002)

- `k2.6`/`k2-6` (case-insensitive substring) MUST resolve to `kimi_k2_6()`.
- `k2.7`/`k2-7` MUST resolve to `kimi_k2_7()` (regression guard).
- Kimi ids matching neither MUST resolve to `kimi_k3()` (fall-through, unchanged).
- Non-Kimi ids MUST resolve byte-for-byte as on `master` (regression guard).

### Identity / uniqueness

The model-id matching is substring-based and order-sensitive within the
Kimi arm: `k2.7` is checked before `k2.6` (cannot match each other). No
uniqueness conflict.

---

## E2 — Panic-Risk Tier (P3)

Each `.unwrap()`/`.expect()` call site is classified into exactly one tier.
This classification drives the hardening pass and is the unit of
FR-006/SC-004 measurement.

### Tiers

| Tier | Definition | Required action |
|---|---|---|
| **safe** | The value provably cannot be `None`/`Err` at this site (e.g. parsing a compile-time constant, indexing a just-built array with a known-valid index, `Lazy::force` on a once-cell). | MAY be left as `.unwrap()`/`.expect()` but MUST carry a `// SAFETY:` or `// invariant:` comment explaining why (FR-007). |
| **external-input** | The value comes from untrusted data (provider JSON/SSE, MCP JSON-RPC, file/config/SQLite/auth decode, `jobs.json`, context/skill files). | MUST be converted to a propagated typed error or an explicit logged fallback (FR-005). MUST ship a dedicated malformed-input regression test (FR-006). |
| **internal-but-recoverable** | The value comes from internal data that is not external input but could still fail (e.g. a hashmap lookup after a code path that may not have populated it). | SHOULD propagate a typed error; fallback with `warn!` acceptable for non-critical paths. |

### Classification source of truth

The `scripts/audit-external-input-unwraps.sh` script (FR-010) is the
enumeration tool. Its heuristic (research.md R4) classifies sites. A site
is **external-input** if its enclosing function matches the curated
path/function allowlist; otherwise **safe** unless manual review
reclassifies it (in which case the reclassification is recorded in the
increment's commit message).

### State transition per site

```
BEFORE                           AFTER
─────────────────────────────    ─────────────────────────────
external-input:                  external-input:
  .unwrap() / .expect()    ──►     ? -> typed error propagation
                                    OR explicit fallback + warn!
                                    + 1 regression test (FR-006)

safe (no comment):               safe (with comment):
  .unwrap() / .expect()    ──►     .unwrap() / .expect()
                                    + // SAFETY: ... (FR-007)
                                  OR idiomatic non-panicking form
                                    (e.g. .unwrap_or_default() if
                                     the default is correct)
```

---

## E3 — Public-Surface Contract (frozen, all tracks)

The set of things no change in this feature may break. Used as the
regression gate (SC-001, SC-006).

### Members

| Surface | Where | How verified |
|---|---|---|
| Public Rust APIs (`pub fn`/`pub struct`/`pub trait` signatures) | all crates | `cargo build --workspace` + existing tests |
| CLI flags & exit codes | `joey-cli` clap tree | existing CLI tests |
| Config keys (dotted paths) | `joey-core` config | existing config tests |
| On-disk formats | SQLite `SCHEMA_VERSION = 22`, `~/.joey/`, `jobs.json`, `.env` | existing format/round-trip tests; `~/.hermes` rename compatibility |
| Wire payloads | provider request/response JSON, SSE event grammar, MCP JSON-RPC | existing provider/MCP tests |
| Trait definitions | `Tool`, `PlatformAdapter`, `Agent`, etc. | existing trait-impl tests |
| Guidance prompt text (existing) | `junior.rs` prompt functions except the new `kimi_k2_6()` | pointer-equality assertions in T005 (all families) implicitly guard byte-equality — no separate test needed |

### Invariant

No member of this set may change without a MAJOR-version bump + documented
migration (Constitution VII). This feature introduces **zero** changes to
this set. The only addition is the new `kimi_k2_6()` `pub fn`, which is an
*addition* (strictly additive, non-breaking) within an existing module —
not a modification of an existing public surface.

---

## E4 — FR-009 Observability Event (logical shape, P3)

The canonical structured `tracing::warn!` event emitted by recovered
fallbacks. This is a logical contract (not a persisted entity); its
physical shape is documented in `contracts/error-handling-contract.md`.

### Fields

| Field | Type | Value domain |
|---|---|---|
| `target` | `&'static str` | `env!("CARGO_CRATE_NAME")` (e.g. `"joey_mcp"`) |
| `level` | — | `WARN` (fixed) |
| `error` | `String` (Display) | sanitized, redacted error description (never raw input) |
| `input_kind` | `&'static str` | one of the fixed vocabulary (research.md R3) |
| `path` | `&'static str` | `file:line` or stable call-site identifier |
| message | format string | `"recovered from malformed external input via fallback"` (convention) |

### Validation rules

- Raw malformed input MUST NOT appear in any field (FR-008).
- `error` field passes through `joey_core::redact::redact_sensitive_text`
  if it may contain secret-adjacent content.
- Propagated typed errors (no fallback) need NOT emit this event (they
  surface at the caller's boundary).

---

## No persisted-schema entities

This feature adds no tables, no columns, no new on-disk files (except the
dev-time `scripts/audit-external-input-unwraps.sh`, which is not runtime
data). SQLite `SCHEMA_VERSION` stays 22. The `~/.joey` directory layout is
unchanged. `jobs.json` format is unchanged.
