//! Fuzzy find-and-replace for the `patch` tool — a faithful port of
//! `tools/fuzzy_match.py`.
//!
//! The 9-strategy chain, tried in order:
//! 1. exact                 2. line_trimmed          3. whitespace_normalized
//! 4. indentation_flexible  5. escape_normalized     6. trimmed_boundary
//! 7. unicode_normalized    8. block_anchor          9. context_aware
//!
//! Every strategy returns ALL matches; more than one match without
//! `replace_all` is an error. Post-match guards (escape drift, conditional
//! `\t`/`\r` unescape, unicode preservation) and replacement re-indentation
//! are ported exactly. All offsets are byte offsets into the same strings, so
//! the arithmetic mirrors CPython's character-offset arithmetic.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::difflib::{ratio_chars, SequenceMatcher, Tag};

/// UNICODE_MAP from fuzzy_match.py — smart quotes, em/en dashes, ellipsis, nbsp.
const UNICODE_MAP: &[(char, &str)] = &[
    ('\u{201c}', "\""),
    ('\u{201d}', "\""),
    ('\u{2018}', "'"),
    ('\u{2019}', "'"),
    ('\u{2014}', "--"),
    ('\u{2013}', "-"),
    ('\u{2026}', "..."),
    ('\u{00a0}', " "),
];

fn unicode_repl(c: char) -> Option<&'static str> {
    UNICODE_MAP.iter().find(|(k, _)| *k == c).map(|(_, v)| *v)
}

/// Normalize Unicode characters to their standard ASCII equivalents.
pub fn unicode_normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match unicode_repl(c) {
            Some(r) => out.push_str(r),
            None => out.push(c),
        }
    }
    out
}

/// Result tuple of [`fuzzy_find_and_replace`] — mirrors the Python
/// `(new_content, match_count, strategy_name, error_message)` tuple.
#[derive(Debug)]
pub struct FuzzyOutcome {
    pub new_content: String,
    pub match_count: usize,
    pub strategy: Option<&'static str>,
    pub error: Option<String>,
}

type Matches = Vec<(usize, usize)>;

