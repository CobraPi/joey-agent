# Feature Specification: NeuroCode Context Relevance Improvements

**Feature Branch**: `019-please-turn-plan`

**Created**: 2026-08-26

**Status**: Draft

**Input**: User description: "please turn this plan into specs" — converting the NeuroCode context-relevance improvement plan (derived from research into context-assembly state of the art) into a feature specification.

## Clarifications

### Session 2026-08-26

- Q: Is automatic capture of verification diagnostics in scope, or capability-only with auto-wiring as a fast-follow? → A: Full wiring is in scope — request field + high-priority matching + automatic capture of verification-run failure output into subsequent coding requests, delivered as the feature's final increment.
- Q: How strict should the generic-word fallback confidence gate be? → A: Balanced — exact name matches (simple or fully-qualified) always pass; partial/fuzzy matches pass only when corroborated by the active file's package/module or by high structural importance (fan-in); uncorroborated partial matches fall to cold mode.
- Q: Does the context-size budget cover the NeuroCode context section only or the broader prompt, and do tier count-limits stay binding when a size hint is present? → A: The budget covers the assembled context section only; tier count-limits (primary/related/depth) remain hard caps in all modes — the effective budget is the smaller of the size hint and the tier caps.

### Session 2026-08-27

- Q: Is raw diagnostic/error text rendered into the assembled context section, or used only as a matching cue? → A: Cue-only — diagnostics drive target matching/ranking only; raw diagnostic text is never rendered in the assembled context section.
- Q: How are the "fixed evaluation set" targets in SC-001/SC-006 materialized and run? → A: A committed, deterministic fixture corpus inside the repository, exercised by the automated test suite (e.g. cargo test), calibrated once against real-project-style queries when authored.
- Q: Which in-session command executions count as a "verification run" for automatic diagnostics capture? → A: Any terminal command executed via joey in the session that fails (non-zero exit) — capture is not restricted to build/test/lint-style commands.
- Q: Do captured diagnostics persist for the whole session, or expire within it? → A: Captured failure output is cleared when a subsequent execution of the same command succeeds, and is superseded when a newer failure is captured; diagnostics never accumulate unboundedly within a session.
- Q: Are terminal failures from subagent executions or background processes also captured as diagnostics? → A: No — capture covers main-session terminal executions only; subagent and background-process failures are excluded.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Trustworthy Target Identification (Priority: P1)

When a developer asks joey to change a specific piece of code (by name, by active file, or by a symbol they are working on), the background context NeuroCode assembles must center on exactly the artifacts the developer meant — not artifacts that merely share a common word. Today, low-confidence word matches can inject unrelated artifacts, and candidates found through the active file or active symbols are not quality-ranked, so the model receives noise alongside signal.

**Why this priority**: Wrong or noisy primary targets corrupt everything downstream — related-artifact expansion, guidance sections, and the model's edit focus. This is the single largest relevance win and changes no output formats.

**Independent Test**: Can be fully tested by issuing requests whose targets are clearly named, ambiguous, or only weakly implied, and verifying the assembled context's primary artifacts match the intended targets (or that no target is claimed when confidence is low). Delivers standalone value: less junk context in every coding turn.

**Acceptance Scenarios**:

1. **Given** a request naming an artifact unambiguously (e.g. a backticked identifier or a dotted full name), **When** context is assembled, **Then** that artifact is selected as a primary target ahead of weaker matches.
2. **Given** a request mentioning only the file the developer is editing, **When** candidates are found from that file, **Then** chosen targets are the ones best matching the request's naming cues, ranked by match quality.
3. **Given** a request whose only matches are low-confidence generic-word hits, **When** no candidate meets the minimum match quality, **Then** the system claims no primary target and falls back to the existing "cold mode" behavior instead of injecting unrelated artifacts.
4. **Given** two artifacts sharing the same short name in different packages/modules, **When** the developer's active file belongs to one of them, **Then** the artifact from the developer's package/module is preferred.
5. **Given** any request, **When** assembly runs twice on identical inputs, **Then** selected targets and their order are identical (determinism preserved).

---

### User Story 2 - Importance-Ordered Related Context (Priority: P2)

When context includes related artifacts (interfaces, implementations, collaborators), the most structurally important and most recently changed ones must come first, so that when budget pressure trims the list, what remains is what matters most.

**Why this priority**: Under real budget pressure, ordering decides what the model sees. Structural importance (how widely an artifact is depended upon) and edit recency are the two strongest available signals, and both are computable from data already collected.

**Independent Test**: Can be tested by assembling context for widely-depended-upon hubs and for recently edited files, verifying related artifacts are ordered ahead of equal-reason alternatives with lower importance or older timestamps.

**Acceptance Scenarios**:

1. **Given** two related artifacts included under the same relationship reason, **When** one is depended upon by many more artifacts, **Then** the more-depended-upon artifact appears earlier in the assembled context.
2. **Given** two related artifacts with equal structural importance, **When** one's source file was modified more recently, **Then** the more recently modified one appears earlier.
3. **Given** the complexity tier is raised from economical to frontier, **When** context is assembled for the same request, **Then** the higher tier still includes strictly more related artifacts (existing tier semantics preserved).

