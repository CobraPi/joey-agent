# Research: Context Economy

Phase 0 output. All decisions verified against code facts (file references inline). No NEEDS CLARIFICATION remains.

## R1: Scratchpad storage location and identity
- Decision: `~/.joey/scratchpads/<sanitized-key>-<fnv1a-hex8>/scratchpad.md`, append-only Markdown with atomic writes and flock sibling lock; zero SQLite involvement.
- Rationale: session keys contain arbitrary platform identifier chars (`/`, `:`, Windows-illegal) — session.rs:385-453 builds them by joining raw parts with `:`, no sanitization — so path safety requires both sanitization and a collision-proofing hash suffix. Append-only matches the memory-tool precedent (memory_tool.rs:26-36, atomic_replace + flock, `.bak` on drift). Plain files survive compaction (external to history) and sessions (FR-001 clarified), require no schema change (SCHEMA_VERSION stays 22), and are readable by sub-agents via plain toolsets (FR-012).
- Alternatives considered: (a) new SQLite table — rejected: forces SCHEMA_VERSION 23, breaking FR-014's "session formats and their versions keep working unchanged"; (b) raw session key as dirname — rejected: path traversal/Windows illegality; (c) `.omo/notepads` reuse — rejected: omo-internal, wrong lifecycle (per plan-name), and not available to generic sub-agents.

## R2: Scratchpad post-session discoverability (clarified FR-001)
- Decision: rely on the messages FTS5 index that already indexes tool_calls arguments (state.rs:202-212: `COALESCE(new.content,'') || ' ' || COALESCE(new.tool_name,'') || ' ' || COALESCE(new.tool_calls,'')`). scratchpad append arguments are therefore already searchable via existing session_search for free.
- Rationale: the clarify decision (persist + discoverable via existing session search) is satisfied with zero new machinery. Scratchpad files themselves are plain files under JOEY_HOME, inspectable manually; FTS gives model-facing discoverability.
- Alternatives considered: a dedicated scratchpad index — rejected: duplicate of existing FTS capability (Principle VI/VIII).

## R2b: Scratchpad redaction + size bound
- Decision: every append runs `joey_core::redact::redact_secrets` (the same layer read_file/terminal use, file_tools.rs:354, terminal_tool.rs:531) before write; entries over scratchpad.max_entry_chars (default 8000) are rejected with guidance text.
- Ractionale: FR-002/FR-003; reusing the redaction layer avoids a second implementation (VI); 8000 chars ≈ 2000 tokens, ample for findings while bounded (VIII).

## R2c: Scratchpad registration
- Decision: always-registered like todo/memory tools (unit struct + per-execute ToolContext::session_id() identity, todo_tool.rs:260 precedent), gated by `check()` returning false when scratchpad.enabled is false — the register_session_tools pattern (builtins.rs:47-54). Sub-agents: delegate_task registers builtins for children, so children inherit scratchpad automatically; no orchestration changes needed.
- Rationale: FR-012 satisfied for free; check()-gating keeps default-off = tool absent from UI but registration code simple.
- Alternatives considered: neurocode-rag-style register-nothing gating — rejected: tool absence confuses the state block pointer line (R6) and guidance references.

## R2d: Scratchpad stats API for state block
- Decision: expose `pub fn stats(session_id) -> Option<Stats>` from scratchpad_tool (entries, total chars, last-entry time) reading the file tail cheaply; state block consumes it (R6). Reuses R1 storage.
- R(session): todo_tool::current precedent (todo_tool.rs:71) proves pub cross-crate read APIs on tools are sanctioned.
- Alternatives considered: Agent-held handle threading — rejected: memory_runtime setter pattern (agent.rs:1280) adds wiring; file-read stats is simpler and session-scoped.

## R3: State block mechanism (tail-injection, no persistence)
- Decision: render deterministically from todo_tool::current(session_id) + scratchpad stats (R2d) + turn counter; stash in a `state_block_context: Mutex<Option<(String, String)>>` field mirroring neurocode_context (agent.rs:587), dedupe key = last user text (agent.rs:593 pattern); append the rendered block as a synthetic user-role message to the REQUEST CLONE at build_request time (after ProviderRequest::new(history.clone()), before send) — never to self.history, never persisted (push_synthetic at agent.rs:1555 only exists for history-side synthetic inserts and is NOT used; the request-clone path bypasses push_message sites entirely).
- Rationale: tail = newest message = zero prompt-prefix cache invalidation (the D2 argument); FR-005's retry-identical requirement is satisfied by the per-turn dedupe key (render once, reuse stash, agent.rs:1959-1963 pattern); user-message-after-tool-result legality proven by POST_TOOL_EMPTY_NUDGE precedent (agent.rs:3210-3228). Message content carries a first line `[STATE BLOCK — deterministic, auto-maintained]` so providers/models can distinguish it from user input.
- Alternatives considered: (a) system-prompt injection — REJECTED: violates the once-per-session render invariant (prompt.rs:790, agent.rs:-prompt-built-once) and invalidates prefix cache every turn; (b) push_synthetic to history — rejected: pollutes session transcript/store and session_search results; (c) background-completion notices pattern (push_message at 2800) — rejected: persists (FR-005 forbids).

