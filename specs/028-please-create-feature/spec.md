# Feature Specification: Context Economy

**Feature Branch**: `028-please-create-feature`

**Created**: 2026-09-10

**Status**: Draft

**Input**: User description: "please create this feature - make sure to enable all the new features by default" — building on the user's detailed context-economy request: the practical goal is functional losslessness — everything needed stays reachable, not necessarily resident; context is treated as a small working set with pointers, not a complete record. Six techniques: (1) externalize state aggressively, with findings written to a scratchpad as they are discovered so later cleanup is safe; (2) ruthless tool-output management (relevant fields only, pagination, one-line summaries after processing, deduplication); (3) sub-agents for context isolation returning distilled conclusions; (4) compaction with structured summaries that preserve decisions, exact IDs/paths/values, constraints, and open questions, keeping recent turns verbatim and never dropping the current task; (5) a pinned, deterministically-maintained current-state block; (6) just-in-time retrieval with verification rather than silent retrieval misses. Smaller wins: concise-output instruction, pointer citation over content pasting. Pitfalls to mitigate: lossy summaries, broken references to pruned content (stable markers plus re-fetch), cache-invalidation cost of deletion (compact at natural boundaries, not continuously), and attention degradation in over-long contexts.

## Clarifications

### Session 2026-09-10

- Q: What happens to scratchpad content when its session ends? → A: Kept on disk after session end; discoverable via existing session search; cleaned only by existing retention policies (no new retention machinery).

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Details Survive Context Cleanups (Priority: P1)

As a developer working through a long task with the assistant, I want every important discovery — file paths, identifiers, decisions, exact values — recorded outside the conversation as it is found, so that when older conversation is summarized or cleared, nothing critical is lost and I never have to re-provide or re-discover it.

The assistant keeps a session scratchpad: a running external record it appends findings to as it works, and can read back on demand. Because details are externalized before any cleanup, compaction becomes safe: the conversation can shrink while everything remains reachable through the scratchpad and other re-read mechanisms.

**Why this priority**: This is the single biggest structural lever — it makes every other cleanup mechanism safe. Without externalization, summarization is forced to gamble on what to keep.

**Independent Test**: Can be fully tested by starting a long task, letting the assistant discover several exact values (paths, identifiers), triggering a context cleanup, then asking for those values — they must be recoverable without the user re-stating them.

**Acceptance Scenarios**:

1. **Given** the assistant has discovered key facts (a config path, an error code, a decision) during a task, **When** context cleanup summarizes the older conversation, **Then** the facts remain recoverable (from the scratchpad or re-readable sources) and the assistant can cite them exactly.
2. **Given** the user asks "what did you find so far?", **When** the assistant answers, **Then** it can enumerate its recorded findings with their exact values, not paraphrases.
3. **Given** a fresh session with no findings recorded, **When** the scratchpad is consulted, **Then** behavior is unchanged from today — no errors, no empty placeholder noise.

---

### User Story 2 - The Assistant Always Knows the Current Plan (Priority: P1)

As a user on step 40 of a 60-step task, I want the assistant to always see a short, accurate picture of the current state — open tasks, key references, progress — without it re-reading the whole history, so that it never loses the thread, repeats finished work, or forgets the goal.

A deterministic current-state block (task list, pointers to key references, progress) is maintained and shown to the assistant on every turn. It is produced by rules, not by memory or recollection, so it cannot drift or hallucinate; and because it rides at the newest end of the conversation, refreshing it does not disturb cached earlier content.

**Why this priority**: Together with story 1 this is the core of a small working set: pinned state plus external pointers replace full-history recall. It directly counters mid-context attention degradation.

**Independent Test**: Can be fully tested by running a long multi-step task with a long task list, then at a late step asking "what's left?" — the answer must match the deterministic task list without the assistant re-reading history.

**Acceptance Scenarios**:

1. **Given** a session with an active task list and recorded findings, **When** any turn executes, **Then** the assistant's view includes a bounded current-state block reflecting task-list status and pointers, rendered identically on retries within the same turn.
2. **Given** the task list is empty and nothing is recorded, **When** a turn executes, **Then** no state block is added (no cost, no noise).
3. **Given** a very long task list, **When** the state block renders, **Then** it stays within its size bound (truncated deterministically, most important information kept) and never exceeds it.

