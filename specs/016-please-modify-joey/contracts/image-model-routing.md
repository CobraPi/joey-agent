# Contract: Image-Model Routing & Provider Vision Support

Feature: specs/016-please-modify-joey | FR-015/FR-016: every provider gains a dedicated, config-only image model for webpage/screenshot understanding. This contract defines config keys, resolution order, wire requirements, and reporting.

## Config keys (additive, joey-core layered config)

| Key | Scope | Default | Wins over |
|-----|-------|---------|-----------|
| `model.image_model` | global | unset | — |
| `providers.<id>.image_model` | per-provider | unset | `model.image_model` |

Both hold a plain model id string (e.g. `gpt-4o-mini`, `claude-sonnet-4-5`, `gemini-2.0-flash`). Not secrets — stored in config.yaml, no `.env` routing. Unknown-key tolerance and layered precedence (defaults < config.yaml < env) follow existing config behavior.

## Resolution order (normative, pure function in joey-agent-core)

```text
1. providers.<active>.image_model                  → explicit_per_provider
2. model.image_model                               → explicit_global
3. provider default multimodal model               → provider_default     (from model catalog supports_vision data)
4. primary model if supports_vision                → primary_if_vision
5. else → ResolvedImageModel::unavailable(reason)  — turn fails with a clear, actionable error
   (message suggests setting model.image_model or providers.<id>.image_model)
```

The resolver is pure and unit-tested for all orders including overrides and fallback failure. Result reports `source` so tool output / logs can state which model served visual content (FR-016 "reports which model was used").

## Routing behavior

- Applies when a turn's message content contains image parts (screenshots via browser_vision / vision_analyze / gateway media attachments).
- Text-only turns are never rerouted — zero behavior change when no visual content exists (Principle VII).
- The image model reuses the active provider's auth, base-URL, and wire stack (same family: openai-chat, openai-responses, anthropic, copilot, zai). No separate provider needed.

## Provider wire requirements (completion work)

Today `ContentPart::ImageUrl` serializes only on the Anthropic wire. This feature completes image-content serialization on:

| Wire | Status today | Required |
|------|--------------|----------|
| openai-chat (`chat.rs` incl. zai via same wire) | missing | serialize image parts (`{"type":"image_url","image_url":{"url":…}}`) |
| openai-responses | missing | input content image items (MUST carry `"type":"message"` — known pitfall) |
| anthropic | partial | verify/complete base64 data-URL → `{"type":"image","source":{…}}` |
| copilot | missing | openai-chat compatible image parts; respects `/responses` wire mode for gpt-5.x majors |

Regression: existing no-image request bodies MUST remain byte-identical (snapshot tests per wire).

## Reporting

- `browser_vision` result includes `"served_by": { "model": "…", "source": "explicit_per_provider|…" }`.
- `joey doctor` (or equivalent) reports image-model resolution per provider when visual tools are used.
- Setup wizard / docs list the keys.

## Versioning

Keys are new public config surface (additive). Renames or semantic changes require MAJOR + migration per constitution VII.
