//! joey-jev: client for the user's local Jev typed-decision service
//! (specs/036-jev-browser-acceleration).
//!
//! Jev is a loopback HTTP service answering `choice`/`score` judgment
//! questions with calibrated confidence — no text generation. This crate
//! encapsulates the wire client, the availability probe/cache, per-point
//! confidence gates, and the JSONL decision log. Every failure mode
//! collapses to [`Decision::Fallback`]; callers never see errors.
//!
//! By construction (contracts/decision-points.md):
//! - Question kinds are judgment-only (`choice`/`score`) — the decider can
//!   never ask for generated text (FR-006).
//! - The decider never selects models; `model` is a fixed alias from config
//!   (FR-012).
//! - Page-derived strings are redacted and length-capped before transport
//!   (untrusted-grade inputs, FR-007); fixed instruction templates are
//!   authored here, never assembled from page content.

pub mod log;
pub mod transport;
pub mod wire;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use joey_core::config::Config;
use tokio::sync::Mutex as AsyncMutex;

pub use log::{append_record, decision_log_path, DecisionRecord};
pub use transport::{DecisionTransport, FixtureTransport, ReqwestTransport, TransportError};
pub use wire::{
    questions_to_wire, Criterion, Question, QuestionKind, QuestionSet, SystemOneRequest,
    SystemOneResponse,
};

/// Negative-probe cache TTL (research.md R3): reconnect stays cheap.
const NEGATIVE_TTL: Duration = Duration::from_secs(10);
/// Consecutive fallbacks on an Available state before a forced re-probe
/// (hysteresis against a dying service; data-model state transitions).
const REPROBE_AFTER_FALLBACKS: u32 = 3;
/// Char cap on caller-supplied strings before transport (cross-cutting
/// invariant: state strings are length-capped untrusted data).
const DATA_CHAR_CAP: usize = 200;

// ---------------------------------------------------------------------------
// Decision surface
// ---------------------------------------------------------------------------

/// The four named hooks (contracts/decision-points.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionPoint {
    TargetResolution,
    Settle,
    SnapshotTrim,
    Verdict,
}

impl DecisionPoint {
    pub fn name(&self) -> &'static str {
        match self {
            DecisionPoint::TargetResolution => "target_resolution",
            DecisionPoint::Settle => "settle",
            DecisionPoint::SnapshotTrim => "snapshot_trim",
            DecisionPoint::Verdict => "verdict",
        }
    }

    fn kind(&self) -> QuestionKind {
        match self {
            DecisionPoint::SnapshotTrim => QuestionKind::Score,
            _ => QuestionKind::Choice,
        }
    }
}

/// Why a decision fell back (data-model.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    Unavailable,
    Timeout,
    WireError,
    Rejected422,
    Disabled,
}

impl FallbackReason {
    fn as_str(&self) -> &'static str {
        match self {
            FallbackReason::Unavailable => "unavailable",
            FallbackReason::Timeout => "timeout",
            FallbackReason::WireError => "wire_error",
            FallbackReason::Rejected422 => "rejected_422",
            FallbackReason::Disabled => "disabled",
        }
    }
}

/// One entry of a batched score answer (snapshot trimming).
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreEntry {
    pub question: String,
    /// Rubric level index (0 = irrelevant … 3 = essential).
    pub score: u32,
    pub confidence: f32,
}

/// Typed extraction per point (contracts/decision-points.md).
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Choice {
        question: String,
        key: String,
        confidence: f32,
        probabilities: serde_json::Map<String, serde_json::Value>,
    },
    Scores {
        entries: Vec<ScoreEntry>,
    },
}

impl Answer {
    /// Primary confidence of the answer (min across a score batch).
    pub fn confidence(&self) -> f32 {
        match self {
            Answer::Choice { confidence, .. } => *confidence,
            Answer::Scores { entries } => entries
                .iter()
                .map(|e| e.confidence)
                .fold(f32::INFINITY, f32::min),
        }
    }

    fn to_log_value(&self) -> serde_json::Value {
        match self {
            Answer::Choice { key, .. } => key.clone().into(),
            Answer::Scores { entries } => {
                let mut m = serde_json::Map::new();
                for e in entries {
                    m.insert(e.question.clone(), e.score.into());
                }
                m.into()
            }
        }
    }
}

