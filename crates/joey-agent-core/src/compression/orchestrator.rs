//! Compression orchestration on the agent (port of
//! `agent/conversation_compression.py` `compress_context` +
//! `check_compression_model_feasibility` + the lock lease refresher).
//!
//! The port always compacts IN PLACE (upstream `compression.in_place: true`,
//! the shipped default): the transcript is rewritten and the system prompt
//! refreshed under the SAME session id via `SessionDb::archive_and_compact`
//! — no session rotation, no parent/child lineage. The legacy
//! rotate-to-child path (and its title renumbering / goal migration) is not
//! ported.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use joey_core::state::{Role, StoredMessage};
use joey_core::SessionDb;
use joey_providers::Message;
use serde_json::json;
use tokio::sync::mpsc;

use super::anchors::ensure_compressed_has_user_turn;
use super::catalog::MINIMUM_CONTEXT_LENGTH;
use super::compressor::commafy;
use super::estimator::estimate_request_tokens_rough;
use crate::agent::Agent;
use crate::events::AgentEvent;
use crate::prompt::{build_system_prompt, PromptInputs};

/// Stable marker the gateway matches on to re-tag the auto-compaction
/// lifecycle status (conversation_compression.py:52).
pub const COMPACTION_STATUS_MARKER: &str = "Compacting context";
/// conversation_compression.py:53-55.
pub static COMPACTION_STATUS: once_cell::sync::Lazy<String> = once_cell::sync::Lazy::new(|| {
    format!(
        "🗜️ {} — summarizing earlier conversation so I can continue...",
        COMPACTION_STATUS_MARKER
    )
});

/// Lock lease TTL (upstream `_compression_lock_ttl_seconds` default).
const COMPRESSION_LOCK_TTL_SECONDS: f64 = 300.0;

// ---------------------------------------------------------------------------
// Lock lease refresher (`_CompressionLockLeaseRefresher`)
// ---------------------------------------------------------------------------

/// Background lease refresher: extends the compression lock while the
/// summary call runs, so a slow compaction cannot lose the lock mid-flight.
/// Tolerates transient refresh failures for at most one lease's worth of
/// time (ttl / interval consecutive failures), so the lock can never be held
/// past its TTL by a stuck refresher.
pub(crate) struct CompressionLockLeaseRefresher {
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl CompressionLockLeaseRefresher {
    pub(crate) fn start(
        db: Arc<Mutex<SessionDb>>,
        session_id: String,
        holder: String,
        ttl_seconds: f64,
        refresh_interval_seconds: Option<f64>,
    ) -> Self {
        let interval = refresh_interval_seconds
            .unwrap_or_else(|| (ttl_seconds / 2.0).clamp(1.0, 60.0))
            .max(0.1);
        let max_consecutive_failures = ((ttl_seconds / interval) as u64).max(1);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_flag = stop.clone();
        let handle = std::thread::Builder::new()
            .name("compression-lock-refresh".to_string())
            .spawn(move || {
                let mut consecutive_failures: u64 = 0;
                loop {
                    // Sleep in small slices so stop() takes effect promptly.
                    let mut waited = 0.0_f64;
                    while waited < interval {
                        if stop_flag.load(std::sync::atomic::Ordering::SeqCst) {
                            return;
                        }
                        let slice = (interval - waited).min(0.1);
                        std::thread::sleep(Duration::from_secs_f64(slice));
                        waited += slice;
                    }
                    if stop_flag.load(std::sync::atomic::Ordering::SeqCst) {
                        return;
                    }
                    let refreshed = {
                        let guard = db.lock().unwrap_or_else(|p| p.into_inner());
                        guard.refresh_compression_lock(&session_id, &holder, ttl_seconds)
                    };
                    if refreshed {
                        consecutive_failures = 0;
                        continue;
                    }
                    consecutive_failures += 1;
                    if consecutive_failures >= max_consecutive_failures {
                        tracing::debug!(
                            "compression lock refresh failed {} times in a row; stopping lease \
                             refresher for session {}",
                            consecutive_failures,
                            session_id
                        );
                        return;
                    }
                }
            })
            .ok();
        Self { stop, handle }
    }

    pub(crate) fn stop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            // A late refresh on an already-released lock is a rowcount-0
            // no-op, so a bounded join is safe.
            let _ = handle.join();
        }
    }
}

