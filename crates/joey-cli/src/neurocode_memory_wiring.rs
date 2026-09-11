//! Production adaptive-memory wiring (feature 027, T012+T014): the
//! `joey_agent_core::memory_hook::MemoryRuntime` implementation the turn
//! loop calls for per-turn prefetch (`prefetch_block`) and post-turn
//! capture (`capture_turn`). agent-core only sees the narrow trait; this
//! module composes the joey-neurocode stores (episodes/preferences), the
//! heuristic + provider distillers, and the joey-neurocode-rag memory
//! search/embed helpers — mirroring the `neurocode_rag_wiring` precedent.
//!
//! Failure contract: prefetch returns `None` on ANY error (silent
//! omission per the injection contract); capture is fire-and-forget on a
//! dedicated std::thread, each step guarded so a hard (sqlite/store)
//! failure skips the later steps and nothing ever panics the caller.

use std::path::{Path, PathBuf};

use joey_agent_core::memory_hook::{
    clamp_char_limit, format_memory_block, MemoryRuntime, MemoryTurnSummary,
};
use joey_neurocode::graph::{project_graph_db_path, GraphStore};
use joey_neurocode::memory::distill::{
    cosine_f32, DistilledPreference, HeuristicDistiller, MemoryDistiller, RECURRENCE_COSINE,
};
use joey_neurocode::memory::episodes::{
    EpisodeKind, EpisodeOutcome, EpisodeSource, EpisodeStore, MemoryEpisode,
};
use joey_neurocode::memory::preferences::{PreferenceOrigin, PreferenceStore};
use joey_neurocode_rag::config::{MemoryConfig, RagConfig};
use joey_neurocode_rag::memory_search::{
    embed_texts, index_memory_vector, search_memory, MemorySearchRequest,
};
use joey_providers::{resolve_profile, Message, ProviderClient, ProviderRequest};

/// Cap a string at ~`n` chars (char-boundary safe).
fn cap_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// ---------------------------------------------------------------------------
// ProductionMemoryRuntime
// ---------------------------------------------------------------------------

/// The production [`MemoryRuntime`]: per-project graph.db-backed prefetch
/// + capture. Installed by [`install_memory_runtime`] only when
/// `neurocode.memory.enabled` (default off ⇒ byte-identical parity).
pub struct ProductionMemoryRuntime {
    project_root: PathBuf,
    session_key: String,
}

impl ProductionMemoryRuntime {
    pub fn new(project_root: PathBuf, session_key: String) -> Self {
        Self { project_root, session_key }
    }
}

impl MemoryRuntime for ProductionMemoryRuntime {
    /// Mirrors `MemoryConfig.enabled` — keyword-only rag degradation is
    /// allowed, so no backend resolvability is required here.
    fn enabled(&self) -> bool {
        let config = joey_core::Config::load().unwrap_or_else(|_| joey_core::Config::defaults());
        MemoryConfig::load(&config).enabled
    }

