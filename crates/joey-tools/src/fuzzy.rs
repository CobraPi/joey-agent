//! Fuzzy find-and-replace for the `patch` tool (port of `tools/fuzzy_match.py`).
//!
//! Tries a chain of increasingly-lenient matching strategies, in order:
//!   1. exact
//!   2. line-trimmed (per-line whitespace-trimmed equality)
//!   3. whitespace-normalized (collapse runs of whitespace)
//!   4. indentation-flexible (ignore leading indentation)
//!   5. escape-normalized (\\n, \\t literals)
//!   6. unicode-normalized (smart quotes/dashes → ASCII)
//!   7. block-anchor (first+last line anchor a multi-line block)
//!   8. context-aware (≥50% line similarity)
//!
//! Strategy order is load-bearing: exact first, fuzzy last.

/// Outcome of a fuzzy replace.
#[derive(Debug)]
pub struct FuzzyReplace {
    pub new_content: String,
    pub match_count: usize,
    pub strategy: &'static str,
}

/// Replace `old` with `new` in `content`. When `replace_all` is false, only the
/// first match is replaced (and an error is returned if the match is ambiguous
/// under the exact strategy). Returns `Err` with a human message on no match.
pub fn find_and_replace(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<FuzzyReplace, String> {
    if old.is_empty() {
        return Err("old_string is empty".to_string());
    }

    // 1. exact
    if content.contains(old) {
        let count = content.matches(old).count();
        if !replace_all && count > 1 {
            return Err(format!(
                "old_string matched {} times; pass replace_all=true or add more context",
                count
            ));
        }
        let new_content = if replace_all {
            content.replace(old, new)
        } else {
            content.replacen(old, new, 1)
        };
        return Ok(FuzzyReplace {
            new_content,
            match_count: if replace_all { count } else { 1 },
            strategy: "exact",
        });
    }

    // Strategies 2-8 operate on line blocks and match a single occurrence.
    let strategies: &[(&'static str, fn(&str, &str) -> Option<(usize, usize)>)] = &[
        ("line_trimmed", match_line_trimmed),
        ("whitespace_normalized", match_whitespace_normalized),
        ("indentation_flexible", match_indentation_flexible),
        ("escape_normalized", match_escape_normalized),
        ("unicode_normalized", match_unicode_normalized),
        ("block_anchor", match_block_anchor),
        ("context_aware", match_context_aware),
    ];

    let content_lines: Vec<&str> = content.lines().collect();
    for (name, strat) in strategies {
        if let Some((start, end)) = strat(content, old) {
            let new_content = splice_lines(&content_lines, start, end, new, content);
            return Ok(FuzzyReplace {
                new_content,
                match_count: 1,
                strategy: name,
            });
        }
    }

    Err("old_string not found in file (tried exact and 7 fuzzy strategies)".to_string())
}

/// Replace lines [start, end) with `new` text, preserving the file's trailing
/// newline convention.
fn splice_lines(lines: &[&str], start: usize, end: usize, new: &str, original: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    out.extend(lines[..start].iter().map(|s| s.to_string()));
    // Re-indent replacement to match the matched block's leading indentation.
    let indent = leading_ws(lines.get(start).copied().unwrap_or(""));
    for (i, l) in new.lines().enumerate() {
        if i == 0 {
            out.push(l.to_string());
        } else if l.is_empty() {
            out.push(String::new());
        } else {
            out.push(format!("{}{}", indent, l.trim_start_matches(|c| c == ' ' || c == '\t')));
        }
    }
    out.extend(lines[end..].iter().map(|s| s.to_string()));
    let mut joined = out.join("\n");
    if original.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

fn leading_ws(s: &str) -> String {
    s.chars().take_while(|c| *c == ' ' || *c == '\t').collect()
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_unicode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201F}' => '"',
            '\u{2013}' | '\u{2014}' => '-',
            '\u{00A0}' => ' ',
            other => other,
        })
        .collect()
}

fn normalize_escapes(s: &str) -> String {
    s.replace("\\n", "\n").replace("\\t", "\t").replace("\\\"", "\"")
}

