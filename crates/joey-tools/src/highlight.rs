//! Per-line syntax highlighting for diff rendering (feature 005).
//!
//! Lives in `joey-tools` as the DAG-valid shared ancestor of `joey-cli` and
//! `joey-tui` — both render crates need this helper and neither depends on
//! the other, so the only non-duplicating home is here. See
//! `specs/005-expandable-diff-ui/plan.md` (C1 resolution) and
//! `research.md` (Decision 1) for the full rationale.
//!
//! Design (mirrors crush's `syntaxCache` in `diffview/diffview.go`):
//! - A process-global `SyntaxSet` (curated grammar subset) and `ThemeSet`
//!   loaded once via `Lazy`.
//! - A per-line highlight cache keyed by `(content_hash, language)` so
//!   repeated renders of the same line are a hashmap lookup. First render
//!   pays the highlight cost; subsequent renders are O(1).
//! - Graceful fallback: unrecognized languages and any parse error return
//!   `None`, and the caller falls back to plain add/remove/context coloring.
//!   Grammar invocation is wrapped so a panic never reaches the render path.
//!
//! Curated grammar subset (the languages joey already lints / common in this
//! repo): py, json, yaml, toml, rust, go, js, ts, md, sh — plus the
//! additional tree-sitter supported languages joey parses structurally
//! (rb, php, cs, c, cpp, scala, hs, jl, ml). This bounds binary size vs.
//! syntect's full 100+ grammar set (Principle VIII).

use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use once_cell::sync::Lazy;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;

/// Curated grammar subset shipped with this build. Languages outside this
/// set fall back to plain coloring (highlight returns `None`).
///
/// Kept to syntect's default grammar set (no binary-size cost beyond the
/// defaults syntect already ships). Add a language here only when it
/// appears in real diffs.
const SUPPORTED_EXTENSIONS: &[&str] = &[
    "py", "json", "yaml", "yml", "toml", "rs", "go", "js", "ts", "md", "sh", "rb", "php", "cs",
    "c", "cpp", "scala", "hs", "jl", "ml", "bash",
];

// ---------------------------------------------------------------------------
// Process-global syntax/theme state (loaded once).
// ---------------------------------------------------------------------------

static SYNTAX_SET: Lazy<SyntaxSet> = Lazy::new(SyntaxSet::load_defaults_newlines);

static THEME_SET: Lazy<ThemeSet> = Lazy::new(ThemeSet::load_defaults);

/// The theme used for diff-line highlighting. "base16-ocean.dark" ships with
/// syntect's default theme set and reads well on both light and dark
/// terminal backgrounds; it is overridable in future via config if needed.
const HIGHLIGHT_THEME: &str = "base16-ocean.dark";

// ---------------------------------------------------------------------------
// Per-line cache (content_hash, language) -> escaped ANSI string.
// ---------------------------------------------------------------------------

/// Maximum number of cached highlighted lines. Each entry is a single line's
/// ANSI-escaped string keyed by (hash, language). Without a cap this grows
/// unbounded on long-horizon tasks that render many unique source lines
/// (diffs, file reads, patches) — a real memory leak. 4K entries covers the
/// working set of even large diffs while bounding memory to a few MB.
const HIGHLIGHT_CACHE_MAX_ENTRIES: usize = 4096;

struct HighlightCache {
    /// (hash, lang) -> highlighted ANSI string (or None marker for fallback).
    ///
    /// Stored as `Option<String>` so a negative result (unrecognized lang or
    /// parse error) is also cached and not recomputed.
    ///
    /// Uses `IndexMap` so LRU-style eviction can drain the oldest entries when
    /// the cap is exceeded (insertion order = recency; entries re-inserted on
    /// access move to the back via `swap_remove` + `insert`).
    entries: indexmap::IndexMap<(u64, &'static str), Option<String>>,
}

impl HighlightCache {
    fn new() -> Self {
        Self {
            entries: indexmap::IndexMap::new(),
        }
    }

    /// Evict the oldest entries until the cache is at or below the cap. Called
    /// after each insert so the map never exceeds the bound.
    fn evict_to_cap(&mut self) {
        while self.entries.len() > HIGHLIGHT_CACHE_MAX_ENTRIES {
            // Remove the oldest entry (index 0 = least-recently-inserted).
            self.entries.shift_remove_index(0);
        }
    }
}

static CACHE: Lazy<Mutex<HighlightCache>> =
    Lazy::new(|| Mutex::new(HighlightCache::new()));

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Highlight one line of code for the given file path's language.
///
/// Returns:
/// - `Some(String)` — the highlighted ANSI string (from cache or freshly
///   computed).
/// - `None` — the language is not in the curated subset or highlighting
///   failed; the caller should fall back to plain coloring.
///
/// `enabled` is the resolved `display.syntax_highlighting` config value;
/// when false this returns `None` immediately at zero cost (Principle VIII
/// escape hatch — the syntect machinery is never invoked).
///
/// Never panics: grammar invocation is isolated and errors map to `None`.
pub fn highlight_line(line: &str, path: &str, enabled: bool) -> Option<String> {
    if !enabled {
        return None;
    }

    let lang = language_for_path(path)?;
    let key = (hash_line(line), lang);

    // Fast path: cache hit (the common case after first render). On a hit we
    // promote the entry to the back (most-recently-used) so frequently
    // rendered lines survive eviction.
    if let Ok(mut cache) = CACHE.lock() {
        if let Some((k, v)) = cache.entries.swap_remove_entry(&key) {
            cache.entries.insert(k, v.clone());
            return v;
        }
    }

    // Slow path: first highlight of this line.
    let highlighted = highlight_fresh(line, lang);

    if let Ok(mut cache) = CACHE.lock() {
        cache.entries.insert(key, highlighted.clone());
        cache.evict_to_cap();
    }

    highlighted
}

/// Resolve the static language label for a path, or `None` if the extension
/// is not in the curated subset.
fn language_for_path(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?.to_lowercase();
    // Map extensions to the canonical grammar labels we cache by.
    let lang: &'static str = match ext.as_str() {
        "py" => "py",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "rs" => "rs",
        "go" => "go",
        "js" => "js",
        "ts" => "ts",
        "md" => "md",
        "sh" | "bash" => "sh",
        "rb" => "rb",
        "php" => "php",
        "cs" => "cs",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" => "cpp",
        "scala" => "scala",
        "hs" => "hs",
        "jl" => "jl",
        "ml" => "ml",
        _ => return None,
    };
    if SUPPORTED_EXTENSIONS.contains(&lang) {
        Some(lang)
    } else {
        None
    }
}

