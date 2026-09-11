//! Feature 028 (context economy) parity suite — FR-013/SC-004.
//! Default-on activation + when-disabled parity for every mechanism switch
//! on the public surfaces. Request/history byte-parity for the agent-internal
//! mechanisms is covered in-crate (agent.rs tests: state_block_disabled_
//! request_byte_identical, hygiene_disabled_history_byte_identical,
//! boundary_disabled_exit_paths_untouched); this file pins the pub contracts.

use joey_agent_core::guidance::CONTEXT_ECONOMY_GUIDANCE;
use joey_agent_core::prompt::{build_system_prompt, PromptInputs};
use joey_agent_core::state_block::{render, ScratchpadSummary, StateBlockInput};
use joey_agent_core::verification::{
    build_verify_on_stop_nudge, build_verify_on_stop_nudge_with_retrieval,
    mark_workspace_edited, RetrievalUsage,
};
use joey_core::Config;
use joey_tools::tools::todo_tool::TodoItem;
use joey_tools::{resolve_toolsets, ToolContext, ToolRegistry};
use serde_json::Value;

/// Tool names in CORE_TOOLS before feature 028 appended `scratchpad`
/// (verified against toolsets.rs at feature start — the pre-feature golden).
const PRE_FEATURE_CORE_TOOLS: &[&str] = &[
    // Web
    "web_search",
    "web_extract",
    // Terminal + process management
    "terminal",
    "process",
    // Desktop GUI terminal pane readers (gated on the GUI upstream)
    "read_terminal",
    "close_terminal",
    // File manipulation
    "read_file",
    "write_file",
    "patch",
    "search_files",
    // Vision + image generation
    "vision_analyze",
    "image_generate",
    // Skills
    "skills_list",
    "skill_view",
    "skill_manage",
    // Browser automation
    "browser_navigate",
    "browser_snapshot",
    "browser_click",
    "browser_type",
    "browser_scroll",
    "browser_back",
    "browser_press",
    "browser_get_images",
    "browser_vision",
    "browser_console",
    "browser_cdp",
    "browser_dialog",
    // Additive verbs (feature 016)
    "browser_hover",
    "browser_select_option",
    "browser_drag",
    "browser_click_coords",
    // Text-to-speech
    "text_to_speech",
    // Planning & memory
    "todo",
    "memory",
    // Session history search
    "session_search",
    // Clarifying questions
    "clarify",
    // LSP code intelligence
    "lsp_diagnostics",
    "lsp_definition",
    "lsp_references",
    "lsp_symbols",
    // Code execution + delegation
    "execute_code",
    "delegate_task",
    // Cronjob management
    "cronjob",
    // Home Assistant smart home control
    "ha_list_entities",
    "ha_get_state",
    "ha_list_services",
    "ha_call_service",
    // Kanban multi-agent coordination
    "kanban_show",
    "kanban_list",
    "kanban_complete",
    "kanban_block",
    "kanban_heartbeat",
    "kanban_comment",
    "kanban_create",
    "kanban_link",
    "kanban_unblock",
    "kanban_attach",
    "kanban_attach_url",
    "kanban_attachments",
    // Computer use
    "computer_use",
];

// ─── Helpers ──────────────────────────────────────────────────────────

/// Write `yaml` to a fresh tempdir `config.yaml` and load it (user values
/// deep-merged over the built-in defaults).
fn yaml_config(yaml: &str) -> Config {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.yaml");
    std::fs::write(&path, yaml).expect("write config.yaml");
    Config::load_from(path).expect("load config")
}

/// A ToolContext over the given config (cwd only feeds path resolution and
/// context-file discovery; the system temp dir carries no context files).
fn ctx_for(config: &Config) -> ToolContext {
    ToolContext::new(
        std::env::temp_dir(),
        config.clone(),
        "parity-test-session",
    )
}