    /// Bounded injection block for the current prompt: hybrid
    /// [`search_memory`] over the per-project memory tables, episodes from
    /// the fused hits, preferences from the active set, formatted through
    /// [`format_memory_block`]. ANY error → `None` (silent omission).
    fn prefetch_block(&self, prompt: &str) -> Option<String> {
        let config = joey_core::Config::load().unwrap_or_else(|_| joey_core::Config::defaults());
        let mem = MemoryConfig::load(&config);
        if !mem.enabled {
            return None;
        }
        let rag = RagConfig::load(&config);
        let db_path = project_graph_db_path(&self.project_root);
        // GraphStore::open applies the schema (memory tables) and gives the
        // raw connection the search helper needs.
        let graph = GraphStore::open(&db_path).ok()?;
        let conn = graph.conn();
        let top_k = mem.top_k.max(1) as usize;
        let req = MemorySearchRequest {
            query: cap_chars(prompt, 2000),
            top_k,
        };
        let hits = search_memory(conn, &rag, None, &req).ok()?;
        drop(graph);

        // Preferences: full rows from the active set (explicit-first
        // ordering is the store's), lines "category: statement (origin,
        // confidence N%)".
        let prefs = PreferenceStore::open(&db_path).ok()?;
        let pref_lines: Vec<String> = prefs
            .resolve_active(None, top_k)
            .ok()?
            .into_iter()
            .map(|p| {
                format!(
                    "{}: {} ({}, confidence {}%)",
                    p.category,
                    p.statement,
                    p.origin.as_str(),
                    p.confidence
                )
            })
            .collect();

        // Episodes: full rows via the store on the fused episode-hit ids,
        // lines "title — outcome (date): first ~160 chars of task".
        let eps = EpisodeStore::open(&db_path).ok()?;
        let mut ep_lines: Vec<String> = Vec::new();
        for hit in hits.iter().filter(|h| h.item_kind == "episode") {
            if let Ok(Some(e)) = eps.get(&hit.item_id) {
                ep_lines.push(format!(
                    "{} — {} ({}): {}",
                    e.title,
                    e.outcome.as_str(),
                    e.created_at,
                    cap_chars(&e.task, 160)
                ));
            }
        }

        let block = format_memory_block(
            &pref_lines,
            &ep_lines,
            clamp_char_limit(mem.injection_char_limit),
        );
        if block.is_empty() {
            None
        } else {
            Some(block)
        }
    }

    /// Fire-and-forget capture on a dedicated thread (the trait contract
    /// demands heavy work off the caller's thread; the turn loop never
    /// waits on this).
    fn capture_turn(&self, summary: &MemoryTurnSummary) {
        let project_root = self.project_root.clone();
        let session_key = self.session_key.clone();
        let summary = summary.clone();
        std::thread::spawn(move || {
            capture_offthread(&project_root, &session_key, &summary);
        });
    }
}

// ---------------------------------------------------------------------------
// Capture body (runs on the spawned thread)
// ---------------------------------------------------------------------------

/// The capture sequence. Steps in order; a hard store failure at any step
/// skips the later steps; embedding failures degrade silently (rows stay
/// keyword-searchable per FR-008); never panics.
fn capture_offthread(project_root: &Path, session_key: &str, summary: &MemoryTurnSummary) {
    // Step 1: gate.
    let config = joey_core::Config::load().unwrap_or_else(|_| joey_core::Config::defaults());
    let mem = MemoryConfig::load(&config);
    if !mem.enabled {
        return;
    }
    let rag = RagConfig::load(&config);

    // One GraphStore open gives the shared conn for vector indexing; the
    // stores own their own connections on the same graph.db file.
    let db_path = project_graph_db_path(project_root);
    let graph = match GraphStore::open(&db_path) {
        Ok(g) => g,
        Err(_) => return,
    };
    let conn = graph.conn();
    let pref_store = match PreferenceStore::open(&db_path) {
        Ok(s) => s,
        Err(_) => return,
    };
    let ep_store = match EpisodeStore::open(&db_path) {
        Ok(s) => s,
        Err(_) => return,
    };

    // Step 2: explicit preference distillation from the user's own words.
    let distiller = HeuristicDistiller;
    for d in distiller.detect_explicit(&summary.user_prompt) {
        if upsert_preference(
            conn,
            &pref_store,
            &rag,
            &d.category,
            &d.statement,
            PreferenceOrigin::Explicit,
            &[],
        )
        .is_err()
        {
            return; // hard store failure: skip later steps
        }
    }

    // Step 3 (T014 substance): episode assembly — one row per completed
    // task. Ok(None) = redacted-empty: skip everything.
    let episode = MemoryEpisode {
        id: String::new(), // store stamps a content-derived id
        kind: EpisodeKind::Task,
        title: cap_chars(&summary.user_prompt, 200),
        task: summary.user_prompt.clone(),
        context: summary.files_touched.join(", "),
        approach: summary.assistant_final.clone(),
        outcome: if summary.turn_error {
            EpisodeOutcome::Failure
        } else {
            EpisodeOutcome::Success
        },
        lessons: String::new(),
        source: EpisodeSource::Interactive,
        origin_run: session_key.to_string(),
        evidence_ids: Vec::new(),
        created_at: String::new(), // store stamps
        updated_at: String::new(), // store stamps
    };
    let episode_id = match ep_store.insert(&episode, None, mem.max_episodes.max(1) as usize) {
        Ok(Some(id)) => id,
        _ => return, // Ok(None) redacted-empty OR store failure
    };
    // Embed title+task+approach+lessons and index the vector (best-effort;
    // the row itself is already persisted and keyword-searchable).
    let ep_text = [&episode.title, &episode.task, &episode.approach, &episode.lessons]
        .into_iter()
        .map(|s| s.as_str())
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if let Ok(vectors) = embed_texts(&rag, None, &[ep_text]) {
        if vectors.len() == 1 {
            let _ = index_memory_vector(conn, &episode_id, "episode", &vectors[0], false);
        }
    }

    // Step 4: continuous heuristic distillation from the episode.
    for d in distiller.distill_episode(&episode) {
        if upsert_preference(
            conn,
            &pref_store,
            &rag,
            &d.category,
            &d.statement,
            PreferenceOrigin::Inferred,
            &[episode_id.clone()],
        )
        .is_err()
        {
            return;
        }
    }

    // Step 5: provider-backed distillation (one cheap non-streaming chat
    // completion). Any failure silently skips — the heuristic passes
    // already ran.
    if let Some(candidates) = ProviderDistiller::distill(&config, &mem, summary) {
        for d in candidates {
            if upsert_preference(
                conn,
                &pref_store,
                &rag,
                &d.category,
                &d.statement,
                PreferenceOrigin::Inferred,
                &[episode_id.clone()],
            )
            .is_err()
            {
                return;
            }
        }
    }
}