---

### User Story 3 - Budget-Respecting Assembly (Priority: P3)

The assembled context must fit the context-size budget the caller provides. Today the budget is enforced only by artifact counts, so a few very large artifacts can overrun the intended size, and guidance sections appended after assembly are never re-checked against the budget.

**Why this priority**: Oversized context wastes the model's window and degrades response quality. A size hint already travels with every coding request but is currently ignored.

**Independent Test**: Can be tested by assembling context for projects containing very large artifacts with a small size hint, verifying output stays within budget while still naming the primary targets.

**Acceptance Scenarios**:

1. **Given** a request carrying a context-size budget, **When** artifacts are selected, **Then** primary targets are included first, related artifacts fill the remaining budget in rank order within the tier caps, and the assembled context section respects the budget.
2. **Given** a single primary artifact whose rendering alone would exceed the budget, **When** assembly completes, **Then** output is truncated deterministically to fit rather than silently exceeding the budget.
3. **Given** appended guidance sections (staleness notes, learned warnings, domain knowledge), **When** total output would exceed the budget, **Then** lower-priority sections are shortened or dropped so the total fits.
4. **Given** no size hint provided, **When** assembly runs, **Then** behavior matches today's count-based limits (backward-compatible default).
5. **Given** an artifact whose members would be rendered twice (once in a member roster, once via related expansion), **When** output is composed, **Then** each artifact appears at most once.

---

### User Story 4 - Failure- and Test-Aware Context (Priority: P4)

When a developer's request arrives with build errors or diagnostics (for example from a verification run), the artifacts named in those errors must be treated as first-class targets; and when a target artifact has tests, those tests must be offered as related context because they encode intended behavior.

**Why this priority**: Errors are the highest-signal retrieval cue a coding agent receives, and tests are simultaneously usage documentation and the acceptance bar. Lower priority because these add new signals and new linkage data rather than fixing existing weaknesses.

**Independent Test**: Error-awareness can be tested by supplying diagnostic text naming an artifact and verifying it becomes a primary target; test co-retrieval can be tested on a project whose tests follow recognized conventions, verifying linked tests appear as related context within budget.

**Acceptance Scenarios**:

1. **Given** a request with diagnostic/error text referencing an artifact by name, **When** context is assembled, **Then** that artifact is selected as a primary target ahead of weaker textual matches.
2. **Given** a request with diagnostics referencing nothing known to the project, **When** assembly runs, **Then** behavior is unchanged — diagnostics add nothing and break nothing.
3. **Given** a primary target with a recognizable test artifact, **When** related context is assembled, **Then** the test artifact is included (within budget) and labeled with a distinct relationship reason.
4. **Given** a request without diagnostics and a target without tests, **When** assembly runs, **Then** output is identical to pre-change behavior for that request (purely additive).
5. **Given** a verification run in the same session fails with output naming an artifact, **When** the next coding request is assembled, **Then** the failure output is automatically attached as diagnostic text and the named artifact is selected as a primary target ahead of weaker textual matches.

### Edge Cases

- Two artifacts share the same short name and the same package — ties must be broken deterministically (stable original ordering), never randomly.
- Budget smaller than the smallest useful rendering of the primary target — deterministic truncation; the target is still named at minimum by identity.
- Stale index: recency ranking must tolerate missing files and timestamps older than index time without failing.
- Word-fallback candidates that coincidentally match a real artifact name in full — treated as confident; match quality, not the producing pathway, decides.
- Partial matches corroborated only by high fan-in — the hub may be legitimately relevant without being the requested target; corroboration raises the partial match above the gate but never above an exact or package-corroborated match of the same name.
- Test files exercising multiple artifacts (integration-style tests) — linked to all named targets without duplicating the test in output.
- Diagnostics naming member symbols (methods/fields) rather than top-level artifacts — the owning artifact is matched and surfaced.
- Empty project or empty graph — existing cold-mode behavior applies, including when diagnostics are present.
- Guidance sections alone exceeding the budget — primary targets still win; sections degrade gracefully.
- Automatically captured output from arbitrary failing commands (e.g. command-not-found, typos) that names no known artifact — contributes no matching cues; behavior unchanged.
- A previously failing command later succeeding — its captured diagnostics are cleared before the next coding request assembles context; stale errors never drive target selection.
- Failing commands run inside subagent contexts or as background processes — excluded from diagnostics capture; parent-session matching cues remain unaffected.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST rank all candidate-to-request matches by match quality (name and symbol similarity), regardless of which pathway produced the candidate (explicit mention, active symbols, active file, filename).
- **FR-002**: System MUST gate generic-word fallback candidates with a balanced rule: exact name matches (simple or fully-qualified) always pass; partial/fuzzy matches pass only when corroborated by the active file's package/module or by high structural importance (fan-in); below the gate, no primary target is claimed and the existing cold-mode fallback applies.
- **FR-003**: When multiple artifacts share a name, System MUST prefer artifacts from the requester's active context (the active file's package or module) as the disambiguation signal.
- **FR-004**: System MUST order included related artifacts by structural importance (count of dependent artifacts) as a secondary key within relationship-reason priority.
- **FR-005**: System MUST use source-file edit recency as a tertiary ordering key for otherwise equally ranked candidates.
- **FR-006**: When a context-size budget is provided, System MUST respect it for the assembled context section only — primary targets first, related artifacts in rank order, appended guidance sections re-checked — with deterministic truncation as the overflow strategy; tier count-limits (primary/related/depth) remain hard caps in all modes, making the effective budget the smaller of size hint and tier caps; absent a hint, current count-based behavior applies.
- **FR-007**: System MUST render each artifact at most once per assembled context, and member rosters MUST be keyed by unique artifact identity rather than display name.
- **FR-008**: System MUST accept optional diagnostic/error text with a coding request, extract artifact and symbol identifiers from it, and treat them as high-priority matching cues; absence of diagnostics changes nothing. System MUST additionally capture verification-run failure output automatically and attach it to subsequent coding requests in the same session (delivered as the final increment of this feature); sessions without verification failures see zero change. Diagnostic text is used as a matching/ranking cue only — it MUST NOT be rendered into the assembled context section, so it consumes no context budget and adds no output-format surface. For automatic capture, a verification run is defined as ANY terminal command executed via joey in the same session that exits non-zero; its captured output is treated as diagnostic text under the same cue-only and additive semantics. Captured failure output is session-scoped with expiry: it MUST be cleared when a subsequent execution of the same command succeeds, and superseded by the most recent failure of that command. Capture applies to main-session terminal executions only — failures from subagent executions and background processes are not captured.
- **FR-009**: System MUST recognize test artifacts that exercise a given artifact (by project conventions) and include them as related context under a distinct relationship label, subject to budget.
- **FR-010**: Assembly MUST remain deterministic for identical inputs, and streamed/progressive assembly MUST produce output identical to direct assembly.
- **FR-011**: All existing output-format guarantees (section headings, relationship reason labels, tier expansion ordering, warning strings) MUST be preserved, with regression coverage proving it.

