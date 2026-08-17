# Feature Specification: Universal Web-Page Browsing & Complex SPA Navigation

**Feature Branch**: `016-please-modify-joey`

**Created**: 2026-08-17

**Status**: Draft

**Input**: User description: "Optimize joey-agent for browsing web pages via a web browser, with a focus on navigating complex pages like the Pega Infinity Studio UI. Incorporate a five-phase blueprint for universal coverage: (1) universal DOM extraction that pierces shadow-DOM encapsulation and same-/cross-origin frames using ephemeral element references re-established before every action; (2) cascading element targeting (unique reference → structural locator → visible text → screen coordinates); (3) an expanded action space (hover, per-container scroll, native dropdown select, drag-and-drop, modified key presses, coordinate clicks); (4) state management via content-settle detection instead of network-idle, automatic dismissal of cookie/consent overlays before the model sees the page, and delta-only snapshots for infinite scroll; (5) a cascading observation model — structural/text first, visual Set-of-Mark screenshot fallback when a page cannot be scraped. Mid-turn addition: every LLM provider must offer a dedicated, configurable image-capable model for webpage/screenshot understanding."

## Clarifications

### Session 2026-08-17

- Q: Which tab does the agent drive when attached to the user's live browser? → A: The agent opens and works in its own dedicated tab; the user's active tab is never hijacked.
- Q: What happens when no attachable browser is running? → A: Attach when available; otherwise the system automatically launches and manages a dedicated browser instance (headless when no display exists).
- Q: How much of a dense page does each snapshot present to the model? → A: Viewport-priority: discovery is page-wide, but in/near-view elements are listed fully and out-of-view regions are summarized compactly, revealed by scrolling.
- Q: What is the default policy for dismissing blocking overlays? → A: Conservative: auto-dismiss only high-confidence standard consent/notification overlays with a clearly safe dismissal control; flag all others to the model; behavior is configurable.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - See Everything on a Complex SPA (Priority: P1)

The agent navigates to a modern single-page application (reference stress test: the Pega Infinity Studio UI) and takes a snapshot. Today, standard element queries miss controls hidden inside web-component encapsulation (shadow trees) and nested frames, so the agent is blind to much of the UI. With this feature, every interactive control — including buttons inside nested shadow roots, content in same-origin frames, and content in cross-origin frames — appears in the snapshot as a first-class, actionable element, each labeled with its frame context and carrying the fallback information needed to act on it later.
Discovery is page-wide, but presentation is viewport-priority: elements in or near the current view are listed in full, while out-of-view regions are summarized compactly (what exists, where) so the model can scroll to reveal them — keeping studio-density pages within a bounded snapshot size.

**Why this priority**: Perception is the foundation of every downstream capability. If the agent cannot see an element, nothing else matters. Pega Studio-style enterprise UIs are dense with encapsulated components and frames; this story alone already delivers value (accurate page understanding) even if no other story lands.

**Independent Test**: Can be fully tested by pointing the agent at fixture pages containing nested shadow roots and same-/cross-origin frames and verifying that every ground-truth interactive element appears in the snapshot with correct role, text, and frame labeling.

**Acceptance Scenarios**:

1. **Given** a page with controls nested three levels deep in shadow roots, **When** the agent takes a snapshot, **Then** all shadow-nested controls are listed alongside the top-level controls.
2. **Given** a page embedding same-origin frames, **When** the agent takes a snapshot, **Then** frame-hosted controls are listed and each carries a frame-context label.
3. **Given** a page embedding a cross-origin frame, **When** the agent takes a snapshot, **Then** the frame's interactive elements are still discovered through a browser-level inspection channel that is not subject to page-origin restrictions.
4. **Given** a page with far more interactive elements than fit the view (e.g., an enterprise studio UI), **When** the agent takes a snapshot, **Then** in/near-view elements are listed fully and out-of-view content is summarized compactly, and scrolling reveals additional elements into the full listing.

---

### User Story 2 - Act Reliably Despite Re-Renders (Priority: P1)

