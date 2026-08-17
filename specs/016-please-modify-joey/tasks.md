---
description: "Task list for Universal Web-Page Browsing & Complex SPA Navigation"
---

# Tasks: Universal Web-Page Browsing & Complex SPA Navigation

**Input**: Design documents from `/specs/016-please-modify-joey/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: Included — the spec defines independent-test criteria per story and the constitution (Principle IV) mandates tests alongside implementation for new crates/modules.

**Organization**: Tasks grouped by user story. Story priority order within equal priority is US3 → US1 → US2 (session is the entry point; perception precedes action) — all three are P1 and each checkpoint is independently testable.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (US1…US8)
- Exact file paths in every task

## Path Conventions

Workspace-relative. New crate: `crates/joey-browser/`. Tools: `crates/joey-tools/src/tools/browser_tools.rs`. Docs: `docs/`.

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Crate scaffolding and config surface — no behavior yet.

- [x] T001 Create `crates/joey-browser/` crate scaffold: `Cargo.toml` (deps: joey-core, tokio, serde_json, base64, thiserror via `[workspace.dependencies]`; first promote `tokio-tungstenite 0.23` to `[workspace.dependencies]` in root `Cargo.toml` — it is already in Cargo.lock via joey-speckit-ui but not yet a workspace dep), `src/lib.rs` with module skeleton (cdp, launch, session, page, extract, refs, actions, snapshot, vision, config), empty `tests/` dir; add crate to workspace members in root `Cargo.toml`. Verify: `cargo build -p joey-browser` green.
- [x] T002 [P] Add browser config keys (read-path only, additive) to `crates/joey-core/src/config.rs`: `browser.cdp_url`, `browser.executable_path`, `browser.headless`, `browser.overlay_policy`, `browser.allow_raw_cdp`, `browser.settle.quiet_ms`, `browser.settle.hard_timeout_ms`, `browser.snapshot.max_step_bytes`, `browser.snapshot.cumulative_cap_bytes`, `browser.snapshot.viewport_margin`, `model.image_model`, `providers.<id>.image_model` — with defaults and clamping per data-model.md §8. Unit tests: defaults, clamping, layered override, no-.env routing for these keys.
- [x] T003 [P] Write `crates/joey-browser/src/config.rs`: `BrowserConfig` struct resolved from joey-core dotted keys (per contracts/cdp-session.md + data-model.md §8) with enum validation (`headless: auto|always|never`, `overlay_policy: never|conservative|aggressive`). Unit tests for all invalid-value rejections.

**Checkpoint**: Crate builds; config surface exists; no runtime behavior.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: CDP transport + tool-registration scaffold that ALL stories depend on.

- [x] T004 Implement `crates/joey-browser/src/cdp/mod.rs`: WebSocket JSON-RPC transport to browser endpoint, flat session mux keyed by `sessionId`, command/response correlation with ids, event fan-out channel, reconnect-free error surface. Canned-JSON unit tests for framing, session routing, protocol-error mapping (no live browser).
- [x] T005 Implement `crates/joey-browser/src/cdp/domains.rs`: typed wrappers for the ~8 domains used (Target, Page, Runtime, Input, DOM, Emulation, Network-info, fetch-metrics) — request/response structs + `serde` round-trip tests per wrapper.
- [x] T006 Define `BrowserError` enum (all variants per contracts/cdp-session.md) in `crates/joey-browser/src/cdp/mod.rs` with `thiserror`; unit test each variant's Display message (they surface in tool errors).
- [x] T007 [P] Scaffold `crates/joey-tools/src/tools/browser_tools.rs`: `BrowserHandle` (Arc wrapper, trait-sealed per plan Structure Decision), `register_browser_tools(registry, Option<Arc<BrowserHandle>>)` with `check()`-gated hidden-until-wired behavior (neurocode pattern). Unit test: tools absent when handle None, present when Some (empty verb set is fine at this point).
- [x] T008 [P] Regression: extend `crates/joey-tools/tests/schema_snapshots.rs` harness so any newly registered browser tool schema is snapshot-pinned from the first registration (mechanism only; entries land with each story's tools).

**Checkpoint**: `cargo test -p joey-browser -p joey-tools` green; transport is testable via canned JSON; registration scaffold ready.

---

## Phase 3: User Story 3 - Work Inside My Logged-In Browser (Priority: P1) 🎯 MVP part 1

**Goal**: Attach to the user's running browser (dedicated tab, logins preserved) or auto-launch a managed instance headless; connect/status/disconnect; basic navigation + console + dialogs + gated raw CDP.

**Independent Test**: With a Chromium at `--remote-debugging-port=9222` and an active login: `/browser connect` → navigate in agent tab → `/browser status` → `/browser disconnect`; user's own tab untouched. With no browser: oneshot task auto-launches managed headless and completes.

### Tests for User Story 3

- [ ] T009 [US3] Integration test scaffold `crates/joey-browser/tests/browser_integration.rs`: Chromium discovery gate (auto-skip when absent), fixture HTTP server helper, /tmp-neutral-harness-safe invocation notes per repo memory. No assertions yet beyond harness self-check.
- [ ] T010 [US3] Integration tests in `crates/joey-browser/tests/browser_integration.rs` (attach + managed modes): dedicated-tab isolation (user tab never navigated/closed), login cookie preserved in agent tab, managed child terminated on disconnect (no orphan).

### Implementation for User Story 3

- [x] T011 [US3] Implement `crates/joey-browser/src/launch.rs`: per-OS Chromium-family discovery (macOS app bundles, Linux/Windows PATH + known locations; Chrome/Edge/Chromium/Brave), `browser.executable_path` override, managed launch with `--remote-debugging-port=0` (parse ephemeral port from stderr `DevTools listening on`), `--headless=new` when headless=auto and no display. Unit tests for discovery order + stderr parsing (canned).
- [x] T012 [US3] Implement `crates/joey-browser/src/session.rs`: `BrowserManager::connect` (probe `/json/version` 2s timeout → Attached; else launch → Managed), state machine per data-model.md §1 (Disconnected/Attached/Managed, child_exit → Disconnected), `ensure_page` (Target.createTarget, idempotent, exactly one agent tab), `disconnect` (kill child iff Managed), `status`.
- [x] T013 [US3] Implement navigation in `crates/joey-browser/src/page.rs`: `navigate`/`back` gated by `url_safety::is_safe_url` (return `UrlBlocked` before any CDP call), title/frame-count readback, console buffer (Runtime.consoleAPICalled subscription), dialog handling (Page.javascriptDialogOpening + accept/dismiss with prompt_text), raw CDP passthrough gated on `browser.allow_raw_cdp`.
- [x] T014 [US3] Implement tools in `crates/joey-tools/src/tools/browser_tools.rs`: `browser_navigate`, `browser_back`, `browser_console`, `browser_dialog`, `browser_cdp` with schemas per contracts/browser-tools.md; wire `BrowserHandle` construction into `crates/joey-cli/src/engine.rs` (connect on first browser tool use or `/browser connect`).
- [x] T015 [US3] Implement `/browser connect|disconnect|status` subcommand handler in `crates/joey-cli/src/slash.rs` (+ handler module; update the existing advertised command to these real subcommands) working identically in REPL and TUI.
- [x] T016 [US3] Unit tests: URL-block path (local/private targets refused before CDP), RawCdpDisabled when gate off, status shape; pin tool schemas from T014 in schema_snapshots.

**Checkpoint**: Basic browsing works end-to-end in both attach and managed modes. MVP demo-able.

---

## Phase 4: User Story 1 - See Everything on a Complex SPA (Priority: P1) 🎯 MVP part 2

**Goal**: Deep perception — shadow-piercing, frame-aware (same- and cross-origin), viewport-priority snapshots with ephemeral refs and fallback locators.

**Independent Test**: Fixture pages (3-deep shadow roots, same-origin iframe nest, cross-origin iframe on second port): every ground-truth interactive element appears with correct role/text/frame label; dense-page fixture yields in-view full listing + compact out-of-view summaries.

### Tests for User Story 1

- [ ] T017 [P] [US1] Fixture pages in `crates/joey-browser/tests/fixtures/`: `shadow-nest.html` (3 levels), `frames.html` + `frame-child.html` (same-origin nest), `cross-origin.html` + `cross-origin-child.html` (served on second port), `dense-studio.html` (hundreds of controls, below-fold regions), each with `data-ground-truth` markers for coverage scoring.
- [ ] T018 [US1] Golden/round-trip tests `crates/joey-browser/tests/snapshot_format.rs`: Snapshot v1 envelope, ElementRef rules (refid pattern, 120-char text cap, attribute allowlist, char-boundary truncation), RegionSummary shape, viewport-priority ordering, unique refids — per contracts/snapshot-format.md.
- [ ] T019 [US1] Integration tests in `browser_integration.rs`: discovery coverage ≥95% vs `data-ground-truth` on all three piercing fixtures (SC-002), frame labels correct, dense fixture shows out-of-view summaries + no silent truncation, refids never reused across scans.

### Implementation for User Story 1

- [x] T020 [US1] Write `crates/joey-browser/src/extract/js/scan.js` (include_str!): recursive interactive-element scanner — standard selectors + `shadowRoot` traversal at any depth (depth-capped, capped regions reported), same-origin frame `contentDocument` traversal, role/text/allowlisted-attributes/geometry/interactable extraction, locator computation (CSS-first structural), zero DOM mutation.
- [ ] T021 [US1] Implement frame tree + cross-origin piercing in `crates/joey-browser/src/page.rs`: `Target.setAutoAttach {flatten:true}` per-target session fan-out, per-frame scan execution + merge with frame-context labels (`main`, `iframe:name`, `oopif:name`), stale-frame detection on navigation, partial-observability marking for resistant frames (spec edge case).
- [x] T022 [US1] Implement `crates/joey-browser/src/refs.rs`: ElementRef registry — refid assignment (`e<N>`, reset per scan), descriptor storage for the cascade (US2 consumes), locator/text/geometry validation.
- [x] T023 [US1] Implement `crates/joey-browser/src/snapshot.rs`: viewport-priority presentation (in-view full, near-view margin `browser.snapshot.viewport_margin`, out-of-view RegionSummary), serialization v1 + compact line grammar, pretty ≤4KB else compact.
- [x] T024 [US1] Implement `browser_snapshot` tool (viewport_only param) in `crates/joey-tools/src/tools/browser_tools.rs`; schema pinned in schema_snapshots.
- [ ] T025 [US1] Perf budget test (plan Technical Context): snapshot ≤500ms median on dense-studio fixture (≤5k elements) — recorded as a `#[ignore]`-by-default benchmark-style test with the number asserted when run explicitly.