/// The decision callers act on: fast path, gate miss, or existing behavior.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Confident(Answer),
    LowConfidence { got: f32, needed: f32 },
    Fallback { reason: FallbackReason },
}

/// A decision request: sanitized state + typed questions.
#[derive(Debug, Clone)]
pub struct DecisionRequest {
    pub state: serde_json::Value,
    pub questions: QuestionSet,
}

// ---------------------------------------------------------------------------
// QuestionSet builders — fixed instruction templates; callers supply data
// ---------------------------------------------------------------------------

/// Redact + length-cap one caller-supplied string (untrusted-grade input).
fn clean(s: &str) -> String {
    let redacted = joey_core::redact::redact_sensitive_text(s);
    // Char-boundary-safe cap (repo audit lesson).
    if redacted.chars().count() <= DATA_CHAR_CAP {
        redacted
    } else {
        redacted.chars().take(DATA_CHAR_CAP).collect()
    }
}

/// ≤ 12 candidates sent to TargetResolution (data-model validation rule).
pub const TARGET_CANDIDATE_CAP: usize = 12;
/// 4-level relevance rubric for snapshot trimming (irrelevant → essential).
pub const SALIENCE_RUBRIC: [&str; 4] = ["irrelevant", "background", "relevant", "essential"];
/// Score levels ≥ this are retained by the Jev trim path (never dropped).
pub const SALIENCE_KEEP_THRESHOLD: u32 = 2;

/// Decision point 1: which ambiguous-match candidate fits the instruction.
pub fn target_resolution(
    instruction: &str,
    page_title: &str,
    url: &str,
    candidates: &[(String, String)],
) -> DecisionRequest {
    let capped: Vec<Criterion> = candidates
        .iter()
        .take(TARGET_CANDIDATE_CAP)
        .map(|(refid, descriptor)| Criterion {
            key: refid.clone(),
            descriptor: clean(descriptor),
        })
        .collect();
    DecisionRequest {
        state: serde_json::json!({
            "instruction": clean(instruction),
            "page_title": clean(page_title),
            "url": clean(url),
        }),
        questions: QuestionSet {
            questions: vec![(
                "target".into(),
                Question::Choice {
                    instructions: "Select the single page element that best matches the user's instruction. Answer with the candidate key only.".into(),
                    criteria: capped,
                },
            )],
        },
    }
}

/// Decision point 2: has the page stopped changing (2-way, strictest gate).
pub fn settle(mutation_summary: &str, content_digest: &str) -> DecisionRequest {
    DecisionRequest {
        state: serde_json::json!({
            "mutation_summary": clean(mutation_summary),
            "content_digest": clean(content_digest),
        }),
        questions: QuestionSet {
            questions: vec![(
                "settle".into(),
                Question::Choice {
                    instructions: "Decide whether the page has finished loading its content and stopped changing. Answer settled or still_churning.".into(),
                    criteria: vec![
                        Criterion { key: "settled".into(), descriptor: "Content complete; no further changes expected.".into() },
                        Criterion { key: "still_churning".into(), descriptor: "Content still loading, mutating, or animating.".into() },
                    ],
                },
            )],
        },
    }
}

/// Decision point 3: relevance score per at-risk snapshot entry (one
/// request, one question per entry — batched round-trip).
pub fn snapshot_trim(
    goal: &str,
    page_title: &str,
    url: &str,
    entries: &[(String, String)],
) -> DecisionRequest {
    DecisionRequest {
        state: serde_json::json!({
            "browsing_goal": clean(goal),
            "page_title": clean(page_title),
            "url": clean(url),
        }),
        questions: QuestionSet {
            questions: entries
                .iter()
                .map(|(name, descriptor)| {
                    (
                        name.clone(),
                        Question::Score {
                            instructions: format!(
                                "Score how relevant this page element is to the browsing goal. Element: {}",
                                clean(descriptor)
                            ),
                            criteria: SALIENCE_RUBRIC.iter().map(|s| s.to_string()).collect(),
                        },
                    )
                })
                .collect(),
        },
    }
}

