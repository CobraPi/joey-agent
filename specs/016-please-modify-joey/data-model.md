# Data Model: Universal Web-Page Browsing

Feature: specs/016-please-modify-joey | All types live in the new `joey-browser` crate unless noted. No persistent storage; all state is in-memory per session. Serialization formats are normative in [contracts/snapshot-format.md](contracts/snapshot-format.md).

## Entities

### 1. BrowserManager (Rust, process-wide singleton behind BrowserHandle)

Owns the connection lifecycle for the whole process.

| Field | Type | Notes |
|-------|------|-------|
| mode | enum `Attached` \| `Managed` \| `Disconnected` | Attach vs. launched (D8) |
| cdp_endpoint | `Url` | ws URL derived from `/json/version` `webSocketDebuggerUrl` |
| browser_session_id | `String` | CDP browser-level session |
| managed_child | `Option<Child>` | set iff mode = Managed |
| pages | `Vec<PageSession>` | agent-owned tabs (normally exactly 1) |
| config | `BrowserConfig` | resolved from joey-core config |

**State machine** (events: `connect`, `first_browse`, `detach`, `child_exit`):

```text
Disconnected --connect(attached)--> Attached
Disconnected --first_browse(no attachable)--> Managed
Attached --detach--> Disconnected
Managed --detach--> Disconnected (child terminated, orphan-guarded)
Attached/Managed --child_exit--> Disconnected (auto-detect via ws close)
```

Validation: connect() probes `/json/version` with 2s timeout; managed launch requires a discovered executable or `browser.executable_path`; detach kills child only when mode = Managed.

### 2. PageSession (Rust, one per agent tab)

| Field | Type | Notes |
|-------|------|-------|
| target_id | `TargetId` | CDP target for the agent's dedicated tab |
| session_id | `SessionId` | page-level CDP session (flat mode) |
| url / title | `String` | last known |
| frame_tree | `Vec<FrameInfo>` | refreshed per scan; frame id, parent, origin, label |
| last_snapshot | `Option<Snapshot>` | for delta computation (feeds) |
| overlay_state | `OverlayState` | counts, last dismissal times, rate-limit ledger |
| settle | `SettleConfig` | quiet window ms, hard timeout ms |

### 3. Snapshot (serializable — the model-facing observation unit)

| Field | Type | Notes |
|-------|------|-------|
| mode | `structural` \| `visual` | FR-014 explicit mode |
| url, title, frame_count | basics | |
| elements | `Vec<ElementRef>` | viewport-priority ordered (in-view first) |
| out_of_view | `Vec<RegionSummary>` | compact summaries per region (FR-004a) |
| blockers | `Vec<Blocker>` | detected-but-not-dismissed overlays |
| delta | `Option<Delta>` | present only for feed steps (FR-012) |
| visual | `Option<VisualObservation>` | present only in visual mode |
| truncation | `TruncationInfo` | always set when any budget applied |

**Validation**: elements list ordered: in-viewport first (by DOM order), then near-view; unique refids within snapshot; refids MUST NOT repeat across consecutive snapshots for different elements (registry reset per scan).

### 4. ElementRef (serializable)

| Field | Type | Notes |
|-------|------|-------|
| refid | `String` (`e<N>`) | unique within snapshot; ephemeral (D2) |
| role | `String` | button/link/input/select/textarea/menuitem/option/… |
| text | `String` | visible label, whitespace-normalized, capped 120 chars |
| value | `Option<String>` | for inputs (never secrets — page-controlled values only) |
| frame | `FrameLabel` | frame-context label, e.g. `main`, `iframe:checkout`, `oopif:ads` |
| locator | `String` | structural fallback locator (CSS-first, XPath as noted) |
| geometry | `Rect { x, y, w, h }` | viewport coords at scan time |
| attributes | `BTreeMap<String,String>` | small allowlist: aria-label, placeholder, href (trimmed), name, type |
| interactable | `bool` | visible + enabled + not-obscured heuristic |

Validation: refid pattern `^e[0-9]+$`; locator non-empty; geometry within page bounds or clipped; text/attribute values UTF-8-safe and char-boundary-truncated (per repo audit lessons).

