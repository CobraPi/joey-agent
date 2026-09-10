# Feature Specification: NeuroCode Adaptive Memory

**Feature Branch**: `027-please-enhance-neurocode`

**Created**: 2026-09-09

**Status**: Draft

**Input**: User description: "please enhance the /neurocode feature and give the agent both episodic and semantic memory - I want the agent to adapt the way it writes code and learn from the users preferences. Use the existing rag pipeline and make sure that this works with /hypercode as well."

## Clarifications

### Session 2026-09-09

- Q: When may an inferred preference start shaping code output? → A: Fully automatic — inferred preferences apply immediately with no approval gate; the user corrects or removes them after the fact, and all existing implementations that would gate or conflict with this model are modified to match it.
- Q: What is the unit of episodic capture? → A: One episode per completed task in interactive use, and one per completed workstream or subtask in orchestrated (/hypercode) runs, whether the outcome was successful or not.
- Q: When does episodic evidence get distilled into semantic memories? → A: Continuously — as each episode is captured, so learned preferences become active as soon as their evidence lands; no batching delay.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Code That Adapts to the User (Priority: P1)

As a developer working with the agent, I want the code it produces to conform to my personal conventions — naming, formatting, error handling, structure, preferred libraries — without me repeating those preferences in every request, so that output needs less rework and feels like it comes from a teammate who knows my style.

The agent maintains a semantic memory of my stable preferences, distilled from my explicit statements and from patterns in the changes I accept, reject, or request. Before producing code, the agent consults this memory and applies what it has learned, so every subsequent code-writing response increasingly matches my style without additional prompting.

**Why this priority**: This is the core user-visible value of the feature — measurable adaptation of code output. Without it, the memory system has no user-facing payoff.

**Independent Test**: Can be fully tested by stating a preference (e.g., "always use constructor injection"), completing a few tasks, then issuing a fresh related task without restating the preference — the produced code must follow it.

**Acceptance Scenarios**:

1. **Given** the user has corrected the agent's error-handling style on three separate occasions, **When** the user asks for new code in a later session, **Then** the agent produces code following the corrected style without being told again, and cites the learned preference when asked why.
2. **Given** the user explicitly states a convention at the start of a project ("I prefer small pure functions"), **When** any subsequent code is generated in that project, **Then** the convention is applied consistently until the user changes or removes it.
3. **Given** the user changes their mind and states a new convention, **When** code is next generated, **Then** the new convention wins over the superseded one, and the superseded memory is not re-applied.

---

### User Story 2 - Remembering Past Work Episodes (Priority: P1)

As a developer returning to a codebase, I want the agent to remember the concrete events of our past work — what was built, what was tried, what failed, what was decided and why — so that it resumes with full context instead of re-asking or re-proposing things we already settled.

Every completed unit of work becomes an episodic memory (task, approach, outcome, lessons). When a later task is related, the relevant episodes are retrieved and used, so the agent builds on history rather than starting from zero.

**Why this priority**: Episodic memory is the raw material the semantic layer learns from and the mechanism that stops repeated mistakes; it is the foundation the other stories depend on.

**Independent Test**: Can be fully tested by completing a task whose approach failed, then starting a related task — the agent must recall the failed approach and avoid it.

**Acceptance Scenarios**:

1. **Given** a previous session ended with a failed approach to a problem plus a note about why it failed, **When** the user starts a new session and describes a related problem, **Then** the agent recalls the episode and proposes a different approach, referencing what happened before.
2. **Given** several completed work episodes exist for a project, **When** the user asks "what did we try so far?", **Then** the agent can summarize the relevant past attempts, outcomes, and decisions.
3. **Given** no episodes exist yet (fresh project), **When** the user works with the agent, **Then** the agent behaves exactly as it does today — graceful cold start, no errors, no fabricated memories.

---

### User Story 3 - Memory Across Orchestrated (/hypercode) Runs (Priority: P2)

