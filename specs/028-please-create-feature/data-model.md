# Data Model: Context Economy

Phase 1 output. Entities from the spec's Key Entities, mapped to concrete structures. No SQLite changes; SCHEMA_VERSION stays 22.

## Scratchpad (FR-001/002/003)
- Storage: `~/.joey/scratchpads/<dir-name>/scratchpad.md` where dir-name = `<sanitized session key>-<fnv1a-hex8>` (sanitized = [A-Za-z0-9._-] whitelist of the raw key, collapsing runs; suffix = FNV-1a 64 hex of raw key, collision-proofing).
- File format: append-only entries, newest last; each entry: `## <RFC3339 timestamp> [<optional label>]\n<redacted text>\n`; parsed only for stats (entry count, last-entry time) and read-tail.
- API (joey-tools/src/tools/scratchpad_tool.rs):
  - struct Scratchpad; (unit, session identity via ToolContext::session_id(), todo-tool pattern)
  - actions: append(text, label?) → redact_secrets → size-check → atomic append; read(tail_entries=20, offset?) → tail entries + "showing X-Y of Z" notice; clear() → truncate file (user or assistant initiated); stats() → {entries, total_chars, last_entry_at}
  - pub fn stats(session_id) -> Option<Stats> (cross-crate, todo_tool::current precedent)
- Validation: max_entry_chars 8000 default (reject with guidance); empty text rejected; redaction before persist (FR-002), redaction-empty result rejected.
- Lifecycle: persists after session end (clarified); cleaned only by existing retention policies; user-clearable via clear action.

## State Block (FR-004/005)
- Renderer: joey-agent-core/src/state_block.rs — fn render(input: &StateBlockInput) -> Option<String>.
- StateBlockInput { todos: Vec<TodoItem>, scratchpad_stats: Option<Stats>, turn: usize, max_turns: usize }
- Output fixed sections: TASKS / SCRATCHPAD pointer / PROGRESS, hard bound state_block.max_chars (1200), deterministic truncation (tail-first over TASKS items, headers kept), None when no todos AND no scratchpad entries.
- Injection (agent.rs): field `state_block_context: Mutex<Option<(String /*key*/, String /*block*/)>>`; render once per turn (dedupe on last-user-text key, neurocode pattern agent.rs:1959-1963); build_request appends Message::user(block) to the request CLONE only. Ordering guard: skip when history tail is unresolved tool results.

## Condensed Tool Result (FR-006/007)
- In-history representation: content replaced in-place with compressor pass-2 one-line summary or PRUND_TOOL_PLACEHOLDER... exact wording: PRUNED_TOOL_PLACEHOLDER = "[Old tool output cleared to save context space]" (compressor.rs:173), reused verbatim.
- Trigger: pre-API pressure branch, ratio in [midturn_threshold, compression.threshold), tail-protected; dedup-first.
- Budget: shared turn-local compression_attempts (MAX 3), shared failure cooldown fields.

## Boundary Cleanup (FR-004/008)
- No new entity: reuses compressor compress() at run_turn exit sites under gate: enabled && todos-complete-or-empty && ratio ≥ boundary_threshold && cooldown clear && budget available. One compression per boundary.
- Post-turn; turn-local attempt accounting (R5c).

## Config keys (FR-013) → contracts/context-economy-config-keys.md

## State transitions
- Scratchpad: empty → active(entries>0) → (clear) empty; persists across session end.
- Session context state: none → state_block rendered per turn (dedupe) → cleared on new user text.
- Hygiene: none → swept (contents rewritten) — irreversible within turn (store keeps verbatim originals).
- Boundary: idle → compressing → done/cooldown.
