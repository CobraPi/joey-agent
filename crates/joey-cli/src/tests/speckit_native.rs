//! Feature 026 dispatch-surface tests (T009/T015/T016 + foundational
//! pieces of T006/T008/T011/T012/T013/T038-seed). Pins the twelve-command
//! surface against contracts/command-surface.md, the dotted↔slash mapping,
//! the unknown-command surface, the pre-flight fallback registry, the
//! failing-script hard error, policy gating, and the bundled floor.

use crate::slash;
use crate::slash_menu;
use crate::speckit_bodies::{self, WorkflowBodySource};
use crate::speckit_slash::{self, PrepPolicy};
use reedline::Completer as _;
use std::fs;
use std::path::PathBuf;

/// Ok/Err matcher that avoids needing `Debug` on `StepPrep`.
fn prep_result(
    step: &'static speckit_slash::StepDef,
    root: &std::path::Path,
    args: &str,
    policy: PrepPolicy,
) -> Result<speckit_slash::StepPrep, String> {
    speckit_slash::prepare_step_opts(step, root, args, None, policy)
}

fn unwrap_prep(
    step: &'static speckit_slash::StepDef,
    root: &std::path::Path,
    args: &str,
    policy: PrepPolicy,
) -> speckit_slash::StepPrep {
    match prep_result(step, root, args, policy) {
        Ok(p) => p,
        Err(e) => panic!("expected Ok, got Err: {e}"),
    }
}

fn unwrap_err_prep(
    step: &'static speckit_slash::StepDef,
    root: &std::path::Path,
    args: &str,
    policy: PrepPolicy,
) -> String {
    match prep_result(step, root, args, policy) {
        Err(e) => e,
        Ok(_) => panic!("expected Err, got Ok"),
    }
}

// ── helpers ─────────────────────────────────────────────────────────────

fn step(name: &str) -> &'static speckit_slash::StepDef {
    speckit_slash::step_by_name(name).unwrap()
}

/// A temp `.specify` repo scaffold with the given scripts and feature state.
struct ScratchRepo {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl ScratchRepo {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::create_dir_all(root.join(".specify")).unwrap();
        Self { _dir: dir, root }
    }