On frameworks that destroy and recreate DOM nodes constantly (React/Vue-era SPAs), an element reference captured in one snapshot may be gone a second later. The agent re-establishes page state immediately before every action and, when a reference has gone stale, automatically retries through an ordered fallback cascade — unique reference → structural locator → visible-text match → screen-coordinate action — reporting which strategy succeeded.

**Why this priority**: Perception without reliable action produces snapshots the agent cannot use. Stale-reference recovery is the difference between a demo and a dependable browsing agent on enterprise UIs that re-render on hover, focus, and polling timers.

**Independent Test**: Can be tested on a fixture page that deliberately destroys and rebuilds its DOM on a short interval: attempt clicks and verify the fallback cascade recovers each action and reports the strategy used.

**Acceptance Scenarios**:

1. **Given** a snapshot whose referenced element was destroyed by a re-render before the action ran, **When** the agent attempts the action, **Then** the system re-scans, resolves the element via the fallback cascade, and completes the action.
2. **Given** a target element with a stale reference and a surviving structural locator, **When** the action executes, **Then** the structural locator is tried before text matching, and the action result states which strategy succeeded.
3. **Given** a page where all structural resolution fails, **When** the agent acts, **Then** the action is performed at the element's last known on-screen position via coordinate-level input.
4. **Given** multiple elements matching by text (e.g., three "Submit" buttons), **When** the agent attempts a text-based fallback, **Then** the system disambiguates using position/context or refuses to act and reports the candidates instead of clicking an arbitrary match.

---

### User Story 3 - Work Inside My Logged-In Browser (Priority: P1)

The agent attaches to the user's already-running browser session — reusing existing logins (e.g., an authenticated Pega Infinity Studio) — rather than launching a sterile, logged-out browser. The user can connect, check status, and disconnect; the agent never handles or stores the user's credentials.
The agent conducts all navigation inside a dedicated tab it opens itself, so the tab the user is actively reading is never navigated away underneath them.
When no attachable browser is running, the system automatically launches and manages its own dedicated browser instance instead — headless when no display is available — so the capability works for first-time users, tests, and unattended automation without pre-configuration.

**Why this priority**: The primary target (Pega Studio) sits behind authentication. Without attaching to an authenticated session, the agent simply cannot reach the UI it is being built to navigate. Session lifecycle is also the entry point for every other story.

**Independent Test**: Can be tested by connecting to a live browser with an active login, verifying connection status is reported, performing one navigation in the attached session, and disconnecting cleanly.

**Acceptance Scenarios**:

1. **Given** a running browser with an active authenticated session, **When** the user connects the agent, **Then** subsequent navigations land in that session with its logins intact.
2. **Given** a connected session with the user actively viewing a tab, **When** the agent navigates, **Then** the navigation happens in the agent's dedicated tab and the user's tab is left untouched.
3. **Given** a connected session, **When** the user asks for connection status, **Then** the system reports the live connection state.
4. **Given** a connected session, **When** the user disconnects, **Then** the agent stops controlling the browser and the user's session continues undisturbed.
5. **Given** no attachable browser is running, **When** the agent needs to browse, **Then** the system launches and manages a dedicated browser instance (headless when no display exists) and proceeds with the task.

---

### User Story 4 - Full Interaction Vocabulary (Priority: P2)

Beyond click-and-type, the agent can hover (menus that only open on hover), scroll a specific nested container (a scrolling panel inside a scrolling page), select options in native dropdowns, drag-and-drop (kanban boards, upload zones), and press key combinations with modifiers (e.g., Cmd+Enter to submit).

**Why this priority**: These verbs are what separate "can read a page" from "can operate a page". Enterprise UIs lean heavily on hover menus, panel scrolling, and keyboard shortcuts, but none of these are needed for the P1 perception/targeting/attach stories to deliver value.

**Independent Test**: Each verb can be tested on its own fixture (hover-revealed menu, nested scroll container, native select, drag source/target, keyboard-shortcut trigger) without any other story in place.

**Acceptance Scenarios**:

