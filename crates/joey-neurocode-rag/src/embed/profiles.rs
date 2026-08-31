//! Model profile table (name, dim, ctx, pooling, prefixes, license).
//!
//! Pooling and prefix handling live in this per-model PROFILE table, NOT in
//! backend code, so swapping models cannot silently corrupt the index
//! (contracts/embedding-backend.md § Model Profiles — normative).
//!
//! Profiles:
//!
//! - `nomic-embed-text-v1.5` — 768-dim, 8192 ctx, mean pooling + client-side
//!   L2, prefixes `search_query: ` / `search_document: `, Apache-2.0 (default).
//! - `CodeRankEmbed` — 768-dim, 8192 ctx, mean pooling + client-side L2,
//!   query-only prefix `Represent this query for searching relevant code: `,
//!   EMPTY document prefix, MIT.
//!
//! Normative rules pinned here (contracts/embedding-backend.md § Model
//! Profiles rules 1–2):
//!
//! 1. The embedder MUST apply the profile's prefixes BEFORE tokenization;
//!    callers pass raw text ([`EmbedProfile::query_input`] /
//!    [`EmbedProfile::document_input`] produce exactly the string the
//!    tokenizer must consume — prefix ++ raw text, nothing else).
//! 2. Pooling MUST match the profile (mean pooling + client-side L2 for
//!    both accepted profiles); the backend never improvises.
//!
//! REJECTED candidate evaluations are RECORDED in [`REJECTED_CANDIDATES`]
//! so tasks never re-litigate the choice (see the module-level record below;
//! research.md R2 "Alternatives considered").

/// Pooling mode pinned by a model profile.
///
/// Only mean pooling is supported: every accepted profile uses mean pooling
/// + client-side L2 (contract profile table). Decoder-style last-token
/// pooling was evaluated and rejected with `nomic-embed-code` (see
/// [`REJECTED_CANDIDATES`]) — adding a variant requires a new accepted
/// profile, never a backend-side improvisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pooling {
    /// Mean pooling over token embeddings followed by client-side L2
    /// normalization (`l2_normalize = true` on the profile).
    Mean,
}

impl Pooling {
    /// Stable identifier persisted in `rag_index_meta.pooling` (schema v3,
    /// T005). Profile identity (name + dim + pooling) is persisted alongside
    /// `model`; mismatch on load → `ProfileMismatch` → full semantic rebuild.
    pub const fn as_str(self) -> &'static str {
        match self {
            Pooling::Mean => "mean",
        }
    }
}

/// A model profile: everything the embedder must know about a model that is
/// NOT backend code — dimension, pooling/norm, and the query/document
/// prefixes that MUST be prepended before tokenization.
///
/// Swapping models without changing profile handling accordingly is a
/// contract violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbedProfile {
    /// Profile identity (persisted in `rag_index_meta.embed_profile`).
    pub name: &'static str,
    /// Embedding dimensionality (768 for both accepted profiles).
    pub dim: u32,
    /// Token context window (8192 for both accepted profiles).
    pub ctx: u32,
    /// Pooling mode — [`Pooling::Mean`] for both accepted profiles.
    pub pooling: Pooling,
    /// L2-normalize the pooled vector client-side before returning
    /// (true for both accepted profiles: "mean pooling + L2 (client-side)").
    pub l2_normalize: bool,
    /// Prefix prepended to RAW query text BEFORE tokenization.
    pub prefix_query: &'static str,
    /// Prefix prepended to RAW document/chunk text BEFORE tokenization.
    /// EMPTY string means documents are embedded unprefixed (CodeRankEmbed).
    pub prefix_document: &'static str,
    /// License of the model artifacts (attribution ships with re-hosted
    /// artifacts — contracts/embedding-backend.md § Local Model Artifacts).
    pub license: &'static str,
}

impl EmbedProfile {
    /// The exact string the tokenizer MUST consume for a query: the profile's
    /// query prefix prepended to the raw text BEFORE tokenization.
    ///
    /// Contract rule 1: callers pass RAW text — prefixes are applied by the
    /// embedder, pre-tokenization. The raw text is passed through verbatim
    /// (no trimming, no case folding): `prefix ++ raw`.
    pub fn query_input(&self, raw: &str) -> String {
        format!("{}{}", self.prefix_query, raw)
    }

    /// The exact string the tokenizer MUST consume for a document/chunk:
    /// the profile's document prefix prepended to the raw text BEFORE
    /// tokenization. For profiles with an empty document prefix
    /// (CodeRankEmbed) this is exactly the raw text.
    pub fn document_input(&self, raw: &str) -> String {
        format!("{}{}", self.prefix_document, raw)
    }
}

/// Profile: `nomic-embed-text-v1.5` — the DEFAULT local model profile
/// (research.md R2: official ONNX exports exist, Apache-2.0, 768-dim, 8192
/// ctx; task prefixes `search_query: ` / `search_document: `).
pub const NOMIC_EMBED_TEXT_V1_5: EmbedProfile = EmbedProfile {
    name: "nomic-embed-text-v1.5",
    dim: 768,
    ctx: 8192,
    pooling: Pooling::Mean,
    l2_normalize: true,
    prefix_query: "search_query: ",
    prefix_document: "search_document: ",
    license: "Apache-2.0",
};

