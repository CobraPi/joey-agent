//! Tree-sitter ingestion pipeline (FR-006).

pub mod java;
pub mod pega;

use std::path::{Path, PathBuf};

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

/// Walk a project source tree, parse each `.java` file via tree-sitter, and
/// upsert nodes + edges into the graph store (T011). Pega rule patterns are
/// recognized during the same pass: matched types get `PegaMetadata` and
/// `ArtifactKind::PegaRule`, and `ReferencesRule`/`InheritsRule` edges are
/// emitted to the referenced/inherited rules (T058).
pub fn ingest_project(graph: &DependencyGraph, project_root: &Path) -> IngestionResult {
    let mut result = IngestionResult {
        files_scanned: 0,
        artifacts_seen: 0,
        edges_created: 0,
        errors: Vec::new(),
    };

    // T058: detect the Pega version once per ingestion run. An empty string
    // (no version detected) still allows pattern-based rule extraction.
    let pega_version = detect_pega_version(project_root, "").unwrap_or_default();

    // Pega rule-reference edges that can only be resolved after every node
    // in the tree has been upserted: (from_id, reference, EdgeKind).
    let mut pending_pega_edges: Vec<(NodeId, String, EdgeKind)> = Vec::new();

    let src = project_root.join("src");
    let scan_root: PathBuf = if src.is_dir() { src } else { project_root.to_path_buf() };

    for entry in walkdir::WalkDir::new(&scan_root)
        .max_depth(10)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.path().extension().map_or(false, |e| e == "java") {
            continue;
        }
        result.files_scanned += 1;
        let rel_path = entry
            .path()
            .strip_prefix(project_root)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .to_string();

        let content = match std::fs::read_to_string(entry.path()) {
            Ok(c) => c,
            Err(e) => {
                result.errors.push(format!("{}: {}", rel_path, e));
                continue;
            }
        };

        let extraction = match java::parse_java_file(&content) {
            Ok(ext) => ext,
            Err(e) => {
                result.errors.push(format!("{}: {}", rel_path, e));
                continue;
            }
        };

        // Upsert type-level nodes (classes, interfaces, enums).
        let mut node_ids: Vec<(String, ArtifactKind, crate::graph::NodeId)> = Vec::new();
        for type_node in &extraction.types {
            let fqcn = if type_node.package.is_empty() {
                type_node.name.clone()
            } else {
                format!("{}.{}", type_node.package, type_node.name)
            };
            let kind = match type_node.kind.as_str() {
                "interface" => ArtifactKind::Interface,
                "enum" => ArtifactKind::Enum,
                _ => ArtifactKind::Class,
            };
            let mut node = CodeArtifactNode::new(kind.clone(), fqcn.clone(), type_node.package.clone(), rel_path.clone());
            node.implemented_interfaces = type_node.implemented_interfaces.clone();
            node.annotations = type_node.annotations.clone();
            node.declared_dependencies = type_node.declared_dependencies.clone();
            node.source_span = Some((type_node.start_byte, type_node.end_byte));

            // T058: Pega rule extraction. The Java extractor surfaces
            // `implements` names (not superclasses), so Pega-pattern
            // interfaces are surfaced as `extends:<X>` pseudo-annotations
            // to let extract_pega_metadata recognize directed inheritance.
            // Simple dependency names are package-qualified so that rules in
            // `com.pega.*` namespaces are recognized as rule references.
            let mut pega_annotations = type_node.annotations.clone();
            for iface in &type_node.implemented_interfaces {
                // Prefer the bare name; otherwise try the package-qualified
                // form (a plain Java identifier in a `com.pega.*` package is
                // only recognizable as a rule when qualified).
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
            let mut kind = kind;
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

            let id = match graph.upsert_node(&node) {
                Ok(id) => id,
                Err(e) => {
                    result.errors.push(format!("{}: upsert failed: {}", rel_path, e));
                    continue;
                }
            };
            node_ids.push((type_node.name.clone(), kind, id));
            result.artifacts_seen += 1;

            // T058: queue ReferencesRule/InheritsRule edges for this rule
            // node; they are resolved after the full tree is upserted.
            if let Some(meta) = &node.pega_metadata {
                for reference in &meta.references_rules {
                    pending_pega_edges.push((id, reference.clone(), EdgeKind::ReferencesRule));
                }
                if let Some(parent) = &meta.inherits_from {
                    pending_pega_edges.push((id, parent.clone(), EdgeKind::InheritsRule));
                }
            }

            // Upsert method nodes.
            for method in &type_node.methods {
                let method_fqcn = format!("{}.{}()", fqcn, method.name);
                let mut m_node = CodeArtifactNode::new(
                    ArtifactKind::Method,
                    method_fqcn,
                    type_node.package.clone(),
                    rel_path.clone(),
                );
                m_node.enclosing_type = Some(type_node.name.clone());
                m_node.annotations = method.annotations.clone();
                m_node.source_span = Some((method.start_byte, method.end_byte));
                if let Ok(mid) = graph.upsert_node(&m_node) {
                    // Edge: method belongs to class (we use Injects for member-of).
                    let _ = graph.upsert_edge(mid, id, EdgeKind::Injects);
                    result.edges_created += 1;
                }
            }

            // Upsert field nodes.
            for field in &type_node.fields {
                let field_fqcn = format!("{}.{}", fqcn, field.name);
                let mut f_node = CodeArtifactNode::new(
                    ArtifactKind::Field,
                    field_fqcn,
                    type_node.package.clone(),
                    rel_path.clone(),
                );
                f_node.enclosing_type = Some(type_node.name.clone());
                f_node.annotations = field.annotations.clone();
                f_node.declared_dependencies = vec![field.type_name.clone()];
                if let Ok(fid) = graph.upsert_node(&f_node) {
                    let _ = graph.upsert_edge(fid, id, EdgeKind::Injects);
                    result.edges_created += 1;
                }
            }
        }

        // Create Implements edges between types in the same file.
        for type_node in &extraction.types {
            let _fqcn = if type_node.package.is_empty() {
                type_node.name.clone()
            } else {
                format!("{}.{}", type_node.package, type_node.name)
            };
            let from = node_ids
                .iter()
                .find(|(name, _, _)| *name == type_node.name)
                .map(|(_, _, id)| *id);
            if let Some(from_id) = from {
                for iface in &type_node.implemented_interfaces {
                    // Try to find the interface node (same file first, then cross-file).
                    let to_id = node_ids
                        .iter()
                        .find(|(name, _, _)| name == iface || name.ends_with(iface.as_str()))
                        .map(|(_, _, id)| *id);
                    if let Some(to_id) = to_id {
                        let _ = graph.upsert_edge(from_id, to_id, EdgeKind::Implements);
                        let _ = graph.upsert_edge(to_id, from_id, EdgeKind::IsImplementedBy);
                        result.edges_created += 2;
                    }
                    // Even if the interface is not in the same file, we note
                    // the dependency for later cross-file edge resolution.
                }
                // Injects edges for declared dependencies.
                for dep in &type_node.declared_dependencies {
                    let to_id = node_ids
                        .iter()
                        .find(|(name, _, _)| name == dep || name.ends_with(dep.as_str()))
                        .map(|(_, _, id)| *id);
                    if let Some(to_id) = to_id {
                        let _ = graph.upsert_edge(from_id, to_id, EdgeKind::Injects);
                        result.edges_created += 1;
                    }
                }
            }
        }
    }

    // T058: resolve pending Pega rule edges now that every node in the tree
    // has been upserted (cross-file references work regardless of walk order).
    for (from_id, reference, kind) in pending_pega_edges {
        if let Some(to_id) = find_node_by_reference(graph, &reference) {
            if graph.upsert_edge(from_id, to_id, kind).is_ok() {
                result.edges_created += 1;
            }
        }
    }

    result
}

