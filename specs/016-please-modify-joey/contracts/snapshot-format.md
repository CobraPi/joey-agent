# Contract: Snapshot Format

Feature: specs/016-please-modify-joey | Normative serialization of `browser_snapshot` output (the model-facing observation unit). Versioned; additive-only evolution (constitution VII).

## Envelope

```json
{
  "v": 1,
  "mode": "structural" | "visual",
  "url": "https://…",
  "title": "…",
  "frame_count": 3,
  "viewport": { "x": 0, "y": 0, "w": 1280, "h": 800, "scroll_y": 0 },
  "elements": [ ElementRef, … ],          // viewport-priority ordered
  "out_of_view": [ RegionSummary, … ],    // FR-004a
  "blockers": [ Blocker, … ],
  "delta": { … } | null,                  // only when since_last=true
  "visual": { … } | null,                 // only when mode=visual
  "truncation": { "applied": false } | { "applied": true, "reason": "step_budget", "omitted": 42 }
}
```

## ElementRef

```json
{
  "refid": "e12",
  "role": "button",
  "text": "Save",
  "value": null,
  "frame": "main" | "iframe:checkout" | "oopif:ads",
  "locator": "button:nth-of-type(2)",
  "geometry": { "x": 340, "y": 510, "w": 120, "h": 40 },
  "attributes": { "aria-label": "Save case", "type": "submit" },
  "interactable": true
}
```

Rules: refid matches `^e[0-9]+$`, unique per snapshot; text capped at 120 chars, whitespace-normalized; attributes restricted to the allowlist (aria-label, placeholder, href≤80 chars, name, type); value present only for form controls and only page-controlled values.

## RegionSummary (out-of-view)

```json
{ "region": "below", "direction": "down", "counts": { "button": 14, "link": 63, "input": 4 }, "note": "form section continues" }
```

Regions: `above | below | left | right` relative to viewport, plus optional named panel regions for frames out of view. `note` ≤ 80 chars, derived from nearest heading/landmark.

## Blocker

```json
{ "kind": "consent|dialog|interstitial|unknown", "description": "Cookie consent (OneTrust)", "frame": "main", "dismissal": "auto_dismissed|refused_unsafe|flagged" }
```

## Delta (feeds, since_last=true)

```json
{
  "new_elements": [ ElementRef, … ],       // only elements not present in previous snapshot
  "gone_refids": ["e5", "e17"],
  "out_of_view": [ RegionSummary, … ],
  "cumulative_bytes": 21000,
  "cumulative_cap_bytes": 65536
}
 mode:"structural", elements omitted when delta present
```

## VisualObservation

```json
{
  "image": "data:image/png;base64,…",
  "strategy": "dom_geometry|coarse_grid",
  "markers": [ { "id": "m1", "label": "button?", "rect": { "x": 340, "y": 510, "w": 120, "h": 40 } } ],
  "marker_table": "m1 (340,510) button? · m2 (610,220) link? …"
}
```

## Budget & truncation

Per-step textual budget (default 8 KB) applies to serialized `elements + out_of_view + delta`; cumulative cap (default 64 KB/task) applies to delta accumulation. When exceeded: drop to RegionSummary granularity for the least-recently-relevant regions, set `truncation.applied=true` with reason `step_budget | cumulative_cap` and `omitted` count. Never silently truncate (FR-004a/FR-012).

## Text-rendering for the model

Tools return the JSON above pretty-printed when ≤ 4 KB, else compact. Element lines may also be rendered as the compact line grammar `e12 [button] "Save" @iframe:checkout (340,510 120x40) locator=…` for token efficiency — normative order: refid, role, text, frame, geometry, locator, interactable flag only when false.

## Versioning

`v` starts at 1. Additive changes only (new optional fields, new enum values documented); consumers MUST ignore unknown fields. Removing/renaming a field or changing semantics requires a MAJOR bump with migration notes in this file.
