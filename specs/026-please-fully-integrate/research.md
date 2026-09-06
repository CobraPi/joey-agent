# Research: Native Spec-Kit Integration with Copilot Command Parity

Date: 2026-09-03 | Branch: 026-please-fully-integrate | Inputs: spec.md + clarifications, upstream spec-kit audit (local checkout @ e3e6a3c), codebase exploration (repl.rs, slash.rs, prompt.rs, agent.rs, neurocode, omo conductor, joey-speckit-ui).

## Upstream parity baseline (audited facts)

- 10 core commands hard-coded upstream: analyze, checklist, clarify, constitution, converge, implement, plan, specify, tasks, taskstoissues.
- Copilot skills mode: `.github/skills/speckit-<name>/SKILL.md`, invoked `/speckit-<name>`. Commands mode: `.github/agents/speckit.<name>.agent.md` + stub `.github/prompts/speckit.<name>.prompt.md`, dispatched via agent addressing (`speckit.<name>`).
- 20 hook points (before_/after_ x each command) driven purely by prompt text reading `.specify/extensions.yml`; invalid YAML → skip silently; enabled:false filtered; non-empty condition left to the HookExecutor.
- Frontmatter semantics: description, optional handoffs (label/agent/prompt/send), optional scripts (sh/ps/py variants), optional tools (only taskstoissues).
- Parallelism upstream = `[P]` task markers only; no multi-agent orchestration.

## Decisions

### D1 — Bundled workflow bodies ship via std include_str!
- Decision: vendor the ten workflow bodies as `crates/joey-cli/src/speckit_bodies/*.md`, embedded with `include_str!` behind a resolution function.
- Rationale: satisfies FR-004 (release-versioned, zero install) with zero new dependencies (constitution VIII: dependency weight must be justified; std wins). Codebase has no rust-embed/include_dir precedent — the single existing embed is an `include_str!` in joey-neurocode-rag.
- Alternatives considered: `include_dir`/`rust-embed` crate (new external dep, rejected); installing bodies into `~/.joey/optional-skills` at package time (drifts from binary version, requires installer changes, machine-dependent — rejected); fetching from skills hub at runtime (network dependency, rejected).

### D2 — Body resolution chain: project-local → user skills → bundled
- Decision: `.github/skills/speckit-<name>/SKILL.md` → `.github/agents/speckit.<name>.agent.md` (+ companion prompt) → `.specify/` project body → `~/.joey/skills/speckit-<name>/SKILL.md` (compat, current behavior) → bundled `include_str!` body.
- Rationale: clarification 1 mandates full discovery parity with upstream layouts; keeping the existing `~/.joey/skills` hop preserves today's behavior (constitution VII); bundled body is the guaranteed floor (FR-004).
- Alternatives considered: `.specify/`-only (rejected in clarification 1); configurable search order (unnecessary knob; fixed documented order is testable).

