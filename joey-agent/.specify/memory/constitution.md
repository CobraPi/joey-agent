<!--
Sync Impact Report
Version change: (none, template) → 1.0.0
Modified principles: N/A (initial ratification — all 5 principle slots filled for the first time)
Added sections: Core Principles (I–V), Architecture Constraints, Development Workflow, Governance
Removed sections: none
Templates requiring updates:
  - .specify/templates/plan-template.md ✅ updated (Constitution Check gate now enumerates crate/trait/surface/seam/YAGNI checks)
  - .specify/templates/spec-template.md ✅ no change needed (spec template is already implementation-agnostic; modularity is a plan/tasks concern, not a spec concern)
  - .specify/templates/tasks-template.md ✅ updated (Polish phase note distinguishes seam-definition/seam-test tasks from concrete-implementation tasks)
  - README.md ⚠ pending (crate table in README.md is a candidate for a short "why crates are split this way" cross-reference to Principle I once repo maintainers review)
Follow-up TODOs:
  - TODO(RATIFICATION_DATE): No prior ratified constitution existed for this repo; 2026-07-23 is used as the ratification date because that is when this constitution was first adopted. If an earlier informal agreement predates this, replace with that date.
-->

# Joey Agent Constitution

## Core Principles

### I. Crate Boundaries Are the Modularity Unit (NON-NEGOTIABLE)
Every capability MUST live in the crate whose single responsibility it matches
(`joey-core` branding/config/state, `joey-providers` wire protocols,
`joey-tools` tool trait + built-ins, `joey-agent-core` turn loop, `joey-cron`
scheduler, `joey-mcp` MCP client, `joey-gateway` platform adapters, `joey-tui`
terminal UI, `joey-cli` command tree). A new feature MUST NOT be implemented by
reaching into an unrelated crate's internals or by growing a crate past its
stated responsibility; if no existing crate fits, a new workspace member MUST
be added rather than bolting the feature onto the nearest crate for convenience.
Cross-crate calls MUST go through each crate's public API (traits, structs,
functions it exports) — never through crate-internal (`pub(crate)`) items, and
never by adding one-off `pub use` re-exports purely to leak an implementation
detail across the boundary. Rationale: the crate graph *is* the dependency
graph; keeping it aligned with responsibility is what makes "add a feature"
mean "touch one crate" instead of "trace call sites across the workspace."

### II. Extend via Traits and Registries, Not Conditionals
Any point where behavior varies by kind — providers, tools, toolsets, gateway
adapters, MCP transports, cron job types, TUI dialogs/widgets — MUST be modeled
as a trait (or enum dispatching to trait objects) plus a registry/factory that
new implementations plug into. Adding a new provider, tool, adapter, or dialog
MUST be possible by adding one new module that implements the trait and
registers itself, WITHOUT editing a central `match`/`if` chain that enumerates
every existing variant by name. Where such a chain already exists (tech debt),
new work MUST NOT deepen it; refactor the touched chain into a
trait+registry as part of the change, or leave a `// TODO(constitution-II)`
note explaining why it wasn't feasible in that change. Rationale: open/closed
extension points are what let "add a feature" stay additive instead of
requiring edits scattered across every existing case.

### III. Explicit, Minimal Public Surface Per Module
Every module and crate MUST expose the smallest public surface that satisfies
its callers: prefer `pub(crate)` and private items by default, and promote to
`pub` only when an external caller (another crate, or `joey-cli`) genuinely
needs it. Data structures crossing a crate boundary MUST be plain data (structs
implementing `serde`/`Clone`/etc. as needed) or trait objects — not shared
mutable state, global singletons, or `unsafe` shortcuts used to avoid defining
a proper interface. Configuration and state that a module needs MUST be passed
in (constructor/function parameters, dependency injection) rather than read
from ambient globals, except for the already-established `joey-core`
path/profile/config singletons that the whole workspace is built around.
Rationale: a small, explicit surface is what makes a module safely replaceable
or extensible without a ripple of breakage in unrelated code.

