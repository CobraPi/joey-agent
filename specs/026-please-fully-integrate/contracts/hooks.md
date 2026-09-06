# Contract: Extension Hooks

Source: `.specify/extensions.yml` at repo root (absent → zero hooks, silent).

## Hook points (20)
before_/after_ × {specify, clarify, plan, constitution, checklist, tasks, analyze, implement, converge, taskstoissues}

## extensions.yml entry schema (per hook point, list of)
```
- extension: <name>
  command: <dotted command id>     # dispatch: dots→hyphens, e.g. speckit.git.commit → /speckit-git-commit
  description: <text>
  prompt: <text>
  optional: <bool>                 # false/absent = mandatory
  enabled: <bool>                  # absent = enabled
  condition: <expr>                # optional; NON-EMPTY values are passed through unevaluated (extension runtime owns evaluation)
```

## Execution semantics (upstream parity)
1. Invalid/unparseable YAML → skip hook discovery silently; command proceeds.
2. enabled=false → excluded.
3. Mandatory (optional=false): execute (as native command turn) and WAIT before proceeding; failure stops the step and reports the hook name.
4. Optional (optional=true): surface block with command/description/prompt and invocation path; do not block.
5. Hook command failure: mandatory stops step, optional logged and skipped.
6. speckit.hooks=false → discovery skipped entirely.