/// Upsert one preference with embedding-recurrence strengthening:
///
/// 1. One `embed_texts` batch over `[statement, same-category active
///    statements...]` (when a backend resolves); cosine ≥
///    [`RECURRENCE_COSINE`] against any candidate strengthens that row
///    instead of inserting a duplicate.
/// 2. `PreferenceStore::upsert` (sanitization + confidence bump happen
///    in-store).
/// 3. Index the statement embedding via `index_memory_vector` on the
///    graph conn (best-effort; the row persists without it).
///
/// `Err` only for hard store failures (embedding failures degrade to a
/// plain insert).
fn upsert_preference(
    conn: &rusqlite::Connection,
    prefs: &PreferenceStore,
    rag: &RagConfig,
    category: &str,
    statement: &str,
    origin: PreferenceOrigin,
    evidence: &[String],
) -> Result<(), String> {
    let mut strengthen_id: Option<String> = None;
    let mut statement_vector: Option<Vec<f32>> = None;
    let same_category = prefs
        .resolve_active(Some(category), 100)
        .map_err(|e| e.to_string())?;
    if !same_category.is_empty() {
        let mut texts = vec![statement.to_string()];
        texts.extend(same_category.iter().map(|p| p.statement.clone()));
        if let Ok(vectors) = embed_texts(rag, None, &texts) {
            if vectors.len() == texts.len() {
                statement_vector = Some(vectors[0].clone());
                let mut best: Option<(usize, f32)> = None;
                for (idx, _) in same_category.iter().enumerate() {
                    let c = cosine_f32(&vectors[0], &vectors[idx + 1]);
                    if c >= RECURRENCE_COSINE && best.map_or(true, |(_, b)| c > b) {
                        best = Some((idx, c));
                    }
                }
                if let Some((idx, _)) = best {
                    strengthen_id = Some(same_category[idx].id.clone());
                }
            }
        }
    }
    let outcome = prefs
        .upsert(
            category,
            statement,
            origin,
            evidence,
            strengthen_id.as_deref(),
            None,
            None,
        )
        .map_err(|e| e.to_string())?;
    if let Some(v) = statement_vector {
        let _ = index_memory_vector(conn, &outcome.id, "preference", &v, false);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Provider-backed distiller (Q3)
// ---------------------------------------------------------------------------

/// Resolve the distillation model: `neurocode.memory.distill_model` wins,
/// else the NeuroCode economical tier (`neurocode.tier.providers.<p>`
/// `.economical`, falling back to the flat `neurocode.tier.economical`
/// `.model` the same way `TierConfig::tiers_for_provider` does). Empty →
/// `None` (skip provider distillation).
fn resolve_distill_model(config: &joey_core::Config, mem: &MemoryConfig) -> Option<String> {
    let pinned = mem.distill_model.trim();
    if !pinned.is_empty() {
        return Some(pinned.to_string());
    }
    let provider = config.get_str("model.provider", "").trim().to_string();
    if !provider.is_empty() && provider != "auto" {
        let scoped = config
            .get_str(&format!("neurocode.tier.providers.{provider}.economical"), "")
            .trim()
            .to_string();
        if !scoped.is_empty() {
            return Some(scoped);
        }
    }
    let flat = config
        .get_str("neurocode.tier.economical.model", "")
        .trim()
        .to_string();
    (!flat.is_empty()).then_some(flat)
}

/// Provider-backed distillation pass: a single non-streaming chat
/// completion on the agent's provider (via `joey_providers`' public API
/// directly — no Agent, no orchestration) asking for 0-3
/// `category|statement` lines. ANY error (no model, no credentials,
/// HTTP, parse) → `None`, silently.
struct ProviderDistiller;

impl ProviderDistiller {
    fn distill(
        config: &joey_core::Config,
        mem: &MemoryConfig,
        summary: &MemoryTurnSummary,
    ) -> Option<Vec<DistilledPreference>> {
        let model = resolve_distill_model(config, mem)?;
        let provider = config.get_str("model.provider", "auto");
        let base_url = config.get_str("model.base_url", "");
        let profile = resolve_profile(&provider, &base_url, &model);
        let base_override = if base_url.trim().is_empty() {
            None
        } else {
            Some(base_url)
        };
        let client = ProviderClient::new(profile, base_override, None).ok()?;
        let prompt = format!(
            "Distill durable user coding preferences from this coding-session turn.\n\
             Reply with 0 to 3 lines, each exactly 'category|statement'.\n\
             category is one of: naming, error-handling, testing, libraries, formatting, structure.\n\
             statement is a short imperative sentence. No other text.\n\n\
             User request: {}\n\nAssistant outcome: {}",
            cap_chars(&summary.user_prompt, 2000),
            cap_chars(&summary.assistant_final, 2000),
        );
        let req = ProviderRequest::new(model, vec![Message::user(prompt)]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        let resp = rt.block_on(client.complete(&req)).ok()?;
        let mut out: Vec<DistilledPreference> = Vec::new();
        for line in resp.content.lines() {
            let line = line.trim().trim_end_matches('.');
            if line.is_empty() {
                continue;
            }
            let Some((category, statement)) = line.split_once('|') else {
                continue;
            };
            let (category, statement) = (category.trim(), statement.trim());
            if category.is_empty() || statement.is_empty() {
                continue;
            }
            out.push(DistilledPreference {
                category: category.to_string(),
                statement: statement.to_string(),
            });
            if out.len() == 3 {
                break;
            }
        }
        Some(out)
    }
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// Install the production memory runtime on an agent when
/// `neurocode.memory.enabled` — mirrors `install_rag_injections`'s shape
/// (no-op when disabled). Shared call site shape for the repl wiring.
pub fn install_memory_runtime(
    agent: &mut joey_agent_core::Agent,
    config: &joey_core::Config,
    project_root: &std::path::Path,
    session_key: &str,
) {
    if !MemoryConfig::load(config).enabled {
        return;
    }
    agent.set_memory_runtime(Some(std::sync::Arc::new(ProductionMemoryRuntime::new(
        project_root.to_path_buf(),
        session_key.to_string(),
    ))));
}
