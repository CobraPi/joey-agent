# Research: Universal Web-Page Browsing & Complex SPA Navigation

**Feature**: specs/016-please-modify-joey | **Date**: 2026-08-17
**Constitution Principle VIII note**: every dependency decision below records alternatives and weight. Net new external dependencies for this feature: **zero**.

---

## D1: Execution engine — direct CDP attach vs. Chrome extension vs. driver-wrapper crate

**Decision**: Direct CDP over a WebSocket (`tokio-tungstenite 0.23`, already in `Cargo.lock` via joey-speckit-ui; `serde_json`; `tokio`), spoken by a new `joey-browser` crate. All DOM work is injected JavaScript evaluated via `Runtime.evaluate` against per-target sessions.

**Rationale**: The user's blueprint suggested a Chrome extension + `chrome.debugger`; but joey-agent is a CLI agent, not an extension host. CDP is the same protocol `chrome.debugger` fronts — the browser-level channel the blueprint requires for cross-origin frames and coordinate input — with no extension install/packaging step. The repo already advertises "connect … via CDP" in the `/browser` slash command, making direct CDP the fidelity-consistent choice.

**Alternatives considered**:
- *Chrome extension + chrome.debugger* (blueprint's Phase-1 suggestion): rejected — requires extension packaging, a distribution/update path, and DevTools-exclusive attach UX; violates lean-dependency discipline and adds a JS build pipeline to a Rust workspace.
- *`chromiumoxide` crate*: rejected — pulls `futures` heavy surface + pinned CDP schema types; binary-size/compile-time cost (Principle VIII) without need since we speak ~8 CDP domains, not all of them.
- *`headless_chrome` crate*: rejected — sync-first API, bundles its own browser download logic, poor attach-to-existing support.
- *Playwright-style out-of-process driver*: rejected — new runtime dependency (Node or driver binary), not in workspace.

## D2: Element identification — ephemeral injected IDs vs. property-based references

**Decision**: Property-based ephemeral references. The scanner computes a stable-ish descriptor per element (role, text, structural XPath-ish locator, geometry) and the Rust side assigns short refids (`e1`, `e2`, …) valid only for the current snapshot. Before every action the page is re-scanned and the action's target descriptor is re-matched via the fallback cascade (refid → structural locator → text → geometry/coordinates). **No DOM mutation is ever performed for identification** (no injected `data-agent-id` attributes).

**Rationale**: The blueprint itself mandates re-scan-before-every-action, making persistent injected IDs redundant; skipping DOM mutation avoids React `key` reconciliation fights, avoids polluting CSP-strict pages, avoids MutationObserver feedback loops with our own settle detector, and is invisible to anti-tamper checks.

**Alternatives considered**:
- *Injected `data-agent-id` attributes* (blueprint literal): rejected for the reasons above; kept only as a last-resort coordinate fallback signal.
- *Backend Node handles* (`Runtime.evaluate` object references): rejected — invalid across navigations/frames and awkward to serialize.

## D3: Cross-origin frame access

**Decision**: `Target.setAutoAttach` with `flatten: true` on the browser endpoint. Every frame (same- and cross-origin) that is a target gets its own CDP session; extraction runs per-session and results are merged into one snapshot with frame-context labels. Frames that are not separate targets (same-origin non-OOPIF iframes) are handled by direct `contentDocument` traversal inside injected JS.

**Rationale**: CDP target-level sessions are the only channel that bypasses same-origin policy uniformly — exactly what FR-003 requires — and `Target.setAutoAttach` is the standard mechanism Chrome's own DevTools uses.

**Alternatives considered**:
- *Only in-page traversal*: fails on cross-origin frames (core requirement).
- *Per-frame `Runtime.enable` on the page session only*: does not exist; frame-level evaluation requires target sessions for OOPIFs.

## D4: Settle detection (replacing network-idle)

**Decision**: Injected `MutationObserver` + quiet-window polling: after each action/navigation, observe subtree mutations; "settled" = no mutation batches for a configurable quiet window (default 1.5 s, per the blueprint) checked via a promise resolved from JS; hard timeout (default 10 s, configurable) always bounds the wait. `Page.loadEventFired` is used only as an early hint, never as a gate.

**Rationale**: Directly implements FR-010 and the blueprint's Phase 4; robust on continuously-mutating SPAs where network-idle never fires.

**Alternatives considered**:
- *Network-idle (CDP `Network` quiet)*: rejected — hangs on SPAs (explicit spec anti-requirement).
- *Fixed sleep*: rejected (spec anti-requirement).
- *RAF-based stability*: rejected — animations that never stop would never settle; mutation quiet-window is content-based.

## D5: Overlay handling

**Decision**: Conservative two-stage: (1) detection heuristics run in injected JS — high z-index fixed-position overlays, `role="dialog"`, known consent text patterns, pointer-events blocking checks; (2) auto-dismiss ONLY when the overlay matches a high-confidence standard consent/notification pattern AND exposes a clearly-safe dismissal control (close/reject with no side effects), honoring intent-vs-accidental-click risk. Everything else is flagged in the snapshot. Policy configurable (`browser.overlay_policy` = `never | conservative | aggressive`, default `conservative`).

**Rationale**: Clarification Q4 answer; protects task-relevant dialogs (tours, required choices) while killing the cookie-banner dead-end.

**Alternatives considered**:
- *Aggressive auto-dismissal*: rejected — destroys task-relevant dialogs.
- *Pure flagging, no automation*: rejected — wastes turns on the most common annoyance; clarification chose conservative automation.

## D6: Vision fallback (Set-of-Mark)

**Decision**: When structural extraction yields zero actionable elements (or repeated action resolution fails), capture `Page.captureScreenshot` of the viewport, then **draw numbered markers as an injected page overlay** (absolutely-positioned labeled boxes at candidate-interaction points: element centroids from any discoverable geometry, or a coarse grid where nothing is discoverable), re-capture with markers burned in, and return both the annotated image and the marker→coordinate table. The model answers with a marker number; the agent executes a coordinate action. Marker overlay is removed after capture (DOM stays clean). Object-detection model: **none** — markers come from DOM geometry when available, else a documented coarse grid; adding a detector later is additive.

**Rationale**: Zero new dependencies (no `image` crate in the workspace lockfile, none added — annotation happens in-page). Canvas-hostile pages are exactly where DOM geometry still exists under the canvas or where the grid suffices; a local detector (Florence-2 class) would be a multi-GB dependency and is not justified for v1 (Principle VIII).

**Alternatives considered**:
- *`image` + `imageproc` Rust crates for annotation*: rejected — two new deps for something the page itself can render perfectly.
- *Local object-detection model*: rejected for v1 (weight/cost); noted as future additive extension.

## D7: Dedicated image model per provider

**Decision**: Two additive config keys in joey-core layered config: `model.image_model` (global default) and `providers.<id>.image_model` (per-provider override; wins over global). Resolution order: per-provider → global → provider's default multimodal model (from model catalog `supports_vision` data in joey-llm-selector) → primary model if image-capable → error with clear message if visual content cannot be served. Keys are plain model names (not secrets — no `.env` routing). Routing happens in joey-agent-core: when a turn's content contains image parts, the request is served by the resolved image model on the same provider stack; text turns are unaffected.

**Rationale**: The mid-turn user requirement ("all LLM providers given the option to set a dedicated image model"); follows existing dotted-key config conventions; leverages existing `supports_vision` catalog data for defaults; no schema changes.

**Alternatives considered**:
- *Separate `vision_provider` + `vision_model` pair*: rejected — over-configurable; a provider-pinned image model still needs the provider's auth/base-URL stack, so staying on the same provider with a different model is the lean shape.
- *Hard-coding per-provider vision defaults with no config*: rejected — violates the explicit user requirement.

## D8: Browser acquisition & tab discipline

**Decision**: Acquisition (clarification Q2): probe `http://127.0.0.1:9222/json/version` (and `browser.cdp_url` config override); if attachable, attach; else discover a Chromium-family binary per-OS (Chrome/Edge/Chromium, Brave; `browser.executable_path` override) and launch managed with `--remote-debugging-port=0` (ephemeral port, read back from stderr `DevTools listening on …`), `--headless=new` when no DISPLAY/tty display is available. Tab discipline (clarification Q1): the agent always creates its own tab via `Target.createTarget` and targets it exclusively; it never activates, navigates, or closes user tabs. Disconnect leaves the attached browser running; a managed instance is terminated on disconnect/session end.

**Rationale**: Attach keeps logins (Pega Studio behind auth); managed launch makes first-run/tests/CI work (fixture-based SCs); headless-when-displayless satisfies cross-platform unattended use.

**Alternatives considered**:
- *Attach-only*: rejected (clarification Q2 = B).
- *Always-managed*: rejected — loses the logged-in session, the feature's primary value.
- *Reuse active tab*: rejected (clarification Q1 = B) — hijacks the user's page.

## D9: Where browser tools live & toolset membership

**Decision**: New `joey-tools/src/tools/browser_tools.rs` with a `BrowserHandle` (Arc to the session manager) and 16 `Tool` impls: the 12 declared names (`browser_navigate`, `browser_snapshot`, `browser_click`, `browser_type`, `browser_scroll`, `browser_back`, `browser_press`, `browser_get_images`, `browser_vision`, `browser_console`, `browser_cdp`, `browser_dialog`) + 4 additive verbs (`browser_hover`, `browser_select_option`, `browser_drag`, `browser_click_coords` — names chosen to extend, not rename). Registered via `builtins::register_browser_tools` following the neurocode conditional-registration pattern (`check()` false when no handle wired). Toolset membership: the 12 declared names are already present in CORE_TOOLS; the 4 new names are appended to the same list — additive, no renames, resolution unchanged for existing members.

**Rationale**: Preserves upstream toolset fidelity (PORTING.md constraint); conditional registration matches the established pattern for context-dependent tools; DAG stays clean (joey-tools → joey-browser → joey-core).

**Alternatives considered**:
- *All tools in joey-browser itself*: rejected — joey-tools owns the `Tool` trait and registry; putting Tool impls in joey-browser would invert the DAG.
- *Fewer, wider tools (one `browser_act` verb tool)*: rejected — breaks declared-name parity and gives the model a worse action grammar.

## D10: Security & safety verification

**Decision**: Reuse, don't reimplement: (1) navigations call `url_safety::is_safe_url` (joey-tools) before `Page.navigate` — includes SSRF/local-network guards; (2) all `browser_*` tool outputs flow through the existing untrusted-content pipeline (agent-core already lists `browser_` in `UNTRUSTED_TOOL_PREFIXES`); (3) raw `browser_cdp` passthrough is expert-gated (off unless `browser.allow_raw_cdp = true`) since it can bypass URL-safety; (4) no credential storage — auth inherited from the attached profile; (5) screenshots pass through the same redaction/sanitization as other external images.

**Rationale**: FR-019/FR-020 demand reuse of existing layers; the raw CDP escape hatch needs its own guard to not become a safety bypass.

**Alternatives considered**: none — reuse is mandated by the spec.

---

## Verification matrix (research → spec)

| Decision | Satisfies |
|----------|-----------|
| D1 CDP engine | FR-003, FR-009, FR-018 |
| D2 ephemeral refs | FR-004, FR-005, FR-006 |
| D3 target auto-attach | FR-002, FR-003 |
| D4 mutation quiet-window | FR-010, SC-004 |
| D5 conservative overlays | FR-011, SC-005 |
| D6 in-page SoM markers | FR-013, FR-014, SC-007 |
| D7 image-model config | FR-015, FR-016, SC-008 |
| D8 attach-or-launch + dedicated tab | FR-017, Story 3 |
| D9 tool placement/naming | FR-018, Principle VII |
| D10 safety reuse | FR-019, FR-020 |
