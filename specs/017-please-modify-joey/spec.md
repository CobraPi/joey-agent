# Feature Specification: Subagent Screen Parity

**Feature Branch**: `017-please-modify-joey`

**Created**: 2026-08-22

**Status**: Draft

**Input**: User description: "please modify this joey-agent and make all the subagent displays have the full functionality and look of the main orchestrator page (scroll, expandable sections, file diff view, etc.) - I want the subagent screen to be identical to the main agent screen."

## Clarifications

### Session 2026-08-22

- Q: Which maximized/full-screen surfaces must be reachable from a focused subagent view for "identical" parity? → A: Rule-based parity — every maximized surface on the orchestrator screen is reachable from a focused subagent view whenever it can display that subagent's content (output viewer, reasoning panel, stats page); mode-specific explorers are reachable only when that mode spawned the subagent.
- Q: How is the target entry chosen for actions like copy, dedicated expand, and opening the output viewer inside a focused subagent view? → A: Same selection model as the orchestrator screen, verbatim — identical way of marking the current entry, identical keys to move it, and copy/expand/viewer actions act on it.
- Q: Does the key-binding help overlay count as a parity surface — what should the help key show inside a focused subagent view? → A: Help is a parity surface: the same key opens the help overlay in subagent views with the same content as the orchestrator screen (one shared help, identical everywhere).

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Scroll affordances parity (Priority: P1)

As a user reviewing a delegated subagent's work in its full-screen view, I want the same scroll affordances as the main orchestrator screen — a visible scrollbar, an indicator of how many entries remain below, and a header showing message count and scroll position — so I can navigate long subagent transcripts without losing my place, using the exact same keys (line, page, top, bottom) and mouse wheel as the orchestrator screen.

**Why this priority**: Scrolling is the most fundamental review capability; today the subagent view scrolls but gives no position feedback, so users get lost.

**Independent Test**: Can be fully tested by opening a subagent with a long transcript and comparing every scroll affordance and key against the orchestrator screen.

**Acceptance Scenarios**:

1. **Given** a focused subagent view with overflowing content, **When** I scroll, **Then** a scrollbar reflects position and an indicator counts entries below.
2. **Given** the view is pinned at the bottom, **When** new streaming content arrives, **Then** it follows, matching orchestrator behavior.
3. **Given** any position, **When** I press the orchestrator screen's scroll keys, **Then** the subagent view scrolls identically.

---

### User Story 2 - Expand/collapse parity (Priority: P1)

As a user drilling into a subagent's work, I want every expandable entry type (tool calls, reasoning, file diffs) in the subagent view to expand and collapse with the same keys and mouse clicks as the orchestrator screen, including the same multi-state cycle (collapsed -> tail -> full), so I can inspect subagent work at any depth without learning different interactions.

**Why this priority**: Expand/collapse is the core drill-in interaction; today the keyboard expand actions only work on the orchestrator transcript.

**Independent Test**: Can be fully tested by cycling each entry type with keyboard and mouse inside a subagent view and comparing states to the orchestrator screen.

**Acceptance Scenarios**:

1. **Given** a tool-call entry in a focused subagent view, **When** I press the expand key or click the entry, **Then** it cycles collapsed -> tail -> full.
2. **Given** a focused subagent view, **When** I use the dedicated tool-expand and reasoning-expand keys, **Then** they act on the focused subagent view's entries (not the orchestrator transcript).
3. **Given** a file-diff entry, **When** it is expanded to full, **Then** its rendering (old/new gutters, added/removed coloring, hunk headers, binary placeholder) is visually identical to the orchestrator screen.

---

### User Story 3 - Copy & search parity (Priority: P1)

As a user who needs to grab or find something a subagent produced, I want the same copy-entry action and in-transcript search inside the subagent view, so I never have to switch back to the orchestrator screen to copy text or find a match.

**Why this priority**: Copy and search are primary review workflows that today force a context switch.

**Independent Test**: Can be fully tested by copying an entry and running a search inside a subagent view; behavior matches the orchestrator screen.

**Acceptance Scenarios**:

1. **Given** an entry selected in a focused subagent view using the same selection mechanism as the orchestrator screen, **When** I use the copy action, **Then** that entry's content is copied.
2. **Given** a focused subagent view, **When** I open search and type a query, **Then** only that subagent's transcript is searched, matches are highlighted, and match navigation scrolls the view.

---

### User Story 4 - Maximized viewers parity (Priority: P2)

As a user reading a subagent's long tool output or reasoning, I want the maximized output viewer and reasoning panel to open from within the focused subagent view and show that subagent's content, so I get the same distraction-free reading layouts as the orchestrator screen.

**Why this priority**: Long output is unreadable inline; the orchestrator screen already solves this.