## R3b: State block ordering guard
- Decision: skip the block for a turn when the history tail is unresolved tool results (trailing tool messages without a following assistant text) — cheap structural check on the last history message(s) before appending to the clone. FR-005's "placed only where it cannot corrupt message ordering".
- Rationale: some providers reject user text directly after tool results; skipping is the safe default (spec edge case).
- Alternatives considered: none needed (cheap guard).

## R3c: State block content and bound
- Decision: fixed section order TASKS (todo render, todo_tool::render markers) → SCRATCHPAD pointer line (path + entry count + last time, never content) → PROGRESS (turn n of max_turns). Hard char bound state_block.max_chars default 1200, truncated tail-first while keeping section headers, deterministic truncation (same input → same output). Empty everything → None (no block).
- Rcess: recency-position argument (story 2): task state rides at the tail where attention is strongest. 1200 chars ≈ 300 tokens — sub-1% of even small windows.
- Alternatives: larger bound — rejected: defeats purpose; unlisted sections (memory, RAG) — deferred: those blocks already render separately.

## R3d: State block default-on wiring
- Decision: state_block.enabled default TRUE (FR-013 default-on mandate) — but all parity guarantees are the when-disabled tests; byte-identical-when-disabled is asserted by a dedicated test (build_request output equality with mechanism off vs pre-feature golden).
- Rationale: FR-013 + FR-005 + VII.

## R3e: Turn counter source
- Decision: reuse the existing turn iteration variable in run_turn's tool loop as the progress numerator; denominator = agent.max_turns config value.
- R: no new state; zero cost.

## R3f: State block sub-agent consideration
- child agents run their own Agent instances; if a child session has todos/scratchpad, a block renders for them too — harmless and consistent (same mechanics). No special handling.