**Checkpoint**: Full perception on Pega-Studio-class pages. MVP complete (US3+US1).

---

## Phase 5: User Story 2 - Act Reliably Despite Re-Renders (Priority: P1)

**Goal**: Pre-action re-validation + cascading fallback resolution (refid → locator → text → geometry) with `resolved_by` reporting and ambiguity refusal.

**Independent Test**: Churn fixture (DOM destroyed/rebuilt every 500ms): ≥95% of click/type attempts succeed via cascade and report the strategy used; ambiguous text matches refused with candidates.

### Tests for User Story 2

- [ ] T026 [P] [US2] Fixture `crates/joey-browser/tests/fixtures/churn.html`: interval-driven destroy/rebuild of controls, plus `ambiguous.html` (three identical "Submit" buttons at different positions).
- [ ] T027 [US2] Unit tests `crates/joey-browser/tests/refs_fallback.rs`: cascade matcher on mock element sets — refid hit, locator hit (preferred over text), text match, geometry fallback, ambiguity refusal with candidates, mid-resolution element-gone (TargetNotFound diagnostic, no wrong-element action).
- [ ] T028 [US2] Integration tests in `browser_integration.rs`: ≥95% action success on churn fixture (SC-003), resolved_by reported per action, ambiguous fixture refuses with 3 candidates.