/// Find and replace text using a chain of increasingly fuzzy matching strategies.
pub fn fuzzy_find_and_replace(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> FuzzyOutcome {
    if old_string.is_empty() {
        return FuzzyOutcome {
            new_content: content.to_string(),
            match_count: 0,
            strategy: None,
            error: Some("old_string cannot be empty".to_string()),
        };
    }
    if old_string == new_string {
        return FuzzyOutcome {
            new_content: content.to_string(),
            match_count: 0,
            strategy: None,
            error: Some("old_string and new_string are identical".to_string()),
        };
    }

    #[allow(clippy::type_complexity)]
    let strategies: &[(&'static str, fn(&str, &str) -> Matches)] = &[
        ("exact", strategy_exact),
        ("line_trimmed", strategy_line_trimmed),
        ("whitespace_normalized", strategy_whitespace_normalized),
        ("indentation_flexible", strategy_indentation_flexible),
        ("escape_normalized", strategy_escape_normalized),
        ("trimmed_boundary", strategy_trimmed_boundary),
        ("unicode_normalized", strategy_unicode_normalized),
        ("block_anchor", strategy_block_anchor),
        ("context_aware", strategy_context_aware),
    ];

    for (strategy_name, strategy_fn) in strategies {
        let matches = strategy_fn(content, old_string);
        if matches.is_empty() {
            continue;
        }
        if matches.len() > 1 && !replace_all {
            return FuzzyOutcome {
                new_content: content.to_string(),
                match_count: 0,
                strategy: None,
                error: Some(format!(
                    "Found {} matches for old_string. Provide more context to make it unique, or use replace_all=True.",
                    matches.len()
                )),
            };
        }

        if *strategy_name != "exact" {
            if let Some(drift_err) = detect_escape_drift(content, &matches, old_string, new_string) {
                return FuzzyOutcome {
                    new_content: content.to_string(),
                    match_count: 0,
                    strategy: None,
                    error: Some(drift_err),
                };
            }
        }

        let mut effective_new = maybe_unescape_new_string(new_string, content, &matches);
        if *strategy_name == "unicode_normalized" {
            effective_new =
                preserve_unicode_in_replacement(content, &matches, old_string, &effective_new);
        }
        let old_for_reindent = if *strategy_name == "exact" { None } else { Some(old_string) };
        let new_content = apply_replacements(content, &matches, &effective_new, old_for_reindent);
        return FuzzyOutcome {
            new_content,
            match_count: matches.len(),
            strategy: Some(strategy_name),
            error: None,
        };
    }

    FuzzyOutcome {
        new_content: content.to_string(),
        match_count: 0,
        strategy: None,
        error: Some("Could not find a match for old_string in the file".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Post-match guards
// ---------------------------------------------------------------------------

fn matched_regions(content: &str, matches: &Matches) -> String {
    matches.iter().map(|&(s, e)| &content[s..e]).collect()
}

fn detect_escape_drift(
    content: &str,
    matches: &Matches,
    old_string: &str,
    new_string: &str,
) -> Option<String> {
    if !new_string.contains("\\'") && !new_string.contains("\\\"") {
        return None;
    }
    let regions = matched_regions(content, matches);
    // (suspect, python-repr-of-suspect, python-repr-of-plain-char)
    let cases: &[(&str, &str, &str)] = &[
        ("\\'", "\"\\\\'\"", "\"'\""),
        ("\\\"", "'\\\\\"'", "'\"'"),
    ];
    for (suspect, suspect_repr, plain_repr) in cases {
        if new_string.contains(suspect) && old_string.contains(suspect) && !regions.contains(suspect)
        {
            return Some(format!(
                "Escape-drift detected: old_string and new_string contain the literal sequence {} but the matched region of the file does not. This is almost always a tool-call serialization artifact where an apostrophe or quote got prefixed with a spurious backslash. Re-read the file with read_file and pass old_string/new_string without backslash-escaping {} characters.",
                suspect_repr, plain_repr
            ));
        }
    }
    None
}

fn maybe_unescape_new_string(new_string: &str, content: &str, matches: &Matches) -> String {
    if !new_string.contains("\\t") && !new_string.contains("\\r") {
        return new_string.to_string();
    }
    let regions = matched_regions(content, matches);
    let mut out = new_string.to_string();
    if out.contains("\\t") && regions.contains('\t') {
        out = out.replace("\\t", "\t");
    }
    if out.contains("\\r") && regions.contains('\r') {
        out = out.replace("\\r", "\r");
    }
    out
}

/// Byte-indexed port of `_build_orig_to_norm_map`: entry `i` (a byte index in
/// `original`) holds the byte position in the normalized string that byte maps
/// to. All bytes of a multi-byte char share the char's normalized start. The
/// returned vec has `len(original) + 1` entries (sentinel one past the end).
fn build_orig_to_norm_map(original: &str) -> Vec<usize> {
    let mut result = Vec::with_capacity(original.len() + 1);
    let mut norm_pos = 0usize;
    for c in original.chars() {
        for _ in 0..c.len_utf8() {
            result.push(norm_pos);
        }
        norm_pos += match unicode_repl(c) {
            Some(r) => r.len(),
            None => c.len_utf8(),
        };
    }
    result.push(norm_pos);
    result
}

fn map_positions_norm_to_orig(orig_to_norm: &[usize], norm_matches: &Matches) -> Matches {
    let mut norm_to_orig_start: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    for (orig_pos, &norm_pos) in orig_to_norm[..orig_to_norm.len() - 1].iter().enumerate() {
        norm_to_orig_start.entry(norm_pos).or_insert(orig_pos);
    }
    let orig_len = orig_to_norm.len() - 1;
    let mut results = Vec::new();
    for &(norm_start, norm_end) in norm_matches {
        let Some(&orig_start) = norm_to_orig_start.get(&norm_start) else {
            continue;
        };
        let mut orig_end = orig_start;
        while orig_end < orig_len && orig_to_norm[orig_end] < norm_end {
            orig_end += 1;
        }
        results.push((orig_start, orig_end));
    }
    results
}

fn preserve_unicode_in_replacement(
    content: &str,
    matches: &Matches,
    old_string: &str,
    new_string: &str,
) -> String {
    let file_region = matched_regions(content, matches);
    let norm_old = unicode_normalize(old_string);
    let norm_file = unicode_normalize(&file_region);
    if norm_old != norm_file {
        return new_string.to_string();
    }

    let file_orig_to_norm = build_orig_to_norm_map(&file_region);
    let mut file_norm_to_orig: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    for (orig_pos, &np) in file_orig_to_norm[..file_orig_to_norm.len() - 1].iter().enumerate() {
        file_norm_to_orig.entry(np).or_insert(orig_pos);
    }

    // Diff norm_old → new_string on chars (as CPython does), then convert
    // char opcode indices to byte offsets for slicing.
    let norm_old_chars: Vec<char> = norm_old.chars().collect();
    let new_chars: Vec<char> = new_string.chars().collect();
    let norm_old_char_to_byte: Vec<usize> = {
        let mut v: Vec<usize> = norm_old.char_indices().map(|(i, _)| i).collect();
        v.push(norm_old.len());
        v
    };
    let new_char_to_byte: Vec<usize> = {
        let mut v: Vec<usize> = new_string.char_indices().map(|(i, _)| i).collect();
        v.push(new_string.len());
        v
    };
    let sm = SequenceMatcher::new(&norm_old_chars, &new_chars);
    let mut result_parts: Vec<String> = Vec::new();
    for (tag, i1, i2, j1, j2) in sm.get_opcodes() {
        match tag {
            Tag::Equal => {
                let i1b = norm_old_char_to_byte[i1];
                let i2b = norm_old_char_to_byte[i2];
                let orig_start = file_norm_to_orig.get(&i1b).copied().unwrap_or(0);
                let mut orig_end = orig_start;
                while orig_end < file_region.len() && file_orig_to_norm[orig_end] < i2b {
                    orig_end += 1;
                }
                result_parts.push(file_region[orig_start..orig_end].to_string());
            }
            Tag::Replace | Tag::Insert => {
                result_parts
                    .push(new_string[new_char_to_byte[j1]..new_char_to_byte[j2]].to_string());
            }
            Tag::Delete => {}
        }
    }
    result_parts.concat()
}

// ---------------------------------------------------------------------------
// Replacement application + re-indentation
// ---------------------------------------------------------------------------

fn leading_whitespace(line: &str) -> &str {
    let end = line
        .char_indices()
        .find(|(_, c)| *c != ' ' && *c != '\t')
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    &line[..end]
}

fn first_meaningful_line(text: &str) -> Option<&str> {
    text.split('\n').find(|line| !line.trim().is_empty())
}

/// Port of `_reindent_replacement` — swap the pattern's base-indent prefix for
/// the file's base indent, preserving relative nesting. Only runs when the two
/// base indents differ.
fn reindent_replacement(file_region: &str, old_string: &str, new_string: &str) -> String {
    if new_string.is_empty() {
        return new_string.to_string();
    }
    let (Some(old_first), Some(file_first)) =
        (first_meaningful_line(old_string), first_meaningful_line(file_region))
    else {
        return new_string.to_string();
    };
    let old_indent = leading_whitespace(old_first);
    let file_indent = leading_whitespace(file_first);
    if old_indent == file_indent {
        return new_string.to_string();
    }
    let mut out_lines: Vec<String> = Vec::new();
    for line in new_string.split('\n') {
        if line.trim().is_empty() {
            out_lines.push(line.to_string());
            continue;
        }
        let line_indent = leading_whitespace(line);
        if line_indent.starts_with(old_indent) {
            let remainder = &line[old_indent.len()..];
            out_lines.push(format!("{}{}", file_indent, remainder));
        } else {
            out_lines.push(format!(
                "{}{}",
                file_indent,
                line.trim_start_matches([' ', '\t'])
            ));
        }
    }
    out_lines.join("\n")
}

fn apply_replacements(
    content: &str,
    matches: &Matches,
    new_string: &str,
    old_string: Option<&str>,
) -> String {
    let mut sorted_matches = matches.clone();
    sorted_matches.sort_by_key(|m| std::cmp::Reverse(m.0));
    let mut result = content.to_string();
    for (start, end) in sorted_matches {
        let adjusted = match old_string {
            Some(old) => {
                let file_region = &content[start..end];
                reindent_replacement(file_region, old, new_string)
            }
            None => new_string.to_string(),
        };
        result = format!("{}{}{}", &result[..start], adjusted, &result[end..]);
    }
    result
}

// ---------------------------------------------------------------------------
// Matching strategies
// ---------------------------------------------------------------------------

fn strategy_exact(content: &str, pattern: &str) -> Matches {
    let mut matches = Vec::new();
    if pattern.is_empty() {
        return matches;
    }
    let mut start = 0;
    while let Some(pos) = content[start..].find(pattern) {
        let abs = start + pos;
        matches.push((abs, abs + pattern.len()));
        // Advance past the whole match so self-overlapping patterns produce
        // non-overlapping spans matching str.replace() semantics.
        start = abs + pattern.len();
    }
    matches
}

fn strategy_line_trimmed(content: &str, pattern: &str) -> Matches {
    let pattern_lines: Vec<String> = pattern.split('\n').map(|l| l.trim().to_string()).collect();
    let pattern_normalized = pattern_lines.join("\n");
    let content_lines: Vec<&str> = content.split('\n').collect();
    let content_normalized_lines: Vec<String> =
        content_lines.iter().map(|l| l.trim().to_string()).collect();
    find_normalized_matches(content, &content_lines, &content_normalized_lines, &pattern_normalized)
}

// SAFETY: compile-time constant regex pattern; correctness verified at author time.
static WS_RUN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[ \t]+").unwrap());

fn strategy_whitespace_normalized(content: &str, pattern: &str) -> Matches {
    let normalize = |s: &str| WS_RUN_RE.replace_all(s, " ").into_owned();
    let pattern_normalized = normalize(pattern);
    let content_normalized = normalize(content);
    let matches_in_normalized = strategy_exact(&content_normalized, &pattern_normalized);
    if matches_in_normalized.is_empty() {
        return Vec::new();
    }
    map_normalized_positions(content, &content_normalized, &matches_in_normalized)
}

fn strategy_indentation_flexible(content: &str, pattern: &str) -> Matches {
    let content_lines: Vec<&str> = content.split('\n').collect();
    let content_stripped_lines: Vec<String> =
        content_lines.iter().map(|l| l.trim_start().to_string()).collect();
    let pattern_normalized = pattern
        .split('\n')
        .map(|l| l.trim_start())
        .collect::<Vec<_>>()
        .join("\n");
    find_normalized_matches(content, &content_lines, &content_stripped_lines, &pattern_normalized)
}

fn strategy_escape_normalized(content: &str, pattern: &str) -> Matches {
    let unescaped = pattern.replace("\\n", "\n").replace("\\t", "\t").replace("\\r", "\r");
    if unescaped == pattern {
        // No escapes to convert, skip this strategy.
        return Vec::new();
    }
    strategy_exact(content, &unescaped)
}

fn strategy_trimmed_boundary(content: &str, pattern: &str) -> Matches {
    let mut pattern_lines: Vec<String> = pattern.split('\n').map(str::to_string).collect();
    if pattern_lines.is_empty() {
        return Vec::new();
    }
    pattern_lines[0] = pattern_lines[0].trim().to_string();
    if pattern_lines.len() > 1 {
        let last = pattern_lines.len() - 1;
        pattern_lines[last] = pattern_lines[last].trim().to_string();
    }
    let modified_pattern = pattern_lines.join("\n");
    let content_lines: Vec<&str> = content.split('\n').collect();
    let pattern_line_count = pattern_lines.len();
    let mut matches = Vec::new();
    if content_lines.len() < pattern_line_count {
        return matches;
    }
    for i in 0..=(content_lines.len() - pattern_line_count) {
        let block_lines = &content_lines[i..i + pattern_line_count];
        let mut check_lines: Vec<String> = block_lines.iter().map(|s| s.to_string()).collect();
        check_lines[0] = check_lines[0].trim().to_string();
        if check_lines.len() > 1 {
            let last = check_lines.len() - 1;
            check_lines[last] = check_lines[last].trim().to_string();
        }
        if check_lines.join("\n") == modified_pattern {
            let (start_pos, end_pos) =
                calculate_line_positions(&content_lines, i, i + pattern_line_count, content.len());
            matches.push((start_pos, end_pos));
        }
    }
    matches
}

fn strategy_unicode_normalized(content: &str, pattern: &str) -> Matches {
    let norm_pattern = unicode_normalize(pattern);
    let norm_content = unicode_normalize(content);
    if norm_content == content && norm_pattern == pattern {
        return Vec::new();
    }
    let mut norm_matches = strategy_exact(&norm_content, &norm_pattern);
    if norm_matches.is_empty() {
        norm_matches = strategy_line_trimmed(&norm_content, &norm_pattern);
    }
    if norm_matches.is_empty() {
        return Vec::new();
    }
    let orig_to_norm = build_orig_to_norm_map(content);
    map_positions_norm_to_orig(&orig_to_norm, &norm_matches)
}

fn strategy_block_anchor(content: &str, pattern: &str) -> Matches {
    let norm_pattern = unicode_normalize(pattern);
    let norm_content = unicode_normalize(content);

    let pattern_lines: Vec<&str> = norm_pattern.split('\n').collect();
    if pattern_lines.len() < 2 {
        return Vec::new();
    }
    let first_line = pattern_lines[0].trim();
    let last_line = pattern_lines[pattern_lines.len() - 1].trim();

    let norm_content_lines: Vec<&str> = norm_content.split('\n').collect();
    let orig_content_lines: Vec<&str> = content.split('\n').collect();
    let pattern_line_count = pattern_lines.len();

    if norm_content_lines.len() < pattern_line_count {
        return Vec::new();
    }
    let mut potential_matches = Vec::new();
    for i in 0..=(norm_content_lines.len() - pattern_line_count) {
        if norm_content_lines[i].trim() == first_line
            && norm_content_lines[i + pattern_line_count - 1].trim() == last_line
        {
            potential_matches.push(i);
        }
    }

    // Thresholding logic: 0.50 for unique matches, 0.70 for multiple candidates.
    let candidate_count = potential_matches.len();
    let threshold = if candidate_count == 1 { 0.50 } else { 0.70 };

    let mut matches = Vec::new();
    for i in potential_matches {
        let similarity = if pattern_line_count <= 2 {
            1.0
        } else {
            let content_middle = norm_content_lines[i + 1..i + pattern_line_count - 1].join("\n");
            let pattern_middle = pattern_lines[1..pattern_line_count - 1].join("\n");
            ratio_chars(&content_middle, &pattern_middle)
        };
        if similarity >= threshold {
            let (start_pos, end_pos) = calculate_line_positions(
                &orig_content_lines,
                i,
                i + pattern_line_count,
                content.len(),
            );
            matches.push((start_pos, end_pos));
        }
    }
    matches
}

fn strategy_context_aware(content: &str, pattern: &str) -> Matches {
    let pattern_lines: Vec<&str> = pattern.split('\n').collect();
    let content_lines: Vec<&str> = content.split('\n').collect();
    if pattern_lines.is_empty() {
        return Vec::new();
    }
    let pattern_line_count = pattern_lines.len();
    if content_lines.len() < pattern_line_count {
        return Vec::new();
    }
    let mut matches = Vec::new();
    for i in 0..=(content_lines.len() - pattern_line_count) {
        let block_lines = &content_lines[i..i + pattern_line_count];
        let mut high_similarity_count = 0usize;
        for (p_line, c_line) in pattern_lines.iter().zip(block_lines.iter()) {
            let sim = ratio_chars(p_line.trim(), c_line.trim());
            if sim >= 0.80 {
                high_similarity_count += 1;
            }
        }
        // Need at least 50% of lines to have high similarity.
        if high_similarity_count as f64 >= pattern_line_count as f64 * 0.5 {
            let (start_pos, end_pos) =
                calculate_line_positions(&content_lines, i, i + pattern_line_count, content.len());
            matches.push((start_pos, end_pos));
        }
    }
    matches
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

fn calculate_line_positions(
    content_lines: &[&str],
    start_line: usize,
    end_line: usize,
    content_length: usize,
) -> (usize, usize) {
    let start_pos: usize = content_lines[..start_line].iter().map(|l| l.len() + 1).sum();
    let end_pos_raw: usize = content_lines[..end_line].iter().map(|l| l.len() + 1).sum();
    let end_pos = end_pos_raw.saturating_sub(1).min(content_length);
    (start_pos, end_pos)
}

fn find_normalized_matches(
    content: &str,
    content_lines: &[&str],
    content_normalized_lines: &[String],
    pattern_normalized: &str,
) -> Matches {
    let pattern_norm_lines: Vec<&str> = pattern_normalized.split('\n').collect();
    let num_pattern_lines = pattern_norm_lines.len();
    let mut matches = Vec::new();
    if content_normalized_lines.len() < num_pattern_lines {
        return matches;
    }
    for i in 0..=(content_normalized_lines.len() - num_pattern_lines) {
        let block = content_normalized_lines[i..i + num_pattern_lines].join("\n");
        if block == pattern_normalized {
            let (start_pos, end_pos) =
                calculate_line_positions(content_lines, i, i + num_pattern_lines, content.len());
            matches.push((start_pos, end_pos));
        }
    }
    matches
}

/// Port of `_map_normalized_positions` (byte-wise walk; only space/tab runs
/// differ between the two strings, so byte semantics match char semantics).
fn map_normalized_positions(
    original: &str,
    normalized: &str,
    normalized_matches: &Matches,
) -> Matches {
    if normalized_matches.is_empty() {
        return Vec::new();
    }
    let ob = original.as_bytes();
    let nb = normalized.as_bytes();
    let mut orig_to_norm: Vec<usize> = Vec::with_capacity(ob.len());
    let mut orig_idx = 0usize;
    let mut norm_idx = 0usize;
    while orig_idx < ob.len() && norm_idx < nb.len() {
        if ob[orig_idx] == nb[norm_idx] {
            orig_to_norm.push(norm_idx);
            orig_idx += 1;
            norm_idx += 1;
        } else if (ob[orig_idx] == b' ' || ob[orig_idx] == b'\t') && nb[norm_idx] == b' ' {
            orig_to_norm.push(norm_idx);
            orig_idx += 1;
            if orig_idx < ob.len() && ob[orig_idx] != b' ' && ob[orig_idx] != b'\t' {
                norm_idx += 1;
            }
        } else {
            orig_to_norm.push(norm_idx);
            orig_idx += 1;
        }
    }
    while orig_idx < ob.len() {
        orig_to_norm.push(nb.len());
        orig_idx += 1;
    }

    let mut norm_to_orig_start: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    let mut norm_to_orig_end: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    for (orig_pos, &norm_pos) in orig_to_norm.iter().enumerate() {
        norm_to_orig_start.entry(norm_pos).or_insert(orig_pos);
        norm_to_orig_end.insert(norm_pos, orig_pos);
    }

    let mut original_matches = Vec::new();
    for &(norm_start, norm_end) in normalized_matches {
        let orig_start = match norm_to_orig_start.get(&norm_start) {
            Some(&s) => s,
            None => match orig_to_norm.iter().enumerate().find(|(_, &n)| n >= norm_start) {
                Some((i, _)) => i,
                None => continue,
            },
        };
        let mut orig_end = if norm_end > 0 {
            match norm_to_orig_end.get(&(norm_end - 1)) {
                Some(&e) => e + 1,
                None => orig_start + (norm_end - norm_start),
            }
        } else {
            orig_start
        };
        // Expand to include trailing whitespace that was normalized, but only
        // when the normalized match itself ended with whitespace.
        if norm_end < nb.len() && norm_end > 0 && nb[norm_end - 1] == b' ' {
            while orig_end < ob.len() && (ob[orig_end] == b' ' || ob[orig_end] == b'\t') {
                orig_end += 1;
            }
        }
        original_matches.push((orig_start, orig_end.min(ob.len())));
    }
    original_matches
}

// ---------------------------------------------------------------------------
// "Did you mean?" feedback
// ---------------------------------------------------------------------------

/// Port of `find_closest_lines`.
pub fn find_closest_lines(
    old_string: &str,
    content: &str,
    context_lines: usize,
    max_results: usize,
) -> String {
    if old_string.is_empty() || content.is_empty() {
        return String::new();
    }
    let old_lines: Vec<&str> = old_string.lines().collect();
    let content_lines: Vec<&str> = content.lines().collect();
    if old_lines.is_empty() || content_lines.is_empty() {
        return String::new();
    }

    let mut anchor = old_lines[0].trim();
    if anchor.is_empty() {
        match old_lines.iter().map(|l| l.trim()).find(|l| !l.is_empty()) {
            Some(a) => anchor = a,
            None => return String::new(),
        }
    }

    let mut scored: Vec<(f64, usize)> = Vec::new();
    for (i, line) in content_lines.iter().enumerate() {
        let stripped = line.trim();
        if stripped.is_empty() {
            continue;
        }
        let ratio = ratio_chars(anchor, stripped);
        if ratio > 0.3 {
            scored.push((ratio, i));
        }
    }
    if scored.is_empty() {
        return String::new();
    }
    // Python: scored.sort(key=lambda x: -x[0]) — stable descending by ratio.
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let top = &scored[..scored.len().min(max_results)];

    let mut parts: Vec<String> = Vec::new();
    let mut seen_ranges: std::collections::HashSet<(usize, usize)> =
        std::collections::HashSet::new();
    for &(_, line_idx) in top {
        let start = line_idx.saturating_sub(context_lines);
        let end = (line_idx + old_lines.len() + context_lines).min(content_lines.len());
        if !seen_ranges.insert((start, end)) {
            continue;
        }
        let snippet = (0..end - start)
            .map(|j| format!("{:4}| {}", start + j + 1, content_lines[start + j]))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(snippet);
    }
    if parts.is_empty() {
        return String::new();
    }
    parts.join("\n---\n")
}

/// Port of `format_no_match_hint`.
pub fn format_no_match_hint(
    error: Option<&str>,
    match_count: usize,
    old_string: &str,
    content: &str,
) -> String {
    if match_count != 0 {
        return String::new();
    }
    match error {
        Some(e) if e.starts_with("Could not find") => {}
        _ => return String::new(),
    }
    let hint = find_closest_lines(old_string, content, 2, 3);
    if hint.is_empty() {
        return String::new();
    }
    format!("\n\nDid you mean one of these sections?\n{}", hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(content: &str, old: &str, new: &str, all: bool) -> FuzzyOutcome {
        fuzzy_find_and_replace(content, old, new, all)
    }

    #[test]
    fn empty_old_string_error() {
        let r = run("abc", "", "x", false);
        assert_eq!(r.error.as_deref(), Some("old_string cannot be empty"));
        assert_eq!(r.match_count, 0);
    }

    #[test]
    fn identical_strings_error() {
        let r = run("abc", "b", "b", false);
        assert_eq!(r.error.as_deref(), Some("old_string and new_string are identical"));
    }

    #[test]
    fn exact_hit_and_ambiguity() {
        let r = run("foo bar baz", "bar", "QUX", false);
        assert_eq!(r.new_content, "foo QUX baz");
        assert_eq!(r.strategy, Some("exact"));
        assert_eq!(r.match_count, 1);

        let amb = run("a b a b", "a", "c", false);
        assert_eq!(
            amb.error.as_deref(),
            Some("Found 2 matches for old_string. Provide more context to make it unique, or use replace_all=True.")
        );

        let all = run("a b a b", "a", "c", true);
        assert_eq!(all.new_content, "c b c b");
        assert_eq!(all.match_count, 2);
        assert_eq!(all.strategy, Some("exact"));
    }

    #[test]
    fn exact_nonoverlapping_replace_all() {
        let r = run("aaaa", "aa", "b", true);
        assert_eq!(r.new_content, "bb");
        assert_eq!(r.match_count, 2);
    }

    #[test]
    fn line_trimmed_hit_ambiguity_and_replace_all() {
        // Tab-indented pattern against space-indented content: exact cannot
        // match, line_trimmed catches it.
        let content = "  x = 1\ny\n  x = 1\n";
        let amb = run(content, "\tx = 1", "\tx = 2", false);
        assert_eq!(
            amb.error.as_deref(),
            Some("Found 2 matches for old_string. Provide more context to make it unique, or use replace_all=True.")
        );
        let all = run(content, "\tx = 1", "\tx = 2", true);
        assert_eq!(all.strategy, Some("line_trimmed"));
        assert_eq!(all.match_count, 2);
        assert_eq!(all.new_content, "  x = 2\ny\n  x = 2\n");

        let one = run("  keep\n  x = 1\n  keep2\n", "\tx = 1", "\tx = 2", false);
        assert_eq!(one.strategy, Some("line_trimmed"));
        assert_eq!(one.new_content, "  keep\n  x = 2\n  keep2\n");
    }

    #[test]
    fn whitespace_normalized_hit() {
        let content = "if  (a &&  b) {\n";
        let r = run(content, "if (a && b) {", "if (a || b) {", false);
        assert_eq!(r.strategy, Some("whitespace_normalized"));
        assert!(r.new_content.contains("||"));
    }

    #[test]
    fn escape_normalized_hit_requires_change() {
        let content = "line1\nline2\n";
        let r = run(content, "line1\\nline2", "lineA\nlineB", false);
        assert_eq!(r.strategy, Some("escape_normalized"));
        assert_eq!(r.new_content, "lineA\nlineB\n");
    }

    #[test]
    fn trimmed_boundary_hit() {
        // The strategy trims ONLY the first and last pattern lines. (In the
        // full chain, any block it matches is also matched by the earlier
        // line_trimmed strategy — same as upstream, where strategy 6 sits
        // after strategy 2 — so exercise the strategy function directly.)
        let content = "  start\n\tmiddle  stays\n  end\n";
        let m = strategy_trimmed_boundary(content, "start\n\tmiddle  stays\nend");
        assert_eq!(m.len(), 1);
        assert_eq!(&content[m[0].0..m[0].1], "  start\n\tmiddle  stays\n  end");
        // A middle-line mismatch (interior whitespace) must NOT match — the
        // middle is compared exactly.
        let none = strategy_trimmed_boundary(content, "start\n\tmiddle stays\nend");
        assert!(none.is_empty());
    }

    #[test]
    fn unicode_normalized_hit_and_preservation() {
        let content = "value \u{2014} with \u{201c}quotes\u{201d}\n";
        let r = run(content, "value -- with \"quotes\"", "value -- with \"QUOTES\"", false);
        assert_eq!(r.strategy, Some("unicode_normalized"));
        // Unchanged spans keep the file's unicode (em dash, opening quote).
        assert!(r.new_content.contains('\u{2014}'), "em dash preserved: {}", r.new_content);
        assert!(r.new_content.contains("QUOTES"));
    }

    #[test]
    fn block_anchor_thresholds() {
        // 4-line pattern, single candidate → 0.50 threshold passes.
        let content = "def f():\n    a = compute_thing(1)\n    b = 2\nreturn a\n";
        let pattern = "def f():\n    a = compute_thing(9)\n    b = 7\nreturn a";
        let r = run(content, pattern, "def f():\n    pass\nreturn 0", false);
        assert_eq!(r.strategy, Some("block_anchor"));

        // Dissimilar middle (single candidate, ratio < 0.5) → no block_anchor match.
        let content2 = "anchor_top\nzzzzqqqq\nwwwwrrrr\nanchor_bot\n";
        let pattern2 = "anchor_top\nalpha beta gamma\ndelta epsilon\nanchor_bot";
        let m2 = strategy_block_anchor(content2, pattern2);
        assert!(m2.is_empty(), "middle too dissimilar for 0.50: {:?}", m2);

        // Two candidates → threshold 0.70; only the similar middle passes.
        let content3 = "top\nmid one x\nbot\ntop\ncompletely different here\nbot\n";
        let pattern3 = "top\nmid one y\nbot";
        let m3 = strategy_block_anchor(content3, pattern3);
        assert_eq!(m3.len(), 1);

        // ≤2-line patterns: similarity fixed at 1.0 (anchor match suffices).
        let content4 = "first\nlast\n";
        let m4 = strategy_block_anchor(content4, "  first\n  last");
        assert_eq!(m4.len(), 1);
    }

    #[test]
    fn context_aware_fifty_percent_rule() {
        let content = "aaa bbb ccc\nxxx yyy zzz\n";
        // 2 lines, one ≥0.8 similar → 50% of lines → match.
        let r = strategy_context_aware(content, "aaa bbb ccX\nqqqqqqq");
        assert_eq!(r.len(), 1);
        // 0 of 2 similar → no match.
        let none = strategy_context_aware(content, "qqqqqq\nrrrrrr");
        assert!(none.is_empty());
    }

    #[test]
    fn reindent_nested_replacement() {
        // File uses 4-space base indent; model sent 2-space with nesting.
        let content = "def f():\n    if x:\n        do()\n";
        let old = "  if x:\n    do()";
        let new = "  if y:\n    do()\n    more()";
        let r = run(content, old, new, false);
        assert!(r.error.is_none(), "{:?}", r.error);
        assert_eq!(r.new_content, "def f():\n    if y:\n      do()\n      more()\n");
    }

    #[test]
    fn escape_drift_detected() {
        let content = "it's here\nanchor\n";
        let old = "it\\'s here\nanchor";
        let new = "it\\'s changed\nanchor";
        let r = run(content, old, new, false);
        let err = r.error.expect("drift error");
        assert!(err.starts_with("Escape-drift detected:"), "{}", err);
        assert!(err.contains("\"\\\\'\""));
    }

    #[test]
    fn conditional_tab_unescape() {
        // File region contains a real tab → \t in new_string is unescaped.
        let content = "\tindented line\n";
        let r = run(content, "    indented line", "\\tchanged line", false);
        assert!(r.error.is_none());
        assert_eq!(r.new_content, "\tchanged line\n");
        // File region has NO real tab → \t stays literal.
        let content2 = "plain line\n";
        let r2 = run(content2, "  plain line", "\\tchanged", false);
        assert!(r2.error.is_none());
        assert!(r2.new_content.contains("\\tchanged"));
    }

    #[test]
    fn no_match_error_and_hint() {
        let content = "alpha beta gamma\ndelta\n";
        let r = run(content, "totally missing text", "x", false);
        assert_eq!(
            r.error.as_deref(),
            Some("Could not find a match for old_string in the file")
        );
        let hint = format_no_match_hint(r.error.as_deref(), 0, "alpha beta gamm", content);
        assert!(hint.starts_with("\n\nDid you mean one of these sections?\n"));
        assert!(hint.contains("   1| alpha beta gamma"));
        // Hint is gated off for other error classes.
        assert_eq!(format_no_match_hint(Some("Found 2 matches"), 0, "a", content), "");
    }

    #[test]
    fn strategy_order_is_upstream_order() {
        // A pattern that would match under both line_trimmed and block_anchor
        // must report line_trimmed (earlier in the chain).
        let content = "  a1\n  b2\n  c3\n";
        let r = run(content, "a1\nb2\nc3", "x\ny\nz", false);
        assert_eq!(r.strategy, Some("line_trimmed"));
    }
}

#[cfg(test)]
mod audit_probe {
    use super::*;

    fn run_all(content: &str, old: &str, new: &str) {
        let out = fuzzy_find_and_replace(content, old, new, false);
        let _ = out.new_content;
        let out_all = fuzzy_find_and_replace(content, old, new, true);
        let _ = out_all.new_content;
        let _ = strategy_whitespace_normalized(content, old);
        let _ = strategy_line_trimmed(content, old);
        let _ = strategy_indentation_flexible(content, old);
        let _ = strategy_trimmed_boundary(content, old);
        let _ = strategy_unicode_normalized(content, old);
        let _ = strategy_block_anchor(content, old);
        let _ = strategy_context_aware(content, old);
    }

    #[test]
    fn probe_non_ascii_all_strategies() {
        let cases: Vec<(&str, &str, &str)> = vec![
            ("héllo wörld ünïcode ✓ text\nsécond liné\n", "héllo  wörld", "néw"),
            ("日本語のテキストです\n二行目\n", "日本語の", "中国語の"),
            ("a\u{00a0}b — dash — x\n", "a b - dash", "y"),
            ("\u{201c}quoted\u{201d} …ellipsis…\n", "\"quoted\" ...", "X"),
            ("emoji 🎉 party 🎊 time\n", "emoji  party", "Z"),
            ("  x = \"日本\"\n  y = 1\n", "x = \"\u{201c}日本\u{201d}\"", "w = 2"),
            ("tab\there ünïcode ✓\n", "tab  here", "Q"),
            ("mixed\r\nline\r\nendings\r\n", "mixed\nline", "blended"),
        ];
        for (content, old, new) in cases {
            run_all(content, old, new);
        }
    }

    #[test]
    fn probe_fuzz_300_random_unicode() {
        let cases: Vec<(&str, &str, &str)> = vec![
    ("\ta b \t—日aaa— …\n é\ta 日 …  b本 ✓","—本日ü…a a","\n “éb "),    (" é ü\na—\t\n✓…b”本bé本\n✓\n…b“— 🎉… b é……✓本\té”","\t\t “本ü—","é本"),    ("🎉…  “üb✓b日","🎉✓"," 本 ”ü\t“ü"),    ("日—ü”éü✓b—\n  🎉\n\nbé\n 🎉éab \nüb”“🎉","“本✓”","\n 本本”“"),    ("本\t“本ü✓“b—…日 é“✓a  “✓” é“ é✓——üé… a…\ta","  \n🎉—✓","🎉✓ü本 "),    ("ü🎉🎉🎉…本 \t日本—\na“bbü\n🎉—” ✓本 日— a🎉日é  \n","\n日","“🎉\n"),    ("…b— —”b 🎉日ü—“ ba… ✓”🎉\n 日a——”é aüü\n\n本","…","—✓“”—日"),    ("a🎉本\tbü—é🎉✓—本é…éa”日ü","本é—”   …","\t“\n\t  "),    ("\n本日 🎉“✓","“ü …🎉…","… "),    ("üé🎉é“","ü本aé🎉","bb"),    ("”🎉🎉✓”é““a✓✓🎉日\nü……","🎉本”“本 🎉","✓éé”"),    ("b日—ü”…a“   ","“ “日","🎉… ✓a"),    ("é\n\n“”é\n✓\t 🎉\n本” 本🎉é …日…日 本\n本\n“üü\nü","”a—ü日a","—"),    ("日a”” ”🎉日 ✓ aa✓\t…日“”üb”“\t”éé —","日 “","\n"),    ("a—b本ab” a✓é日 b✓日……","本é”","a✓🎉üé"),    ("✓ a","é”🎉✓\t ”","b”…é“\t"),    ("🎉ü\t“ 🎉\n 🎉 本  …a—bü\t…éb本“✓b🎉🎉ü 日\t✓a  ","✓","”a“… ü"),    ("—🎉—“—\t","\n","…—\n"),    ("b本éb🎉  ","—a”🎉üé日","é” \n“ “"),    ("本本\n—\n本✓日✓é\n…b本”—b—ab","✓ü “","🎉“”本…"),    ("b🎉本ü🎉\t“——”✓“”✓本é…—üéb“…\n日“✓—”本日🎉…🎉","é本","“…本"),    ("… é… bb✓ü“\n” ü\n✓—✓","” b"," “\té"),    ("b\t\t\n\n\t —\ta ü\n🎉“本”✓—…日 🎉bü✓“— ✓本éé🎉ab🎉🎉—","\t” ✓”\t🎉\n","本 b“a”a"),    ("  —— bbaa日\t“本日本","  ✓🎉🎉"," b…”🎉üü"),    ("本a”a…a本”ab””é","…日","日"),    ("本aü““aü","\t🎉\t"," \t"),    ("bü““ü🎉🎉bb””…日……—🎉✓ \t本 —…🎉\t\né\t","”✓…本本","b "),    ("…\tb日bü \t““—🎉\t本ü\n——本 b","b”aü\t✓é本","é✓   ü🎉"),    ("\t…\t  “✓✓ ”日b✓\n 本 🎉日日…\t“\t üb“✓日","éüü\n日\ta","本ü\nb✓"),    ("🎉…日é\n” \n🎉é\n …🎉—é日ü\t本— ✓ü“","—“","“日ü—本é\n"),    ("\n”本…本”日““  🎉✓bb\ta  本日本é\t \t🎉a—✓\n……\t”…\t ","…\n—","ü"),    ("日a\n——éa日b\nü ✓🎉“— 本本—“\n b日日b\n\t”…—🎉","b”✓🎉é —"," ✓”…日\tü“"),    ("本本\t✓”—\t本…—✓\t…\t✓— éüb“—🎉b—本…日…","b","\t🎉\tü本—”ü"),    ("✓本—","…🎉日é”a本","✓✓🎉本—“é✓"),    ("é\t\na 🎉🎉a\t✓ 🎉 ”日b\n”“”本🎉\t”éa✓“a  本é 本éb“","bb🎉🎉b✓—","\t“\n”"),    ("” 本b……éb“ …\n ","日“日”🎉","ü"),    ("ü🎉✓——日é本b","\t本\n\tab","日\naa” "),    ("—\n \n…本—— 本\t…本a日……本b✓本日","“本✓béb \t","b“—本 本"),    ("“b —\té”🎉\n  \t…—","\nüa✓本日é\n"," a "),    ("日é本”” \n—🎉本\n—”b✓ a\n”ü✓“\n\t…日 \t","\t本\n\t\n本“","ü“b b "),    ("“”本”本✓éé✓✓ü日🎉\t\t本…🎉“✓”b🎉✓本\nébbb","本"," é本——\t"),    ("b…🎉✓”üa","\n”本","ü"),    ("🎉”日”… b","\t\t","本本… ✓ "),    ("\n—a— ba…✓ab\t —üé🎉","bb ","日\t"),    ("✓é✓aa… \nü  a …”🎉üé","🎉 \n✓"," ”✓✓“ "),    ("✓✓”ü日”ü本🎉—","\n🎉本ü","✓  a"),    ("日”a🎉—","”\t✓","—b"),    ("✓🎉\nü日—…🎉—“","b“\t\t","\na…é本…本✓"),    ("ü","—\t““b日","aaü“日”\t "),    ("\t✓bü","”é\n\t—日🎉\n","本\n"),    ("“a日\t日a é…✓✓日本🎉a","”本","✓ 本 é…\n"),    ("✓🎉🎉✓🎉\nü✓ü…ba “—“ 🎉é…日“üa\n—… ”\n“ éü","éb🎉b…”✓"," 日 "),    ("—日“\n🎉\t ü✓—","bb\t","é—"),    ("—é🎉 \n🎉\n✓b…🎉—a本”","本","\n \na\t"),    ("\t本日”本\tüa\na”ü✓bü”\n  \nb““””日”…\n\nü本✓\tabb\n","本 本✓本üa","日 "),    ("…“bb"," é🎉\t“é","🎉üa\t"),    ("ü🎉baé\n  “b日——本\t…✓ ”日”é✓— ”日","”—","✓” “✓b"),    ("é本本日 \t…日b…a…b\t…“日bé\n \t","本é“—本","…“\n”… a…"),    ("b🎉\n\té✓”…🎉日\t”“本🎉…b日— b🎉éb\n\né日b“… …a\t","\n","é\n  —”"),    ("\t”ü\tüb✓”é✓—","b“本","ü—“—"),    ("本 é日🎉…a—日","—ü本 é”ü\n","\t🎉é— ”aé"),    ("b…”ü üb”\na…  \t “🎉…✓—","a b —\t “","”🎉—“”…“"),    ("… \t\tb✓ü🎉é ✓","é—日  ","✓\n"),    ("日“✓ ✓🎉日 “b日b\t\n\n\t…“🎉\t本本—本🎉🎉本…“✓”\n”✓ “\n ü","üü🎉aa— ","✓—… 日"),    ("é日 \n\na“","”本 本日","aü"),    ("\t\n””ü——”…b”aé—","ba🎉\té","b"),    ("本🎉 … 本\né\n”\t \n本 b  …\n🎉 a✓\n“—✓”"," 日 a","日日本"),    ("\nü ✓ü\t","🎉","b\n— 本é✓"),    ("— 🎉本— —a✓\tü…ü \n —本b✓ü—üb \n“…✓"," “ a","”\t本bé\t”"),    ("🎉”a ü”b”\n🎉本日✓”a“日 aé…","bü—…","✓日”b日\n“🎉"),    ("🎉🎉““é\n🎉 …” 本本 …日","🎉"," ” “a"),    ("\n—“ ","…”\t日b ","—“”"),    ("日\t🎉—ü““✓  aébü✓“ "," \t ü","”"),    ("b… ✓✓\tb\tb\n🎉ü 日\n” 🎉é\n\naü —aüé🎉b”日✓…——…—","”“","\n…“—\t \n"),    ("  ✓“\n✓\n日bé\n\n“é” éb 🎉✓本é  …aü ”ü ","é","✓ "),    ("bb“””✓“é“✓““ “a本本”é…✓“🎉日日\n本üé\t日é“ab\n  🎉 ","—”a🎉","—a… "),    ("本…é本 a“🎉—本\té—本✓  —”ü✓é—”🎉éa ","a","”本"),    ("ü","日é…","本本a"),    ("aüé🎉—\n aa🎉—ü\t✓\n ✓ é…\t”本a本 ✓本 a”a ü”","—✓\t\n","b…\nüé✓✓a"),    ("🎉büü\n—🎉—“\t🎉✓\t\t ✓aa🎉  \t…","b”","— é”a "),    (" \t—…✓\n\n","ü”本","“"),    ("ü日…—\n…é é\t\t🎉✓ébé日…é🎉✓“🎉🎉—✓“🎉","— …","—本"),    ("b”\tb …“✓… ”本","日a🎉 \na \t","\n\n\tb\n"),    ("üa“éé"," ü本b✓\t","本“b éb"),    ("🎉日🎉 b\n","本ébü","……\n ü"),    ("🎉🎉\t✓\t"," ü日\t 本🎉\n","bé“…ab"),    ("…\t—a…日—…éü 🎉aéé✓ ”"," “ 日🎉","—“b"),    ("bb“é”\t…“… ……日—","\n本aé🎉\tü","é"),    ("”✓—a\né……\taéé"," ü”éü\t日é","…本\té“ \n"),    ("\n\n✓✓ü日”✓b🎉","\t\t\t","—é…“"),    ("b” aab\n\t”","🎉b","a…"),    ("aa日\nbb—bé本—…—本\t… ——\t\n\n\n—é——","”\n”b","\n"),    ("éa\t \n\n日—a日a \n \n日本b本本a”日日é","✓✓”🎉a","\n “ b”"),    ("—”本本—ü\ta ”…… "," 🎉\né","—✓“ —\n"),    ("本✓","b \n本é🎉é"," b本日… "),    ("🎉“本…üa“✓ ✓日本…日  “✓a日\t \n本本🎉—ü béé日—✓ 本ü","é日日\n日✓"," a\té  \n"),    ("\t—🎉…a\t\t…日 é\t\n…\té本日”é🎉本本\n  日🎉🎉\t","a\t","…übb"),    ("\n\t本a\t日——\tü✓ 本ü ","\t ","”b a"),    ("\n……é ✓\n —a日b 本\t🎉”b\n“b”…”b—ü 🎉","\n”\t日 — ","🎉"),    ("é ✓","日“✓  “✓","… 本🎉"),    ("🎉日日éb\ta…✓ ” \tü“”\t ","“日","…aé é"),    ("…","…","本éa本\t…”"),    ("本b a…日✓é🎉日\t日日🎉日a”b本…\n 日 🎉aü✓本✓é—","b🎉✓b","\n🎉“a ü"),    ("ü本 日b🎉”✓✓ü\n🎉ü…aü\na 日—  ü …✓本 日ü","—a","”\t本 b"),    (" 🎉—\tb”üaü“日— ✓ 🎉bb日a… aü\taé”\n““","日\né","“日"),    ("日a","a…  a“”本","本b \t”"),    ("✓"," b日本✓","本🎉”日本"),    ("🎉…🎉日 abb本 a…本é✓"," ““”—"," 🎉🎉b "),    ("✓ \té”\t”🎉”日——本🎉\t✓✓—\t…日本日本é日\n  \tü 日bb本ü","\t…","”“é\né“”"),    ("\t \ta🎉本“日”","\n\t日”本🎉","éé日 \t"),    ("…é\n✓…é \té\n日\n“日”é\t 🎉…ü🎉 ✓✓✓ \nü✓🎉本…\n\n bb","ü ü”” \tb","本\n"),    ("”—\t… ","\t🎉—","\n日“— \n"),    (" ü…a✓a\t\n✓é “本✓“””✓—\t\t”ü✓”a日 “ ✓ü本é","b  ü   \t","日"),    ("—\n”b日ü本\né “…日 \n🎉—\n   🎉"," a”—…","“"),    (" ”ü“””","\t”日\n","\n  ✓\tb\n"),    ("✓“  \n—”ü\t本\t … —本b","“üü…"," \t"),    ("\n🎉b \tü  日”🎉🎉b üb🎉 … 日\na”日日a“本","…”🎉 ü","é…a……"),    ("…\naé éa…ü✓“\t\n…本\n\n本本ü—é—bé本…日🎉 \t“✓本本ü","🎉\t “”\t本","“"),    ("a—","é\t","🎉"),    ("a 日“—ü“aü日……—ü本 é—“✓"," a… bé本","ü🎉” "),    (" “…本é 🎉“…“\n日日  a…a🎉—”“…ü…🎉 本—本… —“\t","”日\tb日 🎉a","…“ü"),    (" “ \n“…b\n本 日”—…”\nb ba","éb本éé\né","🎉🎉é"),    ("é \n“üb\n“…”“a🎉“———本” —本\n","…bü日","“\n\t“…ü本"),    ("日本“\n bé🎉日“ 🎉é日🎉a✓本\taü\t🎉 ✓","…"," ” "),    (" a🎉—日…\t日“—…b\n“✓ \n\t🎉b…“—a\t本✓aa日本日b\tb日","aé","🎉é本 ✓\t🎉"),    (" 本ü日b““é\t“\n✓✓\n本b\n","\n—本\t “","…—“日"),    ("\t本b”——“b…”\t”“\t\n”本…“本a日 ”b日éaaü🎉本 本🎉✓“—","🎉","aa\t"),    ("—🎉🎉ü—本b","\n…","”"),    ("日…übé a —\n…本 \t","  …","… —"),    ("bé\t\t","é","\t—✓🎉 ✓ "),    ("✓\nü \n ……ü —✓本…\n日 ü…b🎉a 🎉本✓日 本日\t","“🎉🎉本 ✓本","“\t"),    ("”b…—“bé本b\t本üaé本 \tba\néa—\t“日a”","ü“ a✓üab","”ü🎉\t“ 日🎉"),    ("\t”本🎉","日”\t—ü”","““”b…“"),    ("”\n\t”日ü— ü —b\n日“aüba“本—  ”b\n…“\tab—b… b"," ”✓ 本","üaa… "),    ("✓ü🎉 日— ”ü日\n \t—a—“éé🎉\n é…本本—🎉 本a\tü\n\n🎉— ","日","a—🎉 "),    ("✓本—\nü—\n”—本日本—\t✓—é—本 …a本","é”ü“\n"," ” ”“"),    ("日🎉日b—\t\t“日 ","b✓ a本é","🎉"),    ("🎉“\n✓\t\n🎉日“ …\nb本é…… ","日","本  …”…”"),    ("—a","é日aü\t… ","é✓“ü "),    ("éü”\t ✓ü…éé✓","é”","—“\n✓"),    ("“\t \t✓日✓…— ✓“🎉 “\n ”本\tb\n🎉—b本üü🎉éé ü\n ”日本”","🎉 "," "),    ("—✓\n🎉—”  —a日","éü日é🎉 …é","”“a“\t "),    ("b”🎉本\n🎉本\nü","b\t🎉"," …"),    ("b日\tb“”\té本 b—” 🎉…\t—”\n\t✓aüb 🎉本✓日 …\n","日✓本日","本\t ü"),    (" é\nüb\t本本日 ”a…本”\t ”b本本“é”ü✓a…日\tbé✓ü本  “"," 🎉✓  ✓“","a\t…ü…"),    ("日日b—aé","本…a \t","\tü”本 🎉✓b"),    ("日—本本✓","b“日b🎉","—é\t🎉日"),    ("\n”aü…✓日—本a”\t本“日 b本\t \n… 日 b本ü✓a ““”b“","日🎉ü✓“”…","\t🎉"),    ("ü\ta”","“日—é🎉ü","日"),    ("éa\n日a","\n"," éa…é"),    (" ”b本🎉a 🎉\nb—🎉—🎉b \n","  ","本…”aéa é"),    ("✓b\t—”✓”“a\n”  ü ” 日\t本…b🎉 日 “b““—ü日 …ü é日","✓a ","”…\n”b"),    ("b”✓\t bé“ü\tba🎉üb ”—✓“— é —\n\té🎉 ü","“é","“…本\t✓“\n"),    ("…… b🎉—ü  ”\n“ 日🎉 ✓a🎉…—“ü\n —é日日…—“a…”本本","…日✓”","”"),    ("✓“✓b\t”“é✓“—日日— a\n…本ü“—ü— “🎉\n\n“…✓"," \t本ü🎉本…","ü“ ✓🎉—\ta"),    ("……🎉“ba✓\t\na本 ”🎉“é”本—","a…b\na","\t"),    ("üé本üb—“ébü","a“","é本\n\t"),    (" “日\na\n\té🎉🎉\t“—ü \n","aüb…b\t”","日🎉"),    ("bb日”\n“\t日\t“  …本ü““…”🎉” é","“\n—","—  \t“…"),    ("✓  \n","\t\t“","\n…本\tb\na "),    ("—ü ”  本✓\n✓🎉bb—”本本b","\n\n","a✓aé a\t"),    (" b”—本—日","本","é"),    ("ü 🎉b… üa“✓\n🎉——”🎉\t\t\n\t—ü日b ü““b—…“","a…\té …","—本ü日… a"),    ("b—\n✓\nü …b","  é日”“a“","a”aü"),    (" 日✓日b”“✓bé🎉“\tb本…\t日— “✓…“”🎉ü \t“本ü\t✓\n”  b—","“…”…🎉🎉","\t… \t“é"),    ("…b—a本","”” ","本b日b”🎉é\t"),    ("\n\n”a… 🎉ü本🎉\t””b\n✓aab🎉\n🎉—”—✓”","✓🎉本 ","\n““"),    ("“—ü—bb\tb—”","……b本本","日 \nb"),    ("   “✓ü“ü本日“\n—日“本本…\nü üü…\t“—…本a✓b✓”é”✓本b","a🎉日","é”\n\tb"),    (" b✓“ \n\tüüüb本 ””… ”ü””","…","”🎉b日"),    ("ba“","“ 🎉ü”","éüb\t日"),    ("🎉b  \n ” “\n本\né…b\t a”\t 日b🎉”“““✓ \t","—日日…“","…é✓…"),    ("🎉—ü本本本a  🎉✓”aü日“”🎉\n本","ü日  ","\t”🎉\t…b\tü"),    ("a…aüü“\t— 日ü—…”本—“”本—é本ü日a本✓日“”ü…üü”本“ü","b\t”——🎉é","\t✓a"),    ("” …b“é…\t🎉\nbbé\taaa\t✓“b"," —\nü","\n—ü"),    ("ü“✓","b🎉 b\t\n","éü✓🎉🎉 "),    (" 🎉ü\n✓","… \t","日…ü"),    ("🎉b…—…”本\tbé\t”本—…”é\n🎉 a“\n本—"," bb","\tb本\t…—日”"),    (" 日 “🎉✓ “本本✓✓b“🎉éa…日bb\n 本\n本—ü ü 本a…本—a✓ü","—é","\t✓—…—”✓本"),    ("🎉\t\nü🎉\n—b\t\n“”…”b”本日\té\tü✓”本日\n\t本日 ””","✓üü本…b"," 本—””✓b"),    ("…ü\n\n— —本日b…”é”✓\té …é—本 ”b✓ \t\t\n\n…本” b本日"," b","本\t日 \nüb"),    ("🎉✓… 🎉—本a\né”日日本… ü”🎉日本✓”\tbb✓\na —\t","ü日🎉”“","”\t”b"),    ("\n\t🎉—üüb✓✓”✓  日 ","—","a é\n\n—é\n"),    ("…”✓ü🎉日\n—\n本…”b  ü…é\n“\t 🎉\n“\t —ü","——🎉…“é","——"),    (" a日🎉✓“a \n… ““ba”aü\t本“é…\nü\t本\t","✓“üb— …","ü” a"),    ("\t\tü”日 ””","“✓","日”\t"),    ("a\t é“🎉ü✓ü— “b日ééb büb…\n a\n本本","🎉ü—本\t","日é—日✓"),    ("éü","a","“\t\t✓b…b—"),    (" …\n—\tüü —\n\n…a🎉\ta…本…”b🎉 “","—","é "),    ("  bü本🎉日”é日\n日b","本“”é““ \t","\ta"),    (" “本🎉 \t\t 本\n” 日“✓\na aa“本 \n \t日✓…… ”\nab✓b本","é”","é🎉  日…"),    (" 日日"," 日本”b\t","…"),    ("—”🎉“—”本\n✓… 日bü\n🎉…日\n—“\n✓—“bb\t\tb日日ü","b“🎉✓ a"," 日”üb éü"),    ("本"," \t本”é\n本","a"),    ("ü\t日””üa é\t…日日 ","✓✓a✓","a"),    ("aba本ü✓✓b”✓“","本","—"),    ("ü本…\n“ \n—本—ü本é““\t bé✓a🎉”b— é” \n 日a ","🎉“ ","🎉é\n\n"),    ("本🎉日日“🎉本✓“✓b🎉\nbbüüü…—日ü ”” \n日日🎉 ","日é“\t本—","…a\n🎉日“ "),    (" ”✓b””b\tbaüü\n\tb日✓”本b “a—…—\n”ü\n ","🎉\t”\n日","\t🎉"),    ("— \n”\t","\n…ü ","本✓— — 日"),    ("üé✓\nüba🎉✓ba本üéa本本 ”🎉ü”\t—","—","\n —éé"),    ("本本é\tbüü\n …\n","b\t\néüa"," 🎉本 “…a"),    ("— b“b…\t✓—ü ","✓","…b—"),    (" üü🎉🎉aaab \t…   ”\t","a“","bü““ 日✓"),    ("b\n…\t本本é“本 \n✓\t \n本🎉ü … ”日 \t\n“é”🎉","“","aü✓本ü "),    ("日 … é \t” "," —ü “","🎉ü本üü ✓"),    ("a\n\t\n ”…\n✓\t“a🎉本aa\na\n✓🎉✓b\t…\t“🎉🎉🎉 日日✓\n…","\naé🎉\n🎉 a","üü"),    ("éé"," —ü\n日本a🎉","\n✓”"),    (" “a éüé✓日日—”\n","✓ü✓","üabé本…"),    ("✓”日日…—\né… \t本b\n” \n日”","日","— b"),    ("é本é—üé✓\nü\n“ü…é\nb\t\n …✓…","é\t🎉\t","ü—\t…✓"),    ("a🎉 é🎉🎉🎉日\t🎉日🎉éb本 a✓éü🎉 ","b“日","✓🎉bb本…”—"),    ("ü”… a”—a…—aa   \tü\t\t ü…\t “✓日b é—…“","bü”本","\n\tbü “é\n"),    (" a……✓b\t ”本ü“ —本日 日…✓\n““ 🎉a—b🎉üa—a✓é本 ","本” ","本” b\n"),    ("éa🎉…”🎉 é","“a日””ü ü","日—"),    ("\té\n \t\n✓✓日b  …日\t\t”a 日”🎉—é ✓a\t—éb✓本"," …本本a✓","🎉"),    ("\t","”ü “”✓","🎉”—🎉\ta"),    ("b“\t 本ü é✓🎉é“é…“…\tb\n— 🎉—🎉🎉✓✓—日✓日","\tb—","…🎉"),    ("日日✓—é\n…🎉 🎉ü”aéüb“üb","日","—本é本üa\t\n"),    ("🎉—\n🎉\n\ta é…日aü a —\t 🎉 übéü —\n\n\t本\t \t✓ü","日b✓","”"),    ("—本é““a","🎉é  b","—本…\t✓\t—"),    ("\té 本…本—   é  日“本—本✓本✓日b本🎉\t🎉","日\t日本✓","本  🎉ü \t"),    ("ü本—— … ✓éa✓… 🎉日 bb 日 aa本🎉ü”…🎉\n…a—…","a\t“ü“✓","✓\t””””a🎉"),    ("本🎉✓🎉🎉日”b日\té“本—本\t","✓✓ ","é"),    ("é ✓日","éé✓🎉…  —"," éb—“a"),    ("a \t本ü✓日✓b日✓“本🎉é\n\né\t日 🎉本b\n✓✓b\t…éü—🎉éa日\t””","\n","“"),    ("\nü“b🎉…—é üaü…b“  a“ ü— 本 🎉ü","ü✓üüa\n"," 🎉✓🎉"),    ("ü 日…日 🎉本…","“\t本 ","🎉 ✓"),    ("日 🎉\t\té—bü\n✓”” \t” \n üaü\tü “日a本”\t\n","“✓日  …—","é ”✓日\t"),    ("é✓é","✓”","b🎉üa…"),    ("\n…\n本\n —\n✓✓  \n\t“\t—\t”…é“ ü”\n日\t\t ✓日a","\nüa“\nb ü"," \t日é本✓——"),    ("\t日日✓a…“本🎉…\n","\n\néü🎉b"," 本 "),    ("🎉 ✓日é\n …—ü ü\né\t","…b","日 a🎉—"),    ("日\t…ü—","🎉 —üb","a“"),    (" b  ü日本é本—\n “ü—日✓…—…","本 é…日日","“"),    ("🎉üa✓ba本\né✓b—日 🎉 ","”ü…\n日“b","\t日…b\n—\t"),    ("ü✓ééaa ","\n✓\t日a","é…\t✓b\n…"),    (" 🎉a\n日","本","üü✓"),    (" 日éü日日\t…\n日éb““”本\n……","a\t“"," "),    ("—本本b本 \nééé“ü“…é日ü\ta—üa✓”"," bé","”"),    ("日\t✓日…ü 日🎉","日b🎉","✓ 本🎉ü \n"),    ("\n✓”é\n日 …\n“\n\t… 本本”日🎉b\nb本🎉…bü本✓","é\tü\n"," \n\n本é—"),    ("日——…\n“\t\t本\n\n日a🎉aé ✓\t…本\n“日日“本✓“本”日\t日","本日","✓a“\t"),    ("“a","b✓\n日","” 本 ✓"),    ("🎉“\n本—”\t…“\n é”日🎉—✓"," ","…—  \t"),    ("🎉——日\n—\nb—“🎉 ✓é“🎉本本é \n a\t\t本 …\t","\t✓b本本é","b🎉\t✓"),    ("🎉","🎉 ","“"),    (" —é本🎉  🎉\t日…é ✓—🎉ü ","\n日—","本b日"),    ("—🎉é日  b“—… 🎉““b本””b“ “\n"," ","—本\n日"),    ("\t\t🎉“\n…ü”a日a 日✓a✓”—日\t本aé🎉\tb …a✓—a…b🎉—…a” ","…\n本🎉”","\n✓—é"),    ("b日 ü🎉ü\n—é   本 é“——büü…é—b   🎉b日 ——”é","…“","büb日\n"),    ("日 b日✓\n本✓…","日a…”…b“","é—\n…"),    ("“b\t🎉 …✓“ ü本✓ é“—…—日\n日\téé\t\n\n…ü abb…🎉","”…”\n\n …a","a"),    ("b本\t\na\t—🎉🎉日”ü\n”—…éü本本b本日—本ü\nü—\n“ü—","日🎉\n“—","🎉\t”本 a é"),    ("b🎉日\n\n“ \t”…日ü\n✓\tü日a“““ü\n…\n\n\n“ü ","—“  b日✓本","日—é日 🎉\n "),    ("“é — 本\tbbééü“日本“—b🎉🎉✓\n”\né日🎉\n…ü—本","🎉a日 é\n","本日üa"),    ("ü本\tü🎉a🎉—✓\n\t\n…\n\n🎉“日日é  本—\n…\n✓”””b”“\n本“\t🎉","— 🎉✓","”\nba—“éb"),    ("✓本日ü✓✓éé …“本é✓\t✓ b","a—éé","ü\n\n本\n”"),    ("“ ü本é日\n\t ü…bb日“本ba…✓—b 日é日”—","b","“”aü✓✓a"),    ("…\té本日ü—é““a—本é\n","🎉—”\n \t","日—\n"),    ("—ü…”\t✓✓…\tbé 🎉 ✓aaé…é 本日éa✓日”é\n✓本","🎉” ✓ ”","“…—本✓…本"),    ("ü…é ……éé— \n———“\n🎉  é日—  \n🎉   ”本\tü✓”üaü","  日"," b"),    ("日b”  —a…🎉  —\na—日"," 🎉","日\t“"),    ("”✓a—a日ü🎉 日本  \nü 🎉 é—…\né日  a\n\t✓”\nb  \n ","é\nü 🎉é b","”\n✓ "),    ("ab","日”“”…\n","é✓” …"),    ("“\t日üa日—日a ü…\t日✓ éé","”\n\t b日","…” 🎉a”"),    ("a”“ \t🎉é“éa— b”“ a 日…✓a","本✓日 🎉— ","…✓b"),    ("a本\n✓—”é  —✓é\t——🎉ü”…\téé”ü本","🎉é\t","”本🎉ab”"),    ("—“","aü","—本"),    ("   —日日 ”—日b ü “b  ü🎉“ébé”\n \tb🎉“…✓“é\n","é a✓✓”“","\t“本a日"),    ("🎉✓ 🎉…日本“✓…a\t—✓ 🎉”—éa” —é…🎉\t\t” \n日✓日éa","a…—…","🎉““éé"),    ("日” b本","é ","\t “ü✓\ta“"),    ("日\n“” \n“日üa日日","“","—"),    ("本 —本—ü“…本b é✓é ","本🎉\n🎉","✓ éü"),    ("\n\t““b","é","\nb✓\n"),    ("é“✓ … — ” …”日","”本日","üü ✓b"),    (" \n本”b \t”本—✓本üab✓\nb本ü\n “本🎉"," “ b”“","—"),    ("ü—\n”…b”✓本… “b”日日aü 🎉\t \t本✓””“\t  ✓é“b🎉🎉b🎉"," é✓”","日… 日\n"),    ("🎉a🎉本bü🎉🎉🎉\n…\nü\t本é\t日✓","🎉a“\téü"," 本b🎉 a"),    ("日本—🎉日a","é","  "),    ("b日”b✓—\n—🎉””é\n✓“”é\t🎉 日日✓a…✓”b…\t é"," 本é🎉✓—日","\n\té本"),    ("—é—ü\n 日ba”🎉\tb b”日ü“ ","🎉”日✓—✓“","…本\t"),    ("\t…本 é\t“\nb日 日日 \n本 日日🎉—é本a…✓✓é ”“\t a"," 🎉\t \tüa","ü\tb"),    ("a日🎉…\nü🎉 é“ü ü\n","…a\t本","  "),    ("”\n本\n—…“” aü 日üba\n✓”“\nüé ü é日a\n✓aé日é","éé  本","✓üa” "),    ("ü\tü\n”✓b日…\t本a日“\n\t🎉 \nb“✓","\n本","…ba"),    ("…—”\na✓ 本\t”—…a…日üba\nb “—✓—…","\nab本”—","✓🎉ü日"),    ("本…\na …✓\n— 🎉 ü日 é","é日本 ","b“🎉ü aü"),    ("””\t —\téa —“—\t“日 🎉 “b\ta \nü”\nü","✓✓üü\n —\n","\n"),    ("…本\t\n \n日本\nü” üa✓“\n","日🎉🎉 \n日\t","aéa é\nü\n"),    ("\n✓ ü日🎉”“ …b…bbé本“ ","— “✓b本","本日…"),    ("日 本日 ”é日本\t“✓b日🎉… …✓ —é b\n\nb——","✓ ","—"),    ("aa“—b🎉日a\t”ü日” \n\n“本…本 \n ✓","✓“ü——","日 aé"),    ("”…—","\t”🎉🎉\n“”","b"),    ("✓— a” \tü✓ \nb ","b\nb","—\t"),    ("\t日—\n✓aa✓”“日 日🎉“ \t本✓“é✓üa—éü🎉✓” \tü\t🎉 ","🎉…✓éb","—\n"),    ("—\t“b — éaü\t 本…\ta日 🎉","a ","üb—“🎉éb"),    ("\n🎉✓é\n…\t✓”✓ ü—…\tü é本 é”日bü","ü—\t","\t✓bü"),    ("\n“✓—b…\t \n✓","…—\nbé\t","本 …”b”"),    ("🎉é 日本“ ”aü“—✓b\n本✓é\tb“ \n ab\nü— é—✓\n”","日a\t ","本bü …"),
        ];
        for (content, old, new) in &cases {
            run_all(content, old, new);
        }
    }
}
