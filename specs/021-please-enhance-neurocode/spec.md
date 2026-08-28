# Feature Specification: NeuroCode Semantic Code Retrieval (RAG Enhancement)

**Feature Branch**: `021-please-enhance-neurocode`

**Created**: 2026-08-27

**Status**: Draft

**Input**: User description: "Enhance the existing /neurocode feature with a code-aware retrieval pipeline: natural-language code search, hybrid ranking that blends meaning-based and exact-symbol matching, incremental version-control-aware re-indexing, search results expanded with surrounding code context, and relationship-based multi-hop retrieval for the agent."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Natural-Language Code Search (Priority: P1)

As a developer working with the agent, I can ask a question in plain words — "where is token validation handled?" — and receive a ranked list of relevant code locations (file, symbol, symbol kind, line range), even when none of the words in my question literally appear in the code.

**Why this priority**: Today /neurocode offers only keyword search over its typed structural graph (classes, interfaces, enums, methods, fields, with relationship edges and reverse-dependency "used by" lookups). A developer whose vocabulary differs from the code's identifiers gets no useful results and falls back to slow manual file exploration. Meaning-based search is the single core new capability of this enhancement; every other story builds on or protects it.

**Independent Test**: Can be fully tested by indexing a codebase, issuing natural-language queries whose wording does not appear in the source, and verifying that the correct code locations are returned ranked with file path, symbol name, symbol kind, and line range.

**Acceptance Scenarios**:

1. **Given** an indexed repository containing a token-validation routine whose identifiers do not include the words "token" or "validation" as typed, **When** the user searches for "where is token validation handled?", **Then** the top results include the correct file and symbol with its kind and line range.
2. **Given** an indexed repository, **When** a natural-language query matches nothing semantically or by keyword, **Then** a clear no-results response is returned rather than an error.
3. **Given** the agent is asked where a behavior lives in the codebase, **When** it uses the semantic search capability, **Then** the ranked results include file path, symbol name (where the chunk is symbol-aligned), symbol kind, and line range, without the user having to name exact identifiers.

---

### User Story 2 - Hybrid Ranking (Priority: P2)

As a developer, when I search with a mix of natural language and exact symbol names, I get one blended ranked list combining meaning-based relevance with exact-symbol matching — and a query that names an exact symbol still ranks that exact match first.

**Why this priority**: Pure meaning-based matching can demote exact-name matches, which would regress the keyword workflow users already rely on. Hybrid ranking must be trustworthy before semantic search can become the default search path, but it presupposes Story 1 existing first.

**Independent Test**: Can be tested by issuing exact-symbol queries and mixed queries against an indexed repository and asserting the ordering and composition of the single ranked list.

**Acceptance Scenarios**:

1. **Given** an indexed repository containing a symbol named `SessionStore`, **When** the user searches for `SessionStore`, **Then** the exact symbol match is ranked first in the single combined result list.
2. **Given** an indexed repository, **When** the user searches "how does the session store expire idle entries", **Then** the results blend meaning-based matches and exact-name matches in one ranked list ordered by combined relevance.
3. **Given** the semantic search backend is unavailable or unconfigured, **When** any search is issued, **Then** keyword-only results are returned and the response indicates the degraded mode.

---

### User Story 3 - Fast Incremental Re-Indexing (Priority: P3)

As a developer editing a handful of files, I get a freshly updated index within seconds because the automatic background refresh processes only the added, modified, and removed files — entries for deleted files are purged and unchanged files are not reprocessed.

**Why this priority**: The current implementation re-indexes the whole project on refresh, which is slow on large repositories and discourages keeping the index fresh even though staleness detection exists. Incremental updates make fully automatic background refresh practical, but the feature remains usable (just slower) without them, so this sits below the core search stories.

**Independent Test**: Can be tested by modifying, adding, renaming, and deleting a small set of files in an indexed repository, letting the automatic background refresh run, and verifying that only those files' entries changed, deleted files' entries are gone, and the refresh completes within the incremental time target.

**Acceptance Scenarios**:

