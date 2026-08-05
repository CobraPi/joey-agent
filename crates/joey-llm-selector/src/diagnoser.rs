//! Detached LLM diagnoser + learning loop (FR-008, FR-009, FR-010; Phase 5).
//!
//! The diagnoser consumes observations (module, signal, input summary, output)
//! from an unbounded channel, estimates per-module performance `p_j ∈ [0,1]`,
//! and reallocates modules toward better performers within the learning budget.
//! It runs as a detached `tokio::spawn` task — `record_observation` on the
//! engine enqueues and returns immediately; the hot path is never blocked.
//!
//! ## Performance estimation
//!
//! The spec (FR-008) says the diagnoser "estimates per-module performance from
//! the module's inputs and outputs". The primary estimator is a real LLM judge
//! call dispatched via [`DiagnoserClient`] (constructed from the active
//! provider's credentials + the configured diagnoser model). The judge is
//! asked to score the implicated model's output on `[0.0, 1.0]`. If the LLM
//! call fails (network error, malformed response, no credentials, missing
//! diagnoser model), the estimator falls back to a deterministic heuristic
//! driven by the four observable failure signals (FR-009). This keeps the
//! diagnoser honest, testable without network, and never blocks the hot path.

use std::sync::Arc;

use crate::allocator::SelectorEngine;
use crate::map::{DiagnosticRecord, FailureSignal};
use crate::model_allocator::ModelAllocator;
use crate::module::ModuleId;

/// One observation forwarded from the hot path via `record_observation`.
#[derive(Debug, Clone)]
pub(crate) struct Observation {
    pub module: ModuleId,
    pub signal: FailureSignal,
    pub module_input_summary: String,
    pub module_output: String,
}

/// Abstracts the LLM judge call so the selector crate stays testable without
/// a live provider (FR-008/FR-009). The production implementation is a thin
/// adapter over `joey_providers::ProviderClient`; tests inject a stub.
///
/// `estimate_performance` receives the observation and MUST return a score in
/// `[0.0, 1.0]` (higher = better). Implementations are free to call a live LLM
/// or fall back to a heuristic. Returning `None` tells the learning loop to
/// use the signal-driven heuristic instead.
#[async_trait::async_trait]
pub trait DiagnoserClient: Send + Sync {
    async fn estimate_performance(
        &self,
        signal: &FailureSignal,
        module: &ModuleId,
        module_input_summary: &str,
        module_output: &str,
    ) -> Option<f64>;
}

/// Production [`DiagnoserClient`] backed by `joey_providers::ProviderClient`.
///
/// Builds a short judge prompt from the observation, dispatches a
/// non-streaming completion on the configured diagnoser model, and parses a
/// `p_j ∈ [0,1]` from the response. Any failure (network, malformed, missing
/// credentials/model) returns `None`, signalling the caller to fall back to
/// the heuristic.
pub struct LlmDiagnoser {
    client: joey_providers::ProviderClient,
    model: String,
}

impl LlmDiagnoser {
    /// Build a judge client for the active provider + diagnoser model.
    /// Returns `None` when the client cannot be constructed (missing
    /// credentials, unsupported wire mode) — the caller then uses the
    /// heuristic estimator exclusively.
    pub fn try_new(
        provider: &str,
        base_url: &str,
        diagnoser_model: &str,
        api_key: Option<String>,
    ) -> Option<Self> {
        if diagnoser_model.is_empty() {
            return None;
        }
        let client = joey_providers::build_client(provider, base_url, diagnoser_model, api_key).ok()?;
        Some(Self {
            client,
            model: diagnoser_model.to_string(),
        })
    }
}