/// Build a unique holder id for the lock (`_compression_lock_holder`):
/// pid:tid:agent-instance:uuid.
fn compression_lock_holder(agent_addr: usize) -> String {
    format!(
        "pid={}:tid={:?}:agent={:x}:nonce={}",
        std::process::id(),
        std::thread::current().id(),
        agent_addr,
        &uuid::Uuid::new_v4().simple().to_string()[..8],
    )
}

impl Agent {
    fn notice(tx: Option<&mpsc::UnboundedSender<AgentEvent>>, text: impl Into<String>) {
        if let Some(tx) = tx {
            let _ = tx.send(AgentEvent::Notice(text.into()));
        }
    }

    /// Warn when the auxiliary compression model's context window is smaller
    /// than the main model's compression threshold
    /// (`check_compression_model_feasibility`). Hard-rejects auxes below
    /// `MINIMUM_CONTEXT_LENGTH`; auto-lowers the session threshold when the
    /// aux window can't fit it. Run lazily on the first compression attempt.
    pub(crate) fn check_compression_model_feasibility(
        &mut self,
        tx: Option<&mpsc::UnboundedSender<AgentEvent>>,
    ) -> Result<(), String> {
        if !self.compression_enabled {
            return Ok(());
        }
        let Some(backend) = self.compressor_backend() else { return Ok(()) };
        let aux_model = backend.resolved_model();
        let aux_provider_label = backend.resolved_provider();
        if !backend.has_provider() || aux_model.is_empty() {
            let msg = if !aux_provider_label.is_empty() && aux_provider_label != "auto" {
                format!(
                    "⚠ Configured auxiliary compression provider '{}' is unavailable — context \
                     compression will drop middle turns without a summary. Check \
                     auxiliary.compression in config.yaml and reauthenticate that provider.",
                    aux_provider_label
                )
            } else {
                "⚠ No auxiliary LLM provider configured — context compression will drop middle \
                 turns without a summary. Run `joey setup` or set OPENROUTER_API_KEY."
                    .to_string()
            };
            self.compression_warning = Some(msg.clone());
            if tx.is_some() {
                // Delivered live — don't re-deliver via the turn-start replay.
                self.compression_warning_replayed = true;
            }
            Self::notice(tx, msg);
            tracing::warn!(
                "No auxiliary LLM provider for compression — summaries will be unavailable."
            );
            return Ok(());
        }

        let aux_context = backend.aux_context_length();
        // Hard floor: the aux model must have at least 64K context.
        if aux_context > 0 && aux_context < MINIMUM_CONTEXT_LENGTH {
            return Err(format!(
                "Auxiliary compression model {} has a context window of {} tokens, which is below \
                 the minimum {} required by Joey Agent.  Choose a compression model with at least \
                 {}K context (set auxiliary.compression.model in config.yaml), or set \
                 auxiliary.compression.context_length to override the detected value if it is \
                 wrong.",
                aux_model,
                commafy(aux_context),
                commafy(MINIMUM_CONTEXT_LENGTH),
                MINIMUM_CONTEXT_LENGTH / 1000
            ));
        }

        let threshold = self.compressor.threshold_tokens;
        if aux_context < threshold {
            // Auto-correct: lower the live session threshold so compression
            // actually works this session.
            let old_threshold = threshold;
            let new_threshold = aux_context;
            self.compressor.threshold_tokens = new_threshold;
            let main_ctx = self.compressor.context_length;
            if main_ctx > 0 {
                self.compressor.threshold_percent = new_threshold as f64 / main_ctx as f64;
            }
            let safe_pct = if main_ctx > 0 {
                (aux_context as f64 / main_ctx as f64 * 100.0) as i64
            } else {
                50
            };
            let main_label = if self.provider_name.is_empty() {
                self.config.model.clone()
            } else {
                format!("{} ({})", self.config.model, self.provider_name)
            };
            let aux_label = format!("{} ({})", aux_model, aux_provider_label);
            let msg = format!(
                "⚠ Compression model {} context is {} tokens, but the main model {}'s compression \
                 threshold was {} tokens. Auto-lowered this session's threshold to {} tokens so \
                 compression can run.\n  To make this permanent, edit config.yaml — either:\n  1. \
                 Use a larger compression model:\n       auxiliary:\n         compression:\n       \
                    model: <model-with-{}+-context>\n  2. Lower the compression threshold:\n      \
                 compression:\n         threshold: 0.{:02}",
                aux_label,
                commafy(aux_context),
                main_label,
                commafy(old_threshold),
                commafy(new_threshold),
                commafy(old_threshold),
                safe_pct
            );
            self.compression_warning = Some(msg.clone());
            if tx.is_some() {
                self.compression_warning_replayed = true;
            }
            Self::notice(tx, msg);
            tracing::warn!(
                "Auxiliary compression model {} has {} token context, below the main model's \
                 compression threshold of {} tokens — auto-lowered session threshold to {} to \
                 keep compression working.",
                aux_model,
                aux_context,
                old_threshold,
                new_threshold
            );
        }
        Ok(())
    }

