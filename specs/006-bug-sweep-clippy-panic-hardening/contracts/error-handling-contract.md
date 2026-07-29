# Contract: Error Handling & Observability (FR-005, FR-006, FR-008, FR-009)

**Feature**: `006-bug-sweep-clippy-panic-hardening`
**Scope**: P3 panic-hardening track (applies to the 7 in-scope crates)

This contract defines the wire shape of hardened error paths. It is the
authoritative reference implementers copy verbatim; deviations require a
Complexity Tracking entry.

---

## 1. Error propagation (FR-005)

### 1.1 Vehicle — reuse each crate's existing error type

| Crate | Error type | Construct for malformed external input |
|---|---|---|
| `joey-providers` | `ProviderError` (`src/error.rs`) | `ProviderError::Parse(sanitized)` |
| `joey-mcp` | `RequestError` (`src/lib.rs`, private) | `RequestError::Rpc { message: sanitized }` |
| `joey-core` | `anyhow::Result` | `.context("...")` on the failing op, or `anyhow::anyhow!("...")` |
| `joey-cron` | `anyhow::Result` | `.context("jobs.json: ...")` |
| `joey-gateway` | `anyhow::Result` | `.context("...")` |
| `joey-tools` | tool `Result` / `LspError` | existing variant |
| `joey-agent-core` | `anyhow::Result` (turn loop) | `.context("provider/model decode: ...")` |

No new error type is introduced.

### 1.2 Sanitization (FR-008)

Before a string derived from untrusted input reaches an error message, it
MUST pass through the existing redaction layer if it may contain
secret-adjacent content:

```rust
use joey_core::redact::redact_sensitive_text;
let safe = redact_sensitive_text(&raw_untrusted);
// use `safe` in the error message, never `raw_untrusted`
```

For provider/MCP JSON, the typical pattern is to include only the *field
name* that was missing/invalid, not the raw JSON body:

```rust
// GOOD
ProviderError::Parse(format!("missing required field 'content' in response"))

// BAD — leaks raw untrusted JSON
ProviderError::Parse(format!("bad response: {}", raw_json_value))
```

---

## 2. Recovered-fallback observability event (FR-009)

When a hardened site recovers via an explicit fallback (rather than
propagating the error), it MUST emit exactly one structured `tracing::warn!`:

### 2.1 Canonical shape

```rust
tracing::warn!(
    target: env!("CARGO_CRATE_NAME"),
    error = %sanitized_error_description,
    input_kind = "<vocabulary value>",
    path = concat!(file!(), ":", line!()),
    "recovered from malformed external input via fallback"
);
```

### 2.2 `input_kind` vocabulary (fixed set)

| Value | Use when the untrusted source is |
|---|---|
| `"provider_json"` | provider chat-completion / responses JSON body |
| `"provider_sse"` | provider SSE stream chunk |
| `"mcp_jsonrpc"` | MCP JSON-RPC request / response / notification |
| `"jobs_json"` | `jobs.json` (joey-cron schedule file) |
| `"sqlite_row"` | SQLite decoded row (session store) |
| `"config_file"` | `config.yaml` / `.env` / profile config |
| `"context_file"` | context or skill file read from disk |
| `"auth_store"` | auth-store credential decode |
| `"gateway_message"` | platform-adapter message-event decode (joey-gateway) |

If a site does not fit any value, add a new value here and to research.md
R3 (do not overload an existing value).

### 2.3 Field rules

| Field | Rule |
|---|---|
| `target` | Always `env!("CARGO_CRATE_NAME")`. Never a hardcoded string. |
| `error` | Display-formatted (`%expr`). Sanitized via §1.2. Never the raw input. |
| `input_kind` | One of the vocabulary values above. |
| `path` | `concat!(file!(), ":", line!())` for compile-time `file:line`, OR a stable string literal identifying the call site. |
| message | The fixed convention string above. |

### 2.4 When NOT to emit

- **Propagated typed error (no fallback)**: do not emit. The error surfaces
  at the caller's boundary; the caller decides logging.
- **Provably-safe retained `.unwrap()`** (tier *safe*, FR-007): does not
  emit (it cannot fail).
- **Happy path**: never emits (by definition — the event is for recovery).

---

## 3. Regression test contract (FR-006, SC-005)

### 3.1 Granularity

**One dedicated malformed-input regression test per hardened
`.unwrap()`/`.expect()` call site** (Clarifications answer A). Not per
format/protocol.

### 3.2 Test shape

Each test:

1. Constructs the specific malformed input that would have triggered the
   panic at the hardened site (missing field, wrong type, truncated
   stream, non-UTF-8 bytes, corrupt row, etc.).
2. Invokes the now-hardened function.
3. Asserts the function returns a typed error (or performs the documented
   fallback) — **not** a panic.
4. Asserts the error path did not leak raw untrusted content into a
   user-visible string (where feasible via the redaction layer).

```rust
#[test]
fn provider_response_missing_content_field_returns_parse_error_not_panic() {
    let malformed = r#"{"choices":[{"message":{}}]}"#;  // no "content"
    let result = parse_provider_response(malformed);
    assert!(matches!(result, Err(ProviderError::Parse(_))), "got {:?}", result);
    // (optional) assert the error message does not echo the raw body
}
```

### 3.3 Naming convention

`<function_or_path>_<malformed_condition>_returns_<outcome>_not_panic`

### 3.4 Location

Tests live per-crate under `crates/<crate>/tests/` (integration) or inline
`#[cfg(test)]` (unit), matching the existing convention for that crate.

---

## 4. SAFETY comment contract (FR-007)

Retained `.unwrap()`/`.expect()`/`unreachable!()`/`panic!()` sites MUST
carry an inline comment explaining why they cannot panic.

### 4.1 Accepted forms

```rust
// SAFETY: `SCHEMA_VERSION` is a compile-time constant; parse cannot fail.
const SCHEMA: &str = SCHEMA_VERSION.unwrap();
```

```rust
// invariant: index is bounded by the loop `0..len` above.
let v = arr[i].expect("bounded by loop");
```

### 4.2 The audit script (FR-010) checks for these

`scripts/audit-external-input-unwraps.sh` flags any **safe**-tier retained
unwrap that lacks a `// SAFETY:` or `// invariant:` comment on the
preceding non-blank line. Adding the comment clears the flag.
