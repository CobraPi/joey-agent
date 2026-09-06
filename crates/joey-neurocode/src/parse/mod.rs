//! Tree-sitter ingestion pipeline (FR-006) — multi-language.
//!
//! Walks a project source tree, parses each recognized source file with
//! its dedicated tree-sitter grammar — every programming language with a
//! grammar under the tree-sitter org (Java, Python, JS/TS/TSX, Go, Rust,
//! Ruby, PHP, C#, C, C++, Scala, Haskell, Julia, OCaml, Bash, Verilog,
//! Agda) — or the heuristic fallback extractor (Kotlin, Swift, Elixir,
//! Lua, …), and upserts nodes + edges into the graph store (T011).
//! Pega rule patterns are recognized during the same pass on Java
//! extractions: matched types get `PegaMetadata` and
//! `ArtifactKind::PegaRule`, and `ReferencesRule`/`InheritsRule` edges are
//! emitted to the referenced/inherited rules (T058).

pub mod extract;
pub mod golang;
pub mod grammars;
pub mod heuristic;
pub mod java;
pub mod jsts;
pub mod pega;
pub mod python;
pub mod registry;
pub mod rustlang;
pub mod spans;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::graph::edge::EdgeKind;
use crate::graph::node::{ArtifactKind, CodeArtifactNode};
use crate::graph::{DependencyGraph, NodeId};

use pega::{extract_pega_metadata, is_pega_rule};

use crate::pega::version::detect_pega_version;

/// Result of ingesting a project source tree.
#[derive(Debug, Clone, Default)]
pub struct IngestionResult {
    pub files_scanned: usize,
    pub artifacts_seen: usize,
    pub edges_created: usize,
    pub errors: Vec<String>,
}