/// Decision point 4: did the action reach its expected outcome.
pub fn verdict(action: &str, pre_digest: &str, post_digest: &str) -> DecisionRequest {
    DecisionRequest {
        state: serde_json::json!({
            "action": clean(action),
            "pre_digest": clean(pre_digest),
            "post_digest": clean(post_digest),
        }),
        questions: QuestionSet {
            questions: vec![(
                "verdict".into(),
                Question::Choice {
                    instructions: "Judging from the page state before and after the action, did the action achieve its expected effect? Answer achieved or not_achieved.".into(),
                    criteria: vec![
                        Criterion { key: "achieved".into(), descriptor: "The expected effect is visible in the post-action state.".into() },
                        Criterion { key: "not_achieved".into(), descriptor: "The expected effect is absent or contradicted.".into() },
                    ],
                },
            )],
        },
    }
}

// ---------------------------------------------------------------------------
// Config (research.md R5 — dotted getters + inline defaults, browser.* pattern)
// ---------------------------------------------------------------------------

/// Resolved `jev.*` configuration.
#[derive(Debug, Clone)]
pub struct JevConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub request_timeout: Duration,
    pub probe_ttl: Duration,
    pub min_confidence: f32,
    pub settle_min_confidence: f32,
    pub target_resolution_enabled: bool,
    pub settle_enabled: bool,
    pub trim_enabled: bool,
    pub verdict_enabled: bool,
    /// Test override for the decision-log directory; None → joey home logs.
    pub log_dir: Option<PathBuf>,
}

impl Default for JevConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:8765".into(),
            api_key: "local".into(),
            model: "jev-latest".into(),
            request_timeout: Duration::from_millis(1500),
            probe_ttl: Duration::from_secs(30),
            min_confidence: 0.70,
            settle_min_confidence: 0.80,
            target_resolution_enabled: true,
            settle_enabled: true,
            trim_enabled: true,
            verdict_enabled: true,
            log_dir: None,
        }
    }
}

impl JevConfig {
    pub fn from_config(config: &Config) -> Self {
        Self {
            base_url: config.get_str("jev.base_url", "http://127.0.0.1:8765"),
            api_key: config.get_str("jev.api_key", "local"),
            model: config.get_str("jev.model", "jev-latest"),
            request_timeout: Duration::from_millis(
                config.get_i64("jev.request_timeout_ms", 1500).clamp(100, 60_000) as u64,
            ),
            probe_ttl: Duration::from_secs(
                config.get_i64("jev.probe_ttl_secs", 30).clamp(1, 3600) as u64,
            ),
            min_confidence: config.get_f64("jev.min_confidence", 0.70).clamp(0.0, 1.0) as f32,
            settle_min_confidence: config
                .get_f64("jev.settle.min_confidence", 0.80)
                .clamp(0.0, 1.0) as f32,
            target_resolution_enabled: config.get_bool("jev.target_resolution.enabled", true),
            settle_enabled: config.get_bool("jev.settle.enabled", true),
            trim_enabled: config.get_bool("jev.trim.enabled", true),
            verdict_enabled: config.get_bool("jev.verdict.enabled", true),
            log_dir: None,
        }
    }

    fn point_enabled(&self, point: DecisionPoint) -> bool {
        match point {
            DecisionPoint::TargetResolution => self.target_resolution_enabled,
            DecisionPoint::Settle => self.settle_enabled,
            DecisionPoint::SnapshotTrim => self.trim_enabled,
            DecisionPoint::Verdict => self.verdict_enabled,
        }
    }

    fn threshold_for(&self, point: DecisionPoint) -> f32 {
        match point {
            DecisionPoint::Settle => self.settle_min_confidence,
            _ => self.min_confidence,
        }
    }

    fn log_dir(&self) -> PathBuf {
        self.log_dir.clone().unwrap_or_else(decision_log_path)
    }
}

// ---------------------------------------------------------------------------
// JevDecider
// ---------------------------------------------------------------------------

/// Availability cache state (data-model state transitions).
#[derive(Default)]
struct ProbeState {
    available_until: Option<Instant>,
    unavailable_until: Option<Instant>,
    consecutive_fallbacks: u32,
}

