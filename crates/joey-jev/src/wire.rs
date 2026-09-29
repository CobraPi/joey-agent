//! System One wire types for the local Jev decision service
//! (contracts/jev-systemone.md, verified against `jev_server.py` 2026-09-25).
//!
//! Only `choice` and `score` questions are modeled — `noul` answers carry no
//! confidence field and are therefore unusable for gated decisions
//! (research.md R2). The service never generates text.

use serde::{Deserialize, Serialize};

/// One keyed option of a `choice` question (`criteria` entry).
///
/// Preserved as an ordered pair: candidate ordering must survive
/// serialization (serde_json `preserve_order` is enabled workspace-wide).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Criterion {
    pub key: String,
    pub descriptor: String,
}

/// A typed question (discriminated on `type`).
#[derive(Debug, Clone, PartialEq)]
pub enum Question {
    /// 1–255 keyed options.
    Choice {
        instructions: String,
        criteria: Vec<Criterion>,
    },
    /// 1–10 rubric levels (irrelevant → essential).
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
}

impl Question {
    pub fn kind(&self) -> QuestionKind {
        match self {
            Question::Choice { .. } => QuestionKind::Choice,
            Question::Score { .. } => QuestionKind::Score,
        }
    }

    /// 422 shrink: halve the payload — keep the first half of a choice's
    /// criteria, and a question set keeps its first half of score questions.
    /// Returns true when anything was actually dropped.
    pub fn shrink(&mut self) -> bool {
        match self {
            Question::Choice { criteria, .. } => shrink_vec(criteria, 1),
            Question::Score { .. } => false, // rubric levels are fixed by the contract
        }
    }
}

/// Halve `v` keeping at least `min` leading items; true when items dropped.
fn shrink_vec<T>(v: &mut Vec<T>, min: usize) -> bool {
    let half = (v.len() + 1) / 2;
    let keep = half.max(min);
    if keep < v.len() {
        v.truncate(keep);
        true
    } else {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionKind {
    Choice,
    Score,
}

impl QuestionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            QuestionKind::Choice => "choice",
            QuestionKind::Score => "score",
        }
    }
}

/// Ordered question set — `questions` is a dict on the wire, answered in one
/// round-trip (batched score questions for snapshot trimming are one request).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QuestionSet {
    pub questions: Vec<(String, Question)>,
}

impl QuestionSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.questions.is_empty()
    }

    pub fn first_kind(&self) -> Option<QuestionKind> {
        self.questions.first().map(|(_, q)| q.kind())
    }

    /// Shrink every question; true when any item was dropped.
    pub fn shrink(&mut self) -> bool {
        let mut changed = false;
        // ponytail: halve the question count wholesale (keep leading half,
        // min 1) — per-question importance ordering is unknown to the client.
        changed |= shrink_vec(&mut self.questions, 1);
        for (_, q) in &mut self.questions {
            changed |= q.shrink();
        }
        changed
    }
}

/// `POST /v1/systemone` body.
#[derive(Debug, Clone, Serialize)]
pub struct SystemOneRequest {
    /// Page/instruction context — data only, never service-directed prose
    /// (prompt-injection posture, FR-007).
    pub state: serde_json::Value,
    pub model: String,
    pub questions: serde_json::Map<String, serde_json::Value>,
}

/// One answer from `POST /v1/systemone`.
#[derive(Debug, Clone, Deserialize)]
pub struct WireAnswer {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub choice: Option<String>,
    #[serde(default)]
    pub score: Option<u32>,
    #[serde(default)]
    pub legend: Option<Vec<String>>,
    #[serde(default)]
    pub confidence: Option<f32>,
    #[serde(default)]
    pub probabilities: Option<serde_json::Map<String, serde_json::Value>>,
}

/// `POST /v1/systemone` response body. Answers stay raw `Value`s on the
/// wire type (Map<String, WireAnswer> hits no serde impl with
/// preserve_order); parse per-entry via [`parse_answer`].
#[derive(Debug, Clone, Deserialize)]
pub struct SystemOneResponse {
    #[serde(default)]
    pub model: String,
    pub answers: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub usage: Usage,
}

