//! Domain-knowledge ingestion and retrieval (FR-013/014, T043-T046,
//! T063/T064).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::graph::store::GraphStore;

/// The category of an ingested knowledge source (FR-013).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum KnowledgeCategory {
    /// Version-specific framework documentation (e.g., Spring Boot 3.2).
    FrameworkDocs,
    /// Entity/DTO catalogs (schema definitions).
    EntityCatalog,
    /// Historical postmortems (incident learnings).
    Postmortem,
    /// Pega Platform rule-type metadata (ingested from the built-in rule-type
    /// catalog, T060).
    PegaRuleType,
}

impl KnowledgeCategory {
    pub fn as_str(&self) -> &str {
        match self {
            KnowledgeCategory::FrameworkDocs => "FrameworkDocs",
            KnowledgeCategory::EntityCatalog => "EntityCatalog",
            KnowledgeCategory::Postmortem => "Postmortem",
            KnowledgeCategory::PegaRuleType => "PegaRuleType",
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "FrameworkDocs" | "framework_docs" => Ok(KnowledgeCategory::FrameworkDocs),
            "EntityCatalog" | "entity_catalog" => Ok(KnowledgeCategory::EntityCatalog),
            "Postmortem" | "postmortem" => Ok(KnowledgeCategory::Postmortem),
            "PegaRuleType" | "pega_rule_type" => Ok(KnowledgeCategory::PegaRuleType),
            other => Err(format!("unknown knowledge category '{}'", other)),
        }
    }
}

/// An ingested body of knowledge (data-model.md Entity 9).
#[derive(Debug, Clone)]
pub struct KnowledgeSource {
    pub category: KnowledgeCategory,
    pub source_path: String,
    pub version_tag: Option<String>,
    pub provenance: String,
}

/// A domain-knowledge retrieval result.
#[derive(Debug, Clone)]
pub struct DomainKnowledge {
    /// The category of the source this hit came from (None when the hit's
    /// registry row is unknown).
    pub category: Option<String>,
    pub content: String,
    pub provenance: String,
    pub version_tag: Option<String>,
}

// ── Ingestion (T063) ─────────────────────────────────────────────────────

/// Caps for directory ingestion (T063): at most 32 files / 512 KiB total.
const MAX_DIR_FILES: usize = 32;
const MAX_TOTAL_BYTES: usize = 512 * 1024;

