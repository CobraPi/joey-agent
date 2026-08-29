//! Reciprocal Rank Fusion (T017).
//!
//! `score(d) = Σ_legs 1/(60 + rank_leg(d))` with k=60 and deterministic
//! tie-break (keyword rank first, then `chunk_id` lexical ascending) per
//! contracts/hybrid-search.md stage 4.
//!
//! Ranks are the **ascending 1-based ordinals** each leg produces (the
//! keyword leg's come from the T016 bm25-negated → ordinal conversion;
//! the dense leg's are positions in the cosine-ordered candidate list) —
//! never raw bm25 values or cosine scores. A chunk absent from a leg
//! contributes nothing from that leg.

/// The RRF smoothing constant `k` = 60 (contracts/hybrid-search.md
/// stage 4). Shared with the hybrid pipeline ([`crate::search::hybrid`]).
pub const RRF_K: u32 = 60;

/// One document's per-leg ranks entering fusion. `chunk_id`s MUST be
/// unique across the entry list (the pipeline unions the two legs before
/// calling); a `None` rank means the chunk did not match that leg.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RrfEntry {
    pub chunk_id: String,
    /// 1-based ascending ordinal in the keyword leg (T016 conversion).
    pub keyword_rank: Option<u32>,
    /// 1-based rank in the dense leg (cosine order).
    pub dense_rank: Option<u32>,
}

/// A fused result: final RRF score plus the per-leg ranks it came from
/// (carried so the pipeline can populate `keyword_rank`/`semantic_rank`
/// without a second join).
#[derive(Debug, Clone, PartialEq)]
pub struct RrfScored {
    pub chunk_id: String,
    pub score: f64,
    pub keyword_rank: Option<u32>,
    pub dense_rank: Option<u32>,
}

/// One leg's contribution: `1/(k + rank)` with 1-based `rank`.
pub fn leg_contribution(rank: u32) -> f64 {
    1.0 / (RRF_K as f64 + rank as f64)
}

/// Tie-break key for the keyword-first rule: a present keyword rank wins
/// over none (a keyword match is stronger evidence at equal score);
/// `u32::MAX` stands in for "no keyword rank".
fn keyword_tie_key(keyword_rank: Option<u32>) -> u32 {
    keyword_rank.unwrap_or(u32::MAX)
}