---

### User Story 3 - Long Tool-Heavy Sessions Stay Lean (Priority: P2)

As a user whose sessions involve dozens of file reads, searches, and command runs, I want already-processed tool output to stop occupying conversation space, so that sessions remain fast and inexpensive and the assistant's attention stays on current work rather than stale dumps.

Once a tool result is older than a protected recent window and overall usage crosses a moderate threshold, its content is condensed in place to a one-line summary with a stable marker; the underlying detail stays recoverable by re-reading the source. Full summarization remains the backstop for extreme growth.

**Why this priority**: Tool output is the dominant consumer in agentic sessions (commonly 60–80%); condensing it continuously is the biggest continuous saving, but it depends on stories 1–2 making condensation safe.

**Independent Test**: Can be fully tested by running a session with 30+ tool calls, then verifying the assistant's active view contains one-line summaries for stale results while it can still answer questions about them by re-reading sources.

**Acceptance Scenarios**:

1. **Given** a session where many tool results exist and usage crosses the hygiene threshold, **When** the next turn executes, **Then** tool results older than the protected window appear as one-line summaries with markers, and results inside the window remain verbatim.
2. **Given** a condensed tool result the assistant needs again, **When** it re-reads the source (file, search, or session history), **Then** the full detail is recoverable.
3. **Given** identical tool results repeated in one session, **When** hygiene runs, **Then** duplicates are collapsed rather than condensed one-by-one.

---

### User Story 4 - Cleanups Happen at Natural Stopping Points (Priority: P2)

As a cost-conscious user, I want context summarization to run when a task completes (a natural boundary) rather than mid-task whenever a size threshold trips, so that I pay a one-time recompute at a boundary instead of repeated mid-work disruptions that discard cached work and interrupt coherence.

When a task's steps are complete (task list finished or empty) and usage is moderately elevated, cleanup runs once at that boundary. The existing pressure-based trigger remains as a backstop for sessions that grow without boundaries, and failure cooldowns are respected either way.

**Why this priority**: This optimizes the cost/coherence tradeoff of cleanup timing; valuable but dependent on the safety work of stories 1–2.

**Independent Test**: Can be fully tested by completing a task with usage above the boundary threshold but below the emergency threshold and observing cleanup occurs exactly once at completion; and separately by confirming the emergency trigger still protects sessions that grow with open tasks.

**Acceptance Scenarios**:

1. **Given** all task-list items are complete and usage is above the boundary threshold but below the emergency threshold, **When** the turn ends, **Then** cleanup runs exactly once.
2. **Given** open task-list items and usage above the emergency threshold, **When** the next turn executes, **Then** the existing pressure-based cleanup still protects the session (backstop preserved).
3. **Given** a recent cleanup failure, **When** boundary conditions are met, **Then** cooldown behavior prevents immediate re-attempts, exactly as today.

---

### User Story 5 - On-Demand Loading Is Verified and Output Stays Concise (Priority: P3)

As a user who relies on the assistant loading just-in-time context (code structure, retrieved knowledge) instead of preloading everything, I want loaded facts to be verified against their sources before work completes, and I want the assistant to keep its own output concise and cite pointers instead of pasting content, so that silent retrieval misses don't produce wrong answers and the conversation doesn't bloat from the assistant's own verbosity.