    fn compressor_backend(&self) -> Option<Arc<dyn super::summary::SummaryBackend>> {
        self.compressor.summary_backend_arc()
    }

    /// Compress conversation context in place (`compress_context`).
    ///
    /// Rewrites `self.history` and refreshes the system prompt; on the
    /// session store it soft-archives the pre-compaction rows and inserts
    /// the compacted transcript under the SAME session id
    /// (`archive_and_compact`). Returns true when the transcript changed.
    ///
    /// `force=true` (manual /compress) bypasses the summary-failure cooldown
    /// and the automatic-compression guards. Auto-compress callers use false.
    pub async fn compress_context(
        &mut self,
        approx_tokens: Option<i64>,
        focus_topic: Option<&str>,
        force: bool,
        tx: Option<&mpsc::UnboundedSender<AgentEvent>>,
    ) -> bool {
        // Every automatic entrypoint must honor compressor-owned cooldown and
        // breaker state (conversation_compression.py:750-764).
        if !force {
            self.compressor.refresh_durable_guards();
            if self.compressor.automatic_compression_blocked() {
                return false;
            }
        }

        // Lazy feasibility check on the first compression attempt
        // (conversation_compression.py:766-780).
        if !self.compression_feasibility_checked {
            if let Err(fatal) = self.check_compression_model_feasibility(tx) {
                Self::notice(tx, format!("❌ {}", fatal));
                return false;
            }
            self.compression_feasibility_checked = true;
        }

        let pre_msg_count = self.history.len();
        let session_id = self.session_id.clone().unwrap_or_default();
        tracing::info!(
            "context compression started: session={} messages={} tokens=~{} model={} focus={:?}",
            if session_id.is_empty() { "none" } else { &session_id },
            pre_msg_count,
            approx_tokens.map(commafy).unwrap_or_else(|| "unknown".to_string()),
            self.config.model,
            focus_topic
        );
        Self::notice(tx, COMPACTION_STATUS.clone());

        // ── Compression lock (conversation_compression.py:802-1008):
        // atomic, state.db-backed lock per session_id so two paths can't
        // compress overlapping snapshots of the same conversation. ──
        let lock_db = self.session_db.clone();
        let lock_sid = session_id.clone();
        let mut lock_holder: Option<String> = None;
        let mut lock_refresher: Option<CompressionLockLeaseRefresher> = None;
        if let (Some(db), false) = (&lock_db, lock_sid.is_empty()) {
            let holder = compression_lock_holder(self as *const _ as usize);
            let acquired = {
                let guard = db.lock().unwrap_or_else(|p| p.into_inner());
                guard.try_acquire_compression_lock(&lock_sid, &holder, COMPRESSION_LOCK_TTL_SECONDS)
            };
            if !acquired {
                let existing = {
                    let guard = db.lock().unwrap_or_else(|p| p.into_inner());
                    guard.get_compression_lock_holder(&lock_sid)
                };
                tracing::warn!(
                    "compression skipped: another path is compressing session={} (holder={:?}) — \
                     returning messages unchanged to avoid session fork",
                    lock_sid,
                    existing
                );
                // Surface once — quiet for downstream auto-compress loops.
                if self.last_compression_lock_warning_sid.as_deref() != Some(&lock_sid) {
                    self.last_compression_lock_warning_sid = Some(lock_sid.clone());
                    Self::notice(
                        tx,
                        "⚠ Skipping concurrent compression — another path is already compressing \
                         this session. Will retry after it finishes.",
                    );
                }
                return false;
            }
            lock_holder = Some(holder);

            // A delayed contender can acquire the parent lock after a winning
            // path already rotated it (legacy rotation semantics preserved
            // for hermes-written stores).
            let already_rotated = {
                let guard = db.lock().unwrap_or_else(|p| p.into_inner());
                guard.session_was_rotated_by_compression(&lock_sid)
            };
            if already_rotated {
                tracing::info!(
                    "compression skipped: session={} was already rotated by another compression \
                     path",
                    lock_sid
                );
                let guard = db.lock().unwrap_or_else(|p| p.into_inner());
                guard.release_compression_lock(&lock_sid, lock_holder.as_deref().unwrap_or(""));
                return false;
            }

            // Re-read durable breaker state after acquiring the session lock
            // (conversation_compression.py:991-1008).
            if !force {
                self.compressor.refresh_durable_guards();
                if self.compressor.automatic_compression_blocked() {
                    let guard = db.lock().unwrap_or_else(|p| p.into_inner());
                    guard.release_compression_lock(&lock_sid, lock_holder.as_deref().unwrap_or(""));
                    return false;
                }
            }

            lock_refresher = Some(CompressionLockLeaseRefresher::start(
                db.clone(),
                lock_sid.clone(),
                lock_holder.clone().unwrap_or_default(),
                COMPRESSION_LOCK_TTL_SECONDS,
                None,
            ));
        }

        let release_lock = |refresher: &mut Option<CompressionLockLeaseRefresher>| {
            if let Some(r) = refresher.as_mut() {
                r.stop();
            }
            *refresher = None;
            if let (Some(db), Some(holder)) = (&lock_db, &lock_holder) {
                if !lock_sid.is_empty() {
                    let guard = db.lock().unwrap_or_else(|p| p.into_inner());
                    guard.release_compression_lock(&lock_sid, holder);
                }
            }
        };

        // Memory-provider pre-compress hook: external memory providers are
        // not ported — memory_context stays empty.
        let memory_context = String::new();

        let messages_before = self.history.clone();
        let before_snapshot = serde_json::to_string(&messages_before).unwrap_or_default();
        let mut compressed = self
            .compressor
            .compress(messages_before.clone(), approx_tokens, focus_topic, force, &memory_context)
            .await;

        // Capture boundary quality before bookkeeping resets.
        let compression_made_progress = self.compressor.last_compression_made_progress;
        let compression_used_fallback = self.compressor.last_summary_fallback_used;

        // Aborted (aux LLM failed to produce a usable summary): surface the
        // error, skip the boundary work entirely.
        if self.compressor.last_compress_aborted {
            let err = self
                .compressor
                .last_summary_error
                .clone()
                .unwrap_or_else(|| "unknown error".to_string());
            if self.last_compression_summary_warning.as_deref() != Some(&err) {
                self.last_compression_summary_warning = Some(err.clone());
                Self::notice(
                    tx,
                    format!(
                        "⚠ Compression aborted: {}. No messages were dropped — conversation \
                         continues unchanged. Run /compress to retry, or /new to start a fresh \
                         session.",
                        err
                    ),
                );
            }
            release_lock(&mut lock_refresher);
            return false;
        }

        // No-op detection against the pre-dispatch semantic state.
        let after_snapshot = serde_json::to_string(&compressed).unwrap_or_default();
        if after_snapshot == before_snapshot {
            tracing::info!(
                "Compression made no progress (session={}) — skipping boundary rewrite.",
                if session_id.is_empty() { "none" } else { &session_id }
            );
            release_lock(&mut lock_refresher);
            return false;
        }
        if compressed.is_empty() {
            tracing::error!(
                "context compression returned an empty transcript; refusing to rewrite session={} \
                 so it remains resumable",
                if session_id.is_empty() { "none" } else { &session_id }
            );
            Self::notice(
                tx,
                "⚠ Compression returned an empty transcript. No session split was performed; \
                 conversation continues unchanged.",
            );
            release_lock(&mut lock_refresher);
            return false;
        }

        // Summary failure surfaces (fallback marker / recovered aux model).
        if let Some(summary_error) = self.compressor.last_summary_error.clone() {
            if self.last_compression_summary_warning.as_deref() != Some(&summary_error) {
                self.last_compression_summary_warning = Some(summary_error.clone());
                Self::notice(
                    tx,
                    format!(
                        "⚠ Compression summary failed: {}. Inserted a fallback context marker.",
                        summary_error
                    ),
                );
            }
        } else if let Some(aux_fail_model) = self.compressor.last_aux_model_failure_model.clone() {
            let aux_fail_err = self
                .compressor
                .last_aux_model_failure_error
                .clone()
                .unwrap_or_else(|| "unknown error".to_string());
            let key = (aux_fail_model.clone(), aux_fail_err.clone());
            if self.last_aux_fallback_warning_key.as_ref() != Some(&key) {
                self.last_aux_fallback_warning_key = Some(key);
                Self::notice(
                    tx,
                    format!(
                        "ℹ Configured compression model '{}' failed ({}). Recovered using main \
                         model — check auxiliary.compression.model in config.yaml.",
                        aux_fail_model, aux_fail_err
                    ),
                );
            }
        }

        // Todo snapshot re-injection (conversation_compression.py:1164-1170):
        // only pending/in_progress items survive.
        if let Some(snapshot) = todo_snapshot_for_injection(self.ctx.session_id()) {
            let mut msg = Message::user(snapshot);
            msg.synthetic = true; // upstream `_todo_snapshot_synthetic`
            compressed.push(msg);
        }
        ensure_compressed_has_user_turn(&messages_before, &mut compressed);

        // ── Cached-system-prompt refresh (conversation_compression.py:
        // 1173-1194): keep the exact cached prompt when it already embeds
        // the CURRENT rendered memory blocks verbatim (KV-cache retention);
        // rebuild otherwise. ──
        let new_system_prompt =
            if cached_prompt_reflects_builtin_memory(&self.ctx, &self.system_prompt) {
                self.system_prompt.clone()
            } else {
                let valid_tools =
                    crate::agent::valid_tool_names(&self.registry, &self.config.enabled_tools, &self.ctx);
                build_system_prompt(&PromptInputs {
                    ctx: &self.ctx,
                    model: &self.config.model,
                    provider: &self.provider_name,
                    enabled_tools: &valid_tools,
                    pass_session_id: self.config.pass_session_id,
                    session_id: self.session_id.as_deref(),
                })
            };
        self.system_prompt = new_system_prompt.clone();

        // ── In-place DB compaction: soft-archive the previous active rows
        // and insert the compacted transcript, atomically, under the same
        // session id (`archive_and_compact`, #38763). ──
        if let (Some(db), false) = (&self.session_db, session_id.is_empty()) {
            let rows: Vec<StoredMessage> = compressed
                .iter()
                .map(|m| message_to_stored(&session_id, m))
                .collect();
            let guard = db.lock().unwrap_or_else(|p| p.into_inner());
            if let Err(e) = guard.archive_and_compact(&session_id, &rows) {
                tracing::warn!("Session DB compression rewrite failed: {}", e);
            }
            if let Err(e) = guard.update_system_prompt(&session_id, &new_system_prompt) {
                tracing::debug!("update_system_prompt failed: {}", e);
            }
        }

        // Warn on repeated compressions (quality degrades with each pass).
        let cc = self.compressor.compression_count;
        if cc >= 2 {
            let cc_msg = format!(
                "⚠️  Session compressed {} times — accuracy may degrade. Consider /new to start \
                 fresh.",
                cc
            );
            self.compression_warning = Some(cc_msg.clone());
            if tx.is_some() {
                self.compression_warning_replayed = true;
            }
            Self::notice(tx, cc_msg);
        }

        // Adopt the compacted transcript as the live history.
        self.history = compressed;

        // Post-boundary bookkeeping (conversation_compression.py:1449-1477):
        // keep the rough estimate for diagnostics, park last_prompt_tokens at
        // the -1 sentinel, and await real usage.
        let tools = self.compression_tool_schemas();
        let compressed_est = estimate_request_tokens_rough(
            &self.history,
            &new_system_prompt,
            if tools.is_empty() { None } else { Some(&tools) },
        );
        self.compressor.last_compression_rough_tokens = compressed_est;
        self.compressor.last_prompt_tokens = -1;
        self.compressor.last_completion_tokens = 0;
        self.compressor.awaiting_real_usage_after_compression = true;
        if compression_made_progress {
            self.compressor.record_completed_compaction(compression_used_fallback);
        }

        tracing::info!(
            "context compression done: session={} messages={}->{} rough_tokens=~{} awaiting_real_usage=true",
            if session_id.is_empty() { "none" } else { &session_id },
            pre_msg_count,
            self.history.len(),
            commafy(compressed_est)
        );
        release_lock(&mut lock_refresher);
        true
    }

