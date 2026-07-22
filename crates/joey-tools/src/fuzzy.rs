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
    sorted_matches.sort_by(|a, b| b.0.cmp(&a.0));
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
        } else if ob[orig_idx] == b' ' || ob[orig_idx] == b'\t' {
            orig_to_norm.push(norm_idx);
            orig_idx += 1;
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
