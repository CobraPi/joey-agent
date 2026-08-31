//! T006 — model-profile prefix/pooling contract tests.
//!
//! Pins contracts/embedding-backend.md § Model Profiles (normative rules 1–2
//! and the profile table) plus the regression obligations from that
//! contract: "profile prefix/pooling unit tests (prefixes applied
//! pre-tokenization; mean pooling + L2 per profile; CodeRankEmbed's empty
//! document prefix)".

use joey_neurocode_rag::embed::profiles::{
    default_profile, lookup, rejection_reason, EmbedProfile, Pooling, CODERANK_EMBED,
    NOMIC_EMBED_TEXT_V1_5, REJECTED_CANDIDATES,
};

// ---------------------------------------------------------------------------
// Profile table — exact pins (contract table row 1)
// ---------------------------------------------------------------------------

#[test]
fn nomic_embed_text_v15_profile_is_pinned() {
    assert_eq!(NOMIC_EMBED_TEXT_V1_5.name, "nomic-embed-text-v1.5");
    assert_eq!(NOMIC_EMBED_TEXT_V1_5.dim, 768);
    assert_eq!(NOMIC_EMBED_TEXT_V1_5.ctx, 8192);
    assert_eq!(NOMIC_EMBED_TEXT_V1_5.pooling, Pooling::Mean);
    assert!(NOMIC_EMBED_TEXT_V1_5.l2_normalize, "client-side L2 required");
    assert_eq!(NOMIC_EMBED_TEXT_V1_5.prefix_query, "search_query: ");
    assert_eq!(NOMIC_EMBED_TEXT_V1_5.prefix_document, "search_document: ");
    assert_eq!(NOMIC_EMBED_TEXT_V1_5.license, "Apache-2.0");
}

// ---------------------------------------------------------------------------
// Profile table — exact pins (contract table row 2)
// ---------------------------------------------------------------------------

#[test]
fn coderank_embed_profile_is_pinned() {
    assert_eq!(CODERANK_EMBED.name, "CodeRankEmbed");
    assert_eq!(CODERANK_EMBED.dim, 768);
    assert_eq!(CODERANK_EMBED.ctx, 8192);
    assert_eq!(CODERANK_EMBED.pooling, Pooling::Mean);
    assert!(CODERANK_EMBED.l2_normalize, "client-side L2 required");
    assert_eq!(
        CODERANK_EMBED.prefix_query,
        "Represent this query for searching relevant code: "
    );
    // Load-bearing: CodeRankEmbed documents are UNPREFIXED.
    assert_eq!(CODERANK_EMBED.prefix_document, "");
    assert_eq!(CODERANK_EMBED.license, "MIT");
}

// ---------------------------------------------------------------------------
// 768-dim pin for BOTH profiles (task T006 deliverable)
// ---------------------------------------------------------------------------

#[test]
fn both_profiles_are_768_dim() {
    assert_eq!(NOMIC_EMBED_TEXT_V1_5.dim, 768);
    assert_eq!(CODERANK_EMBED.dim, 768);
    // The remote Copilot profile (Joey-native extension) is 1536-dim.
    for p in joey_neurocode_rag::embed::profiles::PROFILES {
        let expected = match p.name {
            "text-embedding-3-small" => 1536,
            _ => 768,
        };
        assert_eq!(p.dim, expected, "profile {} must be {}-dim", p.name, expected);
    }
}

// ---------------------------------------------------------------------------
// Prefixes applied PRE-TOKENIZATION: the profile exposes exactly the strings
// the embedder must produce for the tokenizer — prefix ++ raw text, raw text
// verbatim (contract rule 1: callers pass RAW text; prefixes are applied by
// the embedder BEFORE tokenization).
// ---------------------------------------------------------------------------

#[test]
fn prefixes_are_prepended_verbatim_pre_tokenization() {
    // nomic: query leg
    assert_eq!(
        NOMIC_EMBED_TEXT_V1_5.query_input("where is token validation handled"),
        "search_query: where is token validation handled"
    );
    // nomic: document leg
    assert_eq!(
        NOMIC_EMBED_TEXT_V1_5.document_input("fn validate_token() {}"),
        "search_document: fn validate_token() {}"
    );
    // CodeRankEmbed: query-only prefix
    assert_eq!(
        CODERANK_EMBED.query_input("where is token validation handled"),
        "Represent this query for searching relevant code: where is token validation handled"
    );
    // CodeRankEmbed: EMPTY document prefix — document_input is exactly the
    // raw text, byte-for-byte.
    assert_eq!(
        CODERANK_EMBED.document_input("fn validate_token() {}"),
        "fn validate_token() {}"
    );
}