#[async_trait::async_trait]
impl DiagnoserClient for LlmDiagnoser {
    async fn estimate_performance(
        &self,
        signal: &FailureSignal,
        module: &ModuleId,
        module_input_summary: &str,
        module_output: &str,
    ) -> Option<f64> {
        // Bound the excerpts so a pathological observation can't blow the
        // judge prompt (and the per-module budget) up.
        const MAX_EXCERPT: usize = 1200;
        let trim = |s: &str| -> String {
            if s.len() <= MAX_EXCERPT {
                s.to_string()
            } else {
                format!("{}…[truncated]", &s[..MAX_EXCERPT])
            }
        };
        let prompt = format!(
            "You are a strict LLM-output evaluator for a compound AI agent.\n\
             A sub-module ({module}) produced the output below after the listed input.\n\
             An observable failure signal was also recorded: {signal:?}.\n\n\
             Score the model's performance on this task as a single decimal number\n\
             in [0.0, 1.0] where 1.0 is flawless and 0.0 is a total failure.\n\
             Respond with ONLY the number (no prose, no explanation).\n\n\
             --- INPUT SUMMARY ---\n{input}\n\n\
             --- OUTPUT ---\n{output}\n",
            module = module,
            signal = signal,
            input = trim(module_input_summary),
            output = trim(module_output),
        );
        let mut req = joey_providers::ProviderRequest::new(
            self.model.clone(),
            vec![joey_providers::Message::user(prompt)],
        );
        req.max_tokens = Some(16);
        req.temperature = Some(0.0);
        // The detached diagnoser task is already async (FR-009); awaiting the
        // provider client directly is both correct and never blocks the
        // interactive turn. Any error → None → heuristic fallback.
        match self.client.complete(&req).await {
            Ok(resp) => parse_pj(&resp.content),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "llm-selector diagnoser: LLM judge call failed; using heuristic"
                );
                None
            }
        }
    }
}

