//! Request-text discovery hints (identifier + file-path extraction).
//!
//! Free-text coding requests almost always name the artifacts they target —
//! `fix the UserServiceImpl`, `update \`findById\``, `look at
//! src/main/java/App.java`. This module extracts those mentions so the
//! context assembler (and the classifier's scope-fanout signal) can seed
//! graph search with high-precision symbols instead of blindly FTS-matching
//! every word of the sentence.
//!
//! Ranking (highest precision first):
//! 1. Backtick-quoted spans (explicit symbol mentions).
//! 2. Dotted references (`com.acme.Foo`, `Foo.bar`, `Vec::new`) — full token
//!    first, then non-trivial segments.
//! 3. CamelCase / snake_case identifiers.
//! 4. Long lowercase words that aren't stopwords (weak, last resort).

/// Hints extracted from a request text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryHints {
    /// Identifier candidates, best-first, deduped, capped.
    pub identifiers: Vec<String>,
    /// File/directory path mentions (as written), best-first, capped.
    pub file_paths: Vec<String>,
}

/// Source-ish extensions worth treating as file mentions.
const FILE_EXTS: &[&str] = &[
    "java", "kt", "py", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "rs", "rb", "php", "cs",
    "c", "h", "cpp", "hpp", "cc", "scala", "hs", "jl", "ml", "mli", "sh", "bash", "v", "agda",
    "swift", "lua", "ex", "exs", "sql", "xml", "yaml", "yml", "json", "toml", "gradle", "md",
];

/// Common English glue words — never useful as symbol seeds.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "this", "that", "from", "into", "when", "then", "else", "also",
    "just", "like", "over", "under", "about", "after", "before", "while", "there", "their",
    "would", "could", "should", "might", "must", "have", "been", "were", "does", "done", "same",
    "very", "much", "more", "most", "some", "any", "all", "but", "not", "you", "your", "are",
    "was", "our", "its", "using", "use", "make", "makes", "made", "keep", "keeps", "take",
    "takes", "give", "gives", "need", "needs", "want", "wants", "help", "works", "working",
    "please", "where", "which", "what", "them", "they", "will", "shall", "than", "then",
    "because", "since", "here", "how", "why", "who", "one", "two", "get", "set", "put", "add",
    "remove", "check", "make", "sure", "based", "only", "ever", "each", "every", "other",
];

const MAX_IDENTIFIERS: usize = 10;
const MAX_FILE_PATHS: usize = 5;

fn is_stopword(s: &str) -> bool {
    STOPWORDS.contains(&s)
}

/// Does this token look like a CamelCase identifier? (`UserServiceImpl`,
/// `findById`, `XMLParser`, `HTTPServer`.)
fn is_camel_case(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() < 3 {
        return false;
    }
    let has_lower = chars.iter().any(|c| c.is_lowercase());
    let has_upper = chars.iter().any(|c| c.is_uppercase());
    if !has_lower || !has_upper {
        return false;
    }
    // At least one lowercase→uppercase transition, or an interior uppercase
    // (XMLParser) — i.e. the caps aren't just a leading letter.
    let mut prev_lower = false;
    let mut interior_upper = false;
    for (i, c) in chars.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            interior_upper = true;
        }
        if prev_lower && c.is_uppercase() {
            return true;
        }
        prev_lower = c.is_lowercase();
    }
    interior_upper
}