/// Parse one raw answer entry into its typed shape (None on shape drift).
pub fn parse_answer(v: &serde_json::Value) -> Option<WireAnswer> {
    serde_json::from_value(v.clone()).ok()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

/// Serialize the question set into its wire dict form.
pub fn questions_to_wire(qs: &QuestionSet) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    for (name, q) in &qs.questions {
        let v = match q {
            Question::Choice {
                instructions,
                criteria,
            } => {
                let mut m = serde_json::Map::new();
                m.insert("type".into(), "choice".into());
                m.insert("instructions".into(), instructions.clone().into());
                let mut crit = serde_json::Map::new();
                for c in criteria {
                    crit.insert(c.key.clone(), c.descriptor.clone().into());
                }
                m.insert("criteria".into(), crit.into());
                m.into()
            }
            Question::Score {
                instructions,
                criteria,
            } => {
                let mut m = serde_json::Map::new();
                m.insert("type".into(), "score".into());
                m.insert("instructions".into(), instructions.clone().into());
                m.insert(
                    "criteria".into(),
                    serde_json::Value::Array(criteria.iter().map(|c| c.clone().into()).collect()),
                );
                m.into()
            }
        };
        out.insert(name.clone(), v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_wire_shape() {
        let mut qs = QuestionSet::new();
        qs.questions.push((
            "target".into(),
            Question::Choice {
                instructions: "pick".into(),
                criteria: vec![
                    Criterion { key: "e1".into(), descriptor: "a".into() },
                    Criterion { key: "e2".into(), descriptor: "b".into() },
                ],
            },
        ));
        let wire = questions_to_wire(&qs);
        let t = &wire["target"];
        assert_eq!(t["type"], "choice");
        assert_eq!(t["criteria"]["e1"], "a");
        // Ordering preserved (preserve_order feature).
        let keys: Vec<_> = t["criteria"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["e1", "e2"]);
    }

    #[test]
    fn shrink_halves_criteria_and_questions() {
        let mut qs = QuestionSet::new();
        qs.questions.push((
            "target".into(),
            Question::Choice {
                instructions: "pick".into(),
                criteria: (1..=4)
                    .map(|i| Criterion { key: format!("e{i}"), descriptor: "d".into() })
                    .collect(),
            },
        ));
        for i in 0..4 {
            qs.questions.push((
                format!("q{i}"),
                Question::Score { instructions: "s".into(), criteria: vec!["a".into()] },
            ));
        }
        assert!(qs.shrink());
        // 5 questions → leading ceil(5/2) = 3 kept; choice criteria 4 → 2.
        assert_eq!(qs.questions.len(), 3);
        match &qs.questions[0].1 {
            Question::Choice { criteria, .. } => assert_eq!(criteria.len(), 2),
            _ => panic!("expected choice"),
        }
        // Repeated shrink converges to the minimum (1 question, 1 choice
        // criterion) and then reports no change.
        while qs.shrink() {}
        assert_eq!(qs.questions.len(), 1);
        match &qs.questions[0].1 {
            Question::Choice { criteria, .. } => assert_eq!(criteria.len(), 1),
            _ => panic!("expected choice"),
        }
        assert!(!qs.shrink());
    }

    #[test]
    fn response_parses() {
        let raw = r#"{
            "model": "laya-mlx-0.2.0",
            "answers": {
                "target": {"type": "choice", "choice": "e2", "confidence": 0.9,
                           "probabilities": {"e1": 0.1, "e2": 0.9}}
            },
            "usage": {"input_tokens": 42, "output_tokens": 0}
        }"#;
        let r: SystemOneResponse = serde_json::from_str(raw).unwrap();
        let a = &r.answers["target"];
        assert_eq!(a["choice"].as_str(), Some("e2"));
        assert_eq!(a["confidence"].as_f64(), Some(0.9));
        assert_eq!(r.usage.output_tokens, 0);
    }
}