/// Read the content of a knowledge source: a single file, or a directory
/// (all regular text files concatenated, in deterministic name order, up to
/// [`MAX_DIR_FILES`] files / [`MAX_TOTAL_BYTES`] total bytes, skipping
/// binary-looking content).
fn read_source_content(source_path: &str) -> Result<String, String> {
    let path = Path::new(source_path);
    let metadata = std::fs::metadata(path)
        .map_err(|e| format!("source path '{}' not readable: {}", source_path, e))?;
    if metadata.is_file() {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read '{}': {}", source_path, e))?;
        if looks_binary(&content) {
            return Err(format!("source '{}' looks binary; not ingesting", source_path));
        }
        return Ok(content);
    }
    if metadata.is_dir() {
        let mut out = String::new();
        let mut files = 0usize;
        let mut total = 0usize;
        for entry in walkdir::WalkDir::new(path)
            .max_depth(6)
            .sort_by_file_name()
            .into_iter()
            .filter_map(Result::ok)
        {
            if files >= MAX_DIR_FILES || total >= MAX_TOTAL_BYTES {
                break;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(entry.path()) else {
                continue; // unreadable / non-UTF-8 → skip
            };
            if looks_binary(&content) {
                continue;
            }
            files += 1;
            let room = MAX_TOTAL_BYTES.saturating_sub(total);
            let take = content.len().min(room);
            // Truncate on a char boundary.
            let mut end = take;
            while end > 0 && !content.is_char_boundary(end) {
                end -= 1;
            }
            out.push_str(&content[..end]);
            out.push('\n');
            total += end + 1;
        }
        if out.trim().is_empty() {
            return Err(format!(
                "directory '{}' contained no readable text files",
                source_path
            ));
        }
        return Ok(out);
    }
    Err(format!(
        "source path '{}' is neither a file nor a directory",
        source_path
    ))
}

/// Heuristic binary check: a NUL byte, or a high ratio of non-whitespace
/// control characters in the prefix, marks content as non-text.
fn looks_binary(content: &str) -> bool {
    if content.contains('\0') {
        return true;
    }
    let prefix: Vec<char> = content.chars().take(4096).collect();
    if prefix.is_empty() {
        return false;
    }
    let controls = prefix
        .iter()
        .filter(|c| c.is_control() && !c.is_whitespace())
        .count();
    controls * 10 > prefix.len()
}

/// Ingest a knowledge source (T063, FR-013): read the file (or directory) at
/// `source.source_path`, register the source in the `domain_knowledge`
/// registry, and index its content into `domain_knowledge_fts` so it can be
/// retrieved during context assembly. Returns the registry row id.
///
/// Registration and indexing are kept rowid-aligned (the FTS rowid equals
/// the registry id), which is what category filtering and conflict
/// resolution (T064) rely on.
pub fn ingest_source(store: &GraphStore, source: &KnowledgeSource) -> Result<u64, String> {
    let content = read_source_content(&source.source_path)?;
    let id = store
        .upsert_domain_knowledge(
            source.category.as_str(),
            &source.source_path,
            source.version_tag.as_deref(),
            &source.provenance,
        )
        .map_err(|e| format!("failed to register source: {}", e))?;
    store
        .index_domain_content(&content, &source.provenance, source.version_tag.as_deref())
        .map_err(|e| format!("failed to index content: {}", e))?;
    Ok(id)
}

// ── Retrieval + conflict resolution (T063/T064) ──────────────────────────

/// Registry-side info for one ingested source (id-joined view of
/// `list_domain_sources`).
struct SourceInfo {
    category: String,
    version_tag: Option<String>,
    ingested_at: String,
}

/// Build an id → source-info map from the registry.
fn registry(store: &GraphStore) -> HashMap<i64, SourceInfo> {
    store
        .list_domain_sources()
        .unwrap_or_default()
        .into_iter()
        .map(|s| {
            (
                s.id,
                SourceInfo {
                    category: s.category,
                    version_tag: s.version_tag,
                    ingested_at: s.ingested_at,
                },
            )
        })
        .collect()
}

/// Per-category conflict winners (T064). Resolution semantics: when two or
/// more sources in a category have overlapping version tags (the same
/// `Some(v)`, or a `None` tag, which overlaps everything in its category),
/// the most recently ingested source wins and the other sources' content is
/// not retrievable.
struct CategoryWinners {
    /// Newest `None`-tagged source: `(ingested_at, id)`.
    newest_none: Option<(String, i64)>,
    /// Newest source per explicit version tag.
    newest_per_version: HashMap<String, (String, i64)>,
    /// Newest source overall in the category.
    newest_all: Option<(String, i64)>,
}

fn newer(a: &Option<(String, i64)>, b: &Option<(String, i64)>) -> Option<(String, i64)> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if x >= y { x.clone() } else { y.clone() }),
        (Some(x), None) => Some(x.clone()),
        (None, y) => y.clone(),
    }
}

fn winners_by_category(reg: &HashMap<i64, SourceInfo>) -> HashMap<String, CategoryWinners> {
    let mut by_cat: HashMap<String, CategoryWinners> = HashMap::new();
    for (&id, info) in reg {
        let w = by_cat
            .entry(info.category.clone())
            .or_insert_with(|| CategoryWinners {
                newest_none: None,
                newest_per_version: HashMap::new(),
                newest_all: None,
            });
        // `(ingested_at, id)` ordering: RFC3339 timestamp first, rowid as a
        // monotonic tiebreak (later ingestion → higher rowid).
        let cand = (info.ingested_at.clone(), id);
        if newer(&w.newest_all, &Some(cand.clone())) == Some(cand.clone()) {
            w.newest_all = Some(cand.clone());
        }
        match &info.version_tag {
            None => {
                if newer(&w.newest_none, &Some(cand.clone())) == Some(cand.clone()) {
                    w.newest_none = Some(cand);
                }
            }
            Some(v) => {
                let cur = w.newest_per_version.get(v).cloned();
                if newer(&cur, &Some(cand.clone())) == Some(cand.clone()) {
                    w.newest_per_version.insert(v.clone(), cand);
                }
            }
        }
    }
    by_cat
}