As a power user delegating large goals to orchestrated parallel agent runs (/hypercode), I want those runs to both benefit from and contribute to the same memory — orchestrated subagents write episodes about what they did and found, and read the preferences and lessons that apply to their tasks — so that orchestration output also matches my conventions and every run makes the next one smarter.

**Why this priority**: Extends the value of stories 1 and 2 to the highest-throughput workflow; valuable but secondary to direct interactive use.

**Independent Test**: Can be fully tested by running an orchestrated task that includes a preference correction, then running a second orchestrated task — the second run's output must conform without repeated correction.

**Acceptance Scenarios**:

1. **Given** a semantic preference exists (from interactive use), **When** an orchestrated run produces code through its subagents, **Then** the subagents' output conforms to that preference just as interactive output does.
2. **Given** an orchestrated run completes (whether fully successful or partially), **When** the run finishes, **Then** an episodic record of the run's workstreams, outcomes, and lessons is captured for future recall.
3. **Given** an orchestrated subagent discovers a useful project fact (e.g., the correct way to run a flaky test suite), **When** a later orchestrated run faces the same problem, **Then** the later run retrieves and applies that lesson.

---

### User Story 4 - Transparency and Control Over Memory (Priority: P3)

As a user whose working style is being learned, I want to see everything the agent remembers about me, correct or remove anything wrong or unwanted, and decide whether memory is active at all — so that I stay in control of my data and of how the agent behaves.

**Why this priority**: Trust and control make the learning acceptable; required for a complete feature but not the primary value driver.

**Independent Test**: Can be fully tested by asking the agent to list what it remembers, correcting one item, deleting another, and observing subsequent behavior change accordingly.

**Acceptance Scenarios**:

1. **Given** memory contains learned preferences and episodes, **When** the user asks to see everything remembered, **Then** the agent presents a readable, categorized view (episodes vs preferences, with origin and recency).
2. **Given** the user deletes a specific memory, **When** the next related task runs, **Then** the deleted memory is not applied and does not reappear.
3. **Given** the user disables the memory capability, **When** any session or orchestrated run executes, **Then** nothing new is remembered, nothing is injected, and behavior is identical to a system without the feature.

### Edge Cases

- What happens when two learned preferences conflict (e.g., an old convention vs. a newer one)? The more recent, and any explicitly stated by the user, take precedence; equally strong conflicts are surfaced to the user rather than silently resolved.
- What happens when the user changes preferences over time? Superseded memories must be aged out or marked inactive so stale style does not resurface; recency of evidence outranks sheer frequency.
- What happens when the memory corpus is empty or retrieval returns nothing relevant? The agent proceeds exactly as it would without the feature — no errors, no placeholder context, no invented memories.
- What happens when the underlying retrieval engine or embedding provider is disabled or unavailable? Memory degrades gracefully to inactive for that session with a clear status indication; core agent functionality is unaffected.
- What happens to memories from one project when working in another? Memories are scoped per project by default; one project's conventions are never applied to another unless the user explicitly shares them.
- What happens when a deleted memory was already surfaced in an in-flight session or orchestrated run? Deletion takes effect for subsequent retrievals; the current session is not corrupted, and the memory does not return after deletion.
- What happens when sensitive content (secrets, credentials) appears in a work episode? Such content must not be persisted into memory.
- What happens as the episode volume grows large? Retrieval quality and session responsiveness must remain within the feature's stated performance bounds (bounded context injection, relevant-not-exhaustive recall).

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST record completed tasks as episodic memories — one episode per completed task in interactive use, and one per completed workstream or subtask in orchestrated (/hypercode) runs, whether the outcome was successful or not — capturing task, context, approach, outcome, and lessons learned, persisted locally and scoped to the project they occurred in.
- **FR-002**: System MUST maintain semantic memories — generalized user preferences and coding conventions — each derived from explicit user statements and/or recurring patterns across episodes, with recorded origin and recency.
- **FR-003**: System MUST distill recurring patterns from episodic memories into new or strengthened semantic memories without requiring user intervention, running continuously — as each episode is captured, so learned preferences become active as soon as their evidence lands — and MUST apply inferred preferences immediately: no approval gate or batching delays their effect. Existing gating or batch-delay behavior that would delay or block application MUST be modified to conform to this model rather than reused.
- **FR-004**: System MUST surface relevant memories (episodic and semantic) to the agent as bounded context before or during code-producing work, so output adapts to learned preferences.
- **FR-005**: System MUST store and retrieve all memories through the existing retrieval (RAG) pipeline already used for codebase search — indexed, embedded, and searched with the same machinery — rather than introducing a separate search subsystem.
- **FR-006**: System MUST let orchestrated (/hypercode) runs both contribute episodic memories and consume relevant memories, with subagent work products and outcomes captured as episodes.
- **FR-007**: System MUST provide memory management through the /neurocode command surface: list, search and inspect, correct, delete, enable and disable, and show activation status.
- **FR-008**: System MUST resolve conflicting memories deterministically: explicit user statements over inferred patterns, and more recent over older evidence; unresolvable ties are surfaced to the user.
- **FR-009**: System MUST age out or supersede memories that conflict with newer evidence, so changed preferences take effect and stale ones stop being applied.
- **FR-010**: System MUST keep the feature strictly additive: when memory is disabled (including by default), all existing behavior, commands, outputs, and performance characteristics remain unchanged.
- **FR-011**: System MUST NOT persist secrets or credentials into memory, consistent with the platform's existing secret-handling protections.
- **FR-012**: System MUST NOT apply one project's memories in another project's context unless the user explicitly requests sharing.