/// Walk a project source tree, parse each supported source file, and
/// upsert nodes + edges into the graph store (T011).
pub fn ingest_project(graph: &DependencyGraph, project_root: &Path) -> IngestionResult {
    let mut result = IngestionResult::default();

    // T058: detect the Pega version once per ingestion run. An empty string
    // (no version detected) still allows pattern-based rule extraction.
    let pega_version = detect_pega_version(project_root, "").unwrap_or_default();

    // Edges that can only be resolved after every node in the tree has been
    // upserted: (from_id, target reference, EdgeKind).
    let mut pending_edges: Vec<(NodeId, String, EdgeKind)> = Vec::new();
    // Simple-name → node id index for cross-file edge resolution.
    let mut name_index: HashMap<String, NodeId> = HashMap::new();

    let scan_root = primary_source_root(project_root);

    // ── Phase 1: collect + read + parse in parallel (rayon) ────────────
    // Tree-sitter parsing is pure CPU and per-file independent; the walk
    // itself stays sequential (directory order), then the read+parse work
    // fans out across cores. `collect` preserves input order, so phase 2
    // sees files in exactly the order the sequential version did.
    let candidate_paths: Vec<std::path::PathBuf> = walkdir::WalkDir::new(&scan_root)
        .max_depth(10)
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                return None;
            };
            if !registry::is_supported_extension(&ext.to_lowercase()) {
                return None;
            }
            // Skip vendored/generated trees — noise in the structural graph.
            if is_vendor_path(path) {
                return None;
            }
            Some(path.to_path_buf())
        })
        .collect();
    result.files_scanned = candidate_paths.len();

    enum ParsedFile {
        Ok { rel_path: String, extraction: crate::parse::extract::SourceExtraction },
        Err { rel_path: String, error: String },
    }

    let parsed: Vec<ParsedFile> = candidate_paths
        .into_par_iter()
        .map(|path| {
            let rel_path = path
                .strip_prefix(project_root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    return ParsedFile::Err { rel_path, error: e.to_string() };
                }
            };
            match registry::parse_any(&path, &content) {
                Some(Ok(extraction)) => ParsedFile::Ok { rel_path, extraction },
                Some(Err(e)) => ParsedFile::Err { rel_path, error: e },
                None => unreachable!("supported extensions always parse"),
            }
        })
        .collect();

    // ── Phase 2: graph upserts (sequential — SQLite store) ─────────────
    for file in parsed {
        let (rel_path, extraction) = match file {
            ParsedFile::Ok { rel_path, extraction } => (rel_path, extraction),
            ParsedFile::Err { rel_path, error } => {
                result.errors.push(format!("{}: {}", rel_path, error));
                continue;
            }
        };

        // Package: explicit (Java, Go) or derived from the file's directory
        // path (Python modules, Rust mod paths, TS scopes, …).
        let package = if !extraction.package.is_empty() {
            extraction.package.clone()
        } else {
            derive_package(&rel_path)
        };

        // ── Type-level nodes ────────────────────────────────────────
        // (fq name, kind, node id) for same-file edge resolution.
        let mut node_ids: Vec<(String, ArtifactKind, NodeId)> = Vec::new();

        for type_node in &extraction.types {
            let fqcn = extraction.fq_name(type_node);
            let kind = match type_node.kind_class() {
                "interface" => ArtifactKind::Interface,
                "enum" => ArtifactKind::Enum,
                _ => ArtifactKind::Class,
            };
            let mut node = CodeArtifactNode::new(
                kind.clone(),
                fqcn.clone(),
                package.clone(),
                rel_path.clone(),
            );
            node.implemented_interfaces = type_node.implemented_interfaces.clone();
            node.annotations = type_node.annotations.clone();
            node.declared_dependencies = type_node.declared_dependencies.clone();
            node.source_span = Some((type_node.start_byte, type_node.end_byte));

            // T058: Pega rule extraction (Java only — pattern matching is
            // keyed on Java identifiers/annotations). The Java extractor
            // surfaces `implements` names; Pega-pattern interfaces are
            // surfaced as `extends:<X>` pseudo-annotations to let
            // extract_pega_metadata recognize directed inheritance.
            let mut kind = kind;
            if extraction.language == "java" {
                let mut pega_annotations = type_node.annotations.clone();
                for iface in &type_node.implemented_interfaces {
                    let qualified = if type_node.package.is_empty() {
                        None
                    } else {
                        Some(format!("{}.{}", type_node.package, iface))
                    };
                    let pseudo = if is_pega_rule(iface) {
                        Some(format!("extends:{}", iface))
                    } else {
                        qualified
                            .as_deref()
                            .filter(|q| is_pega_rule(q))
                            .map(|q| format!("extends:{}", q))
                    };
                    if let Some(p) = pseudo {
                        if !pega_annotations.contains(&p) {
                            pega_annotations.push(p);
                        }
                    }
                }
                let pega_deps: Vec<String> = type_node
                    .declared_dependencies
                    .iter()
                    .map(|d| {
                        if is_pega_rule(d) {
                            d.clone()
                        } else if !type_node.package.is_empty() {
                            let fq = format!("{}.{}", type_node.package, d);
                            if is_pega_rule(&fq) {
                                fq
                            } else {
                                d.clone()
                            }
                        } else {
                            d.clone()
                        }
                    })
                    .collect();
                if let Some(meta) = extract_pega_metadata(
                    &fqcn,
                    &pega_annotations,
                    &pega_deps,
                    &pega_version,
                ) {
                    node.kind = ArtifactKind::PegaRule;
                    kind = ArtifactKind::PegaRule;
                    node.pega_metadata = Some(meta);
                }
            }

            let id = match graph.upsert_node(&node) {
                Ok(id) => id,
                Err(e) => {
                    result.errors.push(format!("{}: upsert failed: {}", rel_path, e));
                    continue;
                }
            };
            node_ids.push((type_node.name.clone(), kind, id));
            name_index
                .entry(type_node.name.clone())
                .or_insert(id);
            result.artifacts_seen += 1;

            // Pega rule-reference edges (T058), resolved post-walk.
            if let Some(meta) = &node.pega_metadata {
                for reference in &meta.references_rules {
                    pending_edges.push((id, reference.clone(), EdgeKind::ReferencesRule));
                }
                if let Some(parent) = &meta.inherits_from {
                    pending_edges.push((id, parent.clone(), EdgeKind::InheritsRule));
                }
            }

            // Method nodes.
            for method in &type_node.methods {
                let method_fqcn = member_fqcn(&extraction.language, &fqcn, &method.name, true);
                let mut m_node = CodeArtifactNode::new(
                    ArtifactKind::Method,
                    method_fqcn,
                    package.clone(),
                    rel_path.clone(),
                );
                m_node.enclosing_type = Some(type_node.name.clone());
                m_node.annotations = method.annotations.clone();
                m_node.source_span = Some((method.start_byte, method.end_byte));
                m_node.signature = method.signature.clone();
                if let Ok(mid) = graph.upsert_node(&m_node) {
                    // Edge: member belongs to its enclosing type.
                    if graph.upsert_edge(mid, id, EdgeKind::MemberOf).is_ok() {
                        result.edges_created += 1;
                    } else {
                        result
                            .errors
                            .push(format!("{}: member edge upsert failed", rel_path));
                    }
                }
            }

            // Field nodes.
            for field in &type_node.fields {
                let field_fqcn = member_fqcn(&extraction.language, &fqcn, &field.name, false);
                let mut f_node = CodeArtifactNode::new(
                    ArtifactKind::Field,
                    field_fqcn,
                    package.clone(),
                    rel_path.clone(),
                );
                f_node.enclosing_type = Some(type_node.name.clone());
                f_node.annotations = field.annotations.clone();
                f_node.declared_dependencies = vec![field.type_name.clone()];
                f_node.signature = field.signature.clone();
                if let Ok(fid) = graph.upsert_node(&f_node) {
                    if graph.upsert_edge(fid, id, EdgeKind::MemberOf).is_ok() {
                        result.edges_created += 1;
                    } else {
                        result
                            .errors
                            .push(format!("{}: member edge upsert failed", rel_path));
                    }
                }
            }
        }

        // ── Module-level function nodes ─────────────────────────────
        for func in &extraction.module_functions {
            let fqcn = if package.is_empty() {
                func.name.clone()
            } else {
                format!("{}.{}()", package, func.name)
            };
            let mut f_node =
                CodeArtifactNode::new(ArtifactKind::Method, fqcn, package.clone(), rel_path.clone());
            f_node.annotations = func.annotations.clone();
            f_node.source_span = Some((func.start_byte, func.end_byte));
            f_node.signature = func.signature.clone();
            if graph.upsert_node(&f_node).is_ok() {
                result.artifacts_seen += 1;
            }
        }

        // ── Same-file Implements edges + cross-file pending deps ────
        for type_node in &extraction.types {
            let Some(from_id) = node_ids
                .iter()
                .find(|(name, _, _)| *name == type_node.name)
                .map(|(_, _, id)| *id)
            else {
                continue;
            };
            for iface in &type_node.implemented_interfaces {
                // Exact same-file match only: a suffix match
                // (`AuditService`.ends_with(`Service`) for an `implements
                // Service` clause) mis-wires the edge onto an unrelated
                // local type. Everything else resolves cross-file post-walk.
                if let Some(to_id) = node_ids
                    .iter()
                    .find(|(name, _, _)| name == iface)
                    .map(|(_, _, id)| *id)
                {
                    if graph.upsert_edge(from_id, to_id, EdgeKind::Implements).is_ok() {
                        result.edges_created += 1;
                    } else {
                        result
                            .errors
                            .push(format!("{}: implements edge upsert failed", rel_path));
                    }
                    if graph.upsert_edge(to_id, from_id, EdgeKind::IsImplementedBy).is_ok() {
                        result.edges_created += 1;
                    } else {
                        result.errors.push(format!(
                            "{}: is-implemented-by edge upsert failed",
                            rel_path
                        ));
                    }
                } else {
                    // Cross-file: resolve post-walk by simple name.
                    pending_edges.push((from_id, iface.clone(), EdgeKind::Implements));
                }
            }
            for dep in &type_node.declared_dependencies {
                let dep_base = dep.rsplit('.').next().unwrap_or(dep).to_string();
                // Exact same-file match on the base name only: a suffix
                // match (`MyService`.ends_with(`Service`)) mis-wires the
                // Injects edge onto an unrelated local type. Everything
                // else resolves cross-file post-walk.
                if let Some(to_id) = node_ids
                    .iter()
                    .find(|(name, _, _)| *name == dep_base)
                    .map(|(_, _, id)| *id)
                {
                    if graph.upsert_edge(from_id, to_id, EdgeKind::Injects).is_ok() {
                        result.edges_created += 1;
                    } else {
                        result
                            .errors
                            .push(format!("{}: injects edge upsert failed", rel_path));
                    }
                } else if !dep_base.is_empty() {
                    pending_edges.push((from_id, dep_base, EdgeKind::Injects));
                }
            }
        }
    }

    // ── Post-walk edge resolution ───────────────────────────────────
    for (from_id, reference, kind) in pending_edges {
        if let Some(to_id) = resolve_reference(graph, &name_index, &reference) {
            if graph.upsert_edge(from_id, to_id, kind).is_ok() {
                result.edges_created += 1;
            }
        }
    }

    // ── Tombstone pass ─────────────────────────────────────────────
    // Nodes for files that no longer exist (deleted or renamed away) must
    // not linger as Active: FTS and queries keep returning phantoms, and a
    // renamed file creates a duplicate under the new path while the old one
    // stays. Mark them Deleted.
    if let Err(e) = graph.mark_absent_paths_deleted(project_root) {
        result.errors.push(format!("tombstone pass failed: {}", e));
    }

    result
}