/// Resolve a Pega rule reference (FQCN, dotted name, or `Rule-*`-style name)
/// to an ingested node id via FTS, requiring an exact FQCN/simple-name match.
fn find_node_by_reference(graph: &DependencyGraph, reference: &str) -> Option<NodeId> {
    let results = graph.query_fts(reference, 10).ok()?;
    results
        .iter()
        .find(|n| pega::node_matches_reference(&n.fqcn, reference))
        .map(|n| n.id)
}

/// Whether the target project contains enterprise Java/Pega artifacts
/// (T065, FR-015). Returns true when ANY `.java` file exists in the source
/// tree (bounded like `ingest_project`'s walk: `src/` or the root, depth ≤
/// 10, first ~500 entries) or when a Pega marker is present — a
/// `build.gradle`/`pom.xml` mentioning `com.pega`, or any `Rule-*` file.
///
/// Designed to be fast enough for the assembly hot path: no file contents
/// are read except the (bounded, at-most-two) build files.
pub fn project_has_java(project_root: &Path) -> bool {
    let src = project_root.join("src");
    let scan_root: PathBuf = if src.is_dir() { src } else { project_root.to_path_buf() };

    // Bounded walk: stop as soon as a .java file or a Rule-* file is found,
    // or after ~500 entries (a genuine Java/Pega project shows up quickly).
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
            if path.extension().map_or(false, |e| e == "java") {
                return true;
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