1. **Given** an indexed repository, **When** 10 or fewer files are modified and the automatic background refresh runs, **Then** the refresh completes in under 5 seconds and only the changed files' entries are updated.
2. **Given** an indexed repository, **When** a file is deleted or removed from the project, **Then** its indexed entries are purged and no longer appear in search results.
3. **Given** an indexed repository, **When** a file is renamed or moved, **Then** the old path's entries do not linger and the new path's entries are searchable.

---

### User Story 4 - Context-Expanded Results (Priority: P4)

As an agent or developer inspecting a search hit, I receive the surrounding lines of code (a configurable window) with each result so I can reason about it immediately without extra file reads.

**Why this priority**: A bare file-and-line reference still forces an extra read before anything can be reasoned about, costing one round trip per result. Expanding results with context multiplies the value of every search, but it is an enhancement to the core retrieval experience rather than a precondition for it.

**Independent Test**: Can be tested by searching an indexed repository and verifying each result carries surrounding source lines within the configured window and that the window size is adjustable via configuration.

**Acceptance Scenarios**:

1. **Given** an indexed repository and a configured context window of N lines, **When** a search returns a result, **Then** the result includes the code surrounding the matched location spanning up to N lines.
2. **Given** a result located near the start or end of a file, **When** its context is expanded, **Then** the returned context is clamped to the file's boundaries without error.
3. **Given** the user changes the context-window setting, **When** subsequent searches run, **Then** results reflect the new window size.

---

### User Story 5 - Relationship-Aware Retrieval (Priority: P5)

As an agent working with search results, when a retrieved code element references, calls, or is referenced by other indexed elements, I can follow those recorded relationships (up to a bounded depth) to pull related code into context.

**Why this priority**: The structural graph already records relationship edges and reverse-dependency ("used by") links; exposing them to retrieval lets the agent gather a complete picture (definition plus callers plus callees) in one interaction. This is valuable but only becomes meaningful once basic retrieval and context expansion exist.

**Independent Test**: Can be tested by retrieving a method known to call other indexed methods and verifying that related entities are returned with correct relationship kinds, bounded depth, and no duplicates.

**Acceptance Scenarios**:

1. **Given** an indexed repository where method A calls method B, **When** a search returns A and relationship expansion is requested, **Then** B is included as a related entity with the call relationship.
2. **Given** a chain of related entities deeper than the configured depth bound, **When** relationships are expanded, **Then** expansion stops at the bound.
3. **Given** multiple paths leading to the same related entity, **When** relationships are expanded, **Then** each entity appears only once (deduplicated).

---

### Edge Cases

- **Empty or whitespace-only query**: the system responds with a clear validation message instead of searching or erroring.
- **Query with no matches**: a clear no-results response distinguishes "nothing matched" from failure.
- **Very large files** (10 MB or larger source files): indexing and search handle them without failing; processing is bounded (chunked, with capped per-chunk processing input) and does not stall the agent turn.
- **Renamed, moved, or deleted files**: entries for old paths are purged and never returned as stale results; new paths become searchable.
- **Very large repositories (cold first index)**: the first full index of a very large repository may take longer; progress remains observable and the operation does not block the agent indefinitely.
- **Semantic backend unreachable, unconfigured, or used remotely without recorded per-project consent**: search degrades gracefully to keyword-only results with an explicit status indication; the agent turn never hard-fails.
- **Remote-backend consent revoked mid-operation**: after revocation, subsequent refreshes and searches for that project use local/keyword-only mode and no further content is sent to the remote service.
- **Proactive pre-fetch enabled but fully local semantic backend unavailable or degraded**: nothing is injected into the agent's context and no spurious results appear; on-demand search still degrades gracefully to keyword-only results.
- **Index being updated while queries run**: queries proceed against the last fully consistent index snapshot and are never blocked or served torn state; an in-progress refresh becomes visible only when it completes and is switched in, and refresh progress remains visible in status reporting.
- **Enhancement disabled entirely**: all /neurocode behavior is unchanged from the pre-enhancement implementation.

## Requirements *(mandatory)*