### Implementation for User Story 2

- [x] T029 [US2] Implement fallback cascade resolver in `crates/joey-browser/src/refs.rs`: ordered strategies, position/context disambiguation for text matches, candidate collection on refusal, `ResolvedBy` reporting — pure over re-scan output (FR-005/006/007).
- [x] T030 [US2] Implement `crates/joey-browser/src/actions.rs`: pre-action re-scan pipeline (registry reset → descriptor re-match → execute), click (Input.dispatchMouseEvent at element center) and type (focus + Input.insertText with clear/submit options).
- [x] T031 [US2] Implement `browser_click`, `browser_type` tools in `crates/joey-tools/src/tools/browser_tools.rs` with target-descriptor param per contracts/browser-tools.md; schemas pinned.

**Checkpoint**: All P1 stories done — the core browsing agent is dependable.

---

## Phase 6: User Story 4 - Full Interaction Vocabulary (Priority: P2)

**Goal**: hover, page/container scroll, native select, drag-and-drop, modified key press, coordinate click.

**Independent Test**: Per-verb fixtures: hover menu reveals items; only the targeted nested container scrolls; select reports chosen value; drag moves item; Cmd+Enter handler fires; coordinate click hits element with no handlers.

### Tests for User Story 4

- [ ] T032 [P] [US4] Fixtures in `crates/joey-browser/tests/fixtures/`: `hover-menu.html`, `nested-scroll.html`, `native-select.html`, `drag-board.html`, `shortcut.html`, `handlerless.html` (coordinate-click target) — each self-verifying via DOM markers test helpers read back.
- [ ] T033 [US4] Integration tests in `browser_integration.rs` covering all six verbs' acceptance scenarios (spec Story 4 scenarios 1–5 + FR-009); also assert `browser_get_images` returns the fixture pages' image inventory.