/// Profile: `CodeRankEmbed` — the supported alternative (research.md R2:
/// 137M params, MIT, 8192 ctx, mean pooling + L2; query prefix
/// `Represent this query for searching relevant code: `, documents
/// UNPREFIXED — empty document prefix is load-bearing, not an omission).
pub const CODERANK_EMBED: EmbedProfile = EmbedProfile {
    name: "CodeRankEmbed",
    dim: 768,
    ctx: 8192,
    pooling: Pooling::Mean,
    l2_normalize: true,
    prefix_query: "Represent this query for searching relevant code: ",
    prefix_document: "",
    license: "MIT",
};

/// Profile: `text-embedding-3-small` — GitHub Copilot's OpenAI-compatible
/// REMOTE embedding model, served at `POST {base}/embeddings` when the
/// LLM provider selects a Copilot wire (Joey-native extension; upstream
/// Hermes has no Copilot embeddings path). 1536-dim; NO task prefixes
/// (the service is instruction-free — unlike nomic, `search_query:`/
/// `search_document:` prefixes MUST NOT be prepended); vectors are
/// L2-normalized client-side after decode, pooling happens server-side.
pub const TEXT_EMBEDDING_3_SMALL: EmbedProfile = EmbedProfile {
    name: "text-embedding-3-small",
    dim: 1536,
    ctx: 8192,
    pooling: Pooling::Mean,
    l2_normalize: true,
    prefix_query: "",
    prefix_document: "",
    license: "Proprietary — served via the GitHub Copilot subscription",
};

/// The accepted profile table. Lookup by name via [`lookup`]; the default
/// resolution is [`default_profile`] ([`DEFAULT_PROFILE_NAME`]).
pub const PROFILES: &[EmbedProfile] =
    &[NOMIC_EMBED_TEXT_V1_5, CODERANK_EMBED, TEXT_EMBEDDING_3_SMALL];

/// Default profile identity (research.md R2: nomic-embed-text-v1.5 is the
/// default model profile; resolved whenever no explicit profile is set).
pub const DEFAULT_PROFILE_NAME: &str = "nomic-embed-text-v1.5";

/// Look up an accepted profile by exact name.
///
/// Rejected candidates (e.g. `nomic-embed-code`) are deliberately absent —
/// see [`REJECTED_CANDIDATES`] for the recorded reason.
pub fn lookup(name: &str) -> Option<&'static EmbedProfile> {
    PROFILES.iter().find(|p| p.name == name)
}

/// Default profile resolution — [`NOMIC_EMBED_TEXT_V1_5`].
pub fn default_profile() -> &'static EmbedProfile {
    // Const-table invariant: the default name is present in PROFILES
    // (pinned by unit + integration tests).
    lookup(DEFAULT_PROFILE_NAME)
        .expect("DEFAULT_PROFILE_NAME must resolve to a profile in PROFILES")
}

/// A REJECTED model-candidate evaluation, recorded so tasks never
/// re-litigate the choice (contracts/embedding-backend.md § Model Profiles:
/// "Evaluated and REJECTED … Recorded so tasks do not re-litigate").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RejectedProfile {
    pub name: &'static str,
    /// Why it was rejected — authoritative, from research.md R2.
    pub reason: &'static str,
}

/// REJECTED candidate evaluations (do NOT add these as profiles).
pub const REJECTED_CANDIDATES: &[RejectedProfile] = &[RejectedProfile {
    name: "nomic-embed-code",
    reason: "7B Qwen2.5 decoder, ~28 GB f32, 3584-dim, last-token pooling — \
             resource-incompatible with CLI targets (28 GB memory, 3584-dim \
             storage blowup vs 768, decoder last-token pooling mismatches the \
             mean-pooling pipeline). REJECTED per research.md R2; supported \
             local profiles are nomic-embed-text-v1.5 and CodeRankEmbed.",
}];

/// Recorded rejection reason for a name, if it was evaluated and rejected.
pub fn rejection_reason(name: &str) -> Option<&'static str> {
    REJECTED_CANDIDATES
        .iter()
        .find(|r| r.name == name)
        .map(|r| r.reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_integrity() {
        // Names are unique.
        for (i, p) in PROFILES.iter().enumerate() {
            assert!(
                !PROFILES.iter().skip(i + 1).any(|q| q.name == p.name),
                "duplicate profile name {}",
                p.name
            );
        }
        // No rejected candidate leaks into the accepted table.
        for r in REJECTED_CANDIDATES {
            assert!(lookup(r.name).is_none(), "{} must stay rejected", r.name);
        }
        // Default resolution is present and is nomic.
        assert_eq!(default_profile().name, DEFAULT_PROFILE_NAME);
    }
}
