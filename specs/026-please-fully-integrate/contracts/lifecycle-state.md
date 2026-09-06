# Contract: Lifecycle State

Derivation (pure function of on-disk artifacts; constitution III):

```
input:  repo root
read:   .specify/feature.json → feature_directory (missing/invalid → step=None)
        specs/<feature>/spec.md, plan.md, tasks.md existence + checkbox scan
output: LifecycleState { feature_directory, step }
step:   None | Specify | Clarify | Plan | Tasks | Implement | Acceptance
```

Rules (ordered, first match wins): no spec.md → Specify; no plan.md → Clarify if open questions remain else Plan-ready; no tasks.md → Tasks; unchecked top-level boxes in tasks.md → Implement; all checked → Acceptance.

## Injected context block shape (once per session, pre-first-turn)

```
## Spec-Kit Lifecycle Context
Feature: <feature_directory>
Step: <step> (<one-line guidance per step>)
Artifacts: spec.md [present|absent], plan.md [...], tasks.md [...]
Note: detected automatically; refresh by restarting the session or running /speckit-status.
```

Invariants: injected only when a spec-kit project is detected AND speckit.lifecycle_context=true; never re-rendered mid-session; /speckit-status renders the same derivation on demand.