When a turn used on-demand retrieval, the assistant must re-check the facts it used against source material before declaring done (within existing verification caps). Standing guidance favors: concise responses (the assistant's own output is future context), externalizing findings early, citing pointers, delegating noisy multi-source exploration to sub-agents that return distilled conclusions, and targeted reads over whole-file loads.

**Why this priority**: Refines reliability and steady-state economy; valuable but secondary to the structural stories.

**Independent Test**: Can be fully tested by running a task where retrieval returns a plausible-but-stale fact and checking the assistant re-verified against source before finishing; and by observing response length on repeated similar tasks without information loss.

**Acceptance Scenarios**:

1. **Given** a turn where code context or retrieved knowledge was loaded on demand, **When** the assistant prepares to finish, **Then** it is prompted to confirm the used facts were checked against their sources (bounded number of prompts per session).
2. **Given** a broad multi-file exploration request, **When** the assistant handles it, **Then** noisy gathering is delegated to sub-agents and only distilled conclusions enter the main conversation.
3. **Given** any turn, **When** the assistant responds, **Then** standing economy guidance is in effect (concise, pointer-citing), and disabling the guidance removes it.

---

### User Story 6 - Everything On by Default, Individually Switchable (Priority: P3)

As a user who just wants better sessions without fiddling with settings, I want all the new mechanisms active from the first run after upgrade, each with a simple switch to turn it off, so that I get the benefits immediately and can isolate or disable any mechanism if it ever misbehaves.

All mechanisms ship enabled by default. Each is independently disableable through the existing configuration surface. With any mechanism disabled, the system behaves exactly as it did before the feature existed — identical outputs, formats, and performance characteristics.

**Why this priority**: The user explicitly requested default-on; the switches are the safety net that makes default-on responsible.

**Independent Test**: Can be fully tested by flipping each switch off one at a time and verifying behavior matches the pre-feature system for that mechanism, and by confirming a fresh install needs no setup to benefit.

**Acceptance Scenarios**:

1. **Given** a fresh installation with no user configuration, **When** a session runs, **Then** all new mechanisms are active (scratchpad available and encouraged, state block on, tool-output hygiene on, boundary cleanup on, economy guidance on, retrieval verification on).
2. **Given** any single mechanism disabled, **When** sessions run, **Then** behavior is identical to the pre-feature system for everything that mechanism touches, while other mechanisms continue.
3. **Given** all mechanisms disabled, **When** sessions run, **Then** end-to-end behavior is indistinguishable from the pre-feature system.

### Edge Cases

- What happens when the task list is empty and the scratchpad has no entries? No state block is rendered and no placeholder content is injected — zero overhead, zero noise.
- What happens when a scratchpad entry contains secrets or credentials? They are redacted before anything is persisted, consistent with the platform's existing secret-handling protections.
- What happens when a scratchpad entry is extremely large? Oversized entries are rejected with guidance to summarize, keeping the scratchpad useful and bounded.
- What happens when hygiene and full cleanup would both fire in the same turn? A shared attempt budget prevents double compression; hygiene runs first, full cleanup remains the backstop.
- What happens when sessions grow with no natural boundary (one endless task)? The existing pressure-based cleanup still protects the session; boundary alignment only changes timing when a boundary exists.
- What happens when the state block cannot be placed safely for a provider sensitive to message ordering? The block is skipped for that turn rather than risking a malformed exchange.
- What happens when a condensed tool result is referenced later? Stable markers show where detail was cleared; the underlying sources remain re-readable (paginated reads, search, session history search), so the assistant can re-fetch exactly what it needs.
- What happens when the assistant cites something that was pruned? Guidance requires pointer citations, and pruned references must remain traceable through markers and re-fetch mechanisms rather than dangling.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST provide a session-scoped scratchpad the assistant can append findings to and read back on demand, kept outside the conversation so its content survives context cleanup; scratchpad content MUST persist after the session ends, remain discoverable through existing session search, and be cleaned only by existing retention policies; it MUST be enabled by default with a disable switch.
- **FR-002**: System MUST redact secrets and credentials from scratchpad content before persisting it.
- **FR-003**: System MUST bound scratchpad entry size, rejecting oversized entries with actionable guidance to summarize.
- **FR-004**: System MUST maintain a deterministic current-state block — task-list status, pointers to key external references, and progress — presented to the assistant each turn, bounded in size, produced by fixed rules rather than model recollection; enabled by default with a disable switch; omitted entirely when there is nothing to show.
- **FR-005**: The current-state block MUST NOT be persisted into the conversation record, MUST render identically across retries within a turn, and MUST be placed only where it cannot corrupt message ordering.
- **FR-006**: System MUST condense already-processed tool results older than a protected recent window into one-line summaries with stable markers once usage crosses a moderate threshold — enabled by default with a disable switch — while results inside the protected window remain verbatim and duplicate results are collapsed.
- **FR-007**: Tool-result condensation MUST remain distinguishable from full context summarization (no compaction summary emitted), MUST share the failure/cooldown/attempt budget with existing cleanup so double compression cannot occur, and MUST leave condensed detail re-fetchable through existing re-read mechanisms.
- **FR-008**: System MUST offer boundary-aligned cleanup: when the task list is complete or empty and usage is above a boundary threshold, cleanup runs exactly once at that turn boundary; enabled by default with a disable switch; the existing pressure-based trigger MUST remain active as the backstop, and failure cooldowns MUST be respected.
- **FR-009**: Existing cleanup protections MUST be preserved: the current task definition is never dropped, recent exchanges stay verbatim, and old tool output is dropped before old conversation.
- **FR-010**: When a turn used on-demand retrieval (code structure context or retrieved knowledge), the system MUST require verification of the used facts against their sources before completion, within existing verification caps; enabled by default with a disable switch.
- **FR-011**: System MUST maintain standing context-economy guidance for the assistant — be concise (own output is future context), externalize findings early, cite pointers instead of pasting content, delegate noisy exploration to sub-agents returning distilled conclusions, and prefer targeted paginated reads; enabled by default with a disable switch.
- **FR-012**: Sub-agent delegation MUST continue to return only distilled conclusions (never full child transcripts) to the main conversation, and delegated sub-agents MUST have scratchpad access for their own findings.
- **FR-013**: All new mechanisms MUST ship enabled by default with no user action required, each individually disableable through the existing configuration surface; with any mechanism disabled, all behavior that mechanism affects MUST be identical to the pre-feature system.
- **FR-014**: The feature MUST NOT change existing public surfaces: on-disk session formats and their versions, stored-record compatibility, command behaviors, and existing configuration keys keep working unchanged; existing summarization protections (structured template, verbatim recent turns, task preservation) are reused, not replaced.

### Key Entities *(include if feature involves data)*

- **Scratchpad**: Session-scoped external record of findings (paths, identifiers, values, decisions) appendable and re-readable by the assistant; survives context cleanup and persists beyond its session, discoverable via existing session search; secrets redacted on write.
- **State Block**: Rule-rendered, size-bounded current-state section (task status, reference pointers, progress) shown to the assistant each turn; never persisted to the conversation record; absent when empty.
- **Condensed Tool Result**: One-line replacement for an already-processed tool result, carrying a stable marker showing where the detail went and how to re-fetch it.
- **Boundary Cleanup**: A cleanup event executed once at a task boundary (complete or empty task list) when usage exceeds the boundary threshold; coexists with the pressure-based backstop via a shared attempt/cooldown budget.
- **Economy Guidance**: Standing concise/pointer/delegation/targeted-read instructions in effect for the assistant, individually disableable.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: In sessions of 100+ exchanges, facts recorded in the first quarter of the session remain correctly answerable (exact values, not paraphrases) at session end in at least 80% of spot checks, without the user re-providing them.
- **SC-002**: 100% of context cleanup events — boundary-aligned or pressure-triggered — preserve the current task definition verbatim and keep the protected recent window verbatim.
- **SC-003**: In tool-heavy sessions (50+ tool calls), late-session turns respond within 2x of early-session turn latency, and total context usage for the session is measurably lower than the same session run with all new mechanisms disabled.
- **SC-004**: With every new mechanism disabled, a regression comparison shows 100% identical behavior to the pre-feature system across the standard test scenarios.
- **SC-005**: 100% of seeded secrets in scratchpad input are absent from persisted scratchpad content in adversarial checks.
- **SC-006**: When the boundary condition (task list complete plus usage above the boundary threshold) holds at turn end, cleanup occurs exactly once in at least 95% of cases, and never more than once.

## Assumptions

- "Enable all the new features by default" means every mechanism in this spec is active on a fresh installation after upgrade, requiring no user action; existing sessions and records remain valid without migration.
- The scratchpad is scoped per session for writing (each session keeps its own); its content persists on disk after the session ends, is recallable through existing session search, and is cleaned only by existing retention policies — no new retention machinery; cross-session and per-project sharing are future options.
- Disable switches use the existing configuration surface (no new settings interface); per-mechanism switches are additive configuration, not renames of existing options.
- Existing cleanup machinery (structured summarization template, protected windows, failure cooldowns, attempt caps) is extended rather than replaced; its current protections are treated as non-regression requirements.
- Existing delegation and sub-agent machinery already returns distilled summaries; this feature preserves and reinforces it rather than rebuilding it.
- The verification prompt for retrieved facts reuses the existing bounded verification mechanism and its caps.
- No new external services, accounts, or network dependencies are introduced; everything runs locally within the existing assistant architecture.