/// Parse a `p_j ∈ [0,1]` from the judge's free-text response. Accepts a leading
/// decimal anywhere in the first line; clamps to `[0,1]`. Returns `None` when
/// no parsable number is found.
fn parse_pj(text: &str) -> Option<f64> {
    let candidate = text.trim().lines().next()?;
    let mut end = 0usize;
    for (i, c) in candidate.char_indices() {
        if c.is_ascii_digit() || c == '.' || (i == 0 && (c == '-' || c == '+')) {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    if end == 0 {
        return None;
    }
    let val: f64 = candidate[..end].parse().ok()?;
    if val.is_finite() {
        Some(val.clamp(0.0, 1.0))
    } else {
        None
    }
}

/// The heuristic performance score derived from an observation's signal.
///
/// Returns a value in `[0.0, 1.0]` where higher is better. The implicated
/// model (the one that was running when the signal fired) receives this score;
/// the learning loop then nominates alternative models by their tier (a proxy
/// for unobserved capability) and reallocates if a better candidate exists.
fn estimate_performance(signal: &FailureSignal, module_output: &str) -> f64 {
    match signal {
        // A turn error means the model failed outright → very low confidence.
        FailureSignal::TurnError => 0.15,
        // An empty/null response is a hard quality failure → low.
        FailureSignal::EmptyResponse => 0.10,
        // An auxiliary call failure (e.g. compression) → low-moderate.
        FailureSignal::AuxCallFailure => 0.25,
        // A retry means the first attempt was unsatisfactory but eventually
        // succeeded → moderate degradation.
        FailureSignal::RetryTriggered => {
            // If the output is very short relative to a non-empty input, that's
            // a weaker result; otherwise moderate.
            if module_output.trim().is_empty() {
                0.30
            } else {
                0.45
            }
        }
    }
}

/// Entry point for the runtime-handle spawn path (used by `start_diagnoser`).
/// Takes the receiver from the engine and runs the learning loop.
pub(crate) async fn run_learning_loop_from_handle(engine: Arc<SelectorEngine>) {
    let rx = match engine.take_observation_rx() {
        Some(rx) => rx,
        None => return, // already taken or never set
    };
    run_learning_loop(engine, rx).await;
}

/// The main learning loop: consume observations, estimate performance, and
/// reallocate modules toward better performers within the budget.
async fn run_learning_loop(
    engine: Arc<SelectorEngine>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Observation>,
) {
    while let Some(obs) = rx.recv().await {
        // FR-009: ignore observations when inactive or budget exhausted.
        if !engine.is_active() {
            continue;
        }
        let budget = engine.map_snapshot().learning_budget;
        if budget == 0 {
            continue; // learning disabled
        }
        let used = engine.map_snapshot().budget_used_this_cycle;
        if used >= budget {
            continue; // budget exhausted this cycle
        }

        // Estimate performance of the implicated model.
        // FR-008/T076: prefer the LLM judge (if installed), fall back to the
        // signal-driven heuristic when the judge is absent or returns None.
        let p_j = if let Some(judge) = engine.diagnoser_client() {
            match judge
                .estimate_performance(&obs.signal, &obs.module, &obs.module_input_summary, &obs.module_output)
                .await
            {
                Some(llm_pj) => llm_pj,
                None => estimate_performance(&obs.signal, &obs.module_output),
            }
        } else {
            estimate_performance(&obs.signal, &obs.module_output)
        };

        // Find the implicated model (the one currently allocated to this module).
        let implicated = engine.map_snapshot().get(&obs.module).map(|e| e.model_id.clone());

        // Append the diagnostic record (FR-018).
        let rationale = format!(
            "{:?} signal on {}; estimated p_j={:.2}",
            obs.signal, obs.module, p_j
        );
        let record = DiagnosticRecord {
            at: chrono::Utc::now().to_rfc3339(),
            module: obs.module.clone(),
            signal: obs.signal.clone(),
            implicated_model: implicated.clone().unwrap_or_default(),
            rationale: rationale.clone(),
        };

        // Attempt reallocation: find a better candidate for this module.
        let reallocated = engine.try_reallocate_for_observation(
            &obs.module,
            p_j,
            &implicated,
            &rationale,
        );

        // Always append the diagnostic (FR-018), then persist.
        engine.append_diagnostic_and_persist(record, reallocated);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_performance_turn_error() {
        assert!(estimate_performance(&FailureSignal::TurnError, "x") < 0.2);
    }

    #[test]
    fn test_estimate_performance_empty_response() {
        assert!(estimate_performance(&FailureSignal::EmptyResponse, "") < 0.15);
    }

    #[test]
    fn test_estimate_performance_retry() {
        let p = estimate_performance(&FailureSignal::RetryTriggered, "some output");
        assert!(p > 0.35 && p < 0.5);
    }

    #[test]
    fn test_estimate_performance_in_range() {
        for signal in [
            FailureSignal::TurnError,
            FailureSignal::EmptyResponse,
            FailureSignal::AuxCallFailure,
            FailureSignal::RetryTriggered,
        ] {
            let p = estimate_performance(&signal, "test");
            assert!(p >= 0.0 && p <= 1.0, "p_j out of range for {:?}: {}", signal, p);
        }
    }

    // ── T076: LLM judge plumbing tests ──────────────────────────────────────

    #[test]
    fn test_parse_pj_plain_decimal() {
        assert_eq!(parse_pj("0.75"), Some(0.75));
        assert_eq!(parse_pj("1.0"), Some(1.0));
        assert_eq!(parse_pj("0"), Some(0.0));
    }

    #[test]
    fn test_parse_pj_clamps_and_trims() {
        assert_eq!(parse_pj("  0.42\n"), Some(0.42));
        assert_eq!(parse_pj("1.7"), Some(1.0)); // clamped
        assert_eq!(parse_pj("-0.2"), Some(0.0)); // clamped
    }

    #[test]
    fn test_parse_pj_rejects_prose() {
        assert_eq!(parse_pj("The score is 0.5"), None);
        assert_eq!(parse_pj(""), None);
        assert_eq!(parse_pj("nan"), None);
    }

    /// A stub DiagnoserClient for testing the learning loop's judge-then-
    /// heuristic fallback (FR-008/T076). Records calls and returns a fixed p_j.
    struct StubJudge {
        value: Option<f64>,
        calls: std::sync::Mutex<usize>,
    }

    #[async_trait::async_trait]
    impl DiagnoserClient for StubJudge {
        async fn estimate_performance(
            &self,
            _signal: &FailureSignal,
            _module: &ModuleId,
            _input: &str,
            _output: &str,
        ) -> Option<f64> {
            *self.calls.lock().unwrap() += 1;
            self.value
        }
    }

    #[tokio::test]
    async fn test_learning_loop_uses_judge_when_present() {
        use crate::allocator::SelectorEngine;
        use crate::candidate::{CandidateModel, CandidateModelPool, CapabilityTier, CatalogSource};
        use crate::map::{AllocationEntry, AllocationMap};
        use std::sync::Arc;

        // Two-tier pool so reallocation has a higher-tier candidate to move to.
        let pool = CandidateModelPool::from_consolidated(
            vec![
                CandidateModel {
                    id: "flash-model".into(),
                    provider: "test".into(),
                    context_window: 8192,
                    supports_tools: true,
                    supports_vision: false,
                    tier: CapabilityTier::Flash,
                    cost: None,
                },
                CandidateModel {
                    id: "frontier-model".into(),
                    provider: "test".into(),
                    context_window: 128_000,
                    supports_tools: true,
                    supports_vision: false,
                    tier: CapabilityTier::Frontier,
                    cost: None,
                },
            ],
            CatalogSource::GenericProbe,
        );
        let mut map = AllocationMap::default();
        map.enabled = true;
        map.learning_budget = 4;
        map.budget_used_this_cycle = 0;
        map.entries.push(AllocationEntry {
            module: ModuleId::MainTurn,
            model_id: "flash-model".into(),
            pinned: false,
            implicit_pin: false,
            reason: "cold-start".into(),
            estimated_performance: None,
            updated_at: None,
        });
        let engine = Arc::new(SelectorEngine::new_with_map(
            crate::allocator::SelectorConfig {
                enabled: true,
                configured_model: "auto".into(),
                learning_budget: 4,
                diagnoser_model: "frontier-model".into(),
            },
            map,
        ));
        engine.set_pool(pool);
        // Judge returns a low score → should trigger reallocation.
        let judge = Arc::new(StubJudge {
            value: Some(0.1),
            calls: std::sync::Mutex::new(0),
        });
        engine.set_diagnoser_client(Some(judge.clone() as Arc<dyn DiagnoserClient>));

        // Enqueue one observation and run a single iteration of the loop.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(Observation {
            module: ModuleId::MainTurn,
            signal: FailureSignal::TurnError,
            module_input_summary: "input".into(),
            module_output: "bad output".into(),
        })
        .unwrap();
        drop(tx);
        run_learning_loop(engine.clone(), rx).await;

        // The judge was consulted exactly once.
        assert_eq!(*judge.calls.lock().unwrap(), 1);
        // The judge's low score triggered a reallocation to the frontier model.
        let after = engine.map_snapshot();
        let entry = after.get(&ModuleId::MainTurn).expect("entry exists");
        assert_eq!(entry.model_id, "frontier-model");
        assert!(entry.reason.contains("diagnoser reallocation"));
    }

    #[tokio::test]
    async fn test_learning_loop_falls_back_to_heuristic_when_judge_none() {
        use crate::allocator::SelectorEngine;
        use crate::candidate::{CandidateModel, CandidateModelPool, CapabilityTier, CatalogSource};
        use crate::map::{AllocationEntry, AllocationMap};
        use std::sync::Arc;

        let pool = CandidateModelPool::from_consolidated(
            vec![
                CandidateModel {
                    id: "flash-model".into(),
                    provider: "test".into(),
                    context_window: 8192,
                    supports_tools: true,
                    supports_vision: false,
                    tier: CapabilityTier::Flash,
                    cost: None,
                },
                CandidateModel {
                    id: "frontier-model".into(),
                    provider: "test".into(),
                    context_window: 128_000,
                    supports_tools: true,
                    supports_vision: false,
                    tier: CapabilityTier::Frontier,
                    cost: None,
                },
            ],
            CatalogSource::GenericProbe,
        );
        let mut map = AllocationMap::default();
        map.enabled = true;
        map.learning_budget = 4;
        map.budget_used_this_cycle = 0;
        map.entries.push(AllocationEntry {
            module: ModuleId::MainTurn,
            model_id: "flash-model".into(),
            pinned: false,
            implicit_pin: false,
            reason: "cold-start".into(),
            estimated_performance: None,
            updated_at: None,
        });
        let engine = Arc::new(SelectorEngine::new_with_map(
            crate::allocator::SelectorConfig {
                enabled: true,
                configured_model: "auto".into(),
                learning_budget: 4,
                diagnoser_model: "frontier-model".into(),
            },
            map,
        ));
        engine.set_pool(pool);
        // Judge present but returns None → learning loop must fall back to the
        // heuristic (which scores a TurnError at 0.15, still below 0.5, so
        // reallocation still happens via the heuristic path).
        let judge = Arc::new(StubJudge {
            value: None,
            calls: std::sync::Mutex::new(0),
        });
        engine.set_diagnoser_client(Some(judge.clone() as Arc<dyn DiagnoserClient>));

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tx.send(Observation {
            module: ModuleId::MainTurn,
            signal: FailureSignal::TurnError,
            module_input_summary: "input".into(),
            module_output: "".into(),
        })
        .unwrap();
        drop(tx);
        run_learning_loop(engine.clone(), rx).await;

        // Judge was consulted but returned None; heuristic drove reallocation.
        assert_eq!(*judge.calls.lock().unwrap(), 1);
        let after = engine.map_snapshot();
        let entry = after.get(&ModuleId::MainTurn).expect("entry exists");
        assert_eq!(entry.model_id, "frontier-model");
    }
}