/// The whole public surface consumers get (contracts/decision-points.md).
pub struct JevDecider {
    transport: Arc<dyn DecisionTransport>,
    config: JevConfig,
    probe: AsyncMutex<ProbeState>,
}

impl JevDecider {
    /// Test/injection constructor.
    pub fn new(config: JevConfig, transport: Arc<dyn DecisionTransport>) -> Self {
        Self {
            transport,
            config,
            probe: AsyncMutex::new(ProbeState::default()),
        }
    }

    /// Build from user config with the live reqwest transport.
    /// `None` when all four points are disabled — zero footprint (SC-005).
    pub fn from_config(config: &Config) -> Option<Self> {
        let cfg = JevConfig::from_config(config);
        if !cfg.target_resolution_enabled
            && !cfg.settle_enabled
            && !cfg.trim_enabled
            && !cfg.verdict_enabled
        {
            return None;
        }
        let transport = Arc::new(ReqwestTransport::new(
            &cfg.base_url,
            &cfg.api_key,
            cfg.request_timeout,
        ));
        Some(Self::new(cfg, transport))
    }

    /// Convenience: load config from disk and build (lazy call sites).
    pub fn from_disk() -> Option<Self> {
        match Config::load() {
            Ok(c) => Self::from_config(&c),
            Err(_) => Self::from_config(&Config::defaults()),
        }
    }

    /// One delegated decision. Order (data-model.md): point enabled? →
    /// availability (probe if stale) → transport (bounded) → confidence
    /// gate. Exactly one decision-log row per call, fallbacks included.
    pub async fn decide(&self, point: DecisionPoint, req: DecisionRequest) -> Decision {
        let started = Instant::now();
        let kind = req.questions.first_kind().unwrap_or_else(|| point.kind());

        if !self.config.point_enabled(point) {
            return self.finish_fallback(point, &kind, started, FallbackReason::Disabled);
        }

        // Availability (probe only when stale; one cache shared by all points).
        let available = {
            let mut st = self.probe.lock().await;
            let now = Instant::now();
            if st.available_until.is_some_and(|t| t > now) {
                true
            } else if st.unavailable_until.is_some_and(|t| t > now) {
                false
            } else {
                let ok = tokio::time::timeout(self.config.request_timeout, self.transport.probe())
                    .await
                    .unwrap_or(false);
                if ok {
                    st.available_until = Some(now + self.config.probe_ttl);
                    st.consecutive_fallbacks = 0;
                } else {
                    st.unavailable_until = Some(now + NEGATIVE_TTL);
                }
                ok
            }
        };
        if !available {
            return self.finish_fallback(point, &kind, started, FallbackReason::Unavailable);
        }

        // Transport, bounded by the total timeout; 422 → shrink once → retry.
        let answer = match self.ask_with_shrink(&req).await {
            Ok(a) => a,
            Err(reason) => {
                self.note_fallback().await;
                return self.finish_fallback(point, &kind, started, reason);
            }
        };

        // Confidence gate (+ tie refusal for choice picks).
        let needed = self.config.threshold_for(point);
        let confident = match &answer {
            Answer::Choice { question, probabilities, .. } => {
                answer.confidence() >= needed && !top2_tied(&req.questions, question, probabilities)
            }
            Answer::Scores { .. } => answer.confidence() >= needed,
        };
        if !confident {
            let got = answer.confidence();
            return self.finish(
                point,
                &kind,
                started,
                Decision::LowConfidence { got, needed },
                Some(&answer),
            );
        }
        self.reset_fallback_streak().await;
        self.finish(point, &kind, started, Decision::Confident(answer), None)
    }

    async fn ask_with_shrink(&self, req: &DecisionRequest) -> Result<Answer, FallbackReason> {
        let mut req = req.clone();
        let mut shrunk = false; // one shrink retry, then fallback (contract rule 3)
        loop {
            let wire = SystemOneRequestWire::build(&req, &self.config.model);
            match self.bounded_ask(&wire).await {
                Ok(resp) => return extract_answer(&resp, &req.questions),
                Err(TransportError::Rejected422 { .. }) => {
                    if !shrunk && req.questions.shrink() {
                        shrunk = true;
                        continue;
                    }
                    return Err(FallbackReason::Rejected422);
                }
                Err(e) => return Err(to_reason(e)),
            }
        }
    }