/// The directory to walk: `src/` when present (Java/Kotlin convention),
/// otherwise the project root.
fn primary_source_root(project_root: &Path) -> PathBuf {
    let src = project_root.join("src");
    if src.is_dir() {
        src
    } else {
        project_root.to_path_buf()
    }
}

/// Well-known vendored/generated directory names to skip.
fn is_vendor_path(path: &Path) -> bool {
    path.components().any(|c| {
        matches!(
            c.as_os_str().to_str().unwrap_or(""),
            "node_modules" | "vendor" | "target" | "dist" | "build" | ".git" | "venv"
                | ".venv" | "__pycache__" | ".tox" | "site-packages"
        )
    })
}

/// Derive a dotted package from the file's directory path:
/// `src/com/foo/Bar.java` → `src.com.foo` is wrong, but callers pass the
/// PROJECT-RELATIVE path; `app/models/user.py` → `app.models.user` module
/// grouping → package `app.models`.
fn derive_package(rel_path: &str) -> String {
    let path = rel_path.replace('\\', "/");
    let Some(slash) = path.rfind('/') else {
        return String::new();
    };
    let dir = &path[..slash];
    let dir = dir.split('/').filter(|s| !s.is_empty() && *s != "src");
    dir.collect::<Vec<_>>().join(".")
}