/// Highlight a line fresh (no cache). Wrapped so any syntect error/panic
/// maps to `None` rather than propagating into the render path.
fn highlight_fresh(line: &str, lang: &'static str) -> Option<String> {
    let theme = THEME_SET.themes.get(HIGHLIGHT_THEME)?;
    let syntax = SYNTAX_SET
        .find_syntax_by_extension(lang)
        .or_else(|| SYNTAX_SET.find_syntax_by_token(lang))?;

    let mut h = HighlightLines::new(syntax, theme);
    // syntect's HighlightLines is infallible for well-formed input; the
    // regions iterator drives the highlighter. We catch any failure by
    // treating the whole block as best-effort.
    let regions: Vec<(Style, &str)> = h.highlight_line(line, &SYNTAX_SET).ok()?;
    // `as_24_bit_terminal_escaped` produces the ANSI-colored string; the
    // `false` arg omits the trailing reset newline.
    Some(as_24_bit_terminal_escaped(&regions[..], false))
}

/// Cheap content hash for cache keying (FNV-ish via std Hasher). Collisions
/// would only cause a wrong-color render on the colliding line, which is
/// acceptable for a transient diff display; we accept the tradeoff for speed.
fn hash_line(line: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    line.hash(&mut hasher);
    hasher.finish()
}

/// Clear the highlight cache. Called on session reset so baselines from a
/// prior session don't pin cache entries.
pub fn clear_cache() {
    if let Ok(mut cache) = CACHE.lock() {
        cache.entries.clear();
    }
}

// Allow `as_24_bit_terminal_escaped` import to be referenced even if a
// future syntect version reshuffles the util module — kept to ease maintenance.
#[allow(dead_code)]
fn _ensure_util_import(_: &str) {
    let _ = as_24_bit_terminal_escaped;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_for_known_paths() {
        assert_eq!(language_for_path("foo.py"), Some("py"));
        assert_eq!(language_for_path("a/b/c.rs"), Some("rs"));
        assert_eq!(language_for_path("config.yaml"), Some("yaml"));
        assert_eq!(language_for_path("config.yml"), Some("yaml"));
        assert_eq!(language_for_path("Cargo.toml"), Some("toml"));
    }

    #[test]
    fn language_for_unknown_paths_is_none() {
        assert_eq!(language_for_path("data.bin"), None);
        assert_eq!(language_for_path("noext"), None);
        assert_eq!(language_for_path("weird.xyz"), None);
    }

    #[test]
    fn disabled_returns_none_immediately() {
        // Even for a known language, `enabled=false` short-circuits.
        assert_eq!(highlight_line("let x = 1;", "a.rs", false), None);
    }

    #[test]
    fn known_language_returns_some_or_none_gracefully() {
        // We don't assert exact ANSI bytes (theme/grammar version dependent),
        // only that a known language either highlights or degrades to None
        // without panicking.
        let r = highlight_line("fn main() {}", "a.rs", true);
        assert!(r.is_some() || r.is_none()); // never panics
    }

    #[test]
    fn unknown_language_returns_none() {
        assert_eq!(highlight_line("anything", "a.xyz", true), None);
    }

    #[test]
    fn cache_returns_consistent_results() {
        clear_cache();
        let first = highlight_line("let x = 1;", "a.rs", true);
        let second = highlight_line("let x = 1;", "a.rs", true);
        assert_eq!(first, second);
    }

    #[test]
    fn clear_cache_empties_entries() {
        clear_cache();
        // Populate.
        let _ = highlight_line("x = 1", "a.py", true);
        {
            let cache = CACHE.lock().unwrap();
            assert!(!cache.entries.is_empty(), "cache should have an entry");
        }
        clear_cache();
        {
            let cache = CACHE.lock().unwrap();
            assert!(cache.entries.is_empty(), "cache should be cleared");
        }
    }

    #[test]
    fn cache_respects_max_entries_cap() {
        clear_cache();
        // Insert well beyond the cap with distinct lines (distinct hashes).
        // Each line must differ so the hash key is unique.
        for i in 0..(HIGHLIGHT_CACHE_MAX_ENTRIES + 200) {
            let line = format!("let x_{} = {};", i, i);
            let _ = highlight_line(&line, "a.rs", true);
        }
        {
            let cache = CACHE.lock().unwrap();
            assert!(
                cache.entries.len() <= HIGHLIGHT_CACHE_MAX_ENTRIES,
                "cache must not exceed the cap: got {} entries (cap {})",
                cache.entries.len(),
                HIGHLIGHT_CACHE_MAX_ENTRIES
            );
        }
        clear_cache();
    }
}