    fn compression_tool_schemas(&self) -> Vec<joey_providers::ToolSchema> {
        self.registry
            .definitions(&self.config.enabled_tools, &self.ctx)
            .into_iter()
            .filter_map(|d| serde_json::from_value::<joey_providers::ToolSchema>(d).ok())
            .collect()
    }

    /// The context-usage breakdown payload for status surfaces
    /// (`compute_session_context_breakdown`).
    pub fn context_breakdown(&self) -> serde_json::Value {
        let memory_blocks = vec![
            crate::prompt::format_memory_for_system_prompt(&self.ctx, "memory"),
            crate::prompt::format_memory_for_system_prompt(&self.ctx, "user"),
        ];
        let tools = self.compression_tool_schemas();
        super::breakdown::compute_session_context_breakdown(
            &self.system_prompt,
            &memory_blocks,
            &tools,
            &self.history,
            Some(&self.compressor),
            &self.config.model,
        )
    }

    /// Rough token estimate for the next full request (system prompt +
    /// history + tool schemas) — the figure the CLI `/compress` preamble and
    /// feedback use (cli.py:9869-9873).
    pub fn request_tokens_estimate(&self) -> i64 {
        let tools = self.compression_tool_schemas();
        estimate_request_tokens_rough(
            &self.history,
            &self.system_prompt,
            if tools.is_empty() { None } else { Some(&tools) },
        )
    }