/// Find a contiguous block in `content` whose lines match `old`'s lines under a
/// per-line predicate. Returns the [start, end) line range.
fn match_block_with<F: Fn(&str, &str) -> bool>(content: &str, old: &str, eq: F) -> Option<(usize, usize)> {
    let content_lines: Vec<&str> = content.lines().collect();
    let old_lines: Vec<&str> = old.lines().collect();
    if old_lines.is_empty() || old_lines.len() > content_lines.len() {
        return None;
    }
    for start in 0..=(content_lines.len() - old_lines.len()) {
        let mut matched = true;
        for (i, ol) in old_lines.iter().enumerate() {
            if !eq(content_lines[start + i], ol) {
                matched = false;
                break;
            }
        }
        if matched {
            return Some((start, start + old_lines.len()));
        }
    }
    None
}

fn match_line_trimmed(content: &str, old: &str) -> Option<(usize, usize)> {
    match_block_with(content, old, |c, o| c.trim() == o.trim())
}

fn match_whitespace_normalized(content: &str, old: &str) -> Option<(usize, usize)> {
    match_block_with(content, old, |c, o| normalize_ws(c) == normalize_ws(o))
}

fn match_indentation_flexible(content: &str, old: &str) -> Option<(usize, usize)> {
    match_block_with(content, old, |c, o| c.trim_start() == o.trim_start())
}

fn match_escape_normalized(content: &str, old: &str) -> Option<(usize, usize)> {
    let normalized_old = normalize_escapes(old);
    match_block_with(content, &normalized_old, |c, o| c.trim() == o.trim())
}

fn match_unicode_normalized(content: &str, old: &str) -> Option<(usize, usize)> {
    let nold = normalize_unicode(old);
    match_block_with(content, &nold, |c, o| normalize_unicode(c).trim() == o.trim())
}

/// Anchor a multi-line block by its first and last non-empty trimmed lines.
fn match_block_anchor(content: &str, old: &str) -> Option<(usize, usize)> {
    let content_lines: Vec<&str> = content.lines().collect();
    let old_lines: Vec<&str> = old.lines().collect();
    if old_lines.len() < 3 {
        return None;
    }
    let first = old_lines.first()?.trim();
    let last = old_lines.last()?.trim();
    let span = old_lines.len();
    if content_lines.len() < span {
        return None;
    }
    for start in 0..=(content_lines.len() - span) {
        let end = start + span - 1;
        if content_lines[start].trim() == first && content_lines[end].trim() == last {
            return Some((start, end + 1));
        }
    }
    None
}

/// Match when ≥50% of block lines are similar (Levenshtein ratio).
fn match_context_aware(content: &str, old: &str) -> Option<(usize, usize)> {
    let content_lines: Vec<&str> = content.lines().collect();
    let old_lines: Vec<&str> = old.lines().collect();
    if old_lines.is_empty() || old_lines.len() > content_lines.len() {
        return None;
    }
    let span = old_lines.len();
    let mut best: Option<(usize, f64)> = None;
    for start in 0..=(content_lines.len() - span) {
        let mut sim_sum = 0.0;
        for (i, ol) in old_lines.iter().enumerate() {
            sim_sum += strsim::normalized_levenshtein(content_lines[start + i].trim(), ol.trim());
        }
        let avg = sim_sum / span as f64;
        if avg >= 0.5 && best.map(|(_, b)| avg > b).unwrap_or(true) {
            best = Some((start, avg));
        }
    }
    best.map(|(start, _)| (start, start + span))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_replace() {
        let r = find_and_replace("foo bar baz", "bar", "QUX", false).unwrap();
        assert_eq!(r.new_content, "foo QUX baz");
        assert_eq!(r.strategy, "exact");
    }

    #[test]
    fn ambiguous_without_replace_all() {
        let err = find_and_replace("a a a", "a", "b", false).unwrap_err();
        assert!(err.contains("matched"));
    }

    #[test]
    fn replace_all() {
        let r = find_and_replace("a a a", "a", "b", true).unwrap();
        assert_eq!(r.new_content, "b b b");
        assert_eq!(r.match_count, 3);
    }

    #[test]
    fn line_trimmed_strategy() {
        // Content is tab-indented; old_string is space-indented, so it is NOT
        // an exact substring and the line-trimmed strategy must catch it.
        let content = "fn main() {\n\tlet x = 1;\n}\n";
        let old = "    let x = 1;";
        let r = find_and_replace(content, old, "let x = 2;", false).unwrap();
        assert!(r.new_content.contains("let x = 2;"));
        assert_ne!(r.strategy, "exact");
    }
}