**Independent Test**: Can be fully tested by opening each maximized viewer from a focused subagent view and confirming content, scroll, and layout match orchestrator-screen behavior.

**Acceptance Scenarios**:

1. **Given** a focused subagent view, **When** I open the output viewer, **Then** it maximizes that subagent's selected/last output with full scrolling.
2. **Given** the same view, **When** I open the reasoning panel, **Then** it shows that subagent's reasoning with identical layout and controls.
3. **Given** a focused subagent view, **When** I open the stats page, **Then** it presents that subagent's stats in the same layout and visual style as the orchestrator stats page.

---

### User Story 5 - Visual chrome parity (Priority: P2)

As a user moving between the orchestrator screen and subagent views, I want identical look and feel — borders, headers, title format (including message count and scroll percentage), status line, colors, and stats-page presentation — so the two screens are indistinguishable apart from whose transcript they show.

**Why this priority**: "Look identical" is an explicit user requirement; inconsistent chrome makes subagent views feel second-class.

**Independent Test**: Can be fully tested by a side-by-side comparison of matched interactions on both screens showing no styling differences.

**Acceptance Scenarios**:

1. **Given** both screens showing equivalent content, **When** compared side by side, **Then** borders, headers, colors, and status line follow the same conventions.
2. **Given** the subagent stats page, **When** compared to the orchestrator stats page, **Then** it presents information in the same visual style, retaining its expandable context stream.

---

### User Story 6 - Universal parity across spawn surfaces (Priority: P3)

As a user of any orchestration feature that spawns subagents, I want every full-screen view dedicated to a single subagent's transcript to follow the same parity rules regardless of which feature spawned it, so behavior is predictable everywhere.

**Why this priority**: Consistency across surfaces prevents a patchwork of half-parity views.

**Independent Test**: Can be fully tested by opening the dedicated subagent view from each orchestration surface and verifying the same capabilities.

**Acceptance Scenarios**:

1. **Given** any feature that spawns subagents with a dedicated full-screen transcript view, **When** I focus that view, **Then** all P1/P2 capabilities are present.

---

### Edge Cases

- Streaming content arrives while the user is scrolled up: follow-tail applies only when pinned to the bottom, matching orchestrator-screen behavior.
- Very large single tool output: full expansion is bounded by the same limits as the orchestrator screen.
- Empty subagent transcript: the same empty-state styling as the orchestrator screen is shown.
- Subagent disappears while its view is focused: the user is returned gracefully to the orchestrator screen without losing the orchestrator scroll state.
- Rapid entry arrival during expand interactions: per-entry expansion state stays stable.
- Binary or one-sided file diffs: the same placeholders as the orchestrator screen are rendered.
- Mouse click hit-testing resolves the correct entry in subagent views.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001** *(User Story 1)*: Focused subagent views MUST provide the same scroll affordances as the orchestrator screen — a scrollbar, an entries-below indicator, and a header with message count and scroll position.
  - **Scenario**: Scroll a long subagent transcript; all three affordances are present and accurate.
- **FR-002** *(User Story 1)*: Every scroll navigation action available on the orchestrator screen (line, page, top, bottom, mouse wheel) MUST work identically in focused subagent views.
  - **Scenario**: The same key/mouse inputs produce equivalent movement on both screens.
- **FR-003** *(User Story 2)*: Every entry type that expands on the orchestrator screen (tool calls, reasoning, file diffs) MUST expand and collapse in subagent views via the same keyboard and mouse interactions, with the same multi-state cycle.
  - **Scenario**: Cycle each entry type on both screens; states and transitions match.
- **FR-004** *(User Story 2)*: Dedicated expand actions (tool-expand, reasoning-expand) MUST operate on the focused subagent view when one is focused, and on the orchestrator transcript otherwise.
  - **Scenario**: Focus a subagent view, invoke each action, confirm the subagent's entries respond.
- **FR-005** *(User Story 2)*: File diffs in subagent views MUST render with identical visual treatment as the orchestrator screen (old/new gutters, added/removed coloring, hunk headers, binary placeholders).
  - **Scenario**: The same diff shown on both screens renders identically apart from surrounding chrome text.
- **FR-006** *(User Story 3)*: Entry selection in subagent views MUST follow the orchestrator screen's selection model verbatim — the same way of marking the current entry and the same keys to move it — and the copy-entry action MUST copy the selected entry's content.
  - **Scenario**: Move the selection in a subagent view using the orchestrator screen's selection keys, invoke copy, paste elsewhere; the pasted content matches the selected entry.
- **FR-007** *(User Story 3)*: In-transcript search MUST operate within a focused subagent view, searching that subagent's transcript, highlighting matches, and scrolling to them on navigation.
  - **Scenario**: Search a term unique to one subagent; only that transcript yields matches.
