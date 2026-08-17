# Quickstart: Universal Web-Page Browsing

Feature: specs/016-please-modify-joey | Runnable validation guide proving the feature end-to-end. Prerequisites, commands, expected outcomes. Implementation details live in tasks.md; schemas in [contracts/](contracts/).

## Prerequisites

- Rust stable toolchain; workspace builds (`cargo build --workspace`).
- A Chromium-family browser on PATH or at a known location (Chrome/Edge/Chromium/Brave), **or** a running browser started with `--remote-debugging-port=9222` for attach mode.
- Tests auto-skip browser-dependent integration tests when no Chromium is found — `cargo test --workspace` stays green either way.

## 1. Build & unit/contract tests (no browser needed)

```bash
cargo build --workspace
cargo test -p joey-browser            # refs fallback cascade, snapshot format, overlay heuristics, config
cargo test -p joey-tools              # tool schemas + registration
cargo test -p joey-providers          # image-content wire serialization (incl. no-image regression snapshots)
```

Expected: all green. joey-browser unit tests use canned CDP JSON and mock element sets — no live browser.

## 2. Fixture-page integration tests (Chromium present)

```bash
cargo test -p joey-browser --test browser_integration    # spins local HTTP fixture servers
```

Fixtures (served from `crates/joey-browser/tests/fixtures/`): nested shadow roots (3 deep), same-origin iframe nest, cross-origin iframe (two ports), continuous-mutation SPA, consent modal, first-run tour dialog, native select, hover menu, nested scroll container, drag board, infinite feed, canvas-only page. Expected: SC-002/003/004/005/006/007-shaped assertions pass (coverage ≥95% discovery, ≥95% stale-reference action success, bounded settle, overlay policy, delta budgets, SoM fallback).

## 3. Live end-to-end (attach mode)

```bash
# terminal 1 — your browser with remote debugging:
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --remote-debugging-port=9222
# (log into your target app first, e.g. Pega Studio)

# terminal 2:
cargo run -p joey-cli -- repl        # or your usual joey entrypoint
```

In the session:

```text
/browser connect                     → Attached mode + endpoint reported
"take a snapshot of the studio UI"   → structural snapshot; shadow/frame elements labeled
"click the Create Case button"       → resolved_by reported; dedicated tab only
/browser status                      → mode, page url/title
```

Expected: snapshot lists shadow-nested and frame-hosted controls with frame labels; your original tab is untouched; actions report which fallback strategy resolved.

## 4. Live end-to-end (managed/headless)

With no browser running and no display:

```bash
cargo run -p joey-cli -- -p local oneshot "open https://example.com and list the interactive elements"
```

Expected: managed browser launches headless, task completes, `served_by`-style status shows session mode Managed; browser process exits with the session.

## 5. Dedicated image model (config-only)

```bash
joey config set model.image_model gpt-4o-mini                 # global default
joey config set providers.zai.image_model glm-4.6v            # per-provider override wins
```

Then ask the agent to describe a page visually (`browser_vision`). Expected: visual content served by the configured model; result reports `served_by { model, source }`; unset keys fall back through provider default → primary-if-vision with the used model reported. Text turns never reroute.

## 6. Safety checks (quick verification)

- `browser_navigate` to `http://127.0.0.1:8080` → refused (URL-safety, same message family as web tools).
- `browser_cdp` without `browser.allow_raw_cdp=true` → `RawCdpDisabled`.
- Tool outputs pass the untrusted-content pipeline (spot-check agent-core wrapping of browser_ results).

## 7. Acceptance mapping

| Quickstart step | Spec criteria |
|-----------------|---------------|
| §2 fixtures | SC-002, SC-003, SC-004, SC-005, SC-006, SC-007 |
| §3 attach flow | Story 3, FR-017, clarification Q1 (dedicated tab) |
| §4 managed flow | Clarification Q2 (auto-launch), FR-017 |
| §5 image model | FR-015, FR-016, SC-008 |
| §6 safety | FR-019, FR-020, D10 |