### Key Entities *(include if feature involves data)*

- **Episodic Memory**: A record of one concrete work event — what was done, in what context, with what outcome and lessons; attributes include project scope, timestamp, related task, and outcome. Raw material for learning.
- **Semantic Memory**: A generalized preference or convention ("user prefers X under conditions Y"); attributes include category, statement, origin (explicit statement vs inferred pattern), supporting evidence, recency, and active/superseded status. The applied layer that shapes code output.
- **Memory Corpus**: The full set of episodic and semantic memories for a project, indexed into the existing retrieval pipeline for similarity search.
- **Memory Injection**: The bounded set of memories surfaced to the agent for a given task, selected by relevance within a fixed size budget.
- **User Correction**: An explicit user action that edits, supersedes, or deletes a memory; corrections always outrank inferred evidence in future resolution.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: After a preference has been expressed or corrected in at most 5 interactions, at least 80% of the agent's subsequent code outputs in that project conform to it without the user restating it.
- **SC-002**: For a repeated task type, the agent applies the relevant prior episode or preference in at least 4 out of 5 new sessions without re-prompting.
- **SC-003**: Memory activation adds no more than 2 seconds to the start of any agent response, and orchestrated runs with memory enabled complete at a success rate no lower than runs without it.
- **SC-004**: A user can list, correct, and delete any memory through the /neurocode surface in under one minute per action, and deleted memories never reappear in later output.
- **SC-005**: At least 90% of episodes captured from completed work (interactive or orchestrated) are retrievable and accurately summarized when the user asks about past work.

## Assumptions

- "Episodic memory" means records of specific past work events; "semantic memory" means generalized knowledge distilled from them (the standard cognitive-memory distinction). "Learn from the users preferences" covers both explicit statements and patterns inferred from accepted, rejected, or corrected work.
- "Use the existing rag pipeline" means memories are indexed, embedded, and retrieved via the retrieval machinery /neurocode already provides, extended to carry memory content — no parallel search stack.
- "Works with /hypercode" means orchestrated runs read and write the same memory the interactive agent uses, composing with the existing verified-outcome lessons mechanism rather than replacing it.
- Memory is local-first and per-project scoped, consistent with /neurocode's existing per-project stores and consent model; cross-project sharing only on explicit user action.
- The capability is opt-in (default off until enabled), mirroring the existing /neurocode activation and consent flow; existing secret-redaction protections apply to memory content.
- Learning runs locally with the already-configured embedding providers; no new external services or accounts are required.
- The amount of memory context surfaced per task is bounded by a configurable budget so sessions stay responsive as memory grows.