- **FR-008** *(User Story 4)*: Maximized surfaces on the orchestrator screen MUST open from a focused subagent view and display that subagent's content whenever they can show it — the output viewer, reasoning panel, and stats page — with full scroll; mode-specific full-screen explorers MUST be reachable from a subagent view only when that mode spawned the subagent.
  - **Scenario**: Open each maximized surface (output viewer, reasoning panel, stats page) from a subagent view; content and behavior match orchestrator-screen usage.
- **FR-009** *(User Story 5)*: Visual chrome of subagent views (borders, headers, title format, status line, colors, stats page) MUST match orchestrator-screen conventions.
  - **Scenario**: Side-by-side comparison of both screens and both stats pages shows no styling divergence.
- **FR-010** *(User Story 5)*: Switching between the orchestrator screen and subagent views MUST preserve each view's scroll and expansion state.
  - **Scenario**: Expand an entry and scroll mid-transcript in a subagent view, switch away and back; state is unchanged.
- **FR-011** *(User Story 6)*: All requirements FR-001 to FR-010 MUST apply to every full-screen surface dedicated to a single subagent's transcript, regardless of which orchestration feature spawned it.
  - **Scenario**: Verify each subagent-spawning surface's dedicated view satisfies the same checklist.
- **FR-012** *(All stories)*: Each capability in FR-001 to FR-011 MUST have a repeatable acceptance check (manual or automated) confirming parity, runnable as part of the project's standard verification workflow.
  - **Scenario**: Run the full parity check set; every check passes and is re-runnable.
- **FR-013** *(User Story 5)*: The keyboard-help overlay MUST be reachable from focused subagent views with the same key as the orchestrator screen and MUST display the same content (one shared help overlay, identical everywhere).
  - **Scenario**: Open the help overlay on the orchestrator screen and from a focused subagent view; the invoking key and the displayed content are identical.

### Key Entities *(include if feature involves data)*

- **Orchestrator Screen**: The main agent transcript view; the reference behavior for all parity requirements.
- **Subagent View**: A full-screen view dedicated to one subagent's transcript.
- **Transcript Entry**: An expandable unit (message, tool call, reasoning block, file diff).
- **Expansion State**: The per-entry setting (collapsed / tail / full) tracked per view.
- **Scroll State**: The per-view position and follow mode.
- **Search State**: The per-view query, matches, and navigation.
- **Stats Page**: The per-screen summary presentation.
- **Navigation Rail**: The tab list used to move between the orchestrator screen and subagent views.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A capability checklist covering scrolling, expansion, diff rendering, copy, search, maximized viewers, stats, chrome, and help overlay shows zero items present on the orchestrator screen but missing from any full-screen subagent view.
- **SC-002**: Testers familiar only with the orchestrator screen complete all parity tasks (scroll to the oldest entry, expand a diff to full, copy an output, find and jump to a search match) in subagent views using only orchestrator-screen knowledge, with 100% task success.
- **SC-003**: Side-by-side design review of matched interactions on both screens confirms no visual differences in chrome, coloring, typography, or layout conventions.
- **SC-004**: View-state preservation (scroll + expansion) holds in 100% of switch-away-and-back test scenarios.
- **SC-005**: The repeatable parity acceptance checks pass on every run as part of the project's standard verification workflow, with no regressions to existing screen behavior.

## Assumptions

- "Subagent display" means full-screen views dedicated to a single subagent's transcript (the focused pane that takes over the main area) and equivalent dedicated views; auxiliary overlay panels that are not dedicated to a single subagent's transcript are out of scope. Within scope, maximized-surface reachability follows a rule: any orchestrator-screen maximized surface that can display the focused subagent's content must be reachable from the subagent view; mode-specific explorers apply only when that mode spawned the subagent.
- "Identical" means functional and visual parity of capabilities; the content shown obviously differs (each view shows its own transcript).
- Interactions reuse the orchestrator screen's existing key bindings and mouse gestures; no new binding scheme is introduced, and context-specific actions target the focused view.
- Streaming/follow-tail semantics match existing orchestrator-screen behavior.
- Parity must not degrade rendering responsiveness of subagent views relative to the orchestrator screen; the measurable budget is fixed in the implementation plan (plan.md, Performance Goals): same rendering complexity class as the orchestrator screen, with no additional per-frame allocations or polling/timers beyond what the orchestrator screen already incurs.
- No changes to on-disk formats or public CLI contracts are expected; scope is the interactive terminal UI surface (to be confirmed in planning).
- Out of scope: changing the orchestrator screen's own behavior or adding new capabilities beyond what the orchestrator screen already has (parity only).
