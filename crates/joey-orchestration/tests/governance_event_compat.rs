//! T025r: Debug-snapshot compat pins for `AgentEvent` (Constitution VII).
//!
//! The six feature-030 governance variants (`DelegationBusy`,
//! `DelegationTimeout`, `DelegationRetryBudgetExhausted`,
//! `DelegationCacheHit`, `DelegationDegradedOutput`, `CapacitySnapshot`)
//! must be purely ADDITIVE, and the Debug shapes of pre-existing variants
//! must stay pinned so downstream renderers (CLI/TUI) keep parsing them.
//!
//! `AgentEvent` deliberately has no serde derives (adding them would be a
//! new public surface), so the pins use `format!("{:?}", ev)` snapshot
//! literals. Literals were derived from actual `{:?}` output, not guessed.

use joey_agent_core::AgentEvent;
use joey_providers::Usage;

/// The six governance variants are constructible from a foreign crate
/// (enum is `#[non_exhaustive]`, variants are not) and their Debug shapes
/// are pinned so accidental field renames/types break this test.
#[test]
fn gov_eventcompat_new_governance_variants_debug_pins() {
    let ev = AgentEvent::DelegationBusy {
        queue_depth: 3,
        cap: 4,
    };
    assert_eq!(
        format!("{:?}", ev),
        "DelegationBusy { queue_depth: 3, cap: 4 }"
    );

    let ev = AgentEvent::DelegationTimeout {
        child_id: 7,
        goal: "g".to_string(),
        timeout_secs: 600,
    };
    assert_eq!(
        format!("{:?}", ev),
        "DelegationTimeout { child_id: 7, goal: \"g\", timeout_secs: 600 }"
    );

    let ev = AgentEvent::DelegationRetryBudgetExhausted {
        goal: "g".to_string(),
        budget: 2,
        in_flight: 2,
    };
    assert_eq!(
        format!("{:?}", ev),
        "DelegationRetryBudgetExhausted { goal: \"g\", budget: 2, in_flight: 2 }"
    );

    let ev = AgentEvent::DelegationCacheHit {
        signature: "sig".to_string(),
    };
    assert_eq!(format!("{:?}", ev), "DelegationCacheHit { signature: \"sig\" }");

    let ev = AgentEvent::DelegationDegradedOutput {
        child_id: 7,
        goal: "g".to_string(),
        sample_rate: 0.1,
    };
    assert_eq!(
        format!("{:?}", ev),
        "DelegationDegradedOutput { child_id: 7, goal: \"g\", sample_rate: 0.1 }"
    );

    let ev = AgentEvent::CapacitySnapshot {
        running: 1,
        queued: 2,
        queue_cap: 6,
        max_children: 3,
    };
    assert_eq!(
        format!("{:?}", ev),
        "CapacitySnapshot { running: 1, queued: 2, queue_cap: 6, max_children: 3 }"
    );
}

/// Ten pre-existing variants keep their Debug shapes unchanged (the
/// governance additions must not have perturbed any existing variant).
#[test]
fn gov_eventcompat_existing_variants_debug_pins() {
    let ev = AgentEvent::TurnStart { max_iterations: 4 };
    assert_eq!(format!("{:?}", ev), "TurnStart { max_iterations: 4 }");

    let ev = AgentEvent::ToolStart {
        name: "read_file".to_string(),
        emoji: "F".to_string(),
        summary: "reading src".to_string(),
    };
    assert_eq!(
        format!("{:?}", ev),
        "ToolStart { name: \"read_file\", emoji: \"F\", summary: \"reading src\" }"
    );

    let ev = AgentEvent::ToolEnd {
        name: "read_file".to_string(),
        is_error: false,
        result_preview: "ok".to_string(),
        duration_secs: 0.5,
        exit_code: None,
        full_result: "ok".to_string(),
    };
    assert_eq!(
        format!("{:?}", ev),
        "ToolEnd { name: \"read_file\", is_error: false, result_preview: \"ok\", duration_secs: 0.5, exit_code: None, full_result: \"ok\" }"
    );

    let ev = AgentEvent::AssistantMessage("hello".to_string());
    assert_eq!(format!("{:?}", ev), "AssistantMessage(\"hello\")");

    let ev = AgentEvent::Notice("note".to_string());
    assert_eq!(format!("{:?}", ev), "Notice(\"note\")");

    let ev = AgentEvent::RetryAttempt {
        attempt: 1,
        max_retries: 3,
        error: "boom".to_string(),
        wait_secs: 2.0,
    };
    assert_eq!(
        format!("{:?}", ev),
        "RetryAttempt { attempt: 1, max_retries: 3, error: \"boom\", wait_secs: 2.0 }"
    );

    let ev = AgentEvent::SubagentComplete {
        id: 1,
        goal: "g".to_string(),
        success: true,
        summary_preview: "sp".to_string(),
        token_usage: Usage::default(),
        duration_secs: 9.5,
    };
    assert_eq!(
        format!("{:?}", ev),
        "SubagentComplete { id: 1, goal: \"g\", success: true, summary_preview: \"sp\", token_usage: Usage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0 }, duration_secs: 9.5 }"
    );

    let ev = AgentEvent::SubagentFailed {
        id: 2,
        goal: "g".to_string(),
        error: "err".to_string(),
        duration_secs: 3.25,
    };
    assert_eq!(
        format!("{:?}", ev),
        "SubagentFailed { id: 2, goal: \"g\", error: \"err\", duration_secs: 3.25 }"
    );

    let ev = AgentEvent::Done {
        final_text: "fin".to_string(),
        usage: Usage::default(),
        iterations: 2,
    };
    assert_eq!(
        format!("{:?}", ev),
        "Done { final_text: \"fin\", usage: Usage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0 }, iterations: 2 }"
    );

    let ev = AgentEvent::Failed("boom".to_string());
    assert_eq!(format!("{:?}", ev), "Failed(\"boom\")");
}

/// The enum stays `#[non_exhaustive]`-additive: all six new variants are
/// matchable today (this compiles only if every named arm is a real,
/// in-scope variant) and route through a catch-all-compatible match.
#[test]
fn gov_eventcompat_enum_remains_non_exhaustive_additive() {
    let events = vec![
        AgentEvent::DelegationBusy {
            queue_depth: 3,
            cap: 4,
        },
        AgentEvent::DelegationTimeout {
            child_id: 7,
            goal: "g".to_string(),
            timeout_secs: 600,
        },
        AgentEvent::DelegationRetryBudgetExhausted {
            goal: "g".to_string(),
            budget: 2,
            in_flight: 2,
        },
        AgentEvent::DelegationCacheHit {
            signature: "sig".to_string(),
        },
        AgentEvent::DelegationDegradedOutput {
            child_id: 7,
            goal: "g".to_string(),
            sample_rate: 0.1,
        },
        AgentEvent::CapacitySnapshot {
            running: 1,
            queued: 2,
            queue_cap: 6,
            max_children: 3,
        },
    ];

    let mut governance_hits = 0usize;
    for ev in events {
        match ev {
            AgentEvent::DelegationBusy { .. }
            | AgentEvent::DelegationTimeout { .. }
            | AgentEvent::DelegationRetryBudgetExhausted { .. }
            | AgentEvent::DelegationCacheHit { .. }
            | AgentEvent::DelegationDegradedOutput { .. }
            | AgentEvent::CapacitySnapshot { .. } => governance_hits += 1,
            _ => {}
        }
    }
    assert_eq!(governance_hits, 6);
}