### Implementation for User Story 4

- [x] T034 [US4] Implement verbs in `crates/joey-browser/src/actions.rs`: hover (Input.dispatchMouseEvent mousemove), page scroll (Input.synthesizeScrollGesture or wheel events), container scroll (scroll targeted element via JS after re-scan resolve), select_option (JS value set + change event), drag (mouse down/move/up sequence source→target), press_key (Input.dispatchKeyEvent with modifiers), click_coords (viewport pixels, FR-009).
- [x] T035 [US4] Implement tools in `crates/joey-tools/src/tools/browser_tools.rs`: `browser_scroll` (page-level with optional `target` param for container scroll — additive optional param per contracts versioning), `browser_hover`, `browser_select_option`, `browser_drag`, `browser_click_coords`, `browser_press`, and `browser_get_images` (declared core names made functional — closes the FR-018 surface); append only the 4 genuinely new names (`browser_hover`, `browser_select_option`, `browser_drag`, `browser_click_coords`) to the CORE_TOOLS browser block in `crates/joey-tools/src/toolsets.rs` (additive, order-preserving) + toolset resolution regression test; schemas pinned.

**Checkpoint**: The agent can operate pages, not just read them.

---

## Phase 7: User Story 5 - Smart Waiting and Overlay Removal (Priority: P2)

**Goal**: MutationObserver quiet-window settle (bounded, never hangs) + conservative overlay auto-dismissal with rate limiting and model-visible flags.

**Independent Test**: Never-settling fixture: proceeds within quiet window bounds, never past hard timeout; consent modal auto-dismissed before model sees snapshot; tour dialog flagged, not dismissed; reappearing overlay rate-limited then escalated.

### Tests for User Story 5

- [ ] T036 [P] [US5] Fixtures: `never-settle.html` (continuous mutation), `consent.html` (standard banner), `tour-dialog.html` (task-relevant overlay), `zombie-overlay.html` (reappears after dismiss).
- [ ] T037 [US5] Unit tests `crates/joey-browser/tests/overlay_detect.rs`: overlay heuristics on fixture DOM dumps (canned) — consent vs dialog classification, safe-dismissal-control detection, rate-limit ledger transitions (≥3 → flagged).
- [ ] T038 [US5] Integration tests in `browser_integration.rs`: settle within 2s median after content settles + hard timeout on never-settle (SC-004); consent auto-dismissed pre-snapshot (SC-005 ≥90%); tour flagged; zombie rate-limited and escalated.

### Implementation for User Story 5

- [x] T039 [US5] Write `crates/joey-browser/src/extract/js/observer.js`: MutationObserver settle probe — quiet-window promise (default 1500ms), sentinel-marked so its own mutations don't count (guarantee 7b, cdp-session.md), hard-timeout bounded (default 10s), `Page.loadEventFired` as early hint only.
- [ ] T040 [US5] Wire settle into `crates/joey-browser/src/page.rs` + `actions.rs`: post-action/post-navigation wait returns `settled_ms` or `SettleTimeout` with partial state.
- [x] T041 [US5] Write `crates/joey-browser/src/extract/js/overlays.js`: detection heuristics (high z-index fixed overlays, role=dialog, consent text patterns, pointer-events blocking) + safe-dismissal control identification; conservative-only auto-dismiss honoring `browser.overlay_policy`.
- [ ] T042 [US5] Implement OverlayState in `crates/joey-browser/src/page.rs`: per-(frame, signature) dismissal ledger, ≥3 → permanent flag (spec edge case), Blocker records into Snapshot (kind/description/dismissal) per snapshot-format.md.

