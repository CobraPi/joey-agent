# Contract: Browser Tools (public tool surface for the model)

Feature: specs/016-please-modify-joey | These schemas are the model-facing public surface (constitution VII: additive contracts). Wire format: standard `Tool` trait JSON-schema definitions in `joey-tools/src/tools/browser_tools.rs`; this document is normative for names, parameters, results, and error shapes.

## Registration & availability

- Registered by `builtins::register_browser_tools(registry, handle: Option<Arc<BrowserHandle>>)`; every tool's `check()` returns false when no handle is wired — hidden from the model until a browser session exists (pattern: neurocode tools).
- Toolset membership: the 12 declared names are already in `CORE_TOOLS`; the 4 additive verbs (`browser_hover`, `browser_select_option`, `browser_drag`, `browser_click_coords`) are appended — no renames, no removals, resolution order unchanged.

## Common conventions

- **Target descriptor** (used by click/type/hover/select/drag/scroll_container): an object with optional `refid`, `locator`, `text`, `geometry {x,y,w,h}`. At least one must be present. Resolution cascade: refid → locator → text → geometry (→ refuse with candidates). Result always reports `resolved_by` (one of `refid|locator|text|geometry|refused_ambiguous`).
- **Wait behavior**: every mutating action runs post-action settle detection (quiet-window, D4) and returns the next observation hint. Actions return compact results; full observation comes from `browser_snapshot`.
- **Errors**: `{"error": "..."}` string plus, where applicable, `candidates: [ElementRef…]` when refusing ambiguous matches (FR-007). Errors are diagnostics, never fabricated page state.

## Tools

### browser_navigate `{ url }` → `{ url, title, frame_count, settled_ms }`
Navigates the agent's dedicated tab after `url_safety::is_safe_url` check. Refuses local/private targets with the same message family as web tools.

### browser_snapshot `{ viewport_only?: bool, since_last?: bool }` → Snapshot JSON
Deep scan (shadow + frames) with viewport-priority presentation (FR-004a). `since_last=true` computes feed deltas (FR-012). Output shape per [snapshot-format.md](snapshot-format.md).

### browser_click `{ target }` → `{ ok, resolved_by, clicked_text? }`
### browser_type `{ target, text, submit?: bool, clear?: bool }` → `{ ok, resolved_by }`
### browser_hover `{ target }` → `{ ok, resolved_by }` (opens hover menus)
### browser_scroll `{ direction: up|down, amount?: px|page, target? }` → `{ scroll_y, delta_summary? }`
Optional `target` param selects a specific scrollable container (resolved via the standard cascade) instead of the page-level scroll; omitted = page-level.

### browser_select_option `{ target, value }` → `{ ok, resolved_by, selected }`
### browser_drag `{ source: target, target: target2 }` → `{ ok, resolved_by_source, resolved_by_target }`
### browser_press `{ key, modifiers?: [ctrl|alt|shift|meta|cmd] }` → `{ ok }`
### browser_back `{ }` → `{ url, title }`
### browser_click_coords `{ x, y }` → `{ ok }` (viewport pixel coordinates; FR-009)
### browser_get_images `{ }` → `{ images: [{src, alt, width, height, is_element_visible}] }`
### browser_console `{ }` → `{ entries: [{level, text, source, location}], truncated }`
### browser_dialog `{ action: accept|dismiss, prompt_text? }` → `{ handled }` (JS dialogs via `Page.javascriptDialogOpening`)
### browser_vision `{ prompt? }` → VisualObservation (SoM annotated screenshot + marker table, D6)
### browser_cdp `{ method, params }` → raw CDP result (gated on `browser.allow_raw_cdp`, D10)

## Versioning

- New parameters to existing tools MUST be optional (backwards-compatible schema evolution).
- New verbs append; existing names never change meaning. A breaking change requires MAJOR + migration per constitution VII.
- Snapshot format versioning: see [snapshot-format.md](snapshot-format.md) — `v` field, additive fields only.