### 5. AgentAction (input, from model)

Verb + target + params, per [contracts/browser-tools.md](contracts/browser-tools.md). Verbs: `navigate`, `click`, `type`, `hover`, `scroll`, `scroll_container`, `select_option`, `drag`, `press_key`, `click_coords`, `back`, `snapshot`, `vision`, `console`, `cdp`, `dialog`, `get_images`.

Target descriptor: `{ refid?, locator?, text?, geometry? }` — at least one non-null; the fallback cascade consumes them in order (refid → locator → text → geometry → refuse with candidates).

### 6. VisualObservation (serializable)

| Field | Type | Notes |
|-------|------|-------|
| image | base64 PNG (data URL form) | viewport screenshot with markers burned in |
| markers | `Vec<Marker>` | id, label, rect; id = `m<N>` |
| marker_table | `String` | compact text table mirroring markers |
| strategy | `dom_geometry` \| `coarse_grid` | D6 |

### 7. OverlayState / Blocker (Rust + serializable subset)

Blocker: `{ kind: consent|dialog|interstitial|unknown, description, frame, dismissal: auto_dismissed|refused_unsafe|flagged, }`.

OverlayState: rate-limit ledger (per-frame dismissal counts, max 3 per frame per task; escalate to flagged after limit — spec edge case).

### 8. Config entities (joey-core, additive keys)

| Key | Type | Default | Notes |
|-----|------|---------|-------|
| `model.image_model` | string | unset | global image-model default (D7) |
| `providers.<id>.image_model` | string | unset | per-provider override, wins over global |
| `browser.cdp_url` | string | `http://127.0.0.1:9222` | attach endpoint override |
| `browser.executable_path` | string | unset | skip discovery |
| `browser.headless` | `auto|always|never` | `auto` | auto = headless iff no display |
| `browser.overlay_policy` | `never|conservative|aggressive` | `conservative` | D5 |
| `browser.allow_raw_cdp` | bool | false | expert gate for browser_cdp tool |
| `browser.settle.quiet_ms` | int | 1500 | D4 |
| `browser.settle.hard_timeout_ms` | int | 10000 | D4 |
| `browser.snapshot.max_step_bytes` | int | 8192 | feed delta budget |
| `browser.snapshot.cumulative_cap_bytes` | int | 65536 | feed cumulative cap |
| `browser.snapshot.viewport_margin` | float | 1.0 | viewport heights of "near view" |

Validation: overlay_policy/headless enums validated at read; ints clamped to sane ranges (quiet_ms ∈ [250, 5000], hard_timeout ∈ [2000, 60000]); unknown keys ignored per existing config behavior; none are secrets (no .env routing).

### 9. Image-model routing (joey-agent-core helper)

`resolve_image_model(provider_cfg, catalog) -> ResolvedImageModel { model_id, source: explicit_per_provider|explicit_global|provider_default|primary_if_vision }` — pure function, unit-tested resolution orders; error (with human message) when no vision-capable model can serve content.

## Relationships

```text
BrowserManager 1--* PageSession 1--1 Snapshot (last)
Snapshot 1--* ElementRef; Snapshot 1--* RegionSummary/Blocker; Snapshot *--1 VisualObservation (optional)
AgentAction --> ElementRef (by refid, resolved via cascade at execution time)
BrowserHandle (joey-tools) --> BrowserManager (Arc)   [trait-sealed, D9]
Provider image-model keys --> ResolvedImageModel --> provider request (image parts)
```

## State-transition highlights

- Snapshot refids: ephemeral per scan — registry cleared at each scan start; an action MUST re-resolve (FR-005).
- PageSession.frame_tree refresh: on navigation, frame attach/detach events; stale frame labels detected by frame id change and dropped.
- Managed child crash: BrowserManager → Disconnected; in-flight tool call fails with a diagnostic suggesting reconnect; no auto-relaunch mid-action.
- Overlay rate-limit: dismissal counter per (frame, overlay signature); ≥3 → flagged, never auto-dismissed again that task.
