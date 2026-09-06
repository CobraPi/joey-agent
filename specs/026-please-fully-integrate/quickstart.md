# Quickstart: Validating Native Spec-Kit Integration

Prerequisites: repo built (`cargo build --workspace`); scratch dir outside the repo; joey binary on PATH (or use cargo run -p joey-cli).

## 1. Native command + dotted parity
```
mkdir /tmp/sk && cd /tmp/sk && git init && mkdir -p .specify && echo '{"feature_directory":"specs/001-demo"}' > .specify/feature.json && mkdir -p specs/001-demo
joey   # start session in /tmp/sk
> /speckit-status        # EXPECT: lifecycle context block, Step: Specify
> speckit.status         # EXPECT: identical output (dotted form)
> /speckit-help          # EXPECT: all 10 commands + status/help listed
> speckit.nonexistent    # EXPECT: error listing commands + suggestion
```

## 2. Bundled bodies (fresh machine)
```
mv ~/.joey/skills ~/.joey/skills.bak
> /speckit-constitution  # EXPECT: runs from bundled body (warning if fallback pre-flight used)
mv ~/.joey/skills.bak ~/.joey/skills
```

## 3. Project-local override precedence
```
mkdir -p /tmp/sk/.github/skills/speckit-constitution && printf -- '---\nname: speckit-constitution\n---\nOVERRIDE MARKER 7f3a\n' > /tmp/sk/.github/skills/speckit-constitution/SKILL.md
> /speckit-constitution  # restart session; EXPECT: override body used (marker visible in workflow text)
```

## 4. Extension hooks
```
cat > /tmp/sk/.specify/extensions.yml <<'EOF'
hooks:
  before_specify:
    - extension: demo
      command: demo.marker
      description: marker
      prompt: create marker
      optional: false
EOF
> /speckit-specify demo feature    # EXPECT: mandatory hook surfaces EXECUTE_COMMAND + runs before spec creation
```
(Invalid YAML variant: replace file contents with `:~bad`; EXPECT: silent skip, command proceeds.)

## 5. Orchestration + neurocode awareness
In the joey-agent repo itself (real .specify present): start a hypercode session → EXPECT: conductor reports detected step and dispatches read-only researchers during planning steps; with unchecked tasks.md boxes → implement fan-out respecting write_set exclusivity; neurocode context for a file in the active feature's tasks includes feature entities (verify via /speckit-status + a code question about a listed file).

## 6. Config disable path
```
joey config set speckit.enabled false
> /speckit-status      # EXPECT: pre-feature behavior (no lifecycle block)
joey config set speckit.enabled true
```

## 7. Automated gates
```
cargo test -p joey-cli      # parity: 10 commands x 2 forms, 20 hook points, resolution chain
cargo test -p joey-neurocode
cargo test -p joey-omo
cargo test --workspace      # no regressions
```

Expected end state: full specify→converge cycle drivable in one session with either command form; all workspace tests green.

## Validation Results (2026-09-03)

Recorded in specs/026-please-fully-integrate/validation-results.md (executed 2026-09-04, no deliberate model inference):

- §1 Native command + dotted parity — PASS (real binary, piped REPL; dotted status byte-identical; help lists 12; unknown lists commands, no Closest for far input — matches pinned test)
- §2 Bundled bodies — PASS-via-automated-test (`bundled_floor_at_dispatch`)
- §3 Override precedence — PASS-via-automated-test (`speckit_bodies::tests::resolution_precedence`)
- §4 Extension hooks — PASS-via-automated-test (`hook_notes_shapes`, `invalid_yaml_silent`, `hook_points_cover_all_twenty`)
- §5 Orchestration + neurocode — PASS-via-automated-test (`conductor_lifecycle_block_appended_after_doctrine`; neurocode context_enrichment 9/9); live fan-out NOT-RUNNABLE-needs-inference
- §6 Config disable — PASS (real-binary `config set/get` round-trip false→true) + tests for no-lifecycle-block half
- §7 Automated gates — PASS (joey-cli 482, joey-neurocode 329, joey-omo 143; all 0 failed)

Not exercised end-to-end: artifact authoring via agent turns (see "requires live model inference" in validation-results.md).
