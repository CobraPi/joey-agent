# Contract: Allocation Map JSON Schema

**Feature**: 011-dynamic-llm-selector | **Surface**: on-disk public format
**Path**: `~/.joey/llm-selector/allocations.json` (via `process_joey_home()`)
**Stability**: versioned on-disk public format (Constitution VII, FR-014).
Breaking change requires MAJOR bump + documented migration.
**Initial version**: `schema_version: 1`

This file is the selector's source of truth (Constitution III — filesystem is
authoritative; reads reflect current contents, writes are atomic).

---

## Atomic write contract

Writes use `joey_core::utils::atomic_json_write`
(`crates/joey-core/src/utils.rs:156`): serialize → write to
`<path>.tmp.<pid>.<uuid>` in the same dir → fsync → rename over the target.
A concurrent turn-start read (`refresh_at_turn_start`) therefore never observes
a partial write. Matches the pattern `auth_store`
(`crates/joey-core/src/auth_store.rs:134`) builds on (research.md §3).

Reads use plain `std::fs::read` + `serde_json::from_slice`; a missing file is
NOT an error — it means "cold start, build the map via the scorer".

## JSON schema (v1)

```jsonc
{
  // Constitution VII versioned public format. MUST be 1 for this feature version.
  "schema_version": 1,

  // ISO-8601 UTC timestamp of the last write.
  "updated_at": "2026-08-04T12:00:00Z",

  // Whether dynamic allocation is active. False when auto-disabled (FR-017)
  // or user-disabled via /llm-selector. When false, entries are retained but
  // ignored — the literal cfg.model() is used for all modules.
  "enabled": true,

  // The model id currently acting as the LLM diagnoser (one of the candidates,
  // default versatile tier). Recorded so /llm-selector can report it (FR-001).
  "diagnoser_model": "gpt-4.1",

  // Configured learning budget (max diagnoser/model calls per optimization run).
  // 0 disables learning. Config key: model.selector.budget.
  "learning_budget": 8,

  // Diagnoser calls consumed in the current optimization cycle. Reset when a
  // cycle completes or the feature is re-enabled.
  "budget_used_this_cycle": 0,

  // One entry per module (map semantics — at most one row per ModuleId).
  "entries": [
    {
      // ModuleId serialized snake_case: "main_turn" | "compression" | "subagent"
      // | {"custom":"<name>"}.
      "module": "main_turn",

      // Concrete model id (never "auto"). Validated against the active catalog
      // at resolve time; stale ids are re-resolved (FR-014).
      "model_id": "gpt-4.1",

      // True = user pinned via /llm-selector (FR-012). Exempt from reallocation.
      "pinned": false,

      // True = an existing explicit per-task config (e.g. auxiliary.compression.model)
      // implicitly pins this module (FR-013). Also exempt from reallocation.
      "implicit_pin": false,

      // Human-readable reason for the current assignment (FR-011, SC-008).
      "reason": "cold-start: cheapest versatile tool-capable model",

      // Diagnoser's estimated per-module performance p_j in [0,1]. null until
      // the diagnoser has evaluated this module (FR-008).
      "estimated_performance": null,

      // ISO-8601 timestamp of the last change to this entry.
      "updated_at": "2026-08-04T12:00:00Z"
    }
  ],

  // Bounded ring buffer (last N, default 50) of diagnoser judgments (FR-018).
  "diagnostics": [
    {
      "at": "2026-08-04T12:04:00Z",
      "module": "compression",                 // ModuleId (snake_case / {"custom":..})
      "signal": "empty_response",              // FailureSignal (snake_case)
      "implicated_model": "glm-4.5-flash",     // the model the diagnoser flagged
      "rationale": "empty output on compression call; reallocating to claude-haiku-4-5"
    }
  ]
}
```

## Field reference

| Field | Type | Required | Default | Notes |
|---|---|---|---|---|
| `schema_version` | integer | yes | — | MUST be `1`. Future incompatible change = MAJOR bump. |
| `updated_at` | string (ISO-8601) | no | null | Set on every write. |
| `enabled` | boolean | no | `false` | Activation state (FR-002, FR-017). |
| `diagnoser_model` | string | no | `""` | Defaults to a versatile-tier candidate at enable time. |
| `learning_budget` | integer | no | `8` | From `model.selector.budget`. |
| `budget_used_this_cycle` | integer | no | `0` | Reset per cycle. |
| `entries` | array | no | `[]` | At most one per ModuleId. |
| `entries[].module` | ModuleId | yes | — | snake_case enum or `{"custom":"<name>"}`. |
| `entries[].model_id` | string | yes | — | Concrete; validated at resolve. |
| `entries[].pinned` | boolean | no | `false` | FR-012. |
| `entries[].implicit_pin` | boolean | no | `false` | FR-013. |
| `entries[].reason` | string | no | `""` | FR-011. |
| `entries[].estimated_performance` | number\|null | no | `null` | In `[0,1]`. |
| `entries[].updated_at` | string\|null | no | `null` | ISO-8601. |
| `diagnostics` | array | no | `[]` | Bounded (last 50). |
| `diagnostics[].at` | string | yes | — | ISO-8601. |
| `diagnostics[].module` | ModuleId | yes | — | snake_case. |
| `diagnostics[].signal` | enum | yes | — | `turn_error` \| `aux_call_failure` \| `empty_response` \| `retry_triggered`. |
| `diagnostics[].implicated_model` | string | yes | — | |
| `diagnostics[].rationale` | string | yes | — | |

## Validation rules

1. `schema_version == 1` — any other value is a hard error (load fails loudly;
   the selector auto-disables with a notice rather than silently migrating).
2. `entries` MUST NOT contain two rows with the same `module` (map semantics).
3. `estimated_performance` when present MUST be in `[0.0, 1.0]`.
4. `Custom` module names MUST match `^[a-z][a-z0-9_]{0,31}$`.
5. `diagnostics` is trimmed to the last 50 entries on write.
6. `model_id` is NOT validated against the catalog at load time (the catalog is
   profile/account-specific); staleness is detected and re-resolved at resolve
   time (FR-014). This is why a global map can safely hold ids the active
   profile can't access yet.

## Round-trip guarantee (Constitution IV)

Load → modify one entry → save → reload MUST reproduce the modified map
byte-equivalently (modulo `updated_at`). The `map_round_trip.rs` integration
test asserts this including: missing optional fields (defaults applied),
`Custom` modules, pinned/implicit-pin entries, and the diagnostics ring.

## Migration policy

`schema_version` is the contract. A v1→v2 migration:
- MUST be gated on `schema_version` at load.
- MUST be documented in this file and in `PORTING.md`.
- Requires a MAJOR version bump of the feature.
- MUST be lossless and one-way (v1 read → v2 written); the file is never
  silently rewritten in a way that loses pinned entries.