    /// Manual `/compress` (cli.py `_manual_compress` core): force-compress
    /// with an optional focus topic, returning the upstream feedback shape.
    pub async fn manual_compress(
        &mut self,
        focus_topic: Option<&str>,
        tx: Option<&mpsc::UnboundedSender<AgentEvent>>,
    ) -> super::feedback::ManualCompressionSummary {
        let tools = self.compression_tool_schemas();
        let original_history = self.history.clone();
        let approx_tokens = estimate_request_tokens_rough(
            &original_history,
            &self.system_prompt,
            if tools.is_empty() { None } else { Some(&tools) },
        );
        self.compress_context(Some(approx_tokens), focus_topic, true, tx).await;
        let new_tokens = estimate_request_tokens_rough(
            &self.history,
            &self.system_prompt,
            if tools.is_empty() { None } else { Some(&tools) },
        );
        super::feedback::summarize_manual_compression(
            &original_history,
            &self.history,
            approx_tokens,
            new_tokens,
            Some(&self.compressor),
        )
    }
}

/// The upstream `todo_store.format_for_injection()` result for this session:
/// only pending/in_progress items, with the preserved-list banner. None when
/// nothing survives.
fn todo_snapshot_for_injection(session_id: &str) -> Option<String> {
    let items = joey_tools::tools::todo_tool::current(session_id);
    let active: Vec<_> = items
        .iter()
        .filter(|i| i.status == "pending" || i.status == "in_progress")
        .collect();
    if active.is_empty() {
        return None;
    }
    let mut lines =
        vec!["[Your active task list was preserved across context compression]".to_string()];
    for item in active {
        let marker = match item.status.as_str() {
            "completed" => "[x]",
            "in_progress" => "[>]",
            "pending" => "[ ]",
            "cancelled" => "[~]",
            _ => "[?]",
        };
        lines.push(format!("- {} {}. {} ({})", marker, item.id, item.content, item.status));
    }
    Some(lines.join("\n"))
}