/// Build the FQ name for a method/field member.
fn member_fqcn(language: &str, type_fqcn: &str, member: &str, is_method: bool) -> String {
    let sep = if language == "rust" || language == "go" { "::" } else { "." };
    if is_method {
        format!("{}{}{}()", type_fqcn, sep, member)
    } else {
        format!("{}{}{}", type_fqcn, sep, member)
    }
}

/// Resolve a reference (FQCN, dotted name, or simple name) to an ingested
/// node id — via exact FQCN/reference matching (FTS) first, then the
/// in-memory simple-name index as a fallback.
///
/// Exact-match first matters: the simple-name index is first-wins across
/// files, so consulting it before the exact path would let an unrelated
/// same-named type from another file hijack a fully-qualified reference.
fn resolve_reference(
    graph: &DependencyGraph,
    name_index: &HashMap<String, NodeId>,
    reference: &str,
) -> Option<NodeId> {
    // Exact FQCN / reference match first (node_matches_reference accepts
    // the full FQCN, the simple tail, and Pega rule-name variants).
    let results = graph.query_fts(reference, 10).ok()?;
    if let Some(node) = results
        .iter()
        .find(|n| pega::node_matches_reference(&n.fqcn, reference))
    {
        return Some(node.id);
    }
    // Fallback: simple-name index (cross-file resolution by simple name).
    let simple = reference.rsplit('.').next().unwrap_or(reference);
    let simple = simple.rsplit("::").next().unwrap_or(simple);
    name_index.get(simple).copied()
}