### Functional Requirements

| Requirement ID | Requirement | Priority |
|---|---|---|
| FR-001 | Natural-language (meaning-based) search over indexed code returns ranked results, each carrying file path, symbol name, symbol kind, and line range, even when query words do not literally appear in the code. | P1 |
| FR-002 | Search produces a single ranked list combining semantic relevance and exact-symbol matching; queries naming an exact symbol rank that exact match first. | P2 |
| FR-003 | Search is filterable by file-path scope (for example, restricting results to a subdirectory), and results honor the filter. | P2 |
| FR-004 | Index refresh is incremental and fully automatic: detected changes trigger the refresh, which runs in the background, stays within configured refresh cost budgets, and completes without blocking ongoing agent turns or searches; during a refresh, searches always execute against the last fully consistent index snapshot, which is switched atomically when the refresh completes, so no query ever observes a partially updated or mixed old/new state; only added and modified files' data is updated, and unchanged files are not reprocessed. | P1 |
| FR-005 | Indexed entries for removed files are purged on refresh and no longer appear in results. | P1 |
| FR-006 | Each search result carries surrounding-code context lines with a configurable window size, clamped to file boundaries. | P2 |
| FR-007 | Search results can be expanded with related entities via recorded relationship links, up to a bounded depth, deduplicated. | P3 |
| FR-008 | If the semantic search backend is unavailable, unconfigured, or unreachable, searches return keyword-only results with an explicit degradation indication and never hard-fail the agent turn. | P1 |
| FR-009 | The entire capability is opt-in and disableable; when disabled, existing /neurocode behavior is byte-identical to the pre-enhancement implementation. | P1 |
| FR-010 | Search is available both as an agent-invocable tool and as a user slash-command; results render in both the CLI and TUI. | P2 |
| FR-011 | The existing typed-graph context block, complexity-tier model routing, and staleness reporting continue to function without regression. | P1 |
| FR-012 | Users control whether code content leaves the local machine; a fully-local operation mode exists, and global enablement of a remote semantic backend alone is insufficient: the first use of a remote semantic backend in each project requires an explicit, recorded per-project acknowledgement that this repository's code will be sent to the remote service; consent is revocable per project at any time, and while consent is absent or revoked that project operates in local/keyword-only mode. | P1 |
| FR-013 | Status reporting surfaces index freshness (covering the semantic index as well), entry counts, last update time, search backend health, and the per-project remote-backend consent state (never acknowledged, acknowledged, or revoked). | P3 |
| FR-014 | Semantic search coverage includes code regions that contain no named artifacts via coarse fallback chunks; fallback-chunk results are visibly distinguished from symbol-aligned results in output. | P3 |
| FR-015 | Retrieval results enter the agent's context on demand by default (via the agent-invocable search tool or the user command); an explicit opt-in setting enables proactive pre-fetch of results relevant to the current user prompt into the agent's context; pre-fetch mode is available only when the semantic backend is fully local; when the enhancement is disabled or pre-fetch is off, session context is unchanged. | P3 |

### Key Entities

- **Code Chunk**: the unit of indexed code; comes in two kinds. Symbol-aligned chunks are derived from named artifacts (classes, interfaces, enums, methods, fields) and carry symbol kind, symbol name, and recorded relationships. Fallback coarse chunks cover source regions that contain no named artifacts (such as scripts and top-level code) and carry file and line-range identity but no symbol identity. Both kinds carry identity, content, source file, language, line range, and a content hash used to detect modification.
- **Search Request**: a query for code; carries query text, an optional file-scope filter, and a result limit.
- **Ranked Result**: one entry in a search response; references a Code Chunk and carries a relevance score plus expanded surrounding context.
- **Change Delta**: the set of added, modified, and removed files since the last index refresh; drives incremental updates.
- **Relationship Link**: a recorded connection between two indexed entities; carries from-entity, to-entity, and relationship kind.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: For a benchmark set of natural-language queries whose wording does not literally appear in the target code, the correct code location appears in the top 5 results for at least 80% of queries.
- **SC-002**: Exact-symbol searches rank the exact symbol first at least 95% of the time.
- **SC-003**: Search results are returned to the user in under 2 seconds on repositories with hundreds of thousands of indexed artifacts.
- **SC-004**: After changing 10 or fewer files, an index refresh completes in under 5 seconds (versus minutes for a cold full index) and provably touches only the changed files' entries.
- **SC-005**: With the enhancement disabled, agent sessions are byte-identical to pre-enhancement behavior (parity verification).
- **SC-006**: Agents complete equivalent code-location tasks with measurably fewer manual file reads than with keyword-only search (fewer file-read tool calls for equivalent tasks) (a post-launch outcome metric evaluated through agent-task evaluation, not a build gate — the build-time proxies are the SC-001/SC-002 benchmarks).

