# Implementation Plan: Universal Web-Page Browsing & Complex SPA Navigation

**Branch**: `016-please-modify-joey` | **Date**: 2026-08-17 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/016-please-modify-joey/spec.md`

## Summary

Make the agent's browser tool surface (declared in toolsets but unimplemented today) fully functional, with a new `joey-browser` crate owning a Chrome DevTools Protocol (CDP) client that attaches to the user's running Chromium-family browser (in a dedicated tab) or auto-launches a managed instance (headless when no display). Deep DOM extraction pierces shadow roots and same-/cross-origin frames via per-target CDP sessions; snapshots are viewport-priority with ephemeral property-based element references (no DOM mutation); actions run through a re-scan + cascading fallback (refid → structural locator → text → coordinates). Settle detection uses injected MutationObservers; overlays are conservatively auto-dismissed; feeds get delta snapshots. A visual Set-of-Mark fallback covers unscrapable pages, and every provider gains a config-only dedicated image model (`model.image_model` / `providers.<id>.image_model`) routed through the existing provider stack.

## Technical Context

**Language/Version**: Rust 2021 edition, stable toolchain (rust-toolchain.toml), matching the existing 14-crate workspace.

**Primary Dependencies**: zero new external crates. CDP transport reuses `tokio-tungstenite` (already in the workspace tree via joey-speckit-ui) + `serde_json` + `tokio` (ubiquitous). All DOM logic is injected JavaScript evaluated over CDP `Runtime.evaluate`; no headless-chrome/chromiumoxide wrapper (see [research.md](research.md) §D1).

**Storage**: none persistent — browser session state is in-memory (connection, dedicated tab identity, last snapshot). Config keys in existing `~/.joey/config.yaml` + `.env` via joey-core layered config. No SQLite schema change (`SCHEMA_VERSION` stays 22).

**Testing**: `cargo test -p <crate>` per-crate suites. Unit + contract tests for snapshot serialization, fallback matching, overlay heuristics, config resolution, CDP framing (canned JSON). Fixture-page integration tests (local static servers on two ports for cross-origin frames) auto-skip when no Chromium binary is found, so `cargo test --workspace` stays green on machines without a browser (test-binary SIGKILL workaround from memory notes applies when running locally).

**Target Platform**: macOS, Linux, Windows (Chromium-family browser discovery per-OS; headless when no display). Cross-platform is constitution Principle 0 — no unix-only paths without windows equivalents.

**Project Type**: CLI/TUI agent workspace — new library crate `joey-browser` + tool registrations in `joey-tools` + wiring in `joey-cli`/`joey-agent-core`/`joey-providers`.

**Performance Goals** (budgets per constitution VIII; hot paths marked):
- Snapshot generation: ≤ 500 ms median on pages with ≤ 5,000 discovered interactive elements (page-wide discovery, viewport-priority presentation).
- Post-action settle wait: ≤ 2 s median after content settles (SC-004); hard timeout default 10 s, configurable; zero indefinite hangs.
- Screenshot capture + annotation: ≤ 2 s on 1080p viewport.
- Per-step feed delta snapshot: bounded by config (default target ≤ 8 KB textual material per step; cumulative cap default 64 KB/task).

**Constraints**: strictly additive public-surface change (constitution VII); browser tool results pass existing untrusted-content pipeline (`UNTRUSTED_TOOL_PREFIXES` already covers `browser_`); navigations reuse `url_safety` (`is_safe_url`) from joey-tools; no secrets in config.yaml (image-model keys are model names, not secrets); workspace DAG preserved (joey-browser sits between joey-core and joey-tools).

**Scale/Scope**: 1 new crate (~4–6 kLOC incl. injected JS), ~16 new tools (12 declared names + 4 additive verbs), 2 new config keys + per-provider overrides, provider image-content completion for OpenAI-chat/Responses/Anthropic/copilot wires, `/browser` slash subcommands, fixture suite (~10 pages), docs + PORTING.md updates.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| # | Principle | Pre-design status | Notes |
|---|-----------|-------------------|-------|
| 0 | Cross-platform compatibility | PASS (planned) | Browser discovery for macOS/Linux/Windows; headless fallback; no platform-specific syscalls. |
| I | Workspace-first Rust | PASS | New `crates/joey-browser` crate, independently buildable/testable. |
| II | CLI/TUI parity | PASS | Browser tools identical in CLI REPL and TUI; `/browser` slash command works in both. |
| III | Filesystem source of truth | N/A | No spec-artifact UI involved. |
| IV | Test-first for new crates | PASS (planned) | Tests alongside each phase; contract tests for snapshot format + config resolution; round-trip tests where serialization exists. |
| V | Incremental delivery | PASS | Six independently shippable phases (A–F, below); each builds and tests green alone. |
| VI | Modularity / narrow interfaces | PASS | `joey-browser` exposes a small `BrowserSession` API behind which all CDP detail hides; joey-tools sees only `BrowserHandle`. |
| VII | Backward compat, non-regression | PASS with note | All changes additive. Toolset membership lists gain 4 new names (hover/select/drag/coordinate-act) — additive, no renames; regression coverage task included (schema snapshots, toolset resolution, config round-trips). |
| VIII | Performance discipline / lean code | PASS | Zero new dependencies (tungstenite already linked in workspace); budgets recorded above and in contracts; research.md records dependency alternatives with binary-size/compile-time rationale. |

No violations requiring Complexity Tracking entries at plan time.

## Project Structure

### Documentation (this feature)

```text
specs/016-please-modify-joey/
├── plan.md              # This file
├── research.md          # Phase 0 output — decisions D1..D8
├── data-model.md        # Phase 1 output — entities & state machines
├── quickstart.md        # Phase 1 output — end-to-end validation guide
├── contracts/           # Phase 1 output
│   ├── browser-tools.md        # tool schemas (public surface for the model)
│   ├── cdp-session.md          # joey-browser public API (internal contract)
│   ├── snapshot-format.md      # snapshot grammar + ref/fallback encoding
│   └── image-model-routing.md  # config keys, resolution order, provider wire notes
└── tasks.md             # /speckit-tasks output (NOT created by /speckit-plan)
```

### Source Code (repository root)

```text
crates/
├── joey-browser/                 # NEW crate (depends on joey-core only)
│   ├── src/
│   │   ├── lib.rs                # public API re-exports
│   │   ├── cdp/mod.rs            # WebSocket JSON-RPC transport, session mux
│   │   ├── cdp/domains.rs        # typed wrappers: Target, Page, Runtime, Input, Emulation
│   │   ├── launch.rs             # browser discovery (mac/win/linux) + managed launch
│   │   ├── session.rs            # BrowserSession: attach/connect, dedicated tab, lifecycle
│   │   ├── page.rs               # PageSession: navigation, frame tree, settle detection
│   │   ├── extract/mod.rs        # injected JS runner, shadow/frame recursion driver
│   │   ├── extract/js/           # extraction scripts (include_str!)
│   │   │   ├── scan.js           # deep interactive-element scanner
│   │   │   ├── observer.js       # MutationObserver settle probe
│   │   │   └── overlays.js       # overlay detection + safe-dismissal heuristics
│   │   ├── refs.rs               # ElementRef registry, fallback cascade matcher
│   │   ├── actions.rs            # verb execution incl. coordinate input, drag, hover
│   │   ├── snapshot.rs           # viewport-priority presentation, deltas, budgets
│   │   └── vision.rs             # screenshot, SoM annotation, marker table
│   └── tests/
│       ├── fixtures/             # static pages: shadow nests, frames, feed, canvas, overlays
│       ├── refs_fallback.rs      # cascade matcher on mock element sets
│       ├── snapshot_format.rs    # golden/round-trip tests
│       ├── overlay_detect.rs     # heuristics on fixture DOM dumps
│       └── browser_integration.rs # gated on Chromium presence (auto-skip)
├── joey-tools/
│   └── src/tools/browser_tools.rs  # NEW: BrowserHandle + 16 Tool impls
│       (registered via builtins::register_browser_tools; joins "web" toolset path)
├── joey-core/src/config.rs         # image-model keys (read path only, additive)
├── joey-providers/src/*            # image-content serialization completion for
│                                  #   openai-chat / responses / anthropic / copilot wires
├── joey-agent-core/src/            # image-model routing helper (vision content → image model)
├── joey-cli/src/                   # /browser connect|status|disconnect handler + wiring
└── docs/                           # browser.md (new), providers.md/tools.md updates
```

**Structure Decision**: single new library crate `joey-browser` at the DAG position above `joey-tools` (joey-browser → joey-core; joey-tools → joey-browser). This mirrors the established neurocode pattern (backend trait in joey-tools, concrete engine in its own crate) so joey-tools keeps no CDP knowledge and higher crates wire the handle.

## Delivery Phases (constitution V increments)

- **Phase A — Transport & session**: CDP client, attach vs managed launch (auto, headless when displayless), dedicated agent tab, navigate/back/console/dialog/cdp raw passthrough. Shippable: basic browsing works.
- **Phase B — Deep perception**: shadow-piercing scanner, frame tree via Target auto-attach (cross-origin included), element refs + fallback locators, viewport-priority snapshot presentation (FR-001..004a).
- **Phase C — Actions**: full verb set + pre-action re-validation + fallback cascade + coordinate input (FR-005..009); closes the declared tool surface (browser_press, browser_get_images, base browser_scroll included).
- **Phase D — Resilience**: settle detection, conservative overlay handling, feed delta snapshots + budgets (FR-010..012).
- **Phase E — Vision**: screenshots, SoM annotation, browser_vision/browser_get_images/vision_analyze tools, per-provider image-model config + routing + provider wire completion (FR-013..016).
- **Phase F — Hardening & docs**: URL-safety + sanitization verification tests, /browser subcommands, schema-snapshot regression updates, docs/browser.md, PORTING.md, workspace green.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

No violations at plan time. (Re-checked post-design below — none.)

---

## Post-Design Constitution Re-Check (after Phase 1)

| # | Principle | Post-design status |
|---|-----------|--------------------|
| 0 | Cross-platform | PASS — launch.rs covers macOS/Linux/Windows discovery; contracts specify no platform-specific behavior. |
| I | Workspace-first | PASS — joey-browser crate, independently buildable. |
| II | CLI/TUI parity | PASS — tools registered once in joey-tools; both surfaces consume identically. |
| III | FS source of truth | N/A. |
| IV | Test-first | PASS — each contract has a matching test file; fixtures enumerated in data-model. |
| V | Incremental | PASS — phases A–F each independently green. |
| VI | Modularity | PASS — narrow `BrowserSession` API (cdp-session.md); CDP detail fully encapsulated. |
| VII | Backward compat | PASS — additive only; regression tasks named (schema snapshots, toolset resolution, config). |
| VIII | Lean/perf | PASS — zero new deps; budgets in Technical Context + contracts; D1 records alternatives. |

GATE: PASS — proceed to `/speckit-tasks`.