/// Whether the target project contains source artifacts NeuroCode can
/// ingest (generalized from the original Java-only `project_has_java`,
/// T065/FR-015). Returns true when ANY supported source file exists in the
/// source tree (bounded walk: `src/` or the root, depth ≤ 10, first ~500
/// entries) or when a Pega marker is present — a `build.gradle`/`pom.xml`
/// mentioning `com.pega`, or any `Rule-*` file.
///
/// Designed to be fast enough for the assembly hot path: no file contents
/// are read except the (bounded, at-most-two) build files.
pub fn project_has_source(project_root: &Path) -> bool {
    let scan_root = primary_source_root(project_root);

    // Bounded walk: stop as soon as a supported source file or Rule-* file
    // is found, or after ~500 entries (a genuine code project shows up
    // quickly).
    let mut entries = 0usize;
    for entry in walkdir::WalkDir::new(&scan_root)
        .max_depth(10)
        .into_iter()
        .filter_map(Result::ok)
    {
        entries += 1;
        if entries > 500 {
            break;
        }
        let path = entry.path();
        if entry.file_type().is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if registry::is_supported_extension(&ext.to_lowercase()) && !is_vendor_path(path) {
                    return true;
                }
            }
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .map_or(false, |n| n.starts_with("Rule-"))
            {
                return true;
            }
        }
    }

    // Pega markers: build files referencing com.pega (bounded read).
    for build_file in ["build.gradle", "pom.xml"] {
        let path = project_root.join(build_file);
        if let Ok(content) = std::fs::read_to_string(&path) {
            if content.contains("com.pega") {
                return true;
            }
        }
    }
    false
}