### D3 — Dotted form dispatched by prefix intercept, normalized to the existing pipeline
- Decision: in `repl.rs::process_input` and TUI `handle_slash`, inputs starting with `speckit.` (no leading `/`) are normalized to the corresponding `speckit-<name>` path and run through the SAME `speckit_step_slash` flow; completion candidates include both forms.
- Rationale: FR-003 requires identical behavior for both forms — one implementation, two entry spellings, is the only way to guarantee it by construction. Today there is no dotted-prefix handling anywhere in joey-cli (verified), so the intercept is purely additive.
- Alternatives considered: registering dotted names as separate REGISTRY commands (duplicated surface, drift risk); shelling out to the external Copilot binary (violates the native requirement and FR-003's "without external agent binaries").

### D4 — Hooks parsed with serde_yaml, executed by mapping hook commands to native turns
- Decision: `speckit_hooks.rs` parses `.specify/extensions.yml` (serde_yaml, already in the workspace via joey-core) into a typed model; hook command names map to native slash commands (dots→hyphens per upstream); mandatory hooks are invoked and awaited as agent turns before/after the step; optional hooks are surfaced as display blocks. Non-empty `condition` fields are passed through unevaluated; invalid YAML skips silently.
- Rationale: byte-parity with upstream hook semantics (20 points, optional/mandatory, enabled filter, silent-skip) is a hard FR-005 requirement; reusing serde_yaml adds no dependency weight.
- Alternatives considered: a minimal hand-rolled YAML subset parser (fragile against real extensions files — rejected); executing hooks by spawning shell commands (upstream maps hooks to agent commands, not shell — rejected).

### D5 — Lifecycle context injected once at session construction via the extra-instructions slot
- Decision: at REPL/oneshot session start (engine built once per session today, e.g. neurocode wiring precedent), `speckit_lifecycle.rs` detects feature.json + artifacts and appends one structured block through the existing `extra_instructions`/context-assembly path BEFORE the first turn; never re-rendered per turn (system prompt stays cache-warm).
- Rationale: clarification 2 chose automatic session-start injection; the prompt is deliberately built once per session in this codebase, so session-construction is the only cache-safe injection point. `rebuild_system_prompt` exists but is reserved for mode toggles; we do not touch it.
- Alternatives considered: per-turn artifact re-detection (breaks prompt-prefix caching — rejected); opt-in `/speckit-load` command (rejected in clarification 2).

### D6 — Orchestration reuses the joey-speckit-ui parser; conductor prompt gets a dynamic state block
- Decision: `speckit_lifecycle.rs` converts tasks.md (parsed via joey-speckit-ui's existing model) into orchestration TaskNodes (id/objective/dependencies/read_set/write_set), enforcing the no-two-specialists-same-file rule through existing write_set overlap checks; the conductor template keeps its static SPEC_KIT_DOCTRINE (tests pin it) and appends a dynamic `CURRENT LIFECYCLE STATE` block filled from detection.
- Rationale: joey-speckit-ui already owns spec/plan/tasks parsing with contract tests (constitution III/VI — do not write a second parser); joey-orchestration has NO markdown task source today (verified: zero checkbox/tasks.md parsing), so an adapter is the minimal bridge; keeping the static doctrine text intact preserves existing prompt tests while satisfying FR-008.
- Alternatives considered: teaching joey-orchestration to parse markdown directly (couples crates, duplicates parser — rejected); replacing the doctrine with a fully dynamic prompt (breaks pinned tests and upstream-fidelity wording — rejected).

### D7 — Neurocode scoping via an additive CodingRequest field + verification acceptance input
- Decision: add `scope_files: Vec<String>` (default empty) to CodingRequest; discovery seeds `find_primary_nodes` from scope_files (feature plan/tasks/research file lists) in addition to today's text-hint extraction; auto_index keeps thresholds but orders reindex work scope-first; verification planning accepts an optional acceptance-criteria list so plans can reference the spec's scenarios.
- Rationale: `assemble()` currently has NO caller-supplied file filter (verified: hints come only from request text), so an additive optional field is the narrowest change; empty default keeps non-spec-kit sessions byte-identical (FR-011/SC "no active feature → unchanged").
- Alternatives considered: a separate spec-kit context assembler parallel to ContextAssembler (duplicate ranking logic — rejected); rewriting hints extraction to look for spec paths in text (fragile, indirect — rejected).

### D8 — Config: `speckit.*` section, master switch default-on, inert outside spec-kit repos
- Decision: new defaults block `speckit: { enabled: true, lifecycle_context: true, hooks: true }` in joey-core DEFAULT_CONFIG_YAML (CONFIG_VERSION bump, additive keys only); `speckit.enabled: false` short-circuits every new code path, restoring pre-feature behavior exactly (FR-013); detection no-ops when no `.specify/` exists, so default-on costs one stat call on non-spec-kit projects.
- Rationale: constitution VII requires exact restoration; a master switch plus two narrow sub-toggles is the smallest surface that satisfies FR-013 without a config explosion.
- Alternatives considered: default-off (feature invisible to the users who asked for it — rejected); per-feature toggles in feature.json (mixes project data with agent config — rejected).

## Risks flagged for tasks phase

- Vendoring bodies must record provenance (upstream version) and a refresh procedure (assumption in spec.md: configuration-controlled refresh path, not frozen snapshot).
- Dotted intercept must match ONLY the `speckit.` prefix to avoid capturing unrelated bare-word input (edge case: collision handling disambiguates with an error, never silently shadows).
- Windows: scaffold script variants differ per platform; fallback path (FR-002) must cover PowerShell scaffolds.