/// Split a CamelCase token into subtokens (`UserServiceImpl` →
/// `["User", "Service", "Impl"]`) for fallback queries.
pub fn camel_subtokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    for c in s.chars() {
        if let Some(p) = prev {
            let boundary = (c.is_uppercase() && p.is_lowercase())
                || (c.is_uppercase() && p.is_uppercase() && cur.len() > 1);
            if boundary {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
        prev = Some(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    // Keep the longest runs — single letters and tiny fragments are noise.
    out.into_iter().filter(|t| t.len() >= 4).collect()
}

fn is_identifier_like(s: &str) -> bool {
    if s.len() < 3 || s.len() > 120 {
        return false;
    }
    if !s.chars().next().map_or(false, |c| c.is_alphabetic() || c == '_') {
        return false;
    }
    s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

fn strip_punct(token: &str) -> &str {
    token.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '\\' && c != '.' && c != '_' && c != ':' && c != '-')
}

/// Extract discovery hints from a request text.
pub fn extract_hints(text: &str) -> DiscoveryHints {
    let mut identifiers: Vec<String> = Vec::new();
    let mut file_paths: Vec<String> = Vec::new();

    let push_ident = |v: &mut Vec<String>, s: String| {
        if !s.is_empty() && !v.iter().any(|x| x == &s) && v.len() < MAX_IDENTIFIERS {
            v.push(s);
        }
    };

    // 1. Backtick-quoted spans — the user explicitly marked these.
    let mut rest = text.to_string();
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        match after.find('`') {
            Some(end) => {
                let span = &after[..end];
                let span = span.trim();
                if !span.is_empty() {
                    if looks_like_path(span) {
                        if file_paths.len() < MAX_FILE_PATHS && !file_paths.iter().any(|p| p == span) {
                            file_paths.push(span.to_string());
                        }
                    } else {
                        push_ident(&mut identifiers, span.split_whitespace().next().unwrap_or("").to_string());
                    }
                }
                rest = after[end + 1..].to_string();
            }
            None => break,
        }
    }

    // 2. Tokenize the remainder on whitespace + common punctuation.
    // NOTE: ':' is NOT a split char — `Vec::with_capacity` stays one token.
    let tokens: Vec<String> = rest
        .split(|c: char| c.is_whitespace() || "(),;<>\"'[]{}|".contains(c))
        .filter_map(|t| {
            let t = strip_punct(t);
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        })
        .collect();

    // First pass: paths, dotted references, and CamelCase/snake identifiers.
    for token in &tokens {
        if looks_like_path(token) {
            if file_paths.len() < MAX_FILE_PATHS && !file_paths.iter().any(|p| p == token) {
                file_paths.push(token.clone());
            }
            continue;
        }
        if token.contains('.') || token.contains("::") {
            // Dotted reference — the full token FTS-matches the whole FQCN
            // (unicode61 tokenizes on dots), then non-trivial segments. The
            // LAST segment (the type/member name) always qualifies; earlier
            // segments only when long enough to be signal, not TLD noise
            // (`com`, `org`, `www`).
            if token
                .chars()
                .all(|c| c.is_alphanumeric() || c == '.' || c == ':' || c == '_')
            {
                push_ident(&mut identifiers, token.clone());
            }
            let segs: Vec<&str> = token.split(['.', ':']).filter(|s| !s.is_empty()).collect();
            for (i, seg) in segs.iter().enumerate() {
                let is_last = i == segs.len() - 1;
                if is_identifier_like(seg) && (is_last || seg.len() >= 4) {
                    push_ident(&mut identifiers, seg.to_string());
                }
            }
            continue;
        }
        if !is_identifier_like(token) {
            continue;
        }
        if token.contains('_') {
            push_ident(&mut identifiers, token.clone());
            // snake_case compound: `user_service` → `UserService`? Skip
            // reassembly (FTS tokenizes on `_` anyway; the raw token matches).
            continue;
        }
        if is_camel_case(token) {
            push_ident(&mut identifiers, token.clone());
        }
    }

    // Second pass: long non-stopword lowercase words (weak seeds, appended
    // last so strong candidates keep priority).
    for token in &tokens {
        if identifiers.len() >= MAX_IDENTIFIERS {
            break;
        }
        if token.contains('.') || token.contains("::") || token.contains('/') || looks_like_path(token) {
            continue;
        }
        if !is_identifier_like(token) || token.contains('_') || is_camel_case(token) {
            continue;
        }
        let lower = token.to_lowercase();
        if lower != *token {
            continue; // mixed case handled above
        }
        if token.len() >= 5 && !is_stopword(&lower) {
            push_ident(&mut identifiers, token.clone());
        }
    }

    DiscoveryHints {
        identifiers,
        file_paths,
    }
}

/// Does a token look like a filesystem path mention? Either contains a
/// (back)slash, or ends with a known source-ish extension.
fn looks_like_path(token: &str) -> bool {
    if token.len() < 3 || token.len() > 260 {
        return false;
    }
    if token.contains('/') || token.contains('\\') {
        return token.chars().any(|c| c.is_alphabetic());
    }
    let lower = token.to_lowercase();
    FILE_EXTS.iter().any(|e| lower.ends_with(&format!(".{e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backtick_span_is_top_identifier() {
        let h = extract_hints("please fix `UserServiceImpl` for me");
        assert_eq!(h.identifiers.first().map(String::as_str), Some("UserServiceImpl"));
    }

    #[test]
    fn camel_case_ranked_before_lowercase() {
        let h = extract_hints("refactor the UserServiceImpl and cleanup");
        assert_eq!(h.identifiers.first().map(String::as_str), Some("UserServiceImpl"));
        // "refactor"/"cleanup" are weak seeds and come after.
        assert!(h.identifiers.iter().position(|i| i == "refactor").unwrap() > 0);
    }

    #[test]
    fn dotted_reference_and_segments() {
        let h = extract_hints("check com.acme.user.UserServiceImpl please");
        assert!(h.identifiers.contains(&"com.acme.user.UserServiceImpl".to_string()));
        assert!(h.identifiers.contains(&"UserServiceImpl".to_string()));
        // stopword-ish short segments dropped
        assert!(!h.identifiers.iter().any(|i| i == "com"));
    }

    #[test]
    fn double_colon_reference() {
        let h = extract_hints("what does Vec::with_capacity do");
        assert!(h.identifiers.contains(&"Vec::with_capacity".to_string()));
        assert!(h.identifiers.contains(&"with_capacity".to_string()));
    }

    #[test]
    fn file_paths_extracted() {
        let h = extract_hints("edit src/main/java/App.java and see README.md");
        assert!(h.file_paths.contains(&"src/main/java/App.java".to_string()));
        assert!(h.file_paths.contains(&"README.md".to_string()));
        // paths are not identifiers
        assert!(!h.identifiers.iter().any(|i| i.contains("App.java")));
    }

    #[test]
    fn backtick_path_treated_as_path() {
        let h = extract_hints("open `src/lib/foo.py`");
        assert!(h.file_paths.contains(&"src/lib/foo.py".to_string()));
    }

    #[test]
    fn stopwords_excluded_from_weak_seeds() {
        let h = extract_hints("the service should make about twenty changes");
        assert!(!h.identifiers.iter().any(|i| i == "about"));
        assert!(!h.identifiers.iter().any(|i| i == "should"));
    }

    #[test]
    fn identifiers_capped() {
        let text = (0..30).map(|i| format!("Class{i}Name")).collect::<Vec<_>>().join(" ");
        let h = extract_hints(&text);
        assert!(h.identifiers.len() <= 10);
    }

    #[test]
    fn empty_text() {
        let h = extract_hints("");
        assert!(h.identifiers.is_empty());
        assert!(h.file_paths.is_empty());
    }

    #[test]
    fn camel_subtokens_split() {
        let t = camel_subtokens("UserServiceImpl");
        assert_eq!(t, vec!["User".to_string(), "Service".to_string(), "Impl".to_string()]);
        // Impl filtered when too short? len 4 kept; single letters dropped.
        assert!(!camel_subtokens("AB").iter().any(|s| s.len() < 4));
    }

    #[test]
    fn snake_case_identifier() {
        let h = extract_hints("run parse_user_config again");
        assert!(h.identifiers.contains(&"parse_user_config".to_string()));
    }
}