/// Backward-compatible alias: the original T065 gate was Java-only; it now
/// answers "does this project have ingestible source of any language".
pub fn project_has_java(project_root: &Path) -> bool {
    project_has_source(project_root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::{ArtifactKind, CodeArtifactNode};

    fn node_by_fqcn(graph: &DependencyGraph, fqcn: &str) -> CodeArtifactNode {
        graph
            .query_fts(fqcn, 10)
            .unwrap()
            .into_iter()
            .find(|n| n.fqcn == fqcn)
            .unwrap_or_else(|| panic!("node {fqcn} should be ingested"))
    }

    fn has_edge(graph: &DependencyGraph, from: NodeId, to: NodeId, kind: EdgeKind) -> bool {
        graph
            .traverse_edges(from, Some(kind))
            .map(|edges| edges.iter().any(|(t, _)| *t == to))
            .unwrap_or(false)
    }

    /// Bug: `name.ends_with(iface)` mis-wired Implements onto an unrelated
    /// local type (`Client implements Service` in a file also declaring
    /// `PaymentService`). Exact match only; everything else resolves
    /// cross-file post-walk.
    #[test]
    fn implements_suffix_does_not_miswire_to_unrelated_local_type() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("Local.java"),
            "package com.example;\npublic class PaymentService {}\npublic class Client implements Service {}\n",
        )
        .unwrap();
        std::fs::write(
            src.join("Service.java"),
            "package com.example;\npublic interface Service {}\n",
        )
        .unwrap();

        let graph = DependencyGraph::open_in_memory().unwrap();
        let result = ingest_project(&graph, tmp.path());
        assert!(result.errors.is_empty(), "ingestion errors: {:?}", result.errors);

        let client = node_by_fqcn(&graph, "com.example.Client");
        let service = node_by_fqcn(&graph, "com.example.Service");
        let payment = node_by_fqcn(&graph, "com.example.PaymentService");

        assert!(
            has_edge(&graph, client.id, service.id, EdgeKind::Implements),
            "implements must resolve to the real Service interface (cross-file)"
        );
        assert!(
            !has_edge(&graph, client.id, payment.id, EdgeKind::Implements),
            "suffix match must not mis-wire Implements onto PaymentService"
        );
    }

    /// Same suffix bug on the Injects path: `@Autowired Service` in a class
    /// named `OrderService` created a self-edge via
    /// `OrderService`.ends_with(`Service`). The field is named
    /// `paymentService` (not `service`) so the field node's own tokens can't
    /// hijack the cross-file resolution.
    #[test]
    fn injects_suffix_does_not_miswire_to_unrelated_local_type() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("Order.java"),
            "package com.example;\nimport org.springframework.beans.factory.annotation.Autowired;\npublic class OrderService {\n    @Autowired\n    private Service paymentService;\n}\n",
        )
        .unwrap();
        std::fs::write(
            src.join("Service.java"),
            "package com.example;\npublic interface Service {}\n",
        )
        .unwrap();

        let graph = DependencyGraph::open_in_memory().unwrap();
        let result = ingest_project(&graph, tmp.path());
        assert!(result.errors.is_empty(), "ingestion errors: {:?}", result.errors);

        let order = node_by_fqcn(&graph, "com.example.OrderService");
        let service = node_by_fqcn(&graph, "com.example.Service");

        assert!(
            has_edge(&graph, order.id, service.id, EdgeKind::Injects),
            "injects must resolve to the real Service interface (cross-file)"
        );
        assert!(
            !has_edge(&graph, order.id, order.id, EdgeKind::Injects),
            "suffix match must not create an OrderService→OrderService self-edge"
        );
    }

    /// Bug: the simple-name index (first-wins across files) was consulted
    /// BEFORE exact-FQCN matching, so a fully-qualified reference could be
    /// hijacked by an unrelated same-named type. Exact match must win.
    #[test]
    fn resolve_reference_prefers_exact_fqcn_over_simple_name_index() {
        let graph = DependencyGraph::open_in_memory().unwrap();
        let a = graph
            .upsert_node(&CodeArtifactNode::new(
                ArtifactKind::Class,
                "com.ex.Service".into(),
                "com.ex".into(),
                "src/a/Service.java".into(),
            ))
            .unwrap();
        let b = graph
            .upsert_node(&CodeArtifactNode::new(
                ArtifactKind::Class,
                "org.other.Service".into(),
                "org.other".into(),
                "src/b/Service.java".into(),
            ))
            .unwrap();
        // Simulate the first-wins simple-name index landing on the WRONG one.
        let mut name_index: HashMap<String, NodeId> = HashMap::new();
        name_index.insert("Service".to_string(), b);

        assert_eq!(
            resolve_reference(&graph, &name_index, "com.ex.Service"),
            Some(a),
            "exact FQCN must beat the simple-name index"
        );

        // The simple-name fallback still applies when nothing matches exactly.
        let c = graph
            .upsert_node(&CodeArtifactNode::new(
                ArtifactKind::Class,
                "Widget".into(),
                String::new(),
                "src/w.rs".into(),
            ))
            .unwrap();
        let mut idx: HashMap<String, NodeId> = HashMap::new();
        idx.insert("Widget".to_string(), c);
        assert_eq!(
            resolve_reference(&graph, &idx, "some.pkg.Widget"),
            Some(c),
            "simple-name fallback must still resolve cross-file references"
        );
    }
}