    async fn bounded_ask(
        &self,
        wire: &SystemOneRequestWire,
    ) -> Result<wire::SystemOneResponse, TransportError> {
        let owned = wire.as_request();
        let fut = self.transport.ask(&owned);
        match tokio::time::timeout(self.config.request_timeout, fut).await {
            Ok(r) => r,
            Err(_) => Err(TransportError::Timeout),
        }
    }

    async fn note_fallback(&self) {
        let mut st = self.probe.lock().await;
        st.consecutive_fallbacks += 1;
        if st.consecutive_fallbacks >= REPROBE_AFTER_FALLBACKS {
            // Hysteresis: force a re-probe on the next decision.
            st.available_until = None;
        }
    }

    async fn reset_fallback_streak(&self) {
        self.probe.lock().await.consecutive_fallbacks = 0;
    }

    fn finish(
        &self,
        point: DecisionPoint,
        kind: &QuestionKind,
        started: Instant,
        decision: Decision,
        raw: Option<&Answer>,
    ) -> Decision {
        let (answer, confidence, error, fallback) = match &decision {
            Decision::Confident(a) => (a.to_log_value(), Some(a.confidence()), None, false),
            Decision::LowConfidence { got, .. } => (
                raw.map(|a| a.to_log_value()).unwrap_or(serde_json::Value::Null),
                Some(*got),
                None,
                false,
            ),
            Decision::Fallback { reason } => (
                serde_json::Value::Null,
                None,
                Some(reason.as_str()),
                true,
            ),
        };
        append_record(
            &self.config.log_dir(),
            &DecisionRecord {
                ts: chrono::Utc::now().to_rfc3339(),
                point: point.name().into(),
                question_kind: kind.as_str().into(),
                answer,
                confidence,
                latency_ms: started.elapsed().as_millis() as u64,
                fallback_used: fallback,
                error: error.map(str::to_string),
            },
        );
        decision
    }

    fn finish_fallback(
        &self,
        point: DecisionPoint,
        kind: &QuestionKind,
        started: Instant,
        reason: FallbackReason,
    ) -> Decision {
        self.finish(point, kind, started, Decision::Fallback { reason }, None)
    }
}

/// Owned wire request (private — keeps reqwest shapes out of the API).
struct SystemOneRequestWire {
    state: serde_json::Value,
    model: String,
    questions: serde_json::Map<String, serde_json::Value>,
}

impl SystemOneRequestWire {
    fn build(req: &DecisionRequest, model: &str) -> Self {
        Self {
            state: req.state.clone(),
            model: model.to_string(),
            questions: questions_to_wire(&req.questions),
        }
    }

    fn as_request(&self) -> wire::SystemOneRequest {
        wire::SystemOneRequest {
            state: self.state.clone(),
            model: self.model.clone(),
            questions: self.questions.clone(),
        }
    }
}

fn to_reason(e: TransportError) -> FallbackReason {
    match e {
        TransportError::Timeout => FallbackReason::Timeout,
        TransportError::Rejected422 { .. } => FallbackReason::Rejected422,
        TransportError::Unauthorized => FallbackReason::Unavailable,
        TransportError::Wire(_) => FallbackReason::WireError,
    }
}

/// Tie refusal: the top two answer probabilities for the asked choice
/// question being equal means no single winner (target resolution must
/// refuse ambiguous picks; conservative for every choice point).
fn top2_tied(
    questions: &QuestionSet,
    question: &str,
    probabilities: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    if probabilities.len() < 2 {
        return false;
    }
    // Only consider options that were actually sent.
    let sent: Option<std::collections::HashSet<&str>> = questions.questions.iter().find_map(
        |(name, q)| {
            if name == question {
                match q {
                    Question::Choice { criteria, .. } => {
                        Some(criteria.iter().map(|c| c.key.as_str()).collect())
                    }
                    Question::Score { .. } => None,
                }
            } else {
                None
            }
        },
    );
    let mut probs: Vec<f32> = probabilities
        .iter()
        .filter(|(k, _)| sent.as_ref().map_or(true, |s| s.contains(k.as_str())))
        .filter_map(|(_, v)| v.as_f64().map(|f| f as f32))
        .collect();
    probs.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    probs.len() >= 2 && probs[0] == probs[1]
}