## Assumptions

- The enhancement builds on the existing /neurocode structural graph (kinds: class/interface/enum/method/field, with relationship edges and reverse-dependency "used by" lookups) and its keyword search; keyword search remains the always-available fallback.
- Semantic backend selection (hosted remote service versus fully local operation) is a configuration decision deferred to the plan phase; the default is disabled until configured, and no code content leaves the machine unless explicitly enabled.
- Remote semantic backends follow a per-project consent model: a global configuration switch alone never causes code to leave the machine; the first use in each project requires an explicit recorded acknowledgement that this repository's code will be sent, revocable at any time, and absent or revoked consent keeps that project in local/keyword-only mode.
- Semantic retrieval results enter the agent's context on demand by default; proactive pre-fetch of prompt-relevant results is opt-in and available only when the semantic backend is fully local.
- Target scale: repositories up to approximately 1 million lines of code.
- Change detection uses version-control metadata when available and falls back to file-modification scanning otherwise.
- Index refresh reuses the existing automatic re-indexing thresholds and debouncing behavior of the /neurocode subsystem; refresh work happens in the background, never blocks agent turns or searches, and respects refresh cost budgets.
- The first (cold) index of a repository may take longer; only subsequent incremental updates must meet the incremental time target.
- Brief staleness (seconds) during a background refresh is accepted in exchange for snapshot consistency: searches always run against the last fully consistent index snapshot until the completed refresh is switched in atomically.
- Dependency choices and their binary-size and compile-time tradeoffs are evaluated in the plan phase per the project constitution (Principle VIII); they are out of scope for this specification.
- No changes to upstream Hermes parity constraints or to existing on-disk formats of other subsystems.

## Clarifications

### Session 2026-08-27

- Q: Should semantic search operate over the existing typed artifacts, or over a separate, finer-grained chunking of the code? → A: Hybrid — named artifacts (classes/interfaces/enums/methods/fields) remain the first-class search units, AND coarse fallback chunks cover code regions that contain no named artifacts (scripts, top-level code); both populations are searchable and clearly distinguished in results.
- Q: Is semantic-index refresh automatic, manual, or hybrid? → A: Fully automatic — the index refreshes in the background whenever changes are detected, reusing existing thresholds/debouncing; it never blocks agent turns or searches; remote-backend usage only occurs where the user has explicitly enabled it (per the local-consent requirement).
- Q: Should semantic retrieval results be pulled into the agent's context proactively, or only on demand? → A: Configurable hybrid — on demand by default (results enter context only when the agent invokes the search tool or the user runs the command); an opt-in setting enables proactive pre-fetch of results relevant to the current user prompt into the agent's context, and that pre-fetch mode is restricted to fully local semantic backends, preserving the privacy stance.
- Q: When a user enables a remote semantic backend, what consent model applies? → A: Per-project consent with recorded acknowledgement — the first use per project requires explicit acknowledgement that this repository's code will be sent; consent is recorded, revocable per project, and absent/revoked consent forces local/keyword-only operation for that project.
- Q: During a background refresh, do searches see the old complete index or partially updated results? → A: Atomic swap — searches always execute against the last fully consistent index snapshot; an in-progress refresh becomes visible only when complete, so no query ever observes a mixed old/new state; brief staleness is accepted.
