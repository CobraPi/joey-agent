//! T035 — FR-009 parity pins: byte-identical behavior while RAG is disabled.
//!
//! The enhancement must be invisible when `neurocode.rag.enabled = false`
//! (the default): the model sees the exact same tool surface, the same tool
//! schemas, and the same engine behavior (tier routing + staleness sections)
//! as before the feature existed.
//!
//! Sibling parity pins in other crates (keep in sync):
//! - `joey-cli` — `disabled_yields_no_section_at_all`: the `/neurocode rag`
//!   command surface emits nothing RAG-shaped when disabled.
//! - `joey-agent-core` — `rag_prefetch` byte-identity tests: the assembled
//!   agent context is byte-identical with RAG off.
//!
//! This file pins the `joey-tools`-registry and `joey-neurocode`-engine legs
//! of that contract:
//!
//! (a) `register_neurocode_rag_tools(_, false, _)` registers NOTHING — the
//!     `neurocode_search` tool is absent from the registry entirely, and the
//!     registry's name-set equals one that never ran RAG registration.
//! (b) The four pre-existing NeuroCode tools keep byte-stable parameter
//!     schemas (golden JSON pinned from the source literals).
//! (c) Tier routing (classifier route shape) and the `### Index Staleness`
//!     section shape are unchanged.
//! (d) `RagConfig::default()` is disabled with an empty mirror URL
//!     (no fetch path can activate implicitly).

use joey_neurocode_rag::config::RagConfig;
use joey_neurocode::{
    CodingRequest, ComplexityClassifier, ComplexityRoute, ComplexityTier, DefaultEngine,
    NeuroCodeConfig, NeuroCodeEngine,
};
use joey_tools::registry::ToolRegistry;
use joey_tools::tools::neurocode_tools::{register_neurocode_rag_tools, register_neurocode_tools};
use serde_json::json;

// ─── (a) disabled RAG registration is a no-op ───────────────────────────────

/// Registry exactly as a pre-enhancement binary would build it: builtins +
/// the four NeuroCode tools, no RAG registration call at all.
fn registry_without_rag() -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    joey_tools::builtins::register_all(&mut reg);
    register_neurocode_tools(&mut reg, None);
    reg
}

/// Registry as the enhanced binary builds it with `rag_enabled = false`.
fn registry_with_rag_disabled() -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    joey_tools::builtins::register_all(&mut reg);
    register_neurocode_tools(&mut reg, None);
    register_neurocode_rag_tools(&mut reg, false, None);
    reg
}

#[test]
fn disabled_rag_registration_yields_identical_name_set() {
    let plain = registry_without_rag();
    let disabled = registry_with_rag_disabled();

    let mut plain_names = plain.names();
    let mut disabled_names = disabled.names();
    plain_names.sort();
    disabled_names.sort();

    assert_eq!(
        plain_names, disabled_names,
        "rag_enabled=false must leave the registry name-set byte-identical"
    );

    // The RAG tool is ABSENT entirely — not merely check()-disabled (FR-009:
    // the model never sees it in disabled state).
    assert!(disabled.get("neurocode_search").is_none());

    // Counter-check: the gate actually opens when enabled (with no backend
    // the tool registers but stays inactive — same as the four above).
    let mut enabled = ToolRegistry::new();
    register_neurocode_tools(&mut enabled, None);
    register_neurocode_rag_tools(&mut enabled, true, None);
    assert!(enabled.get("neurocode_search").is_some());
}

// ─── (b) the four tools' schemas are byte-stable ────────────────────────────

#[test]
fn neurocode_tool_schemas_are_byte_stable() {
    // Goldens transcribed from the `parameters()` literals in
    // joey-tools/src/tools/neurocode_tools.rs — these are public surface
    // (the model-visible tool schema) and must not drift.
    let goldens: &[(&str, serde_json::Value)] = &[
        (
            "neurocode_index",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute or relative path to the project root to index."
                    },
                    "force": {
                        "type": "boolean",
                        "description": "If true, rebuild the index from scratch instead of incrementally updating. Default: false.",
                        "default": false
                    }
                },
                "required": ["path"]
            }),
        ),
        (
            "neurocode_query",
            json!({
                "type": "object",
                "properties": {
                    "query_type": {
                        "type": "string",
                        "description": "The kind of query: `dependencies`, `dependents`, `definition`, or `references`.",
                        "enum": ["dependencies", "dependents", "definition", "references"]
                    },
                    "symbol": {
                        "type": "string",
                        "description": "The seed symbol to query — an FQCN (e.g. `com.example.Foo.bar`) or a simple symbol name."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of results to return. Default: 20.",
                        "default": 20,
                        "minimum": 1
                    }
                },
                "required": ["query_type", "symbol"]
            }),
        ),
        (
            "neurocode_status",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        (
            "neurocode_ingest",
            json!({
                "type": "object",
                "properties": {
                    "category": {
                        "type": "string",
                        "description": "Knowledge category: `pattern`, `antipattern`, `rule`, or `convention`.",
                        "enum": ["pattern", "antipattern", "rule", "convention"]
                    },
                    "source_path": {
                        "type": "string",
                        "description": "Path to the knowledge source (a file containing the knowledge to ingest)."
                    },
                    "version_tag": {
                        "type": "string",
                        "description": "Optional framework version the knowledge applies to (e.g. `8.x`, `infinity-24.2`). Omit if version-agnostic."
                    },
                    "provenance": {
                        "type": "string",
                        "description": "Where the knowledge came from — a URL, doc reference, or a short human-authored note (e.g. `docs.pega.com/casing`, `learned from build failure 2024-01-15`)."
                    }
                },
                "required": ["category", "source_path", "provenance"]
            }),
        ),
    ];

    let reg = registry_with_rag_disabled();
    for (name, golden) in goldens {
        let tool = reg
            .get(name)
            .unwrap_or_else(|| panic!("{name} must be registered"));
        let schema = tool.parameters();

        // Schema is a JSON object...
        assert!(schema.is_object(), "{name}: schema must be an object");
        // ...byte-stable across calls (deterministic rendering)...
        assert_eq!(
            serde_json::to_string(&schema).unwrap(),
            serde_json::to_string(&tool.parameters()).unwrap(),
            "{name}: schema must be deterministic across calls"
        );
        // ...and exactly the golden (object equality; property order is not
        // part of the contract, contents are).
        assert_eq!(&schema, golden, "{name}: schema drifted from golden");
    }
}