/// Typed extraction per point from the wire response.
fn extract_answer(
    resp: &wire::SystemOneResponse,
    questions: &QuestionSet,
) -> Result<Answer, FallbackReason> {
    match questions.first_kind() {
        Some(QuestionKind::Choice) => {
            let (name, _) = questions.questions.first().ok_or(FallbackReason::WireError)?;
            let raw = resp.answers.get(name).ok_or(FallbackReason::WireError)?;
            let a = wire::parse_answer(raw).ok_or(FallbackReason::WireError)?;
            if a.kind != "choice" {
                return Err(FallbackReason::WireError);
            }
            let choice = a.choice.clone().ok_or(FallbackReason::WireError)?;
            let probabilities = a.probabilities.clone().unwrap_or_default();
            Ok(Answer::Choice {
                question: name.clone(),
                key: choice,
                confidence: a.confidence.unwrap_or(0.0),
                probabilities,
            })
        }
        Some(QuestionKind::Score) => {
            let mut entries = Vec::with_capacity(questions.questions.len());
            for (name, _) in &questions.questions {
                let raw = resp.answers.get(name).ok_or(FallbackReason::WireError)?;
                let a = wire::parse_answer(raw).ok_or(FallbackReason::WireError)?;
                if a.kind != "score" {
                    return Err(FallbackReason::WireError);
                }
                entries.push(ScoreEntry {
                    question: name.clone(),
                    score: a.score.ok_or(FallbackReason::WireError)?,
                    confidence: a.confidence.unwrap_or(0.0),
                });
            }
            Ok(Answer::Scores { entries })
        }
        None => Err(FallbackReason::WireError),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_caps_and_redacts() {
        let long: String = "x".repeat(500);
        assert_eq!(clean(&long).chars().count(), DATA_CHAR_CAP);
        // A sk- style token must not survive redaction verbatim (the exact
        // masking marker is joey-core's business; token absence is the contract).
        assert!(!clean("key sk-abc...vwxy here").contains("sk-abc"));
    }

    #[test]
    fn target_resolution_caps_candidates() {
        let cands: Vec<(String, String)> = (1..=20)
            .map(|i| (format!("e{i}"), format!("btn {i}")))
            .collect();
        let req = target_resolution("click Login", "T", "http://u", &cands);
        match &req.questions.questions[0].1 {
            Question::Choice { criteria, .. } => assert_eq!(criteria.len(), TARGET_CANDIDATE_CAP),
            _ => panic!("expected choice"),
        }
        assert_eq!(req.state["instruction"], "click Login");
    }

    #[test]
    fn settle_request_shape() {
        let req = settle("dom quiet 375ms", "title|url|12345");
        match &req.questions.questions[0].1 {
            Question::Choice { criteria, .. } => {
                assert_eq!(criteria.len(), 2);
                assert_eq!(criteria[0].key, "settled");
            }
            _ => panic!("expected choice"),
        }
    }

    #[test]
    fn snapshot_trim_batches_score_questions() {
        let entries = vec![("e1".to_string(), "a".into()), ("e2".to_string(), "b".into())];
        let req = snapshot_trim("find pricing", "T", "http://u", &entries);
        assert_eq!(req.questions.questions.len(), 2);
        assert_eq!(req.questions.first_kind(), Some(QuestionKind::Score));
        match &req.questions.questions[0].1 {
            Question::Score { criteria, .. } => assert_eq!(criteria.len(), 4),
            _ => panic!("expected score"),
        }
    }

    #[test]
    fn config_from_dotted_keys() {
        let cfg = Config::defaults();
        let jc = JevConfig::from_config(&cfg);
        assert_eq!(jc.base_url, "http://127.0.0.1:8765");
        assert_eq!(jc.api_key, "local");
        assert_eq!(jc.min_confidence, 0.70);
        assert_eq!(jc.settle_min_confidence, 0.80);
        assert_eq!(jc.request_timeout, Duration::from_millis(1500));
        assert!(jc.target_resolution_enabled && jc.settle_enabled && jc.trim_enabled && jc.verdict_enabled);
    }
}