/// Whether the cached system prompt already embeds the CURRENT rendered
/// built-in memory blocks verbatim (`_cached_prompt_reflects_builtin_memory`):
/// each non-empty block must appear in the prompt; a leftover block header for
/// a now-empty/disabled target forces a rebuild.
fn cached_prompt_reflects_builtin_memory(ctx: &joey_tools::ToolContext, cached_prompt: &str) -> bool {
    // tools/memory_tool.py MEMORY_BLOCK_HEADERS.
    const MEMORY_HEADER: &str = "MEMORY (your personal notes)";
    const USER_HEADER: &str = "USER PROFILE (who the user is)";
    let cfg = ctx.config();
    for (target, header, enabled) in [
        ("memory", MEMORY_HEADER, cfg.get_bool("memory.memory_enabled", true)),
        ("user", USER_HEADER, cfg.get_bool("memory.user_profile_enabled", true)),
    ] {
        let block = if enabled {
            crate::prompt::format_memory_for_system_prompt(ctx, target)
        } else {
            String::new()
        };
        let block = block.trim();
        if !block.is_empty() {
            if !cached_prompt.contains(block) {
                return false;
            }
        } else if cached_prompt.contains(header) {
            return false;
        }
    }
    true
}

/// Convert a live message into a session-store row (the same shape the
/// agent's persistence path writes — agent.rs `persist_row`).
fn message_to_stored(session_id: &str, msg: &Message) -> StoredMessage {
    let mut row = StoredMessage::new(
        session_id.to_string(),
        Role::from_label(&msg.role),
        msg.text_content(),
    );
    row.tool_call_id = msg.tool_call_id.clone();
    row.tool_name = msg.name.clone();
    if !msg.tool_calls.is_empty() {
        let arr: Vec<serde_json::Value> = msg
            .tool_calls
            .iter()
            .map(|c| json!({"name": c.function.name, "arguments": c.function.arguments}))
            .collect();
        row.tool_calls = serde_json::to_string(&arr).ok();
    }
    if msg.role == "assistant" {
        row.reasoning = msg.reasoning.clone();
    }
    row
}