/// Fuse the two legs' ranked lists (contracts/hybrid-search.md stage 4):
///
/// ```text
/// score(d) = Σ_legs 1/(60 + rank_leg(d))
/// ```
///
/// Output order is **deterministic**: fused score descending (`total_cmp`
/// — NaN-free by construction), then keyword rank ascending (`None` last),
/// then `chunk_id` lexical ascending. Entries with neither rank score 0
/// and sort to the end (the pipeline never produces them; the ordering is
/// still total for robustness).
pub fn rrf_fuse(entries: Vec<RrfEntry>) -> Vec<RrfScored> {
    let mut scored: Vec<RrfScored> = entries
        .into_iter()
        .map(|e| {
            let mut score = 0.0f64;
            if let Some(k) = e.keyword_rank {
                score += leg_contribution(k);
            }
            if let Some(d) = e.dense_rank {
                score += leg_contribution(d);
            }
            RrfScored {
                chunk_id: e.chunk_id,
                score,
                keyword_rank: e.keyword_rank,
                dense_rank: e.dense_rank,
            }
        })
        .collect();
    scored.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| {
                keyword_tie_key(a.keyword_rank).cmp(&keyword_tie_key(b.keyword_rank))
            })
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
    });
    scored
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, kw: Option<u32>, dense: Option<u32>) -> RrfEntry {
        RrfEntry {
            chunk_id: id.to_string(),
            keyword_rank: kw,
            dense_rank: dense,
        }
    }

    /// The contract's constant is k = 60.
    #[test]
    fn t017_k_is_60() {
        assert_eq!(RRF_K, 60);
        assert_eq!(leg_contribution(1), 1.0 / 61.0);
    }

    /// Hand-computed fusion scores:
    ///
    /// | chunk | keyword | dense | score |
    /// |---|---|---|---|
    /// | both | 1 | 2 | 1/61 + 1/62 ≈ 0.03252247488101534 |
    /// | kw_only | 3 | — | 1/63 ≈ 0.015873015873015872 |
    /// | dense_only | — | 1 | 1/61 ≈ 0.01639344262295082 |
    ///
    /// Expected order: both > dense_only > kw_only (a rank-1 dense hit
    /// beats a rank-3 keyword hit — 1/61 > 1/63).
    #[test]
    fn t017_hand_computed_scores_and_ordering() {
        let fused = rrf_fuse(vec![
            entry("kw_only", Some(3), None),
            entry("both", Some(1), Some(2)),
            entry("dense_only", None, Some(1)),
        ]);
        assert_eq!(
            fused.iter().map(|s| s.chunk_id.as_str()).collect::<Vec<_>>(),
            vec!["both", "dense_only", "kw_only"]
        );
        // Exact hand-computed values (same f64 ops as the implementation).
        assert_eq!(fused[0].score, 1.0 / 61.0 + 1.0 / 62.0);
        assert_eq!(fused[1].score, 1.0 / 61.0);
        assert_eq!(fused[2].score, 1.0 / 63.0);
        assert!(fused[0].score > fused[1].score && fused[1].score > fused[2].score);
        // Per-leg ranks carried through verbatim.
        assert_eq!((fused[0].keyword_rank, fused[0].dense_rank), (Some(1), Some(2)));
        assert_eq!((fused[1].keyword_rank, fused[1].dense_rank), (None, Some(1)));
        assert_eq!((fused[2].keyword_rank, fused[2].dense_rank), (Some(3), None));
    }

    /// Exact tie: kw 1 + dense 4 and kw 4 + dense 1 both score
    /// 1/61 + 1/64 — the keyword-first tie-break puts the BETTER keyword
    /// rank first, deterministically.
    #[test]
    fn t017_tie_breaks_keyword_rank_first() {
        let fused = rrf_fuse(vec![
            entry("worse_kw", Some(4), Some(1)),
            entry("better_kw", Some(1), Some(4)),
        ]);
        assert_eq!(
            fused[0].score,
            fused[1].score,
            "precondition: genuinely tied scores"
        );
        assert_eq!(
            fused.iter().map(|s| s.chunk_id.as_str()).collect::<Vec<_>>(),
            vec!["better_kw", "worse_kw"],
            "at equal score the smaller keyword rank leads"
        );
    }

    /// Tie with one keyword-only and one dense-only entry at the same
    /// single-leg rank (kw 5 vs dense 5 → both 1/65): the keyword-present
    /// entry leads; a second keyword-present entry at the same score
    /// resolves by `chunk_id` lexical ascending.
    #[test]
    fn t017_tie_keyword_present_beats_absent_then_chunk_id() {
        let fused = rrf_fuse(vec![
            entry("zzz_dense", None, Some(5)),
            entry("zzz_kw", Some(5), None),
            entry("aaa_kw", Some(5), None),
        ]);
        assert_eq!(fused[0].score, fused[1].score);
        assert_eq!(fused[1].score, fused[2].score);
        assert_eq!(
            fused.iter().map(|s| s.chunk_id.as_str()).collect::<Vec<_>>(),
            vec!["aaa_kw", "zzz_kw", "zzz_dense"],
            "keyword-present entries first (chunk_id asc), keyword-absent last"
        );
    }

    /// Degenerate-input determinism: identical rank patterns tie exactly
    /// and resolve purely by `chunk_id` lexical ascending.
    #[test]
    fn t017_identical_patterns_resolve_by_chunk_id_lexical() {
        let fused = rrf_fuse(vec![
            entry("zzz", Some(7), Some(7)),
            entry("aaa", Some(7), Some(7)),
        ]);
        assert_eq!(fused[0].score, fused[1].score);
        assert_eq!(
            fused.iter().map(|s| s.chunk_id.as_str()).collect::<Vec<_>>(),
            vec!["aaa", "zzz"]
        );
        assert_eq!(fused[0].score, 2.0 * leg_contribution(7));
    }

    /// Both legs absent → score 0 (tail of the list); empty input fuses to
    /// empty output; single-leg-only input is the T016 single-leg formula.
    #[test]
    fn t017_edges_zero_score_empty_input() {
        assert_eq!(rrf_fuse(vec![]), Vec::new());
        let fused = rrf_fuse(vec![entry("orphan", None, None), entry("kw1", Some(1), None)]);
        assert_eq!(fused[0].chunk_id, "kw1");
        assert_eq!(fused[0].score, 1.0 / 61.0);
        assert_eq!(fused[1].chunk_id, "orphan");
        assert_eq!(fused[1].score, 0.0);
    }

    /// A both-legs chunk ALWAYS outranks any single-leg chunk whose ranks
    /// are no better (the blend principle: agreement across legs wins).
    #[test]
    fn t017_both_legs_agreement_outranks_single_leg() {
        let fused = rrf_fuse(vec![
            entry("single", Some(1), None), // 1/61
            entry("agreed", Some(2), Some(2)), // 2/62
        ]);
        assert_eq!(fused[0].chunk_id, "agreed");
        assert!(fused[0].score > fused[1].score);
    }
}
