# Data Model: Native Spec-Kit Integration

Entities below are logical (constitution III: filesystem remains the source of truth; no database).

## SpecKitCommand
- Fields: name (one of the ten lifecycle names), slash_form (`/speckit-<name>`), dotted_form (`speckit.<name>`), preflight (script id + flags, per upstream table), body (resolved WorkflowBody), handoffs (0..n Handoff), tools (0..n string refs).
- Validation: name ∈ {specify, clarify, plan, constitution, checklist, tasks, analyze, implement, converge, taskstoissues}; slash_form and dotted_form derive from name (no free-form aliases).
- State transitions: none (immutable per dispatch).

## WorkflowBody
- Fields: text, source (enum: GithubSkillsLayout | GithubAgentsLayout | SpecifyDir | UserSkills | Bundled), provenance (upstream version for Bundled).
- Resolution order (D2): .github/skills → .github/agents(+prompt) → .specify/ → ~/.joey/skills → bundled.
- Validation: body must be non-empty; frontmatter (if present) must parse to handoffs/scripts/tools.

## ExtensionHook
- Fields: extension, command (dots normalized to hyphens for dispatch), description, prompt, optional (bool), enabled (default true), condition (optional, passed through unevaluated), hook_point (one of 20: before_/after_ × ten command names).
- Validation: invalid or missing extensions.yml → zero hooks, silent skip.

## Handoff
- Fields: label, target command name, prompt, send (bool; auto-invoke when true).

## LifecycleState
- Fields: feature_directory (from .specify/feature.json), step (enum: Specify | Clarify | Plan | Tasks | Implement | Acceptance | None).
- Derivation: no spec.md → Specify; spec only → Clarify/Plan; plan only → Tasks; tasks with unchecked items → Implement; all checked → Acceptance; no feature.json → None.
- Consumers: session-start context block (D5), conductor dispatch doctrine (D6), status surface.

## FeatureScope
- Fields: read_files, write_files (from plan/tasks), acceptance_criteria (from spec scenarios).
- Consumers: orchestration TaskNode conversion (write_files → write_set, overlap = conflict), neurocode scope_files (D7).

## SpeckitConfig
- Fields: enabled (bool, default true), lifecycle_context (bool, default true), hooks (bool, default true).
- Validation: enabled=false → all new paths short-circuit; behavior identical to pre-feature.