**Checkpoint**: Dead-end failure modes (hangs, banners) eliminated.

---

## Phase 8: User Story 6 - Dedicated Image Model per Provider (Priority: P2)

**Goal**: Config-only per-provider image model with documented-default fallback, routed visual content, completed image serialization on all wires, `served_by` reporting.

**Independent Test**: Set/unset `model.image_model` + `providers.<id>.image_model` for each provider: routing check with captured visual payload shows image model serving images and primary serving text; defaults applied and reported when unset (SC-008).

### Tests for User Story 6

- [x] T043 [US6] Unit tests `crates/joey-core/src/config.rs` (extend T002's): `model.image_model` + `providers.<id>.image_model` (per-provider wins), unset paths.
- [x] T044 [US6] Unit tests for resolver in `crates/joey-agent-core` (new test module beside the helper): all five resolution orders incl. `unavailable(reason)` with actionable message naming the config keys.
- [x] T045 [US6] Wire regression tests in `crates/joey-providers` (per contracts/image-model-routing.md table): image parts serialize on openai-chat (incl. zai), openai-responses (with `"type":"message"` on input items), anthropic (data-URL source), copilot (both wire modes); no-image request bodies byte-identical to pre-feature snapshots.

### Implementation for User Story 6

- [x] T046 [P] [US6] Implement `resolve_image_model` pure helper in `crates/joey-agent-core/src/` (new module): per-provider → global → catalog multimodal default (joey-llm-selector `supports_vision` data) → primary-if-vision → unavailable; returns `ResolvedImageModel { model_id, source }`.
- [x] T047 [US6] Complete image-content serialization: `crates/joey-providers/src/chat.rs` (openai-chat image_url parts), request builder for openai-responses input image items (MUST carry `"type":"message"`), verify/complete `crates/joey-providers/src/anthropic.rs` base64 source, `crates/joey-providers/src/copilot.rs` image passthrough in both wire modes.
- [x] T048 [US6] Implement routing in `crates/joey-agent-core/src/`: turns whose content contains image parts are served by the resolved image model on the same provider stack; text turns byte-identical behavior (regression test); `served_by { model, source }` surfaced in vision-capable tool results and logs.
- [x] T049 [US6] Implement `vision_analyze` tool (declared core name, currently unregistered) in `crates/joey-tools/src/tools/file_tools.rs` or new `vision_tools.rs`: image file/data-URL input → image-model analysis with `served_by` reporting; register in `builtins.rs`; schema pinned.

**Checkpoint**: Visual understanding is configurable per provider, no code changes.

---

## Phase 9: User Story 7 - Navigate What Cannot Be Scraped (Priority: P3)

**Goal**: Automatic visual fallback — SoM-annotated screenshots with numbered markers, marker→coordinate execution, auto mode switching.

**Independent Test**: Canvas-only fixture (zero extractable elements) with a click goal: fallback engages, markers presented, goal completed via coordinate action; page that becomes readable returns to structural mode (SC-007).

### Tests for User Story 7

- [ ] T050 [P] [US7] Fixtures: `canvas-only.html` (all interactions drawn on canvas), `login-then-dom.html` (starts opaque, becomes DOM-driven after interaction).
- [ ] T051 [US7] Integration tests in `browser_integration.rs`: zero-element page triggers visual mode with marker table; marker pick executes at coordinates; mode flips back to structural on `login-then-dom` after reveal; snapshot carries explicit `mode` field both ways.

### Implementation for User Story 7

- [x] T052 [US7] Implement `crates/joey-browser/src/vision.rs`: `Page.captureScreenshot` viewport capture, marker overlay injection (sentinel-marked DOM, removed after capture — guarantee 7b), strategy selection (dom_geometry when any geometry exists, else coarse_grid), marker table + VisualObservation serialization per snapshot-format.md.
- [ ] T053 [US7] Implement fallback trigger + mode state in `crates/joey-browser/src/page.rs`: zero actionable elements OR repeated resolution failures (threshold, e.g. 3 consecutive) → visual mode; structural viability re-checked each observation; mode explicit in every snapshot (FR-013/014).
- [x] T054 [US7] Implement `browser_vision` tool in `crates/joey-tools/src/tools/browser_tools.rs` (prompt param, VisualObservation + `served_by` result) and marker-pick path in `browser_click_coords` (accepts `marker` param — additive); schemas pinned.

**Checkpoint**: No page class is a dead end.

---

## Phase 10: User Story 8 - Infinite Feeds Without Context Explosion (Priority: P3)

**Goal**: Delta snapshots (new elements + gone refids + bounded out-of-view summary) with per-step and cumulative budgets.

**Independent Test**: Feed fixture appending items on scroll across several screens: each step lists only new elements + bounded summary; cumulative material ≤ cap regardless of depth (SC-006).

### Tests for User Story 8

- [ ] T055 [P] [US8] Fixture `crates/joey-browser/tests/fixtures/feed.html`: scroll-appending list (50+ items over several viewports).
- [ ] T056 [US8] Unit tests `crates/joey-browser/tests/snapshot_format.rs` (extend): delta computation on synthetic snapshot pairs — new/gone diffing, budget enforcement (default 8KB step / 64KB cumulative), truncation info with reason + omitted count, never-silent rule.

### Implementation for User Story 8

- [ ] T057 [US8] Implement delta pipeline in `crates/joey-browser/src/snapshot.rs`: element identity across scans (descriptor match), `new_elements`/`gone_refids`, RegionSummary compaction under budget, cumulative ledger per task, `truncation` emission per snapshot-format.md.
- [ ] T058 [US8] Add `since_last` param to `browser_snapshot` in `crates/joey-tools/src/tools/browser_tools.rs` (additive optional); integration test on feed fixture asserting budget bounds.

**Checkpoint**: All eight stories complete.

---

## Phase 11: Polish & Cross-Cutting Concerns

**Purpose**: Regression hardening, docs, parity records, full-suite green.

- [ ] T059 [P] Regression: run and extend `crates/joey-tools/tests/schema_snapshots.rs` — all 16 browser_* tool schemas (12 declared + 4 new) + vision_analyze pinned; `crates/joey-tools/src/toolsets.rs` toolset-resolution regression tests: 4 appended names resolve alongside the 12 declared ones, membership order preserved.
- [ ] T060 [P] Regression: untrusted-content pipeline verification — `crates/joey-agent-core` test asserting browser_* outputs flow through the same wrapping/sanitization as other UNTRUSTED_TOOL_PREFIXES tools (FR-019); redaction spot-checks on snapshot text fields.
- [x] T061 [P] Docs: write `docs/browser.md` (architecture, config keys, attach vs managed, safety model, budget knobs); update `docs/tools.md`, `docs/providers.md` (image-model keys + routing), `docs/cli.md` (/browser subcommands); add joey-browser to `docs/architecture.md` crate table + `docs/README.md` index.
- [x] T062 [P] Update `PORTING.md`: browser toolset status (declared→implemented), vision_analyze, image-model config keys, upstream-parity notes for the new surfaces.
- [ ] T063 Cross-platform verification: launch discovery paths exercised on macOS + Windows (CI-style matrix run or documented manual check per constitution Principle 0); headless fallback verified displayless.
- [ ] T064 Run `specs/016-please-modify-joey/quickstart.md` end-to-end (§1–§7) on a machine with Chromium; record results; fix any drift between docs and behavior.
- [ ] T065 Full acceptance sweep: `cargo build --workspace` + `cargo test --workspace` green (SIGKILL-workaround per repo memory when running locally); SC-001 fixture-suite run (studio-density suite, ≥90% task success) documented in feature dir.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Phase 1 (Setup)**: no deps — start immediately.
- **Phase 2 (Foundational)**: depends on Phase 1; BLOCKS all stories (transport + registration scaffold).
- **Phases 3–10 (Stories)**: each depends on Phase 2 (+ prior story where noted); otherwise parallelizable.
- **Phase 11 (Polish)**: after all desired stories.

### User Story Dependencies

- **US3 (P1)**: after Phase 2 only. Entry point.
- **US1 (P1)**: after Phase 2; needs US3's session/navigate (T012/T013) for integration tests — unit/golden parts parallel-safe.
- **US2 (P1)**: after US1 (refs registry T022).
- **US4 (P2)**: after US2 (action pipeline T030).
- **US5 (P2)**: after US1 (snapshot pipeline); settle wiring touches US2's action paths (T040).
- **US6 (P2)**: after Phase 2 only — fully independent of the browser stories (config/providers/agent-core); parallel track.
- **US7 (P3)**: after US6 (image-model routing) + US1 (snapshot mode field).
- **US8 (P3)**: after US1 (snapshot pipeline).

### Within Each User Story

- Fixtures/tests first (fail before implementation where feasible), then modules, then tools, then schema pinning.
- Models/extract scripts before services/actions before tool registration.

### Parallel Opportunities

- T002 ∥ T003 (different crates); T007 ∥ T008; fixture tasks T017, T026, T032, T036, T050, T055 are all [P] (different fixture files).
- **US6 is a fully parallel track** (joey-core/joey-providers/joey-agent-core files, disjoint from joey-browser) — can run alongside US3–US5.
- After Foundational: US3, US6 can start simultaneously; US1 unit/golden parts (T018, T022, T023) parallel with US3 integration work.

---

## Parallel Example: User Story 1

```bash
# Fixtures + golden tests in parallel (different files):
Task: T017 fixtures (tests/fixtures/*.html)
Task: T018 snapshot_format golden tests (tests/snapshot_format.rs)

# Then scanner + registry + presentation:
Task: T020 extract/js/scan.js
Task: T022 refs.rs registry        # after scan.js shape known
Task: T023 snapshot.rs             # after refs shape known
```

---

## Implementation Strategy

### MVP First (US3 + US1)

1. Phase 1 + Phase 2 (setup + transport).
2. Phase 3 (US3): attach/managed browsing, dedicated tab, navigate/status/disconnect.
3. Phase 4 (US1): deep perception snapshots.
4. **STOP and VALIDATE**: run quickstart §3/§4 against a real logged-in complex page; perception is demo-able.

### Incremental Delivery

1. Foundation → 2. US3+US1 (MVP) → 3. US2 (dependable actions) → 4. US4 (full verbs) → 5. US5 (resilience) ∥ US6 (image model, independent track) → 6. US7 (vision fallback) → 7. US8 (feeds) → 8. Polish/regression/docs.

Each increment builds and tests green alone (constitution V); every story checkpoint is independently demo-able.

---

## Notes

- [P] = different files, no dependencies. Fixture tasks are always [P].
- Regression tasks T059/T060 exist because the feature touches public surfaces (tool schemas, toolset membership, config keys) — constitution VII mandates coverage.
- T025's perf assertion is #[ignore]-by-default so CI stays deterministic; run explicitly when benchmarking.
- Integration tests auto-skip without Chromium — `cargo test --workspace` green on browserless machines.
- When running local integration tests, remember the external SIGKILL killer quirk: copy test binaries to /tmp/neutral_harness/ per repo memory.


## Phase 12: Convergence

- [ ] T066 Wire `/browser connect|disconnect|status` into the TUI slash-dispatch path so browser session control works identically in the TUI and the line REPL — dispatch in crates/joey-tui (or the joey-cli engine layer it drives) alongside the existing repl.rs handler; shared_browser_handle makes state common to both surfaces (Constitution II) (contradicts)
- [ ] T067 Implement first-use auto-connect: when any browser_* tool executes while disconnected, connect lazily via BrowserConfig::from_config (resolve in joey-tools browser_tools.rs run() path or a wrapper), per T014's stated wiring (FR-017) (partial)
- [ ] T068 Surface image-model routing: pass the joey-llm-selector catalog multimodal default into the resolve_image_model call site (replacing the hardcoded None) and report `served_by { model, source }` in vision-capable tool results (browser_vision, vision_analyze) and a debug log line (FR-016, contracts/image-model-routing.md) (partial)
- [ ] T069 Correct stale docs: docs/tools.md inventory (browser_*/vision_analyze now implemented; 36-tool coding toolset), docs/providers.md (image-model keys + resolution order), docs/cli.md (/browser subcommands), docs/architecture.md + docs/README.md crate tables (include joey-browser) (T061 remainder) (partial)