    fn write_script(&self, name: &str, body: &str) {
        let p = self.root.join(".specify/scripts/bash").join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn set_feature(&self, rel: &str) {
        fs::write(
            self.root.join(".specify/feature.json"),
            serde_json::json!({ "feature_directory": rel }).to_string(),
        )
        .unwrap();
    }

    fn feature_json(&self) -> String {
        fs::read_to_string(self.root.join(".specify/feature.json")).unwrap()
    }
}

/// The twelve names per contracts/command-surface.md.
const TWELVE: [&str; 12] = [
    "speckit-constitution",
    "speckit-specify",
    "speckit-clarify",
    "speckit-plan",
    "speckit-checklist",
    "speckit-tasks",
    "speckit-analyze",
    "speckit-implement",
    "speckit-converge",
    "speckit-taskstoissues",
    "speckit-status",
    "speckit-help",
];

// ── T009: command surface pins contracts/command-surface.md ────────────

#[test]
fn command_surface_matches_contract() {
    let expect = |name: &str, script: &str, args: &[&str]| {
        let s = step(name);
        assert_eq!(s.script, Some(script), "{name} script");
        assert_eq!(s.script_args, args, "{name} script_args");
    };
    expect(
        "speckit-constitution",
        "resolve-template.sh",
        &["constitution-template", "--json"],
    );
    assert!(step("speckit-constitution").script_optional);
    expect(
        "speckit-specify",
        "create-new-feature.sh",
        &["--json", "--allow-existing-branch"],
    );
    assert!(step("speckit-specify").script_gets_user_args);
    expect("speckit-clarify", "check-prerequisites.sh", &["--json", "--paths-only"]);
    expect("speckit-plan", "setup-plan.sh", &["--json"]);
    expect(
        "speckit-checklist",
        "check-prerequisites.sh",
        &["--json", "--template", "checklist-template"],
    );
    assert!(step("speckit-checklist").script_optional);
    expect("speckit-tasks", "setup-tasks.sh", &["--json"]);
    for name in [
        "speckit-analyze",
        "speckit-implement",
        "speckit-converge",
        "speckit-taskstoissues",
    ] {
        expect(
            name,
            "check-prerequisites.sh",
            &["--json", "--require-tasks", "--include-tasks"],
        );
    }
    // The 2 auxiliary commands exist in the slash registry (implemented).
    for name in ["speckit-status", "speckit-help"] {
        let def = slash::lookup(name).unwrap_or_else(|| panic!("{name} in REGISTRY"));
        assert!(def.implemented, "{name} implemented");
    }
    // And the lifecycle table is exactly the ten steps in order.
    let names: Vec<&str> = speckit_slash::LIFECYCLE.iter().map(|s| s.name).collect();
    assert_eq!(names.len(), 10);
    assert_eq!(names, &TWELVE[..10]);
}

// ── T015: dotted forms dispatch identically (pure mapping) ─────────────

#[test]
fn dotted_forms_dispatch_identically() {
    for name in TWELVE {
        let bare = name.strip_prefix("speckit-").unwrap();
        let dotted = format!("speckit.{bare}");
        let (canon, args) = speckit_slash::dotted_to_slash(&dotted)
            .unwrap_or_else(|| panic!("{dotted} must map"));
        assert_eq!(canon, name);
        assert_eq!(args, "");
        // With args.
        let dotted_args = format!("speckit.{bare} some args here");
        let (canon, args) = speckit_slash::dotted_to_slash(&dotted_args).unwrap();
        assert_eq!(canon, name);
        assert_eq!(args, "some args here");
    }
    // Unknown dotted name → None (never shadowed).
    assert!(speckit_slash::dotted_to_slash("speckit.nonexistent").is_none());
    // Non-dotted input → None.
    assert!(speckit_slash::dotted_to_slash("/speckit-plan").is_none());
    assert!(speckit_slash::dotted_to_slash("speckit").is_none());
    assert!(speckit_slash::dotted_to_slash("speckit.").is_none());
    // The canonical form resolves through the same registry the slash arm
    // uses (step_by_name or the two auxiliary names).
    for name in TWELVE {
        let known = speckit_slash::step_by_name(name).is_some()
            || name == "speckit-status"
            || name == "speckit-help";
        assert!(known, "{name} must be dispatchable");
    }
}

// ── T014: unknown command lists + suggests ─────────────────────────────

#[test]
fn unknown_command_lists_and_suggests() {
    let msg = speckit_slash::unknown_command_error("speckit-planX");
    assert!(msg.contains("unknown spec-kit command: speckit-planX"), "{msg}");
    for name in TWELVE {
        assert!(msg.contains(&format!("/{name}")), "listing {name}: {msg}");
    }
    assert!(msg.contains("Closest: /speckit-plan"), "{msg}");
    // Slash-form input also works (leading slash stripped for matching).
    let msg = speckit_slash::unknown_command_error("/speckit-xyzzy");
    assert!(msg.contains("unknown spec-kit command: /speckit-xyzzy"));
    // Far-away input gets no Closest line.
    let msg = speckit_slash::unknown_command_error("qqqqqqqqqqqq");
    assert!(!msg.contains("Closest:"), "{msg}");
}

// ── T009: failing script → hard error naming variants ──────────────────

#[test]
fn failing_script_hard_error_names_variants() {
    let repo = ScratchRepo::new();
    repo.write_script("check-prerequisites.sh", "#!/bin/bash\necho boom >&2\nexit 1\n");
    let err = unwrap_err_prep(step("speckit-implement"), &repo.root, "", PrepPolicy::Native);
    assert!(err.contains("check-prerequisites.sh"), "{err}");
    assert!(err.contains("exit 1"), "{err}");
    // Static variant naming even when the variant files are absent.
    assert!(err.contains("powershell/check-prerequisites.ps1"), "{err}");
    assert!(err.contains("python/check_prerequisites.py"), "{err}");
    assert!(err.contains("boom"), "{err}");
}

// ── T008: missing script → internal fallback ───────────────────────────

#[test]
fn missing_script_uses_internal_fallback() {
    // Repo with feature state but NO scripts dir.
    let repo = ScratchRepo::new();
    fs::create_dir_all(repo.root.join("specs/001-demo")).unwrap();
    fs::write(repo.root.join("specs/001-demo/spec.md"), "# spec\n").unwrap();
    repo.set_feature("specs/001-demo");

    let prep = unwrap_prep(step("speckit-implement"), &repo.root, "", PrepPolicy::Native);
    assert!(
        prep.preflight.contains("pre-flight fallback: `check-prerequisites.sh`"),
        "{}",
        prep.preflight
    );
    assert!(prep.preflight.contains("FEATURE_DIR\":\"specs/001-demo"), "{}", prep.preflight);

    // Internal create-new-feature: specify allocates specs/002-… and writes
    // feature.json.
    let prep = unwrap_prep(step("speckit-specify"), &repo.root, "demo feature two", PrepPolicy::Native);
    assert!(prep.preflight.contains("pre-flight fallback: `create-new-feature.sh`"));
    let created = repo.root.join("specs/002-demo-feature-two");
    assert!(created.is_dir(), "feature dir created on disk");
    assert!(
        repo.feature_json().contains("specs/002-demo-feature-two"),
        "feature.json updated: {}",
        repo.feature_json()
    );

    // Empty description still errors (script_gets_user_args gate runs first).
    let err = unwrap_err_prep(step("speckit-specify"), &repo.root, "   ", PrepPolicy::Native);
    assert!(err.contains("requires a feature description"), "{err}");

    // setup-plan/setup-tasks fallback needs an active feature.
    let prep = unwrap_prep(step("speckit-plan"), &repo.root, "", PrepPolicy::Native);
    assert!(prep.preflight.contains("pre-flight fallback: `setup-plan.sh`"));
    assert!(prep.preflight.contains("plan-template.md"));

    // Without a feature.json, setup-plan hard-errors with guidance.
    let bare = ScratchRepo::new();
    let err = unwrap_err_prep(step("speckit-plan"), &bare.root, "", PrepPolicy::Native);
    assert!(err.contains("no active feature"), "{err}");
    assert!(err.contains("/speckit-specify"), "{err}");

    // Platform-variant note: a powershell sibling of the missing bash
    // script is named in the warning.
    let psw = ScratchRepo::new();
    fs::create_dir_all(psw.root.join("specs/001-demo")).unwrap();
    psw.set_feature("specs/001-demo");
    let ps1 = psw.root.join(".specify/scripts/powershell/check-prerequisites.ps1");
    fs::create_dir_all(ps1.parent().unwrap()).unwrap();
    fs::write(&ps1, "# ps\n").unwrap();
    let prep = unwrap_prep(step("speckit-implement"), &psw.root, "", PrepPolicy::Native);
    assert!(
        prep.preflight
            .contains("(platform variant present: powershell/check-prerequisites.ps1)"),
        "{}",
        prep.preflight
    );

    // --include-tasks appends tasks.md content.
    let tasks_repo = ScratchRepo::new();
    fs::create_dir_all(tasks_repo.root.join("specs/001-demo")).unwrap();
    fs::write(
        tasks_repo.root.join("specs/001-demo/tasks.md"),
        "- [ ] T001 do the thing\n",
    )
    .unwrap();
    tasks_repo.set_feature("specs/001-demo");
    let prep = unwrap_prep(step("speckit-implement"), &tasks_repo.root, "", PrepPolicy::Native);
    assert!(prep.preflight.contains("- [ ] T001 do the thing"), "{}", prep.preflight);
}

// ── T038 seed / FR-013: Legacy restores pre-feature behavior ───────────

#[test]
fn legacy_policy_restores_pre_feature() {
    let repo = ScratchRepo::new();
    // A project-level override body (only the Native chain reads it).
    let skill = repo.root.join(".github/skills/speckit-plan/SKILL.md");
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    fs::write(&skill, "---\ndescription: custom\n---\nMARKER-custom-override-body-9f2b").unwrap();
    repo.write_script("setup-plan.sh", "#!/bin/bash\necho '{\"ok\":true}'\n");

    let native = unwrap_prep(step("speckit-plan"), &repo.root, "", PrepPolicy::Native);
    assert!(native.prompt.contains("MARKER-custom-override-body-9f2b"), "native prefers override");

    // Legacy: home-skills load only. Under a relocated JOEY_HOME the skill
    // is absent → clean "not installed" error; under a dev machine with the
    // real skills installed the override marker must NOT appear either way.
    match prep_result(step("speckit-plan"), &repo.root, "", PrepPolicy::Legacy) {
        Ok(prep) => {
            assert!(
                !prep.prompt.contains("MARKER-custom-override-body-9f2b"),
                "legacy must not see project overrides"
            );
        }
        Err(e) => assert!(e.contains("not installed"), "legacy error must be the skill lookup: {e}"),
    }
}

// ── T015 / FR-013: config gating ───────────────────────────────────────

#[test]
fn disabled_config_gates_new_paths() {
    // Default → enabled.
    assert!(speckit_slash::speckit_enabled(&joey_core::Config::defaults()));
    // speckit.enabled=false → disabled (via a temp config file).
    let tmp = tempfile::NamedTempFile::new().unwrap();
    fs::write(tmp.path(), "speckit:\n  enabled: false\n").unwrap();
    let cfg = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
    assert!(!speckit_slash::speckit_enabled(&cfg));
}

// ── T016: bundled floor at the dispatch level ──────────────────────────

#[test]
fn bundled_floor_at_dispatch() {
    let repo = ScratchRepo::new(); // no overrides, no skills
    // The internal check-prerequisites equivalent needs an active feature.
    fs::create_dir_all(repo.root.join("specs/001-demo")).unwrap();
    repo.set_feature("specs/001-demo");
    // Isolated joey home (the dev machine has real user skills installed,
    // which would legitimately win over the floor — use the crate-internal
    // resolver entry with an empty home so the floor is observable).
    let empty_home = tempfile::tempdir().unwrap();
    let wf = speckit_bodies::resolve_with_home(Some(&repo.root), "implement", empty_home.path());
    assert_eq!(wf.source, WorkflowBodySource::Bundled);
    let prep = unwrap_prep(step("speckit-implement"), &repo.root, "", PrepPolicy::Native);
    assert!(prep.prompt.contains("## Outline"), "bundled implement body present");
}

// ── T013: dotted completion ────────────────────────────────────────────

#[test]
fn dotted_completion_offers_both_forms() {
    // Typed dotted fragment → dotted candidates.
    let mut c = slash_menu::SmartCompleter::new(PathBuf::from("."));
    let got = c.complete("speckit.pl", 10);
    let values: Vec<&str> = got.iter().map(|s| s.value.as_str()).collect();
    assert!(values.contains(&"speckit.plan"), "got {values:?}");
    assert!(values.iter().all(|v| v.starts_with("speckit.pl")), "got {values:?}");

    // The factored candidate list (pub(crate) helper) covers all twelve.
    let all = slash_menu::dotted_suggestions("speckit.", reedline::Span::new(0, 8));
    assert_eq!(all.len(), 12, "all twelve dotted forms");
    let values: Vec<&str> = all.iter().map(|s| s.value.as_str()).collect();
    assert!(values.contains(&"speckit.status"));
    assert!(values.contains(&"speckit.help"));

    // Slash branch untouched: /speckit-pl still resolves slash names.
    let got = c.complete("/speckit-pl", 10);
    assert!(got.iter().any(|s| s.value == "/speckit-plan"));

    // Plain non-slash text still yields nothing.
    assert!(c.complete("hello world", 11).is_empty());
}

// ── T006 foundational (dispatch-level re-pins; bodies/hooks have their
// own inline suites in speckit_bodies.rs / speckit_hooks.rs) ────────────

#[test]
fn dispatch_surface_foundational_pins() {
    // The ten lifecycle skills all resolve through the bundled floor
    // (isolated home — the dev machine has real user skills installed).
    let empty_home = tempfile::tempdir().unwrap();
    for name in speckit_bodies::COMMAND_NAMES {
        let wf = speckit_bodies::resolve_with_home(None, name, empty_home.path());
        assert_eq!(wf.source, WorkflowBodySource::Bundled, "{name} floor");
        assert!(!wf.body().is_empty(), "{name} body non-empty");
    }
    // unknown_command_error covers exactly the twelve (10 + 2).
    let msg = speckit_slash::unknown_command_error("speckit-nope");
    let listed = TWELVE.iter().filter(|n| msg.contains(&format!("/{n}"))).count();
    assert_eq!(listed, 12);
}

// ── T020: hook execution surface (contracts/hooks.md) ──────────────────

#[test]
fn hook_points_cover_all_twenty() {
    assert_eq!(crate::speckit_hooks::HOOK_POINTS.len(), 20);
    for name in speckit_bodies::COMMAND_NAMES {
        assert!(
            crate::speckit_hooks::HOOK_POINTS.contains(&format!("before_{name}").as_str()),
            "missing before_{name}"
        );
        assert!(
            crate::speckit_hooks::HOOK_POINTS.contains(&format!("after_{name}").as_str()),
            "missing after_{name}"
        );
    }
}

/// The (b)/(g)/(h) fixture repo: four `before_implement` hook entries
/// (optional, mandatory, disabled, conditioned) + a working pre-flight
/// script + active feature so both policies prepare.
fn hook_fixture_repo() -> ScratchRepo {
    let repo = ScratchRepo::new();
    fs::create_dir_all(repo.root.join(".specify")).unwrap();
    fs::write(
        repo.root.join(".specify/extensions.yml"),
        "hooks:\n  before_implement:\n    - extension: opt-ext\n      command: speckit.opt.hook\n      description: An optional hook\n      prompt: Do the optional thing\n      optional: true\n    - extension: mand-ext\n      command: speckit.mand.hook\n      description: A mandatory hook\n      prompt: Do the mandatory thing\n      optional: false\n    - extension: disabled-ext\n      command: speckit.disabled.hook\n      enabled: false\n    - extension: cond-ext\n      command: speckit.cond.hook\n      condition: env==ci\n",
    )
    .unwrap();
    repo.write_script(
        "check-prerequisites.sh",
        "#!/bin/bash\necho '{\"FEATURE_DIR\":\"specs/001-demo\"}'\n",
    );
    fs::create_dir_all(repo.root.join("specs/001-demo")).unwrap();
    repo.set_feature("specs/001-demo");
    repo
}

#[test]
fn hook_notes_shapes() {
    let repo = hook_fixture_repo();
    let notes = speckit_slash::gather_hook_notes(&repo.root, "before_implement");
    // Optional block.
    assert!(notes.contains("Optional Pre-Hook"), "{notes}");
    assert!(notes.contains("**Optional Pre-Hook**: opt-ext"), "{notes}");
    assert!(notes.contains("Description: An optional hook"), "{notes}");
    assert!(notes.contains("Prompt: Do the optional thing"), "{notes}");
    // Mandatory block.
    assert!(notes.contains("Automatic Pre-Hook"), "{notes}");
    assert!(notes.contains("EXECUTE_COMMAND:"), "{notes}");
    assert!(notes.contains("EXECUTE_COMMAND: speckit.mand.hook"), "{notes}");
    // Slash-normalized command names (dots → hyphens, no leading slash).
    assert!(notes.contains("`/speckit-opt-hook`"), "{notes}");
    assert!(notes.contains("`/speckit-mand-hook`"), "{notes}");
    // Disabled entry excluded entirely.
    assert!(!notes.contains("disabled-ext"), "{notes}");
    assert!(!notes.contains("speckit-disabled-hook"), "{notes}");
    // Conditioned entry surfaced as a skip line, unevaluated.
    assert!(
        notes.contains("- Hook 'cond-ext' skipped: condition 'env==ci' is left to the extension runtime"),
        "{notes}"
    );
    // No other hook point leaks in.
    assert!(speckit_slash::gather_hook_notes(&repo.root, "after_implement").is_empty());
    assert!(speckit_slash::gather_hook_notes(&repo.root, "before_plan").is_empty());
}

#[test]
fn invalid_yaml_silent() {
    let repo = ScratchRepo::new();
    fs::write(repo.root.join(".specify/extensions.yml"), ":~bad").unwrap();
    assert!(speckit_slash::gather_hook_notes(&repo.root, "before_implement").is_empty());
    assert!(crate::speckit_hooks::hooks_for(&repo.root, "before_implement").is_empty());
    assert!(crate::speckit_hooks::discover(&repo.root).is_empty());
}

#[test]
fn disabled_config_skips_discovery() {
    use joey_core::Config;
    assert!(crate::speckit_hooks::config_allows(&Config::defaults()));
    let tmp = tempfile::NamedTempFile::new().unwrap();
    fs::write(tmp.path(), "speckit:\n  hooks: false\n").unwrap();
    let cfg = Config::load_from(tmp.path().to_path_buf()).unwrap();
    assert!(!crate::speckit_hooks::config_allows(&cfg));
    let tmp = tempfile::NamedTempFile::new().unwrap();
    fs::write(tmp.path(), "speckit:\n  enabled: false\n").unwrap();
    let cfg = Config::load_from(tmp.path().to_path_buf()).unwrap();
    assert!(!crate::speckit_hooks::config_allows(&cfg));
}

// ── T022: handoff resolution (US5) ──────────────────────────────────────

#[test]
fn handoff_definitions_match_upstream() {
    let repo = ScratchRepo::new(); // no overrides: floor frontmatter decides
    // (target, send) per the VENDORED bodies (speckit_bodies/*.md):
    // specify lists plan (offer) FIRST, then clarify (send) → primary=plan.
    let expect: &[(&str, &str, bool)] = &[
        ("speckit-specify", "speckit-plan", false),
        ("speckit-clarify", "speckit-plan", false),
        ("speckit-plan", "speckit-tasks", true),
        ("speckit-tasks", "speckit-analyze", true),
        ("speckit-constitution", "speckit-specify", false),
    ];
    for (step_name, want_target, want_send) in expect {
        let step = step(step_name);
        let (label, prompt, send, target) = speckit_slash::primary_handoff(step, &repo.root)
            .unwrap_or_else(|| panic!("{step_name} must have a primary handoff"));
        assert_eq!(target, *want_target, "{step_name} target");
        assert_eq!(send, *want_send, "{step_name} send flag");
        assert!(!label.is_empty(), "{step_name} label");
        assert!(!prompt.is_empty(), "{step_name} prompt");
        // handoff_offer is the (label, prompt, send) projection.
        assert_eq!(speckit_slash::handoff_offer(step, &repo.root), Some((label.clone(), prompt.clone(), send)));

        // Cross-check against the BUNDLED frontmatter read via the API at
        // test time (isolated home → Bundled source).
        let empty_home = tempfile::tempdir().unwrap();
        let bare = step.skill.strip_prefix("speckit-").unwrap();
        let wf = speckit_bodies::resolve_with_home(None, bare, empty_home.path());
        let primary = wf
            .frontmatter
            .handoffs
            .first()
            .unwrap_or_else(|| panic!("{bare} bundled primary handoff"));
        assert_eq!(label, primary.label, "{step_name} label vs bundled");
        assert_eq!(prompt, primary.prompt, "{step_name} prompt vs bundled");
        assert_eq!(send, primary.send, "{step_name} send vs bundled");
    }
}

#[test]
fn handoff_offer_absent_for_others() {
    let repo = ScratchRepo::new();
    for name in [
        "speckit-checklist",
        "speckit-analyze",
        "speckit-implement",
        "speckit-converge",
        "speckit-taskstoissues",
    ] {
        assert!(
            speckit_slash::handoff_offer(step(name), &repo.root).is_none(),
            "{name} declares no handoff upstream"
        );
    }
}

// ── T018 wiring: policy-dependent hook notes in StepPrep ───────────────

#[test]
fn prepare_step_opts_legacy_has_no_hook_notes() {
    let repo = hook_fixture_repo();
    // Legacy keeps the exact pre-feature prompt: no RESOLVED hook state.
    // (The workflow BODY itself may contain generic "Extension Hooks"
    // instructions — upstream text — so assert on the fixture's unique
    // markers instead. Skill lookup may fail under a relocated JOEY_HOME —
    // then the error must be the skill-lookup one, never a hooks leak.)
    match prep_result(step("speckit-implement"), &repo.root, "", PrepPolicy::Legacy) {
        Ok(prep) => {
            assert!(prep.hooks_note.is_empty(), "{}", prep.hooks_note);
            assert!(!prep.preflight.contains("opt-ext"), "{}", prep.preflight);
            assert!(!prep.preflight.contains("mand-ext"), "{}", prep.preflight);
            assert!(!prep.preflight.contains("speckit.mand.hook"), "{}", prep.preflight);
            assert!(!prep.prompt.contains("opt-ext"), "{}", prep.prompt);
            assert!(!prep.prompt.contains("mand-ext"), "{}", prep.prompt);
        }
        Err(e) => assert!(e.contains("not installed"), "{e}"),
    }
}

#[test]
fn prepare_step_opts_native_includes_hook_notes() {
    let repo = hook_fixture_repo();
    let prep = unwrap_prep(step("speckit-implement"), &repo.root, "", PrepPolicy::Native);
    // The notes are surfaced in hooks_note AND prepended to preflight
    // (hooks section first), which flows into the composed prompt.
    assert!(prep.hooks_note.contains("Automatic Pre-Hook"), "{}", prep.hooks_note);
    assert!(prep.hooks_note.contains("EXECUTE_COMMAND: speckit.mand.hook"), "{}", prep.hooks_note);
    assert!(prep.preflight.contains("Automatic Pre-Hook"), "{}", prep.preflight);
    // Hooks come BEFORE the pre-flight script section.
    let hooks_at = prep.preflight.find("## Extension Hooks").unwrap();
    let preflight_at = prep.preflight.find("## Pre-flight").unwrap();
    assert!(hooks_at < preflight_at, "hooks section first: {}", prep.preflight);
    assert!(prep.prompt.contains("Automatic Pre-Hook"), "prompt carries the hook state");
}

// ── T037: SC-003 lifecycle dispatch timing budget ──────────────────────

/// A temp spec-kit fixture with an active feature and a stub
/// check-prerequisites.sh echoing valid JSON.
fn timing_fixture_repo() -> ScratchRepo {
    let repo = ScratchRepo::new();
    fs::create_dir_all(repo.root.join("specs/001-demo")).unwrap();
    fs::write(repo.root.join("specs/001-demo/spec.md"), "# spec\n").unwrap();
    repo.set_feature("specs/001-demo");
    repo.write_script(
        "check-prerequisites.sh",
        "#!/bin/bash\necho '{\"FEATURE_DIR\":\"specs/001-demo\"}'\n",
    );
    repo
}

#[test]
fn dispatch_prep_under_two_seconds() {
    let repo = timing_fixture_repo();
    let start = std::time::Instant::now();
    let prep = unwrap_prep(step("speckit-implement"), &repo.root, "", PrepPolicy::Native);
    let elapsed = start.elapsed();
    assert!(!prep.prompt.is_empty());
    eprintln!(
        "T037 dispatch_prep_under_two_seconds: prepare_step_opts(implement, Native) took {} ms",
        elapsed.as_millis()
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "SC-003: lifecycle dispatch prep must stay under 2s, took {} ms",
        elapsed.as_millis()
    );
}

// ── T038: pre-feature behavior regression (constitution VII / SC-006a) ──

/// Build a Config from raw YAML via a temp file (same pattern as
/// disabled_config_gates_new_paths above).
fn config_with(yaml: &str) -> joey_core::Config {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    fs::write(tmp.path(), yaml).unwrap();
    joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap()
}

/// RAII pin of the process-global joey home (serialized on joey-core's
/// TEST_HOME_OVERRIDE_LOCK — the same workspace-wide lock other home-
/// override tests take) so the Legacy home-skills load resolves against a
/// temp home we control.
struct PinnedHome {
    _lock: std::sync::MutexGuard<'static, ()>,
    _guard: joey_core::constants::HomeOverrideGuard,
    _dir: tempfile::TempDir,
}

fn pin_home_with_implement_skill() -> PinnedHome {
    let _lock = joey_core::constants::TEST_HOME_OVERRIDE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join("skills/speckit-implement/SKILL.md");
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    fs::write(
        &skill,
        "---\ndescription: legacy implement\n---\nLEGACYBODY7",
    )
    .unwrap();
    let _guard = joey_core::constants::HomeOverrideGuard::new(dir.path().to_path_buf());
    PinnedHome { _lock, _guard, _dir: dir }
}

fn pin_empty_home() -> PinnedHome {
    let _lock = joey_core::constants::TEST_HOME_OVERRIDE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let _guard = joey_core::constants::HomeOverrideGuard::new(dir.path().to_path_buf());
    PinnedHome { _lock, _guard, _dir: dir }
}

#[test]
fn pre_feature_behavior_regression() {
    let repo = ScratchRepo::new();
    fs::create_dir_all(repo.root.join("specs/001-demo")).unwrap();
    repo.set_feature("specs/001-demo");
    repo.write_script(
        "check-prerequisites.sh",
        "#!/bin/bash\necho '{\"FEATURE_DIR\":\"specs/001-demo\"}'\n",
    );

    // (a) Legacy with a home skill installed: prompt carries the PRIOR
    // body source (LEGACYBODY7), never the bundled body's substituted
    // placeholders section, never a tools section.
    {
        let _home = pin_home_with_implement_skill();
        let prep = unwrap_prep(step("speckit-implement"), &repo.root, "", PrepPolicy::Legacy);
        assert!(prep.prompt.contains("LEGACYBODY7"), "legacy loads the home skill body");
        assert!(!prep.prompt.contains("## Outline"), "no bundled implement body: {}", prep.prompt);
        assert!(!prep.prompt.contains("{SCRIPT}"), "no substituted placeholders section");
        assert!(!prep.prompt.contains("## Tools referenced"), "legacy has no tools section");
    }

    // (b) Same repo, Native with no project overrides (isolated home →
    // bundled floor): the bundled implement workflow is used, not the
    // legacy skill body.
    {
        let _home = pin_empty_home();
        let prep = unwrap_prep(step("speckit-implement"), &repo.root, "", PrepPolicy::Native);
        assert!(prep.prompt.contains("## Outline"), "bundled implement body present");
        assert!(!prep.prompt.contains("LEGACYBODY7"), "native must not see the legacy skill");
        // The bundled implement frontmatter declares no tools → no section.
        assert!(!prep.prompt.contains("## Tools referenced"), "implement declares no tools");
    }

    // (c) All 12 names still resolve with UNCHANGED descriptions (pins
    // today's strings verbatim: ten lifecycle + status + help).
    let lifecycle_descriptions: &[&str] = &[
        "Create or update the project constitution from interactive Q&A",
        "Create or update the feature specification from a description",
        "Identify underspecified areas in the current feature spec",
        "Execute the implementation planning workflow (design artifacts)",
        "Generate a custom checklist for the current feature",
        "Generate an actionable dependency-ordered tasks.md",
        "Cross-artifact consistency and coverage analysis",
        "Execute the implementation plan task by task",
        "Assess implementation against the spec and list gaps",
        "Convert tasks into actionable GitHub issues",
    ];
    for (def, want) in speckit_slash::LIFECYCLE.iter().zip(lifecycle_descriptions) {
        assert_eq!(def.description, *want, "{} description", def.name);
    }
    for (name, want) in [
        ("speckit-status", "Show the current spec-kit feature and artifact readiness"),
        ("speckit-help", "Show the spec-kit workflow overview"),
    ] {
        let def = slash::lookup(name).unwrap_or_else(|| panic!("{name} in REGISTRY"));
        assert_eq!(def.description, want, "{name} REGISTRY description");
        assert!(def.implemented, "{name} implemented");
    }
    // And the registry's lifecycle descriptions agree with the table.
    for def in speckit_slash::LIFECYCLE {
        let reg = slash::lookup(def.name).unwrap_or_else(|| panic!("{} in REGISTRY", def.name));
        assert_eq!(reg.description, def.description, "{} registry/table agree", def.name);
    }
}

// ── T036: tools frontmatter honored at dispatch (FR-004a) ──────────────

#[test]
fn native_tools_frontmatter_surfaced() {
    // taskstoissues is the one bundled body declaring tools (2 entries).
    let repo = ScratchRepo::new();
    fs::create_dir_all(repo.root.join("specs/001-demo")).unwrap();
    fs::write(repo.root.join("specs/001-demo/tasks.md"), "- [ ] T001 do it\n").unwrap();
    repo.set_feature("specs/001-demo");
    repo.write_script(
        "check-prerequisites.sh",
        "#!/bin/bash\necho '{\"FEATURE_DIR\":\"specs/001-demo\"}'\n",
    );
    // Isolated home → bundled floor decides.
    let _home = pin_empty_home();
    let native = unwrap_prep(step("speckit-taskstoissues"), &repo.root, "", PrepPolicy::Native);
    assert!(
        native.prompt.contains("## Tools referenced by this workflow"),
        "tools section present: {}",
        native.prompt
    );
    assert!(
        native.prompt.contains("- github/github-mcp-server/list_issues"),
        "tool line: {}",
        native.prompt
    );
    assert!(
        native.prompt.contains(
            "These tools are referenced by the upstream workflow; ensure the corresponding MCP servers (e.g. github-mcp-server) are configured before relying on them."
        ),
        "tools guidance line"
    );
    // Legacy: pre-feature exact — no tools section.
    match prep_result(step("speckit-taskstoissues"), &repo.root, "", PrepPolicy::Legacy) {
        Ok(prep) => {
            assert!(!prep.prompt.contains("## Tools referenced"), "legacy has no tools section");
        }
        Err(e) => assert!(e.contains("not installed"), "legacy error must be the skill lookup: {e}"),
    }
}

// ── T023/T035 gates ────────────────────────────────────────────────────

#[test]
fn lifecycle_gates() {
    use crate::speckit_lifecycle::lifecycle_context_allowed;
    // Defaults: allowed.
    assert!(lifecycle_context_allowed(&joey_core::Config::defaults()));
    // speckit.lifecycle_context=false → blocked.
    assert!(!lifecycle_context_allowed(&config_with("speckit:\n  lifecycle_context: false\n")));
    // speckit.enabled=false → blocked (master switch).
    assert!(!lifecycle_context_allowed(&config_with("speckit:\n  enabled: false\n")));

    // Non-spec-kit tempdir → no session block.
    let plain = tempfile::tempdir().unwrap();
    assert!(lifecycle_context_allowed(&joey_core::Config::defaults()));
    assert!(
        lifecycle_session_block(plain.path()).is_none(),
        "no block outside a spec-kit repo"
    );

    // Fixture with an active feature → Some with the contract header.
    let repo = timing_fixture_repo();
    let block = lifecycle_session_block(&repo.root)
        .expect("block inside a spec-kit repo with lifecycle context on");
    assert!(block.contains("## Spec-Kit Lifecycle Context"), "{block}");
    assert!(block.contains("Feature: specs/001-demo"), "{block}");
    // Gated off by config even inside the repo.
    assert!(
        lifecycle_session_block_cfg(&repo.root, &config_with("speckit:\n  enabled: false\n"))
            .is_none()
    );
    assert!(lifecycle_session_block_cfg(
        &repo.root,
        &config_with("speckit:\n  lifecycle_context: false\n")
    )
    .is_none());

    // feature_scope_files: empty when speckit.enabled=false …
    assert!(lifecycle_feature_scope_files(
        &repo.root,
        &config_with("speckit:\n  enabled: false\n")
    )
    .is_empty());
    // … and for a repo without an active feature …
    assert!(lifecycle_feature_scope_files(plain.path(), &joey_core::Config::defaults()).is_empty());
    // … non-empty for a fixture with tasks.md target files.
    fs::write(
        repo.root.join("specs/001-demo/tasks.md"),
        "- [ ] T001 [P] Write `src/a.rs`\n- [ ] T002 Also `src/b.rs` and `src/a.rs`\n",
    )
    .unwrap();
    let files = lifecycle_feature_scope_files(&repo.root, &joey_core::Config::defaults());
    assert!(!files.is_empty(), "scope files for active feature with tasks");
    assert_eq!(files, vec!["src/a.rs".to_string(), "src/b.rs".to_string()], "dedup order preserved");
}

/// Local indirection so lifecycle_gates reads clearly.
fn lifecycle_session_block(cwd: &std::path::Path) -> Option<String> {
    crate::speckit_lifecycle::session_context_block(cwd, &joey_core::Config::defaults())
}

fn lifecycle_session_block_cfg(
    cwd: &std::path::Path,
    config: &joey_core::Config,
) -> Option<String> {
    crate::speckit_lifecycle::session_context_block(cwd, config)
}

fn lifecycle_feature_scope_files(cwd: &std::path::Path, config: &joey_core::Config) -> Vec<String> {
    crate::speckit_lifecycle::feature_scope_files(cwd, config)
}

#[test]
fn status_includes_lifecycle_block() {
    let repo = timing_fixture_repo();
    let s = speckit_slash::SpeckitStatus {
        root: repo.root.clone(),
        branch: "001-demo".into(),
        feature_dir: "specs/001-demo".into(),
        has_spec: true,
        has_plan: true,
        has_tasks: true,
        raw: String::new(),
    };
    // Enabled (default) config → the lifecycle context block is appended.
    let enabled = joey_core::Config::defaults();
    let text = speckit_slash::render_status_with_config(&s, &enabled);
    assert!(text.contains("Step:"), "{text}");
    assert!(text.contains("## Spec-Kit Lifecycle Context"), "{text}");
    // speckit.enabled=false → EXACTLY the ungated render (parity).
    let disabled = config_with("speckit:\n  enabled: false\n");
    assert_eq!(
        speckit_slash::render_status_with_config(&s, &disabled),
        speckit_slash::render_status(&s),
        "disabled config renders byte-identically to the ungated path"
    );
}

// ── status() fallback + structured JSON (fix 2) ────────────────────────

/// A scratch spec-kit repo WITHOUT .specify/scripts: status() must fall
/// back to disk derivation (derive_state + .git/HEAD) and stay Ok.
#[test]
fn status_derives_from_disk_when_script_absent() {
    let repo = ScratchRepo::new();
    fs::create_dir_all(repo.root.join("specs/001-demo")).unwrap();
    fs::write(repo.root.join("specs/001-demo/spec.md"), "# spec\n").unwrap();
    fs::write(repo.root.join("specs/001-demo/tasks.md"), "- [ ] T001 do it\n").unwrap();
    repo.set_feature("specs/001-demo");
    // A .git/HEAD so the branch resolves to a name (not required, but pins
    // the fallback branch source).
    fs::create_dir_all(repo.root.join(".git")).unwrap();
    fs::write(repo.root.join(".git/HEAD"), "ref: refs/heads/001-demo\n").unwrap();
    // NO scripts dir at all.
    let s = speckit_slash::status(&repo.root).expect("status Ok without scripts");
    assert_eq!(s.feature_dir, "specs/001-demo");
    assert!(s.has_spec, "spec.md detected via derive_state");
    assert!(!s.has_plan, "plan.md absent");
    assert!(s.has_tasks, "tasks.md detected");
    assert_eq!(s.branch, "001-demo", "branch from .git/HEAD fallback");
}

/// Structured lookup: a VALUE that embeds a `"FEATURE_DIR":"…"`-shaped
/// payload can no longer fool field extraction.
#[test]
fn status_json_lookup_is_structured() {
    let repo = ScratchRepo::new();
    fs::create_dir_all(repo.root.join("specs/001-demo")).unwrap();
    repo.set_feature("specs/001-demo");
    repo.write_script(
        "check-prerequisites.sh",
        "#!/bin/bash\necho 'note: run with \"FEATURE_DIR\":\"specs/decoy\" for tests'\nprintf '{\"FEATURE_DIR\":\"specs/001-demo\",\"BRANCH\":\"main\"}'\n",
    );
    let s = speckit_slash::status(&repo.root).expect("status Ok");
    assert_eq!(s.feature_dir, "specs/001-demo", "structured field wins: {}", s.raw);
    assert_eq!(s.branch, "main");
}

// ── render_help lists all 12 commands (fix 3) ──────────────────────────

#[test]
fn render_help_lists_all_twelve_commands() {
    let help = speckit_slash::render_help();
    for name in TWELVE {
        assert!(help.contains(&format!("/{name}")), "listing /{name}: {help}");
    }
    // The /speckit-help line sits after the status line.
    let status_pos = help.find("/speckit-status").expect("status line");
    let help_pos = help.find("/speckit-help").expect("help line");
    assert!(status_pos < help_pos, "help line after status line");
}

// ── T025 task-graph embedding (fix 5) ──────────────────────────────────

/// implement-step prep on a fixture tasks.md with two same-phase tasks
/// sharing a target file: the embedded graph JSON contains BOTH ids and
/// the sequencing edge (document-earlier → document-later).
#[test]
fn native_prep_embeds_task_graph_with_collision_sequencing() {
    let repo = ScratchRepo::new();
    fs::create_dir_all(repo.root.join("specs/001-demo")).unwrap();
    fs::write(repo.root.join("specs/001-demo/spec.md"), "# spec\n").unwrap();
    fs::write(
        repo.root.join("specs/001-demo/tasks.md"),
        "# Tasks\n\n## Phase 1: Core\n\n- [ ] T001 [P] Write `src/a.rs`\n- [ ] T002 [P] Also write `src/a.rs`\n",
    )
    .unwrap();
    repo.set_feature("specs/001-demo");
    let prep = unwrap_prep(step("speckit-implement"), &repo.root, "", PrepPolicy::Native);
    assert!(
        prep.prompt.contains("ORCHESTRATION TASK GRAPH (derived from tasks.md"),
        "graph section present"
    );
    // Both ids in the embedded JSON.
    assert!(prep.prompt.contains("\"t001\""), "t001 in graph: {}", prep.prompt);
    assert!(prep.prompt.contains("\"t002\""), "t002 in graph: {}", prep.prompt);
    // The sequencing edge: t002 depends on t001. Parse the JSON array to
    // assert structurally (substring order is not guaranteed by pretty-print).
    let start = prep.prompt.find("ORCHESTRATION TASK GRAPH").unwrap();
    let json_start = prep.prompt[start..].find('[').unwrap() + start;
    let json_end = prep.prompt[start..].rfind(']').unwrap() + start;
    let nodes: serde_json::Value =
        serde_json::from_str(&prep.prompt[json_start..=json_end]).expect("embedded JSON parses");
    let t002 = nodes
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == "t002")
        .expect("t002 node");
    assert!(
        t002["dependencies"].as_array().unwrap().iter().any(|d| d == "t001"),
        "t002 depends on t001 (sequencing edge): {t002}"
    );
    // Collision note names both raw ids and the file.
    assert!(
        prep.prompt.contains("T001") && prep.prompt.contains("T002"),
        "collision note raw ids: {}",
        prep.prompt
    );
    assert!(prep.prompt.contains("src/a.rs"), "collision note file");
    // Native-only: Legacy never embeds the graph (feature parity).
    match prep_result(step("speckit-implement"), &repo.root, "", PrepPolicy::Legacy) {
        Ok(p) => assert!(!p.prompt.contains("ORCHESTRATION TASK GRAPH"), "legacy has no graph"),
        Err(_) => {} // legacy may fail on the missing skill; fine
    }
}

// ── handoff_prompt helper (fix 6) ──────────────────────────────────────

#[test]
fn handoff_prompt_labels_and_truncates() {
    let out = crate::repl::handoff_prompt_for_test("NEXT STEP", "short prior");
    assert!(out.contains("Prior step output:"), "{out}");
    assert!(out.contains("short prior"), "{out}");
    assert!(out.contains("NEXT STEP"), "{out}");

    // Long prior output → truncated to ≤2000 prior chars, keeping the TAIL.
    let long: String = "x".repeat(5000) + "TAIL-MARKER";
    let out = crate::repl::handoff_prompt_for_test("NEXT", &long);
    assert!(out.contains("Prior step output:"), "{out}");
    assert!(out.contains("TAIL-MARKER"), "tail kept");
    let prior_len = out
        .strip_prefix("Prior step output:\n")
        .and_then(|rest| rest.find("\n\n---\n\n"))
        .map(|i| out["Prior step output:\n".len().."Prior step output:\n".len() + i].chars().count())
        .expect("section layout");
    assert!(prior_len <= 2000, "prior section ≤2000 chars, got {prior_len}");
    assert!(prior_len >= 1900, "close to 2000 (tail slice), got {prior_len}");
}