1. **Given** a menu that opens only on hover, **When** the agent hovers the trigger, **Then** the menu items become visible in the next snapshot.
2. **Given** a scrollable panel nested inside the page's main scroll area, **When** the agent scrolls that specific panel, **Then** only the panel's content advances.
3. **Given** a native dropdown, **When** the agent selects an option, **Then** the control reports the chosen value.
4. **Given** a drag source and drop target, **When** the agent performs drag-and-drop, **Then** the item moves to the target.
5. **Given** a control listening for a modified key press, **When** the agent sends the key with modifiers, **Then** the control's handler fires.

---

### User Story 5 - Smart Waiting and Overlay Removal (Priority: P2)

After each action or navigation, the agent waits for the page to settle based on observed content changes (a bounded quiet window) instead of waiting for "network idle" — which never arrives on continuously-mutating SPAs — or fixed sleeps. Before the model ever sees a snapshot, blocking overlays (cookie/consent banners, newsletter pop-ups, interstitial dialogs) are detected. By default the system auto-dismisses only high-confidence standard consent/notification overlays that offer a clearly safe dismissal control; every other overlay is flagged explicitly for the model to decide — protecting cases where the overlay itself is part of the task. The auto-dismissal policy is configurable.

**Why this priority**: Hanging on never-idle SPAs and getting stuck behind cookie banners are the two most common hard failures in practice. Fixing them converts frequent dead-ends into completed tasks, but the P1/P2 stories above remain independently valuable without this polish.

**Independent Test**: Can be tested with a fixture page that mutates continuously (no idle ever) plus a consent modal on load: verify the agent settles within the bounded window, auto-dismisses the modal, and proceeds without model involvement in the dismissal.

**Acceptance Scenarios**:

1. **Given** a page that mutates continuously (polling, animations), **When** the agent waits for stability, **Then** it proceeds after a bounded quiet window rather than hanging, and never waits past a hard timeout.
2. **Given** a page with a high-confidence standard consent banner on load, **When** the agent prepares the snapshot, **Then** the banner is dismissed automatically before the model sees the page.
3. **Given** a blocking overlay with no safe dismissal control, **When** the agent prepares the snapshot, **Then** the snapshot explicitly flags the blocker so the model can decide how to handle it.
4. **Given** a task-relevant dialog (e.g., a first-run tour whose steps contain the target button, or a required cookie choice), **When** the agent prepares the snapshot, **Then** the overlay is flagged for the model rather than auto-dismissed.

---

### User Story 6 - Dedicated Image Model per Provider (Priority: P2)

