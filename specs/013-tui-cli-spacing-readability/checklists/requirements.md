# Specification Quality Checklist: TUI & CLI Spacing / Vertical Rhythm (Crush-Style Readability)

**Purpose**: Validate specification completeness and quality before proceeding to planning
**Created**: 2026-08-05
**Feature**: [spec.md](../spec.md)

## Content Quality

- [x] No implementation details (languages, frameworks, APIs)
- [x] Focused on user value and business needs
- [x] Written for non-technical stakeholders
- [x] All mandatory sections completed

## Requirement Completeness

- [x] No [NEEDS CLARIFICATION] markers remain
- [x] Requirements are testable and unambiguous
- [x] Success criteria are measurable
- [x] Success criteria are technology-agnostic (no implementation details)
- [x] All acceptance scenarios are defined
- [x] Edge cases are identified
- [x] Scope is clearly bounded
- [x] Dependencies and assumptions identified

## Feature Readiness

- [x] All functional requirements have clear acceptance criteria
- [x] User scenarios cover primary flows
- [x] Feature meets measurable outcomes defined in Success Criteria
- [x] No implementation details leak into specification

## Notes

- Spec is presentation-only (confined to `joey-tui/widgets.rs` and
  `joey-cli/render.rs`); no `AgentEvent`/`TranscriptItem`/public-surface
  change and no new dependency — consistent with constitution Principles
  VII and VIII and the scope of the predecessor specs 007/008.
- File/line references in Assumptions (e.g. crush's
  `MessageLeftPaddingTotal = 2`, `maxTextWidth = 120`) are cited as the
  *reference source* for the spacing intent, not as implementation
  instructions; the plan/research phase will map them to joey's renderer.
- Deliberate scope boundary: this feature ports crush's *vertical rhythm*
  (spacing) and (TUI) *width cap / indent*. It does NOT re-litigate the
  block structures already delivered by specs 007 (TUI) and 008 (CLI);
  those are assumed stable and are referenced as dependencies.
- One known cross-cutting coupling flagged for the plan phase: the TUI
  `transcript_hit_test` line accounting (spec 007 T026) must stay in sync
  with any per-item line-count change the spacing rule introduces (SC-006,
  Assumptions). This is a planning concern, not a spec gap.
- No [NEEDS CLARIFICATION] markers were ever needed; instead three genuine
  ambiguities were resolved via the `/speckit-clarify` session (2026-08-05)
  and encoded under `## Clarifications` in spec.md: (Q1) "ample" = exactly
  one blank line between adjacent blocks, deduplicated at boundaries; (Q2)
  the TUI ~120-col width cap applies to body text only (headers/borders stay
  at panel width); (Q3) the CLI token-usage line is trailing metadata
  (tight before, one blank after). The request itself ("optimize TUI
  readability, follow crush style; fix CLI so there is ample spacing between
  all elements") is grounded in the crush reference and the existing
  007/008 block-layout work.