// ─── (c) tier routing + staleness shapes unchanged ──────────────────────────

fn request(text: &str, root: std::path::PathBuf) -> CodingRequest {
    CodingRequest {
        text: text.to_string(),
        active_file: None,
        active_symbols: vec![],
        project_root: root,
        token_budget_hint: 0,
        scope_files: Vec::new(),
    }
}

#[test]
fn tier_routing_shape_is_unchanged() {
    let classifier = ComplexityClassifier::from_config(&NeuroCodeConfig::default());
    let root = std::path::PathBuf::from(".");

    // Economical: keyword signal ("unit test").
    let eco: ComplexityRoute = classifier.classify(&request("write a unit test for this", root.clone()));
    assert_eq!(eco.tier, ComplexityTier::Economical);
    assert!(!eco.overridden);
    assert!(eco.override_tier.is_none());
    assert!(!eco.reasoning.is_empty(), "route must carry reasoning");
    assert!(!eco.signals.is_empty(), "keyword signal must be recorded");

    // Frontier: keyword signals ("refactor", "architecture", "concurrency").
    let front = classifier.classify(&request("refactor the architecture for concurrency", root.clone()));
    assert_eq!(front.tier, ComplexityTier::Frontier);
    assert!(!front.overridden);
    assert!(!front.reasoning.is_empty());

    // No decisive signals → ambiguous default, same route shape.
    let amb = classifier.classify(&request("print hello world", root.clone()));
    assert_eq!(amb.tier, ComplexityTier::AmbiguousDefault);
    assert!(!amb.overridden);

    // Pinned override wins and is reported with overridden = true (FR-002).
    classifier.pin_tier(ComplexityTier::Frontier);
    let pinned = classifier.classify(&request("write a unit test for this", root));
    assert_eq!(pinned.tier, ComplexityTier::Frontier);
    assert!(pinned.overridden);
    assert_eq!(pinned.override_tier, Some(ComplexityTier::Frontier));
    classifier.unpin_tier();
    assert!(classifier.pinned_tier().is_none());
}

#[test]
fn staleness_section_shape_is_unchanged() {
    // Fresh index → NO staleness section.
    let fresh = tempfile::tempdir().unwrap();
    let src_dir = fresh.path().join("src/main/java/com/acme/user");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(
        src_dir.join("UserServiceImpl.java"),
        "package com.acme.user; public class UserServiceImpl {}\n",
    )
    .unwrap();

    let mut cfg = NeuroCodeConfig::default();
    cfg.enabled = true;
    let engine = DefaultEngine::new(cfg.clone(), fresh.path().to_path_buf());
    let result = engine.index_project();
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(result.artifacts_seen > 0);

    let ctx = engine.assemble_context(
        &request("fix UserServiceImpl", fresh.path().to_path_buf()),
        ComplexityTier::Frontier,
    );
    assert!(
        !ctx.formatted_context.contains("### Index Staleness"),
        "fresh index must not warn"
    );

    // Backdated index (mtime > indexed_at) → the exact staleness header.
    let stale = tempfile::tempdir().unwrap();
    let stale_src = stale.path().join("src/main/java/com/acme/user");
    std::fs::create_dir_all(&stale_src).unwrap();
    std::fs::write(
        stale_src.join("UserServiceImpl.java"),
        "package com.acme.user; public class UserServiceImpl {}\n",
    )
    .unwrap();
    let engine = DefaultEngine::new(cfg, stale.path().to_path_buf());
    let result = engine.index_project();
    assert!(result.errors.is_empty(), "{:?}", result.errors);

    // Backdate indexed_at via the store directly (std-only; no filetime dep):
    // mtime (now) then postdates indexed_at.
    engine.with_graph(|g| {
        if let Some(graph) = g {
            let _ = graph.store().conn().execute(
                "UPDATE code_artifacts SET indexed_at = '2000-01-01T00:00:00+00:00'",
                [],
            );
        }
    });

    let ctx = engine.assemble_context(
        &request("fix UserServiceImpl", stale.path().to_path_buf()),
        ComplexityTier::Frontier,
    );
    assert!(
        ctx.formatted_context.contains("### Index Staleness"),
        "stale index must warn, got:\n{}",
        ctx.formatted_context
    );
}

// ─── (d) RagConfig defaults keep the feature off ────────────────────────────

#[test]
fn rag_config_defaults_are_disabled_and_offline() {
    let cfg = RagConfig::default();
    assert!(!cfg.enabled, "neurocode.rag.enabled must default to false");
    assert!(
        cfg.mirror_url.is_empty(),
        "neurocode.rag.local.mirror_url must default to empty (fetch disabled)"
    );
}