Every LLM provider configuration gains an optional dedicated image-capable model setting used for webpage/screenshot understanding — independent of the primary text model. When set, screenshots and page imagery are routed to that model while text workflows remain on the primary model. When unset, the system falls back to a sensible default (the provider's multimodal default, or the primary model if it is image-capable). Switching the setting is configuration-only: no code changes, effective for new sessions.

**Why this priority**: Visual understanding of webpages (Story 7) needs an image-capable model, but the best text model is often not the best image model. This decoupling lets each provider pair a strong text model with a strong image model. It is configuration surface, independently deliverable and testable before any vision interaction exists.

**Independent Test**: Configure the setting for each supported provider and verify — via a routing check with a captured visual payload — that the image model receives visual content and the primary model receives text, with defaults applying when unset.

**Acceptance Scenarios**:

1. **Given** any supported provider, **When** the user sets its dedicated image model in configuration, **Then** subsequent visual-understanding requests are served by that model with no code changes.
2. **Given** a provider with no image model set, **When** visual content must be understood, **Then** the system falls back to the documented default and reports which model was used.
3. **Given** mixed content (text task plus screenshot), **When** the turn executes, **Then** the screenshot goes to the image model and the text task to the primary model.

---

### User Story 7 - Navigate What Cannot Be Scraped (Priority: P3)

For pages that fundamentally resist structural extraction (canvas-rendered UIs, maps, obfuscated viewers), the agent automatically falls back to visual observation: it captures the screen, annotates candidate interactables with numbered markers, presents the annotated image, and executes the model's chosen marker as a coordinate action. The agent returns to structural observation as soon as it becomes available again.

**Why this priority**: This is the long-tail insurance policy — most tasks never need it, but without it a class of pages is simply impossible. It depends on image-model configuration (Story 6) being present with defaults.

**Independent Test**: Point the agent at a canvas-only fixture page with no extractable elements, assign a click goal, and verify the visual fallback engages, presents numbered markers, and completes the goal by coordinate action.

**Acceptance Scenarios**:

1. **Given** a page where structural extraction yields no actionable elements, **When** the agent observes the page, **Then** it automatically switches to annotated-screenshot mode with numbered markers.
2. **Given** annotated-screenshot mode, **When** the model picks a marker, **Then** the system performs the action at that marker's coordinates.
3. **Given** visual mode on a page that later becomes structurally readable (e.g., after login), **When** the next observation runs, **Then** the system returns to structural mode.

---

### User Story 8 - Infinite Feeds Without Context Explosion (Priority: P3)

When a task requires walking an incrementally loading feed, the agent's snapshots stay bounded: each step reports only the elements newly revealed since the last scroll plus a compact summary of what lies out of view, with a hard cap on cumulative snapshot growth per task.

**Why this priority**: Unbounded feeds would otherwise exhaust the model's context and make long walks impossible. It is an efficiency multiplier on already-working perception and action rather than a gate for them.

**Independent Test**: Drive a fixture feed that appends items on scroll for several screens and verify each step's snapshot contains only new items plus a bounded summary, and that cumulative growth respects the cap.

**Acceptance Scenarios**:

1. **Given** a feed that appends content as the agent scrolls, **When** the agent takes the next snapshot, **Then** only newly revealed interactive elements are listed, plus a bounded out-of-view summary.
2. **Given** a long-running task over a growing feed, **When** snapshots accumulate, **Then** cumulative snapshot material stays within the configured budget regardless of scroll depth.

---

### Edge Cases

- The target element is destroyed in the interval between the pre-action re-scan and the action itself (mid-click re-render): the action must fail gracefully with a diagnostic, not act on the wrong element.
- Cross-origin frames that actively resist inspection: discovery must degrade to whatever the browser-level channel permits and clearly mark the frame as partially observable rather than silently omitting it.
- Circular or unusually deep nesting of shadow roots and frames: traversal must be depth-capped to terminate, with capped regions flagged in the snapshot.
- Pages where zero interactive elements are extractable (canvas-rendered): triggers visual fallback rather than an empty dead-end snapshot.
- Overlays that reappear after dismissal (persistent consent flows): repeated dismissals must be rate-limited and escalated to the model instead of looping.
- Text-based fallback ambiguity (multiple identical labels): disambiguate by position/context or refuse with candidates listed.
- Never-settling pages (continuous animation/polling): bounded quiet-window detection with a hard timeout and an informative "still unstable" state.
- Navigations blocked by URL-safety rules (local/private network targets): must be refused by the same protections that govern the agent's other web tools.
- Frame navigates away mid-session (frame context becomes invalid): stale frame contexts must be detected and refreshed on the next scan.
- Extremely large DOMs: snapshot generation must remain within its latency budget by prioritizing interactive elements over full structural dumps.

## Requirements *(mandatory)*

### Functional Requirements

**Perception**

- **FR-001**: The system MUST discover all user-interactive elements on a page, including elements inside web-component encapsulation (shadow trees) at any nesting depth, and present them as first-class actionable elements in the snapshot.
- **FR-002**: The system MUST discover elements inside nested same-origin frames and label each element with its frame context.
- **FR-003**: For cross-origin frames that resist in-page inspection, the system MUST obtain element information through a browser-level inspection channel not subject to page-origin restrictions.
- **FR-004**: Every discovered element MUST be presented with a compact unique reference plus the properties needed to act on it later: visible text, a structural fallback locator, and on-screen position/geometry.
- **FR-004a**: Element discovery MUST run page-wide, but snapshot presentation MUST be viewport-priority: elements in or near the current viewport are listed in full, and out-of-view regions are summarized compactly (e.g., counts/description by region) with enough information for the model to decide to scroll and reveal them. Presentation MUST NOT silently truncate without indicating that out-of-view content exists.

**Targeting resilience**

- **FR-005**: The system MUST re-validate live page state immediately before every action; it MUST NOT rely on element references captured in an earlier snapshot when acting.
- **FR-006**: When an element reference is stale or unresolvable at action time, the system MUST automatically retry through an ordered fallback cascade — unique reference → structural locator → visible-text match → screen-coordinate action — and MUST report which strategy was used.
- **FR-007**: When a fallback match is ambiguous (multiple elements match), the system MUST disambiguate by position or context, or refuse the action and report the candidates; it MUST NOT act on an arbitrary match.

**Action space**

- **FR-008**: The action vocabulary MUST include: click, type, hover, page scroll, scroll of a specific nested container, native dropdown option select, drag-and-drop from a source element to a target element, and key press with modifier combinations.
- **FR-009**: The system MUST support coordinate-level input (acting at a screen position) so elements that never expose page-level handlers remain operable.

**State management**

- **FR-010**: After each action or navigation, the system MUST determine page stability from observed content-change settling (a bounded quiet window), with a configurable window and a hard timeout fallback; it MUST NOT depend on network-idle signals or fixed sleeps as the primary strategy.
- **FR-011**: Before presenting a snapshot to the model, the system MUST detect common blocking overlays (consent banners, modal dialogs, interstitials). By default it MUST auto-dismiss only high-confidence standard consent/notification overlays offering a clearly safe dismissal control, and MUST flag every other detected overlay explicitly in the snapshot for the model to decide. The auto-dismissal policy MUST be configurable (never / conservative-default / aggressive).
- **FR-012**: For incrementally loading feeds, snapshots MUST contain only newly revealed interactive elements plus a bounded summary of out-of-view content, and MUST enforce a configurable cap on cumulative snapshot material per task.

**Observation fallback**

- **FR-013**: When structural extraction yields no actionable elements or repeated actions fail to resolve their targets, the system MUST automatically switch to visual observation: a screenshot annotated with numbered markers on candidate interactables, presented in place of the structural snapshot.
- **FR-014**: In visual mode, the system MUST execute the model's chosen marker as a coordinate action, MUST make the active observation mode (structural vs visual) explicit in the snapshot, and MUST return to structural mode when structural extraction becomes viable again.

**Dedicated image model per provider**

- **FR-015**: Every supported LLM provider configuration MUST offer an optional dedicated image-capable model setting used for webpage and screenshot understanding, independent of the primary model, configurable without code changes and effective for new sessions.
- **FR-016**: When a dedicated image model is set, visual content MUST be routed to that model while text workflows continue on the primary model; when unset, the system MUST fall back to a documented default (provider multimodal default, or the primary model if image-capable) and report which model served the visual content.

**Session, integration, and safety**

- **FR-017**: The system MUST support attaching to the user's already-running browser session (preserving existing authenticated logins) and MUST expose connect, status, and disconnect controls; it MUST NOT handle or store user credentials itself. All agent navigation and actions MUST occur in a dedicated tab opened by the agent; the system MUST NOT navigate, alter, or close tabs the user is actively using. When no attachable browser is available, the system MUST automatically launch and manage a dedicated browser instance (headless where no display exists) so browsing works without manual browser configuration.
- **FR-018**: The browsing capability MUST be delivered through the agent's existing declared browser tool surface (navigate, snapshot, click, type, scroll, back, press, image capture, vision, console, raw protocol access, dialog handling) and MUST be reachable from both the CLI and interactive sessions with parity.
- **FR-019**: All browser-derived content MUST pass through the existing untrusted-content sanitization layers before reaching the model, consistent with how the agent already treats other external content.
- **FR-020**: Browser navigations MUST respect the agent's existing URL-safety rules (e.g., local/private network protections) that govern its other web tools.

### Key Entities *(include if feature involves data)*

- **Browser Session**: an attachment to a running browser (attachment state, lifecycle, status); the context in which all navigation and actions occur. Includes the agent's dedicated working tab and its identity, so the agent can reliably re-find its own page after re-renders or frame changes without touching user tabs.
- **Managed Browser**: a browser instance launched and supervised by the system itself when no user browser is attachable; carries its own lifecycle (launch, health, shutdown) and, without a display, runs invisibly.
- **Page Snapshot**: the observation unit presented to the model; carries the observation mode (structural or visual), the element set or annotated image, frame contexts, detected blockers, and — for feeds — the delta since the previous snapshot.
- **Interactive Element**: one discovered control; unique reference, role, visible text, frame context, structural fallback locator, and on-screen geometry.
- **Agent Action**: a verb (click, type, hover, scroll, scroll-container, select, drag-drop, key-press, coordinate-act) with a target descriptor comprising the reference and its fallback chain, plus verb-specific parameters.
- **Provider Image-Model Setting**: a per-provider optional configuration binding webpage/screenshot understanding to a dedicated image-capable model, with a documented fallback default.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: On a suite of representative complex pages — including an authenticated enterprise studio UI of Pega Infinity Studio complexity — the agent completes assigned navigation/interaction goals end-to-end with at least a 90% task success rate and no human intervention.
- **SC-002**: Element discovery covers at least 95% of ground-truth interactive elements on pages using encapsulated components, nested frames, and dynamic rendering, measured against a labeled fixture set.
- **SC-003**: At least 95% of attempted actions succeed on a fixture that deliberately destroys and rebuilds its DOM between observation and action (stale-reference recovery via the fallback cascade).
- **SC-004**: On continuously-mutating pages, stability detection completes within a 2-second median after content settles and never exceeds the configured hard timeout — zero indefinite hangs.
- **SC-005**: In the fixture suite, high-confidence standard consent overlays are auto-dismissed before the model sees the page in at least 90% of cases, and every remaining overlay is surfaced to the model (never silently ignored) so that no case requires human intervention.
- **SC-006**: On scroll-appending feeds, per-step snapshot material stays within the configured per-step budget and cumulative material respects its cap regardless of scroll depth.
- **SC-007**: On pages with zero structurally extractable elements, the visual fallback engages automatically and completes the assigned goal via marker-based interaction.
- **SC-008**: A dedicated image model can be configured for every supported provider through configuration alone; changing it requires no code change and takes effect on new sessions, with defaults documented and applied when unset.

## Assumptions

- The browser tool names already declared in the agent's toolset registry (navigate, snapshot, click, type, scroll, back, press, image capture, vision, console, raw protocol access, dialog handling) are the intended public surface this feature makes functional; they are currently declared-but-unregistered and resolved away at definition time.
- The supported browser scope is the Chromium family, consistent with the existing declared "connect to your live Chromium-family browser" surface; other engines are out of scope for this feature.
- Attaching to the user's own browser may require it to be started with remote-debugging enabled; the feature provides connection instructions for that path. When no attachable browser exists, the system launches its own managed Chromium-family instance (per FR-017), so a pre-configured user browser is not required.
- Dedicated image-model configuration keys live alongside existing model/provider configuration and follow the same layered-configuration and secret-routing rules; visual-capability varies by provider, so the setting is optional with documented defaults.
- Existing image-understanding plumbing (vision tooling and image content types in the provider layer) is extended rather than replaced; where a provider's request pipeline does not yet carry image content end-to-end, completing that path is part of this feature's scope.
- Existing sanitization/threat-scan and URL-safety layers are reused as-is; this feature routes browser-derived content through them and adds no bypasses.
- Any new runtime dependency required by the implementation must be justified against alternatives in the feature's research notes (per project governance), keeping binary size and compile-time cost in check.
- Anti-bot evasion and fingerprint spoofing are out of scope; the feature attaches to the user's real browser session, which inherits the user's normal browsing posture.
- Automated handling of payment flows, CAPTCHA solving, and credential entry are out of scope; authentication is inherited from the attached logged-in session.
- Mobile-browser-only rendering behavior is out of scope; the capability targets desktop-class browser sessions.
