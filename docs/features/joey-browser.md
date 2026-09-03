# joey-browser — CDP browser automation

`joey-browser` is the browser control plane of feature 016: a self-contained
Chrome DevTools Protocol (CDP) client that attaches to the user's running
Chromium-family browser (preserving logins) or auto-launches a managed
headless instance, then drives a dedicated agent tab — deep DOM perception
piercing shadow roots and frames, resilient actions with a cascading
fallback resolver, settle detection, conservative overlay handling,
Set-of-Mark visual fallback, and bounded feed deltas. All CDP wire detail
lives here; nothing else in the workspace speaks CDP.

> See also: [../browser.md](../browser.md)

## Overview

- **Feature 016** (`specs/016-please-modify-joey/`): universal web-page
  browsing & complex SPA navigation.
- Speaks CDP over a single WebSocket to the browser-level
  `webSocketDebuggerUrl` via `tokio-tungstenite` (flat session mux keyed by
  `sessionId`; commands carry optional `sessionId` and the browser routes
  them to the attached target).
- Consumed by [joey-tools.md](joey-tools.md), which wraps it in the 16
  `browser_*` tools (one shared `BrowserHandle`), and by
  [joey-cli.md](joey-cli.md) (`/browser` slash commands reuse the same
  process-global handle). joey-browser sits low in the DAG: it depends only
  on `joey-core` (config) and cannot depend on joey-tools — the URL-safety
  checker is therefore injected upward (see [Security](#security)).
- Re-exports from `lib.rs`: `BrowserError`, `BrowserConfig`,
  `ElementRef`/`ResolvedBy`, `BrowserManager`/`BrowserStatus`/`Mode`,
  `Blocker`/`Delta`/`RegionSummary`/`Snapshot`, `VisualObservation`.
- Not a port of upstream Hermes functionality (upstream has no CDP layer);
  a Joey extension tracked in `PORTING.md`.

## Module map

12 source files under `src/` (plus `tests/`, fixtures, and one example):

| File | Role |
|---|---|
| `src/lib.rs` | crate docs + public re-exports |
| `src/config.rs` | `BrowserConfig` resolution from dotted config keys, clamping, `HeadlessPolicy`/`OverlayPolicy` parsing |
| `src/cdp/mod.rs` | `CdpConnection`: WebSocket transport, id/response correlation, event fan-out, `BrowserError` |
| `src/cdp/domains.rs` | typed wrappers for the ~8 CDP domains used (`Target`, `Page`, `Runtime`, `Input`, `Log`), `mouse_event`/`key_event`/`modifier_bitmask` builders |
| `src/launch.rs` | browser discovery (macOS bundles / `which` / Windows paths), `launch_managed`, stderr `DevTools listening on` parsing, headless policy resolution |
| `src/session.rs` | `BrowserManager`: attach-or-launch lifecycle, the dedicated agent tab, page-level verbs (`navigate`, `back`, `evaluate`, `status`, …) |
| `src/actions.rs` | action verbs on `BrowserManager`: pre-action re-scan, cascade resolution, CDP `Input` events |
| `src/refs.rs` | `ElementRef`, ephemeral `e<N>` refids, `ElementRefRegistry`, the cascading resolver, `TargetDescriptor` |
| `src/extract.rs` | the injected-JS corpus: `SCAN_JS`, `OBSERVER_JS`, `OVERLAYS_JS`, `MARKERS_JS`, `CLEANUP_MARKERS_JS` |
| `src/snapshot.rs` | `Snapshot` envelope: viewport-priority partitioning, region summaries, byte budgets, feed deltas, line grammar |
| `src/vision.rs` | Set-of-Mark capture: marker injection, `Page.captureScreenshot`, coarse-grid fallback |
| `src/url_safety_bridge.rs` | injectable URL-safety function pointer + conservative std-only default |

Support files:

| Path | Role |
|---|---|
| `tests/browser_integration.rs` | 16 gated live-browser tests (see [Testing](#testing)) |
| `tests/fixtures/*.html` | 20 fixture pages (shadow nests, frames, consent walls, canvas-only, drag boards, churn feeds, …) |
| `examples/diag.rs` | manual diagnostic: connect, inject a DOM, run `SCAN_JS`, print the registry |

## BrowserConfig

Resolved from joey-core dotted keys (`browser.*`) by
`BrowserConfig::from_config`; `Default` matches. All keys are read-path
additive surface, none are secrets (none route to `.env`).

| Key | Default | Clamp | Meaning |
|---|---|---|---|
| `browser.cdp_url` | `http://127.0.0.1:9222` | — | attach endpoint probed for a running browser |
| `browser.executable_path` | unset | — | skip discovery, use this executable |
| `browser.headless` | `auto` | — | `auto` (headless iff no display) \| `always` \| `never`; unknown values fall back to `auto` |
| `browser.overlay_policy` | `conservative` | — | `never` \| `conservative` \| `aggressive`; unknown → `conservative` |
| `browser.allow_raw_cdp` | `false` | — | expert gate for `raw_cdp` passthrough |
| `browser.allow_local_urls` | `false` | — | skip the URL-safety gate (test knob; fixture servers live on 127.0.0.1) |
| `browser.settle.quiet_ms` | `1500` | 250–5000 | mutation-quiet window before "settled" |
| `browser.settle.hard_timeout_ms` | `10000` | 2000–60000 | settle hard cap → `SettleTimeout` |
| `browser.snapshot.max_step_bytes` | `8192` | 1024–1048576 | per-step textual budget |
| `browser.snapshot.cumulative_cap_bytes` | `65536` | 8192–8388608 | cumulative per-task delta budget |
| `browser.snapshot.viewport_margin` | `1.0` | 0.0–5.0 | "near view" band in viewport heights |

`quiet_window`/`hard_timeout` land in `BrowserConfig` as `Duration`s; the
snapshot knobs as `SnapshotBudgets { max_step_bytes, cumulative_cap_bytes,
viewport_margin }`.

## Connect & launch

`BrowserManager::connect(cfg)` is attach-first:

1. **Attach**: `GET {cdp_url}/json/version` with a **2 s timeout**
   (`reqwest`); if it answers, take `webSocketDebuggerUrl` and open the
   CDP WebSocket → `Mode::Attached`. An attached browser is never killed
   and its user tabs are never touched.
2. **Managed launch** when the probe fails: `launch::discover`
   (`browser.executable_path` first if set and a file; else macOS app-bundle
   candidates → Windows known paths → `which` over `UNIX_NAMES`
   `google-chrome`, `google-chrome-stable`, `chromium`, `chromium-browser`,
   `microsoft-edge`, `brave-browser`). No find → `NoBrowserFound`.

Managed launch flags:

```text
--remote-debugging-port=0        ephemeral port
--user-data-dir=<tmp>/joey-browser-profile-<uuid-v4>
--no-first-run --no-default-browser-check
--window-size=1280,800           deterministic viewport (coarse-grid + fixtures)
[--headless=new]                 when HeadlessPolicy resolves to headless
```

- The WebSocket URL is parsed from the child's stderr line
  `DevTools listening on ws://…` under a **20 s deadline**; `kill_on_drop`
  guarantees no orphan children.
- Headless resolution: `Always` → headless; `Never` → headed; `Auto` →
  headless iff neither `DISPLAY` nor `WAYLAND_DISPLAY` is set (Windows
  always counts as having a display).
- **Dedicated tab**: the agent always creates its own target
  (`Target.createTarget` `about:blank` + `Target.attachToTarget` with
  `flatten: true`), enables `Page`/`Runtime`/`Log` on that session, and
  sets `Target.setAutoAttach { autoAttach: true, flatten: true }` so
  cross-origin OOPIF frames get their own sessions over the same socket.
  `ensure_page()` is idempotent and re-creates the tab if the target died
  (liveness re-checked via `Target.getTargets`).
- **Managed hygiene**: a freshly launched browser opens an initial tab of
  its own; the manager creates the agent tab first (never drop to zero page
  targets — headless Chrome exits) and then closes every *other* page
  target. "Never touch user tabs" applies to attached mode; in managed mode
  the throwaway profile is ours.
- `disconnect()` kills the child **iff Managed**, clears the page, sets
  `Mode::Disconnected`.

## Error taxonomy

`BrowserError` (`src/cdp/mod.rs`) is the normative error surface; display
strings are user-visible in tool errors and pinned by unit test:

| Variant | Display / meaning |
|---|---|
| `NotConnected` | `browser not connected` — no socket / socket died (pending commands fail with this on close) |
| `AttachFailed(String)` | `attach failed: {0}` — WebSocket/probe failure attaching to `cdp_url` |
| `LaunchFailed(String)` | `launch failed: {0}` — spawn failure or no `DevTools listening on` line within 20 s |
| `NoBrowserFound` | `no Chromium-family browser found; set browser.executable_path or start one with --remote-debugging-port` |
| `PageGone` | `agent page is gone (closed or crashed); call ensure_page to recover` |
| `TargetNotFoundRaw(String)` | `target not found: {0}` — refusal carrying candidate descriptors (`target_not_found()` joins them) |
| `AmbiguousRaw(String)` | `ambiguous match: {n} [cand; …] candidates refused` — candidates are LISTED, not just counted |
| `SettleTimeout { waited_ms }` | `page did not settle within the hard timeout ({waited_ms}ms); partial state returned` |
| `UrlBlocked(String)` | `navigation blocked by URL safety policy: {0}` |
| `RawCdpDisabled` | `raw CDP passthrough is disabled; set browser.allow_raw_cdp=true to enable` |
| `Protocol(String)` | `CDP protocol error: {0}` — CDP-level error objects, decode failures, eval exceptions |
| `Timeout(String)` | `timeout: {0}` |

## Element refs & actions

- **Ephemeral refids**: every scan (`scan_to_registry`) builds a fresh
  `ElementRefRegistry`; `push` assigns sequential `e1`, `e2`, … Refids are
  registry-local and **reset per scan** — a stale refid simply misses and
  falls through the cascade.
- **Identification never mutates the DOM.** Each scan computes descriptors
  (role / normalized text / structural CSS locator / geometry); before
  every action the page is re-scanned and the model-supplied
  `TargetDescriptor { refid?, locator?, text?, geometry? }` is re-resolved.
- **Cascade** (`ElementRefRegistry::resolve`), in order:
  1. `refid` — only against the current registry;
  2. `locator` — exact match, then **suffix** match (re-renders often
     re-root the subtree); multiple suffix hits refuse with candidates;
  3. `text` — exact whitespace-normalized match; multiple hits
     disambiguate by geometry proximity when the descriptor carries
     geometry, else refuse-with-candidates;
  4. `geometry` — nearest element center, accepted only when inside the
     rect or within half a rect's dimension (≥8 px guard);
  5. otherwise `TargetGone` / empty-descriptor refusal.
  The winning strategy is reported as `ResolvedBy`
  (`refid`/`locator`/`text`/`geometry`/`refused_ambiguous`).
- **Attribute allowlist** `ATTR_ALLOWLIST`: `aria-label`, `placeholder`,
  `href`, `name`, `type` (each value capped at 80 chars in the scanner).
  `MAX_TEXT_CHARS = 120` with char-boundary-safe truncation
  (`truncate_char_safe`).
- **Actions** (all on `BrowserManager`, in `actions.rs` unless noted):

| Verb | Mechanism |
|---|---|
| `click(target)` | resolve fresh; `Input.dispatchMouseEvent` Moved → Pressed → Released at the element center (works for handlerless elements) |
| `click_coords(x, y)` | physical click at viewport coordinates |
| `type(target, text, clear, submit)` | focus (and optionally clear) via JS locator, then `Input.insertText` (IME-compatible, no per-key events); `submit` sends Enter `keyDown`/`keyUp` with `text:"\r"` |
| `hover(target)` | `mouseMoved` to the element center (hover-only menus) |
| `scroll(target?, dir, px)` | page: `window.scrollBy` via JS (works headless); container: resolve target, adjust `e.scrollTop` |
| `drag(source, target)` | mouse down, **5 interpolated moves**, mouse up so drag UIs register motion |
| `select_option(target, value)` | JS value set + `change` event dispatch on the native `<select>` |
| `press_key(key, ctrl, alt, shift, meta)` | `rawKeyDown`/`keyUp` with modifier bitmask **Alt=1, Ctrl=2, Meta=4, Shift=8** and best-effort US-layout virtual key codes |
| `wait_settle()` | install `OBSERVER_JS` with `%QUIET_MS%` substituted, await `window.__joeySettle` bounded by the hard timeout; returns `waitedMs` |
| `scan_to_registry()` | run `SCAN_JS`, parse rows (hostile rows are skipped), build a fresh registry |
| `snapshot()` (`session.rs`) | structural snapshot: URL/title/frame count/viewport + partitioned elements |
| `apply_overlay_policy()` (`session.rs`) | run `OVERLAYS_JS`, act per policy (see below) |
| `observation_mode()` (`session.rs`) | `structural` when the last scan has any interactable element, else `visual` |
| `raw_cdp(method, params)` | browser-level raw passthrough, **gated** on `allow_raw_cdp` |
| `raw_eval_diag(method, params)` | crate-internal ungated send to the page session (diagnostics; not tool-exposed) |
| `handle_dialog(accept, prompt_text?)` | `Page.handleJavaScriptDialog` |
| `back()` / `evaluate(expr)` / `eval_string(expr)` / `frame_tree()` / `frame_count()` / `viewport()` / `status()` / `disconnect()` | session-level verbs (`session.rs`) |

## Snapshot & overlays

- `SCAN_JS` (deep scanner): property-based, **zero DOM mutation** (a unit
  test pins that the source contains no `setAttribute`). Walks
  `document` for interactive elements (buttons, links, inputs, selects,
  textareas, ARIA roles, `contenteditable`), pierces same-origin iframes
  via `contentDocument` and shadow trees via `shadowRoot`, depth-capped at
  `MAX_DEPTH = 24` (circular/deep nesting terminates; capped regions are
  reported in the `capped` flag). Selects report the selected option's
  label; password values are never read.
- `OBSERVER_JS` (settle probe): installs a `MutationObserver` +
  100 ms poll; resolves once the DOM has been quiet for `%QUIET_MS%`.
  Sentinel-marked nodes (`data-joey-sentinel`) are ignored so joey's own
  markers don't count as churn. The hard cap lives Rust-side (the promise
  stays pending; the Rust timeout fires).
- `OVERLAYS_JS` (overlay heuristics): finds fixed/sticky/absolute elements
  covering a meaningful viewport chunk with high z-index; classifies
  `consent` (via `CONSENT_HINTS` like "accept cookies", "we use cookies",
  "reject all", …) vs `dialog` vs `unknown`; identifies a **safe dismissal
  control** via the `SAFE_DISMISS` regex
  (`^(accept|reject|decline|close|dismiss|got it|okay|ok|i agree|manage|preferences|settings|not now|no thanks|continue without)`)
  plus exact glyph matches (`x`, `×`, `✕`, `✖`, `⨯`, `close`),
  **preferring reject/close-style labels** to minimize consent grants.
  `apply_overlay_policy`: `never` → report only; `conservative` →
  auto-dismiss only consent overlays with a safe control; `aggressive` →
  auto-dismiss anything with a safe control. Non-dismissible findings are
  `flagged` for the model.
- Snapshot envelope (`Snapshot { v: 1, mode, url, title, frame_count,
  viewport, elements, out_of_view, blockers, delta?, visual?, truncation }`):
  - `Blocker { kind, description, frame, dismissal }` —
    `auto_dismissed`/`refused_unsafe`/`flagged`;
  - `RegionSummary { region, direction, counts, note }` — out-of-view
    content compressed to role→count buckets (`above`/`below`);
  - `Delta { new_elements, gone_refids, out_of_view, cumulative_bytes,
    cumulative_cap_bytes }` — feed deltas keyed by
    (role, normalized text, frame);
  - viewport-priority ordering: in-view first, then near-view (within
    `viewport_margin` viewport heights), rest summarized;
  - `enforce_step_budget` drops the bottom-most elements into the "below"
    summary until serialized size fits `max_step_bytes` — never silently
    (`TruncationInfo { applied, reason, omitted }`);
  - `render_line`: token-efficient grammar
    `e1 [button] "Save" @main (x,y WxH) locator=… [(not-interactable)]`;
    `render()` pretty-prints ≤4 KB else compact.
- **Set-of-Mark** (`vision.rs`): `visual_observe(geometry_hints?)` injects
  `MARKERS_JS` — a fixed overlay layer (`data-joey-sentinel`,
  `z-index: 2147483647`, `#ff3b30` 2 px boxes, labels beneath) — then
  `Page.captureScreenshot { format: "png" }`, then
  `CLEANUP_MARKERS_JS` removes every sentinel node. Strategy
  `dom_geometry` when hints exist (≤24 markers, ids `m1`…`m24`) else
  `coarse_grid`: a 6×4 grid of cells over the 1280×800 viewport. Result:
  `VisualObservation { image: "data:image/png;base64,…", strategy,
  markers, marker_table }`.

## Security

- `navigate()` runs `url_safety_bridge::url_safety_check(url)` **unless**
  `browser.allow_local_urls` is set (fixture-server knob; default off, so
  production navigation always runs the gate — FR-020).
- The bridge's conservative **default** (std-only, used until the real
  checker is installed) blocks: hosts `localhost`, `::1`, `[::1]`, empty
  host, `metadata.google.internal`, and private IPv4 ranges `10/8`,
  `127/8`, `0/8`, `172.16/12` (16–31), `192.168/16`, `169.254/16`
  (link-local). Unparseable URLs are refused.
- The **real** checker is injected via
  `install_url_safety_check(fn(&str) -> Result<(), String>)` — joey-browser
  cannot depend on joey-tools (DAG direction), so joey-tools' browser
  handle installs `joey_tools::url_safety::is_safe_url` at wiring time:
  the same policy the web tools use.
- `raw_cdp` requires `browser.allow_raw_cdp=true` and deliberately
  bypasses the URL-safety gate — that is why it is off by default and
  marked expert.
- All `browser_*` tool output passes joey-tools' untrusted-content
  pipeline; no credential handling (auth is inherited from the attached
  profile).

## The 16 browser tools

Registered by joey-tools (`browser_tools.rs`) against the shared
`BrowserHandle` (process-global, shared with the CLI slash commands). Each
tool's `check()` returns `handle.is_connected()` — **hidden until a
session is connected**, and a `browser_*` tool invoked while disconnected
**lazily auto-connects** (attach when possible, else managed launch;
mirrors `/browser connect`). Toolset: `web`. Details in
[joey-tools.md](joey-tools.md).

| Tool | Wraps |
|---|---|
| `browser_navigate` | `navigate` + settle |
| `browser_snapshot` | `snapshot` (optional `since_last` delta) |
| `browser_click` | `click` via cascade |
| `browser_type` | `type` (clear / submit options) |
| `browser_scroll` | `scroll` (page or container) |
| `browser_back` | `back` |
| `browser_press` | `press_key` + modifiers |
| `browser_get_images` | image listing (src/alt/dimensions/visibility) |
| `browser_vision` | `visual_observe` |
| `browser_console` | buffered console entries |
| `browser_cdp` | `raw_cdp` (gated) |
| `browser_dialog` | `handle_dialog` |
| `browser_hover` | `hover` |
| `browser_select_option` | `select_option` |
| `browser_drag` | `drag` |
| `browser_click_coords` | `click_coords` |

The first 12 are the declared core names; `browser_hover`,
`browser_select_option`, `browser_drag`, `browser_click_coords` are the
additive verbs appended order-preserving after them.

## Testing

- **Inline unit suites** in every module, all browserless: pinned
  `BrowserError` display strings, canned CDP JSON framing, config
  clamping, cascade resolution over mock element sets, `SCAN_JS` source
  assertions (no `setAttribute`, `shadowRoot`/`contentDocument` present,
  balanced parens, `%QUIET_MS%`/`%MARKER_SPEC%` placeholders), coarse-grid
  coverage, URL-safety default blocking.
- **`tests/browser_integration.rs`**: 16 `#[tokio::test]` functions — a
  harness self-check plus 15 scenario tests (managed-launch tab isolation,
  shadow/frame piercing coverage ≥95% of ground truth, churn actions with
  fallback, ambiguous-text refusal, hover/select/press/coords verbs,
  container scroll scoping, dense-page viewport priority + perf budget,
  consent auto-dismiss vs flagged tour dialogs, never-settle boundedness,
  visual fallback on canvas-only pages, structural-mode recovery after
  login, feed delta budgets, URL-safety blocking a local target).
- **Gating**: every integration test early-returns when
  `JOEY_BROWSER_TESTS=0` **or** `launch::discover(None)` finds no
  Chromium — `cargo test --workspace` stays green on browserless machines.
- **Fixture servers**: two minimal std-only TCP HTTP servers bound to
  `127.0.0.1:0` (ephemeral ports); server B's port is rewritten into
  `frames.html` as a cross-origin stand-in. No extra dependencies
  (Constitution VIII).

## See also

- [../browser.md](../browser.md) — user-facing browser automation guide
- [joey-tools.md](joey-tools.md) — the `browser_*` tool wrappers, toolsets, URL-safety policy
- [joey-cli.md](joey-cli.md) — `/browser` slash commands and the shared handle
- [joey-core.md](joey-core.md) — dotted config keys, `~/.joey` home
- [README.md](README.md) — the features index