/// The winning (newest) source id for a source with the given version tag
/// in a category, or None when there is no candidate.
fn winner_for(w: &CategoryWinners, version: &Option<String>) -> Option<i64> {
    let best = match version {
        // A `Some(v)` source conflicts with same-version sources AND every
        // `None`-tagged source in its category.
        Some(v) => newer(&w.newest_per_version.get(v).cloned(), &w.newest_none),
        // A `None`-tagged source conflicts with everything in its category.
        None => w.newest_all.clone(),
    };
    best.map(|(_, id)| id)
}

/// Retrieve domain knowledge by FTS query, optionally filtered by category
/// (T063, FR-013). Conflict-aware (T064): when several sources in a category
/// have overlapping version tags, only the most recently ingested source's
/// content is returned. Content whose registry row was removed is not
/// returned (removal hides the content).
pub fn retrieve(
    store: &GraphStore,
    query: &str,
    category: Option<&KnowledgeCategory>,
    limit: usize,
) -> Vec<DomainKnowledge> {
    if limit == 0 {
        return Vec::new();
    }
    let reg = registry(store);
    let winners = winners_by_category(&reg);
    let category_filter: Option<HashSet<i64>> = category.map(|c| {
        store
            .fts_domain_ids_by_category(c.as_str())
            .unwrap_or_default()
            .into_iter()
            .collect()
    });

    // Over-fetch so category/conflict filtering can still fill `limit` hits.
    let rows = match store.query_domain_fts(query, limit.saturating_mul(4)) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut out = Vec::new();
    for row in rows {
        if let Some(f) = &category_filter {
            if !f.contains(&row.id) {
                continue;
            }
        }
        let Some(info) = reg.get(&row.id) else {
            continue; // orphaned FTS content (registry row removed) → hidden
        };
        // Conflict resolution (T064): drop this hit when a newer source with
        // an overlapping version tag exists in the same category.
        if let Some(winner) = winners.get(&info.category).and_then(|w| winner_for(w, &info.version_tag))
        {
            if winner != row.id {
                continue;
            }
        }
        out.push(DomainKnowledge {
            category: Some(info.category.clone()),
            content: row.content,
            provenance: row.provenance,
            version_tag: row.version_tag,
        });
        if out.len() >= limit {
            break;
        }
    }
    out
}

/// A conflict between two or more ingested sources (T064, spec edge case
/// "conflicting sources").
#[derive(Debug, Clone)]
pub struct ConflictReport {
    pub category: String,
    pub version_tag: Option<String>,
    /// `(id, provenance, ingested_at)` — newest first.
    pub sources: Vec<(i64, String, String)>,
}

/// Detect conflicting domain-knowledge sources (T064): two or more sources
/// with the same category and overlapping version tags — the same `Some(v)`,
/// or a `None` tag, which overlaps everything in its category.
pub fn resolve_conflicts(store: &GraphStore) -> Vec<ConflictReport> {
    let sources = match store.list_domain_sources() {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    // Group explicit-version sources by (category, version); collect
    // None-tagged sources per category. The listing is newest-first.
    let mut version_groups: HashMap<(String, String), Vec<(i64, String, String)>> =
        HashMap::new();
    let mut none_by_cat: HashMap<String, Vec<(i64, String, String)>> = HashMap::new();
    for s in &sources {
        let triple = (s.id, s.provenance.clone(), s.ingested_at.clone());
        match &s.version_tag {
            Some(v) => version_groups
                .entry((s.category.clone(), v.clone()))
                .or_default()
                .push(triple),
            None => none_by_cat
                .entry(s.category.clone())
                .or_default()
                .push(triple),
        }
    }

    let mut reports: Vec<ConflictReport> = Vec::new();
    for ((category, version), mut group) in version_groups {
        // None-tagged sources in the same category overlap this version group.
        if let Some(nones) = none_by_cat.get(&category) {
            group.extend(nones.iter().cloned());
        }
        if group.len() >= 2 {
            reports.push(ConflictReport {
                category,
                version_tag: Some(version),
                sources: group,
            });
        }
    }
    for (category, nones) in none_by_cat {
        if nones.len() >= 2 {
            reports.push(ConflictReport {
                category,
                version_tag: None,
                sources: nones,
            });
        }
    }
    reports.sort_by(|a, b| {
        a.category
            .cmp(&b.category)
            .then_with(|| a.version_tag.cmp(&b.version_tag))
    });
    reports
}
