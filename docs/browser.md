# Browser Automation (joey-browser)

Feature 016 — universal web-page browsing & complex SPA navigation.
Spec: `specs/016-please-modify-joey/`.

## Overview

`joey-browser` owns CDP-driven browser control: attach to your running
Chromium-family browser (logins preserved) or auto-launch a managed
instance (headless when no display); deep DOM perception piercing shadow
roots and frames; resilient actions with a cascading fallback resolver;
settle detection; conservative overlay handling; Set-of-Mark visual
fallback; bounded feed deltas. All CDP detail is encapsulated — joey-tools
consumes only `BrowserManager` (contracts/cdp-session.md).

## Sessions

- **Attach**: `/browser connect` probes `browser.cdp_url`
  (default `http://127.0.0.1:9222`). Your browser must run with
  `--remote-debugging-port=9222`. Logins carry over.
- **Managed**: no attachable browser → a managed instance launches
  (`--remote-debugging-port=0`, unique temp profile, `--headless=new`
  when headless policy says so). Killed on disconnect — no orphans.
- **Tab discipline**: the agent always works in a **dedicated tab it
  creates itself**. It never navigates/closes your tabs.
- `/browser status`, `/browser disconnect`.

## Tools

12 declared core names + 4 additive verbs: `browser_navigate`,
`browser_snapshot`, `browser_click`, `browser_type`, `browser_scroll`
(optional `target` for container scroll), `browser_back`, `browser_press`,
`browser_get_images`, `browser_vision`, `browser_console`, `browser_cdp`,
`browser_dialog`, `browser_hover`, `browser_select_option`,
`browser_drag`, `browser_click_coords`. All hidden until a session is
connected (`check()` gate). `vision_analyze` analyzes image files.

Target descriptors accept `refid`/`locator`/`text`/`geometry`; resolution
cascades refid → locator → text → geometry, refusing ambiguous text
matches with candidates. Actions re-scan before executing (stale-element
safe) and report `resolved_by`.

## Config keys

| Key | Default | Meaning |
|-----|---------|---------|
| `browser.cdp_url` | `http://127.0.0.1:9222` | attach endpoint |
| `browser.executable_path` | unset | skip discovery |
| `browser.headless` | `auto` | `always`/`never`/`auto` (auto = headless iff no display) |
| `browser.overlay_policy` | `conservative` | `never`/`conservative`/`aggressive` |
| `browser.allow_raw_cdp` | `false` | expert gate for `browser_cdp` |
| `browser.settle.quiet_ms` | 1500 (250–5000) | mutation quiet window |
| `browser.settle.hard_timeout_ms` | 10000 (2000–60000) | settle hard cap |
| `browser.snapshot.max_step_bytes` | 8192 | per-step snapshot budget |
| `browser.snapshot.cumulative_cap_bytes` | 65536 | feed delta cap |
| `browser.snapshot.viewport_margin` | 1.0 | near-view band (viewport heights) |

Image-model keys (all providers): `model.image_model` (global) and
`providers.<id>.image_model` (per-provider, wins). Resolution order:
per-provider → global → provider multimodal default → primary if
vision-capable; visual turns route to the resolved model (unpinned turns
only — `--model` always wins).

## Safety

- Navigations run through the same `url_safety` policy as web tools
  (SSRF/local-network refusal). The check is injected at wiring time so
  joey-browser keeps one implementation.
- All `browser_*` tool output passes the untrusted-content pipeline
  (`UNTRUSTED_TOOL_PREFIXES`).
- `browser_cdp` bypasses URL-safety by design → gated behind
  `browser.allow_raw_cdp` (default off).
- No credential handling — auth is inherited from the attached profile.

## Testing

Unit/contract tests are browserless (canned CDP JSON, mock element sets,
fixture DOM dumps). Live-browser integration tests auto-skip when no
Chromium is found, so `cargo test --workspace` is green everywhere.