### Key Entities *(include if feature involves data)*

- **Coding Request**: what the developer asked for — request text, active file, active symbols, project root, context-size budget, and (new, optional) diagnostic/error text.
- **Assembled Context**: the output — primary target artifacts, related artifacts with relationship reasons, appended guidance sections (staleness, learned warnings, domain knowledge), and a size estimate.
- **Match Quality Score**: how well a candidate artifact's names match the request's cues; drives target selection and fallback gating.
- **Importance Signals**: dependency fan-in (how many artifacts depend on this one) and edit recency (how recently its source changed); determine ordering among related artifacts.
- **Test Linkage**: association between a test artifact and the artifact(s) it exercises; the source of test co-retrieval.
- **Context Budget**: tier-based limits (depth, primary count, related count) plus an optional total-size ceiling over the assembled context section only; tier count-limits bind in all modes, so the effective budget is the smaller of size ceiling and tier caps.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: For requests that clearly name a target artifact, the correct artifact is selected as primary in at least 90% of a fixed evaluation set drawn from real project queries.
- **SC-002**: For vague or ambiguous requests, unrelated artifacts included as primary targets drop to zero (no primary claimed below the confidence gate), compared to today's fallback behavior.
- **SC-003**: When a context-size budget is provided, at least 99% of assemblies produce output within budget; the remainder truncate deterministically.
- **SC-004**: Context assembly time per request regresses by no more than 20% relative to the current baseline on the same hardware and project.
- **SC-005**: 100% of existing context-assembly behavior guarantees (formats, labels, tier ordering, streaming parity) remain passing — zero regressions.
- **SC-006**: In a sample of coding turns carrying failing-verification diagnostics, the artifact named in the diagnostics is present in the assembled context in at least 90% of cases.
- **SC-007**: The evaluation sets referenced by SC-001 and SC-006 are committed, deterministic in-repo fixture corpora exercised by the automated test suite, so the 90% targets are continuously enforceable, not one-time measurements.

## Assumptions

- Tier semantics (economical/frontier budgets) and all current output-format guarantees are preserved, not changed; ranking and budgeting changes are internal.
- The context-size budget hint already carried by coding requests is the budget referred to in FR-006; introducing user-facing budget configuration is out of scope.
- Diagnostic text is optional and additive; requesters that do not provide it see zero behavior change. Automatic capture covers any terminal command executed via joey that fails (non-zero exit) in the same session only; diagnostics are never persisted across sessions. Diagnostic text is never rendered into the assembled context section; it influences matching and ranking only.
- Test recognition relies on naming and file-placement conventions common in supported languages; heuristic misses are acceptable (linkage is best-effort and budget-gated).
- Explicitly deferred to future features (recorded, not abandoned): semantic/vector retrieval, iterative multi-pass retrieval, conversation-history signals, automatic re-indexing on staleness detection, source-body slicing, and configurable budget keys.
- No new external dependencies are introduced; all signals derive from data already collected.
- Scope covers the NeuroCode context-assembly subsystem, the automatic diagnostics capture wired through its existing consumers, and those consumers' existing contracts; unrelated subsystems are untouched.