#[test]
fn prefixing_never_mutates_raw_text() {
    // Raw text passes through verbatim: no trimming, no case folding — the
    // embedder must not alter caller text beyond prefixing (whitespace and
    // case carry signal for code).
    let raw_q = "  MixedCASE query with  extra  spaces\tand tab ";
    assert!(NOMIC_EMBED_TEXT_V1_5.query_input(raw_q).ends_with(raw_q));
    assert!(CODERANK_EMBED.query_input(raw_q).ends_with(raw_q));
    let raw_d = "\nfn main() {\n    let x = 1;\n}\n";
    assert!(NOMIC_EMBED_TEXT_V1_5.document_input(raw_d).ends_with(raw_d));
    assert!(CODERANK_EMBED.document_input(raw_d).ends_with(raw_d));
}

#[test]
fn coderank_embed_empty_document_prefix_is_exactly_empty() {
    // Pin the EMPTY prefix as a distinct, deliberate state: it is an empty
    // STRING (no document prefix), not whitespace, not None-ish.
    assert_eq!(CODERANK_EMBED.prefix_document.len(), 0);
    // And it differs from nomic's non-empty document prefix — the two
    // supported profiles already disagree on the mere existence of a
    // document prefix, which is why profiles exist at all (R2 rationale).
    assert_ne!(CODERANK_EMBED.prefix_document, NOMIC_EMBED_TEXT_V1_5.prefix_document);
    assert!(!NOMIC_EMBED_TEXT_V1_5.prefix_document.is_empty());
}

// ---------------------------------------------------------------------------
// Pooling pins (contract rule 2: pooling MUST match the profile — mean
// pooling + client-side L2 for both; the backend never improvises)
// ---------------------------------------------------------------------------

#[test]
fn both_profiles_use_mean_pooling_with_client_side_l2() {
    assert_eq!(NOMIC_EMBED_TEXT_V1_5.pooling, Pooling::Mean);
    assert_eq!(CODERANK_EMBED.pooling, Pooling::Mean);
    assert!(NOMIC_EMBED_TEXT_V1_5.l2_normalize);
    assert!(CODERANK_EMBED.l2_normalize);
    // Stable identifier persisted in rag_index_meta.pooling (schema v3).
    assert_eq!(Pooling::Mean.as_str(), "mean");
}

// ---------------------------------------------------------------------------
// Lookup by name + Default resolution (API shape)
// ---------------------------------------------------------------------------

#[test]
fn lookup_by_name_resolves_both_profiles() {
    let nomic = lookup("nomic-embed-text-v1.5").expect("nomic profile must resolve");
    assert_eq!(nomic, &NOMIC_EMBED_TEXT_V1_5);
    let cre = lookup("CodeRankEmbed").expect("CodeRankEmbed profile must resolve");
    assert_eq!(cre, &CODERANK_EMBED);
    // Exact-name lookup only — no fuzzy resolution, no case folding.
    assert!(lookup("Nomic-Embed-Text-v1.5").is_none());
    assert!(lookup("nomic_embed_text_v1_5").is_none());
    assert!(lookup("").is_none());
    assert!(lookup("nomic-embed-code").is_none(), "rejected candidate must not resolve");
}

#[test]
fn default_resolution_is_nomic_embed_text_v15() {
    let d: &EmbedProfile = default_profile();
    assert_eq!(d.name, "nomic-embed-text-v1.5");
    assert_eq!(d, &NOMIC_EMBED_TEXT_V1_5);
}

// ---------------------------------------------------------------------------
// RECORDED rejection: nomic-embed-code must never be re-litigated
// (contract: "Evaluated and REJECTED … Recorded so tasks do not
// re-litigate"; research.md R2 alternatives considered)
// ---------------------------------------------------------------------------

#[test]
fn text_embedding_3_small_profile_pins() {
    let p = lookup("text-embedding-3-small").expect("text-embedding-3-small must resolve");
    assert_eq!(p.name, "text-embedding-3-small");
    assert_eq!(p.dim, 1536);
    assert_eq!(p.prefix_query, "");
    assert_eq!(p.prefix_document, "");
    assert_eq!(p.query_input("x"), "x");
    assert_eq!(p.document_input("x"), "x");
}

#[test]
fn rejected_nomic_embed_code_is_recorded_not_resolvable() {
    let recorded = REJECTED_CANDIDATES
        .iter()
        .find(|r| r.name == "nomic-embed-code")
        .expect("nomic-embed-code rejection must be RECORDED");
    // The recorded reason must carry the decisive facts (7B Qwen2.5 decoder,
    // ~28 GB, 3584-dim, last-token pooling, CLI resource incompatibility).
    let reason = recorded.reason;
    assert!(reason.contains("28 GB"), "reason must record the ~28 GB footprint: {reason}");
    assert!(reason.contains("3584"), "reason must record the 3584-dim mismatch: {reason}");
    assert!(reason.contains("last-token pooling"), "reason must record pooling mismatch: {reason}");
    // Same text is retrievable via the helper.
    assert_eq!(rejection_reason("nomic-embed-code"), Some(recorded.reason));
    assert_eq!(rejection_reason("nomic-embed-text-v1.5"), None);
}