### IV. Test the Seam, Not Just the Implementation
Every new trait/registry extension point introduced under Principle II MUST
ship with at least one test that exercises it through the seam (e.g. a fake
provider registered against the provider registry, a stub tool run through the
tool dispatch path) rather than only unit-testing the concrete implementation
in isolation. Contract-shaped changes (a trait's method signatures, a
serialized state-store schema, a wire payload shape) MUST include a test that
would fail if a future change silently broke that contract. This is lighter
than full TDD: tests MAY be written alongside implementation rather than
strictly before it, but a PR that adds a new pluggable implementation MUST NOT
merge without a seam-level test proving the plugin mechanism itself still
works. Rationale: modularity is only real if the seams are verified — an
untested trait boundary silently rots into an implicit, tightly-coupled one.

### V. Simplicity and YAGNI Bound the Modularity Effort
Decoupling MUST be applied where Principles I–IV already require it (crate
boundaries, existing variance points, public surfaces, seams) — it MUST NOT be
used to justify speculative abstraction layers, extra indirection, or new
plugin systems for capabilities the project does not yet have a second
implementation of. When a design choice trades simplicity for future
extensibility, the PR/plan MUST state in one sentence which concrete,
near-term feature the extensibility is for. If no concrete near-term feature
is named, the simpler, less abstract design MUST be chosen. Rationale: the
goal is being able to *easily add* features, not maximizing indirection;
premature generalization is its own form of coupling (to a guessed-at future).

## Architecture Constraints

- The Cargo workspace crate list and dependency direction defined in
  `Cargo.toml` (`joey-core` → `joey-providers`/`joey-tools`/`joey-cron`/
  `joey-mcp`/`joey-gateway` → `joey-agent-core` → `joey-tui`/`joey-cli`) MUST be
  treated as a directed acyclic graph: a lower-level crate (e.g. `joey-core`)
  MUST NOT depend on a higher-level crate (e.g. `joey-tui`). New inter-crate
  dependencies MUST be justified in the plan's Constitution Check against this
  direction.
- Feature/behavior parity work against an external reference implementation
  (e.g. Hermes Agent in Python, or the Crush TUI in Go) MUST be scoped to
  matching externally observable behavior and data formats, not to mirroring
  that project's internal module layout — Joey Agent's own crate boundaries
  (Principle I) take precedence over the reference project's file/package
  structure.
- Any new pluggable kind (provider, tool, adapter, dialog, job type) MUST
  document its trait and registration mechanism in that crate's module-level
  doc comment or a `README.md` in the crate, so the next contributor can find
  the extension point without reading the whole crate.

## Development Workflow

- Every `/speckit-plan` for a feature MUST include a Constitution Check section
  that explicitly states: (a) which crate(s) the feature touches and why, (b)
  whether it introduces or extends a trait/registry extension point per
  Principle II, and (c) what seam-level test (Principle IV) will cover it.
- Code review (self- or peer-) MUST flag any new cross-crate `match`/`if`
  chain enumerating concrete types by name, any new `pub` item that isn't
  called from outside its crate, and any new abstraction layer that Principle
  V's near-term-feature test fails.
- Complexity MUST be justified in the PR description or plan document when a
  reviewer would otherwise read the change as violating Principles I–III;
  silence is not sufficient justification.

## Governance

This constitution supersedes ad hoc conventions for any conflict between them
and this document. Amendments are made by editing this file directly via the
`/speckit-constitution` workflow: proposed changes MUST update the Sync Impact
Report, bump `CONSTITUTION_VERSION` per semantic versioning (MAJOR for
backward-incompatible principle removals/redefinitions, MINOR for new
principles or materially expanded guidance, PATCH for clarifications/wording),
and propagate any resulting changes to `.specify/templates/plan-template.md`,
`.specify/templates/tasks-template.md`, and other dependent guidance. All
`/speckit-plan` and `/speckit-tasks` runs MUST verify compliance with this
constitution's Constitution Check requirement before implementation begins;
a plan that cannot satisfy Principles I–V MUST document the deviation and a
concrete remediation path rather than silently proceeding. Use `README.md` and
each crate's module-level docs for day-to-day runtime development guidance
that elaborates on (but never contradicts) this constitution.

**Version**: 1.0.0 | **Ratified**: 2026-07-23 | **Last Amended**: 2026-07-23