/// The `function.name` list of `definitions()`, in emitted order.
fn def_names(registry: &ToolRegistry, enabled: &[String], ctx: &ToolContext) -> Vec<String> {
    registry
        .definitions(enabled, ctx)
        .iter()
        .map(|d| {
            d["function"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Build the full system prompt for the given config + enabled tool list.
fn prompt_for(config: &Config, tools: &[String]) -> String {
    let ctx = ctx_for(config);
    build_system_prompt(&PromptInputs {
        ctx: &ctx,
        model: "parity-model",
        provider: "parity-provider",
        enabled_tools: tools,
        pass_session_id: false,
        session_id: None,
    })
}

/// Serialize tests that touch the process-global tool `check()` TTL cache
/// (keyed by tool name only — a cached `scratchpad` result from one config
/// would leak into the other). `invalidate_check_cache()` is pub, so the
/// cache is dropped under this lock before each definitions() comparison.
fn registry_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

fn one_todo() -> TodoItem {
    TodoItem {
        id: "1".to_string(),
        content: "parity task".to_string(),
        status: "in_progress".to_string(),
    }
}

fn one_scratchpad() -> Option<ScratchpadSummary> {
    Some(ScratchpadSummary {
        path: "~/.joey/scratchpad/parity.md".to_string(),
        entries: 1,
        last_entry_at: Some("2026-09-10T12:00:00Z".to_string()),
    })
}

// ─── Tests ────────────────────────────────────────────────────────────

/// Default config turns every mechanism ON with the pinned default
/// magnitudes, and the whole surface activates end-to-end.
#[test]
fn default_on_activates_every_mechanism() {
    let cfg = Config::defaults();
    assert!(cfg.scratchpad_enabled());
    assert!(cfg.state_block_enabled());
    assert!(cfg.midturn_tool_hygiene_enabled());
    assert!(cfg.boundary_trigger_enabled());
    assert!(cfg.context_economy_guidance_enabled());
    assert!(cfg.retrieval_verification_nudge_enabled());
    assert_eq!(cfg.scratchpad_max_entry_chars(), 8000);
    assert_eq!(cfg.state_block_max_chars(), 1200);
    assert!((cfg.midturn_threshold() - 0.35).abs() < 1e-9);
    assert!((cfg.boundary_threshold() - 0.35).abs() < 1e-9);
    assert!(cfg.midturn_threshold() < cfg.get_f64("compression.threshold", 0.50));

    // Toolset resolution: the pre-feature golden is a subset of the default
    // enabled list, and scratchpad (the 61st name) is appended.
    let enabled = resolve_toolsets(&cfg.get_str_list("toolsets"));
    assert!(enabled.contains(&"scratchpad".to_string()));
    for name in PRE_FEATURE_CORE_TOOLS {
        assert!(
            enabled.contains(&name.to_string()),
            "pre-feature golden core tool missing from default toolsets: {name}"
        );
    }

    // Registry: the scratchpad tool is registered AND passes its config-gated
    // check() under default config.
    {
        let _g = registry_lock();
        joey_tools::registry::invalidate_check_cache();
        let ctx = ctx_for(&cfg);
        let reg = ToolRegistry::with_builtins();
        let names = def_names(&reg, &enabled, &ctx);
        assert!(
            names.contains(&"scratchpad".to_string()),
            "definitions contain scratchpad under default config"
        );
    }

    // System prompt carries the context-economy guidance.
    let prompt = prompt_for(&cfg, &enabled);
    assert!(prompt.contains(CONTEXT_ECONOMY_GUIDANCE));

    // State-block renderer produces a block for a todo + scratchpad entry.
    let todos = vec![one_todo()];
    let input = StateBlockInput {
        todos: &todos,
        scratchpad: one_scratchpad(),
        turn: 2,
        max_turns: 90,
    };
    let block = render(&input, cfg.state_block_max_chars()).expect("state block renders");
    assert!(block.contains("TASKS:"));

    // Verify-on-stop nudge with a retrieval signal appends the reminder line.
    // (Attempts = 0 — MAX_VERIFY_ATTEMPTS is private in verification.rs and
    // equals 2; 0 is safely below budget. A fresh ledger session reads
    // NotApplicable, so seed Unverified via the pub mark_workspace_edited.)
    let sess = "parity-nudge-default-on";
    let cwd = "/tmp/repo";
    let paths = vec!["src/lib.rs".to_string()];
    mark_workspace_edited(sess, cwd, &paths);
    let nudge = build_verify_on_stop_nudge_with_retrieval(
        sess,
        cwd,
        &paths,
        0,
        RetrievalUsage {
            rag_prefetch: true,
            ..Default::default()
        },
        &cfg,
    )
    .expect("retrieval nudge under default config");
    assert!(nudge.ends_with("Also: re-check retrieved facts against their sources before finishing."));
}

/// `scratchpad.enabled: false` removes exactly the scratchpad definition —
/// the rest of the wire output is byte-identical.
#[test]
fn scratchpad_disabled_registry_parity_bytes() {
    let cfg_a = Config::defaults();
    let cfg_b = yaml_config("scratchpad:\n  enabled: false\n");
    assert!(!cfg_b.scratchpad_enabled());

    let _g = registry_lock();
    let enabled = resolve_toolsets(&cfg_a.get_str_list("toolsets"));

    joey_tools::registry::invalidate_check_cache();
    let reg_a = ToolRegistry::with_builtins();
    let ctx_a = ctx_for(&cfg_a);
    let defs_a = reg_a.definitions(&enabled, &ctx_a);
    let names_a = def_names(&reg_a, &enabled, &ctx_a);

    joey_tools::registry::invalidate_check_cache();
    let reg_b = ToolRegistry::with_builtins();
    let ctx_b = ctx_for(&cfg_b);
    let defs_b = reg_b.definitions(&enabled, &ctx_b);
    let names_b = def_names(&reg_b, &enabled, &ctx_b);

    // (a) scratchpad gone; unconditional builtins survive.
    assert!(!names_b.contains(&"scratchpad".to_string()));
    for expected in ["todo", "memory", "read_file"] {
        assert!(
            names_b.contains(&expected.to_string()),
            "`{expected}` lost when scratchpad disabled"
        );
    }

    // (b) Nothing else is lost: every pre-feature golden core tool that was
    // present under defaults (i.e. the with_builtins-registered subset) is
    // still present when scratchpad is disabled.
    for golden in PRE_FEATURE_CORE_TOOLS {
        if names_a.iter().any(|n| n == golden) {
            assert!(
                names_b.iter().any(|n| n == golden),
                "golden core tool lost when scratchpad disabled: {golden}"
            );
        }
    }

    // (c) BYTE PARITY: default wire output minus the scratchpad entry is
    // byte-identical to the disabled-config wire output.
    let minus: Vec<&Value> = defs_a
        .iter()
        .filter(|d| d["function"]["name"] != "scratchpad")
        .collect();
    assert_eq!(
        serde_json::to_string(&minus).unwrap(),
        serde_json::to_string(&defs_b).unwrap(),
        "definitions minus scratchpad must be byte-identical to disabled-config definitions"
    );
}

/// The ten context-economy config keys: expected surface values.
#[derive(Clone)]
struct Surface {
    scratchpad_on: bool,
    max_entry: usize,
    state_on: bool,
    max_chars: usize,
    hygiene: bool,
    midturn_t: f64,
    boundary_on: bool,
    boundary_t: f64,
    guidance: bool,
    nudge: bool,
}

fn surface_defaults() -> Surface {
    Surface {
        scratchpad_on: true,
        max_entry: 8000,
        state_on: true,
        max_chars: 1200,
        hygiene: true,
        midturn_t: 0.35,
        boundary_on: true,
        boundary_t: 0.35,
        guidance: true,
        nudge: true,
    }
}

fn assert_surface(cfg: &Config, exp: &Surface) {
    assert_eq!(cfg.scratchpad_enabled(), exp.scratchpad_on);
    assert_eq!(cfg.scratchpad_max_entry_chars(), exp.max_entry);
    assert_eq!(cfg.state_block_enabled(), exp.state_on);
    assert_eq!(cfg.state_block_max_chars(), exp.max_chars);
    assert_eq!(cfg.midturn_tool_hygiene_enabled(), exp.hygiene);
    assert!((cfg.midturn_threshold() - exp.midturn_t).abs() < 1e-9);
    assert_eq!(cfg.boundary_trigger_enabled(), exp.boundary_on);
    assert!((cfg.boundary_threshold() - exp.boundary_t).abs() < 1e-9);
    assert_eq!(cfg.context_economy_guidance_enabled(), exp.guidance);
    assert_eq!(cfg.retrieval_verification_nudge_enabled(), exp.nudge);
}

/// Flipping any ONE of the 10 keys leaves the other nine at defaults
/// (deep-merge only touches the flipped subtree).
#[test]
fn every_switch_off_is_isolated() {
    assert_surface(&Config::defaults(), &surface_defaults());

    // Boundary values for the numeric keys prove the clamps distinct from
    // defaults; 0.20 for the thresholds is inside the clamp band and below
    // compression.threshold, so it routes through unclamped.
    let cases: &[(&str, fn(&mut Surface))] = &[
        ("scratchpad:\n  enabled: false\n", |s: &mut Surface| {
            s.scratchpad_on = false;
        }),
        ("scratchpad:\n  max_entry_chars: 64000\n", |s: &mut Surface| {
            s.max_entry = 64000;
        }),
        ("state_block:\n  enabled: false\n", |s: &mut Surface| {
            s.state_on = false;
        }),
        ("state_block:\n  max_chars: 8000\n", |s: &mut Surface| {
            s.max_chars = 8000;
        }),
        (
            "compression:\n  midturn_tool_hygiene: false\n",
            |s: &mut Surface| {
                s.hygiene = false;
            },
        ),
        ("compression:\n  midturn_threshold: 0.20\n", |s: &mut Surface| {
            s.midturn_t = 0.20;
        }),
        ("compression:\n  boundary_trigger: false\n", |s: &mut Surface| {
            s.boundary_on = false;
        }),
        (
            "compression:\n  boundary_threshold: 0.20\n",
            |s: &mut Surface| {
                s.boundary_t = 0.20;
            },
        ),
        (
            "agent:\n  context_economy_guidance: false\n",
            |s: &mut Surface| {
                s.guidance = false;
            },
        ),
        (
            "agent:\n  retrieval_verification_nudge: false\n",
            |s: &mut Surface| {
                s.nudge = false;
            },
        ),
    ];
    for (yaml, flip) in cases {
        let cfg = yaml_config(yaml);
        let mut exp = surface_defaults();
        flip(&mut exp);
        assert_surface(&cfg, &exp);
    }
}

/// `state_block.enabled` gates the caller, not the renderer: `render` is
/// config-blind and produces identical output under both configs; the
/// `max_chars` getter clamps at the 200 minimum.
#[test]
fn state_block_switch_and_renderer_parity() {
    let cfg_default = Config::defaults();
    let cfg_off = yaml_config("state_block:\n  enabled: false\n");
    assert!(!cfg_off.state_block_enabled());
    // isolation: the switch flip left max_chars at its default
    assert_eq!(cfg_off.state_block_max_chars(), 1200);

    let todos = vec![one_todo()];
    let input = StateBlockInput {
        todos: &todos,
        scratchpad: one_scratchpad(),
        turn: 2,
        max_turns: 90,
    };
    let a = render(&input, cfg_default.state_block_max_chars());
    let b = render(&input, cfg_off.state_block_max_chars());
    assert!(a.is_some(), "renderer stays live under default config");
    assert!(b.is_some(), "renderer stays live under disabled switch");
    assert_eq!(a, b, "render is pure — same input, same output regardless of the enabled switch");
    assert!(b.unwrap().contains("TASKS:"));

    let cfg_min = yaml_config("state_block:\n  max_chars: 200\n");
    assert_eq!(cfg_min.state_block_max_chars(), 200);
}

/// When the guidance/nudge switches are off, the pub surfaces are
/// byte-identical to the pre-feature baseline.
#[test]
fn guidance_and_nudge_when_disabled_byte_parity() {
    // (a) context-economy guidance off restores the pre-feature prompt.
    let cfg_default = Config::defaults();
    let cfg_off = yaml_config("agent:\n  context_economy_guidance: false\n");
    let enabled_full = resolve_toolsets(&cfg_default.get_str_list("toolsets"));
    let enabled_pre: Vec<String> = enabled_full
        .iter()
        .filter(|t| t.as_str() != "scratchpad")
        .cloned()
        .collect();

    let p_off = prompt_for(&cfg_off, &enabled_full);
    assert!(!p_off.contains("Work economically with context"));
    // Pre-feature golden: defaults with the scratchpad-free tool list
    // (guidance gated off by tool absence) == guidance-off config with the
    // full tool list. The guidance push was the only scratchpad-dependent
    // addition, so the disabled switch restores the pre-feature prompt
    // byte-for-byte.
    let p_pre = prompt_for(&cfg_default, &enabled_pre);
    assert_eq!(p_pre, p_off, "guidance-off prompt must equal the pre-feature prompt");
    // And the default prompt does carry it (the delta is exactly the guidance).
    assert!(prompt_for(&cfg_default, &enabled_full).contains(CONTEXT_ECONOMY_GUIDANCE));

    // (b) retrieval-verification nudge off: the wrapper collapses to the
    // base (pre-feature) nudge byte-for-byte for every retrieval shape.
    let cfg_nudge_off = yaml_config("agent:\n  retrieval_verification_nudge: false\n");
    let sess = "parity-nudge-disabled";
    let cwd = "/tmp/repo";
    let paths = vec!["src/lib.rs".to_string()];
    mark_workspace_edited(sess, cwd, &paths);
    let retrievals = [
        RetrievalUsage {
            rag_prefetch: true,
            ..Default::default()
        },
        RetrievalUsage {
            neurocode_cold: true,
            ..Default::default()
        },
        RetrievalUsage::default(),
    ];
    for retrieval in retrievals {
        let wrapped =
            build_verify_on_stop_nudge_with_retrieval(sess, cwd, &paths, 0, retrieval, &cfg_nudge_off);
        let base = build_verify_on_stop_nudge(sess, cwd, &paths, 0);
        assert_eq!(
            wrapped.as_deref(),
            base.as_deref(),
            "nudge-off wrapper must equal the base (pre-feature) nudge"
        );
    }
}

/// Threshold switches route through their clamps and stay below
/// compression.threshold.
#[test]
fn threshold_switches_route_through() {
    let cfg = yaml_config("compression:\n  midturn_threshold: 0.45\n  boundary_threshold: 0.10\n");
    assert!((cfg.midturn_threshold() - 0.45).abs() < 1e-9);
    assert!(cfg.midturn_threshold() < cfg.get_f64("compression.threshold", 0.50));
    assert!((cfg.boundary_threshold() - 0.10).abs() < 1e-9);

    // Clamps: 0.9 → 0.45 (upper), 0.01 → 0.10 (lower), both keys.
    let hi_mid = yaml_config("compression:\n  midturn_threshold: 0.9\n");
    assert!((hi_mid.midturn_threshold() - 0.45).abs() < 1e-9);
    let lo_mid = yaml_config("compression:\n  midturn_threshold: 0.01\n");
    assert!((lo_mid.midturn_threshold() - 0.10).abs() < 1e-9);
    let hi_boundary = yaml_config("compression:\n  boundary_threshold: 0.9\n");
    assert!((hi_boundary.boundary_threshold() - 0.45).abs() < 1e-9);
    let lo_boundary = yaml_config("compression:\n  boundary_threshold: 0.01\n");
    assert!((lo_boundary.boundary_threshold() - 0.10).abs() < 1e-9);
}
