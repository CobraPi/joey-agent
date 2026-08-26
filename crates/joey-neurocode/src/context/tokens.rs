//! Conservative token estimation for context-budget arithmetic.
//!
//! The assembler previously used `len()/4` (chars per token), which
//! under-counts code/symbol-heavy text (identifiers tokenize worse than
//! prose). This module provides a slightly conservative estimator so budget
//! caps mean something: for symbol-dense context text the effective ratio
//! is closer to ~3 chars/token than 4.

/// Estimate the token count of a context string.
///
/// Blend of a length ratio (conservative 3.5 chars/token) and a
/// whitespace-token floor (`a b c` is at least 3 tokens). Takes the max —
/// never under-report below the naive word count.
pub fn estimate_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let by_len = (text.len() as f64 / 3.5).ceil() as usize;
    let words = text.split_whitespace().count();
    by_len.max(words).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn at_least_word_count() {
        let text = "alpha beta gamma delta";
        // 22 chars / 3.5 ≈ 7; words = 4 → 7. But never below words.
        assert!(estimate_tokens(text) >= 4);
    }

    #[test]
    fn symbol_heavy_text_not_undercounted() {
        // A long identifier string with no spaces: length-based dominates.
        let text = "com.enterprise.auth.service.UserServiceImpl";
        assert!(estimate_tokens(text) >= 12);
    }

    #[test]
    fn monotonic_growth() {
        let a = estimate_tokens(&"x".repeat(100));
        let b = estimate_tokens(&"x".repeat(200));
        assert!(b > a);
    }
}