## R4: Hygiene sweep (mid-turn tool-result condensation)
- Decision: new branch in the existing pre-API pressure check (agent.rs:2853-2898): when ratio ≥ compression.midturn_threshold (0.35) AND ratio < compression.threshold (0.50) AND compression.midturn_tool_hygiene enabled: rewrite in-place the CONTENTS of tool-result messages older than the protected tail (reuse the compressor's pass-2 helper: deterministic one-line summary or PRUNED_TOOL_PLACEHOLDER wording, compressor.rs:1325-1404,173) — then re-estimate. When ratio ≥ 0.50 the existing path (full compression) fires as today (backstop unchanged).
- Rationale: attacks the 60-80% tool-output share continuously rather than only at compaction; deterministic pass-2 wording means zero model calls; reusing exact wording keeps one mental model for the model (it already sees this placeholder after compactions). Dedup of identical tool results reuses pass-1 dedup logic location.
- Alternatives considered: (a) full compaction at 0.35 — rejected: emits summary + summary marker (FR-007 requires distinguishability) and burns a model call mid-turn; (a2) hygiene at ratio ≥ 0.50 pre-compression — rejected: redundant with backstop; (b) new placeholder wording — rejected: FR-007 and model-familiarity arguments; (c) JSON field projection — rejected: parity surfaces.

## R4b: Hygiene shared budget
- Decision: thread the existing turn-local `compression_attempts` counter (agent.rs:2826) into the hygiene branch; hygiene consumes an attempt, shares failure cooldown (session fields compression_failure_cooldown_until/error) and MAX_COMPRESSION_ATTEMPTS=3 accounting. Hygiene runs FIRST in the check order; full compression remains the backstop. Never both in one turn.
- R: FR-007 (shared budget, no double compression); verified counter is turn-local &mut-threaded (Gap C Q5), not an Agent field — no new Agent state.
- Alternatives: separate counter — rejected: FR-007 says SHARED budget.

## R4c: Hygiene scope
- Decision: contents only. Hygiene rewrites message content strings in self.history; it does not delete messages, does not touch tool_calls on assistant messages, and does not modify the session store (store rows keep verbatim content; add_message writes happened at execution time). Store fidelity preserved: later compaction's pass-2 sees the same message stream as today.
- R: FR-007 re-fetchability: store keeps verbatim; session_search reads the store; scratchpad holds the agent's own distilled findings.

## R4d: Hygiene disabled parity
- midturn_tool_hygiene default TRUE (FR-013) with when-disabled test = no message content changes below threshold 0.50 (byte-identical history).

## R5: Boundary cleanup
- Decision: at run_turn exit paths (the neurocode_auto_reindex sites pattern, ~agent.rs:2705/2801/2815), gated on compression.boundary_trigger enabled AND todo list all-complete-or-empty (todo_tool::current) AND ratio ≥ compression.boundary_threshold (0.35) AND session-level compression failure cooldown clear AND compression_attempts budget not exhausted for the turn: run the existing compressor compress() once.
- Rationale: natural-boundary timing (cache tradeoff pitfall); todo-complete is the deterministic signal (deterministic state block story); reuses the whole existing compression machinery (aux model call, template, cooldowns) — one new trigger, zero new machinery.
- Alternatives considered: (a) model self-report of "done" — rejected: unreliable; (a2) verification-nudge completion — rejected: coupling two mechanisms; (b) new lighter summary call — rejected: two summarizer implementations (VI); (b2) naive "turn end" without conditions — rejected: would compact mid-task constantly, violating the cache tradeoff.

## R5b: Boundary disabled parity
- boundary_trigger default TRUE with when-disabled test = exit paths take zero extra actions (byte-identical transcript/store).
## R5c: Boundary attempt accounting
- All boundary compressions count against the SAME shared compression_attempts budget (R4b) — a boundary compression exhausts attempts for that turn's future needs... boundary runs post-turn, budget is turn-local, so boundary uses its own fresh turn-local budget slot — accounted as one attempt in its turn.

## R6: Economy guidance (guidance.rs const + gated prompt injection)
- Decision: CONTEXT_ECONOMY_GUIDANCE constant in guidance.rs (Joey-only addition documented in PORTING.md; wording cites this feature), injected via the existing gated pattern (prompt.rs:820-835, like MEMORY/SESSION_SEARCH/SKILLS guidance) gated on the scratchpad tool being present + agent.context_economy_guidance (default true). Mention scratchpad explicitly so the model knows the affordance exists.
- R: D4 (Joey-only channel, verbatim strings untouched); prompt built once per session keeps cache stable (session-stable guidance).
- Alternatives: per-turn reminder messages — rejected: repetition noise + persistence.

## R6b: Guidance content (five points)
- concise (own output is future context); externalize findings early via scratchpad; cite pointers (paths/ids) not pasted content; delegate noisy exploration to sub-agents (distilled conclusions); targeted paginated reads. Wording pinned in contracts/context-economy-guidance.md... (contract name: see FILE 7) contracts/context-economy-guidance.md — NOTE to implementor: use the exact filename given below (contracts/context-economy-guidance.md does not exist; the guidance contract is part of FILE 6 contracts/state-block-injection.md §Guidance).

## R7: Retrieval-verification nudge
- Decision: extend verification.rs verify-on-stop path (verification.rs:515 build_verify_nudge, capped by max_verify_nudges) with a one-line addition when the turn's dedupe keys show rag prefetch or neurocode cold-mode usage: "retrieved facts re-checked against sources before finishing." — same cap, same mechanism.
- R: FR-010 + retrieval-fails-silently caveat; existing capped nudge mechanism reused (VI).
- Alternatives: new per-retrieval nudge — rejected: new uncapped mechanism; hard verification step — rejected: alters turn structure.

## R7b: Verification default-on parity
- retrieval_verification_nudge default TRUE; when-disabled test = verify nudge text identical to pre-feature when retrieval was used.

## R8: Config keys (full contract in contracts/context-economy-config-keys.md)
- 11 additive keys, all default-on (true) except thresholds: scratchpad.enabled, scratchpad.max_entry_chars, state_block.enabled, state_block.max_chars, compression.midturn_tool_hygiene, compression.midturn_threshold, compression.boundary_trigger, compression.boundary_threshold, agent.context_economy_guidance, agent.retrieval_validation_nudge... exact names in contract file. No .env routing (none are secrets); unknown keys ignored as today.

## R9: Registry definitions memoization
- Decision: cache serialized ToolSchema definitions in ToolRegistry behind a mutation counter; definitions() returns cloned cache; invalidate on register/enable/disable/toolset changes.
- R: per-request schema serialization is pure waste (tool_schemas() at agent.rs:1877 serializes every request); memoization is wire-identical output with less CPU. Not strictly required by any FR but is a free perf win aligned with VIII and the smaller-wins dedup goal.
- Alternatives: none needed.

## R9b: `tracing` observability
- Decision: every mechanism emits a one-line tracing::info! at activation (hygiene swept N results, boundary cleanup fired, state block rendered/skipped, scratchpad written) for observability (SC-003 requires measurable evidence; TUI event stream can show these).
- R: tracing is the workspace standard; cheap.

## R10: PORTING.md duty
- Decision: plan documents that implementation MUST add a Joey-only-additions ledger entry for: guidance constant (R6), state-block marker wording (R3), placeholder reuse (R4), config keys (R8), scratchpad tool (R1), registry memoization (R9). Per repo mandate (PORTING.md is a living audit document).
- Alternatives: none.

## R11: Risks (carry to tasks)
- R1 state block vs strict providers — ordering guard (R3b). R2 hygiene+boundary same-turn double-fire — shared turn-local budget (R4b/R5c). R3 boundary at exit sites — must verify current exit-site list during implementation (line numbers drift). R4 scratchpad dir growth — scratchpads are small; existing retention policies apply (clarify A). R5 state block + steers/background notices interplay — block appended after those in clone; render order pinned in contract. R6 guidance wording vs upstream parity — Joey-only channel, PORTING.md entry (R6/R10).
