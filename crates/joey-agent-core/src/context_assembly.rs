//! Dynamic context assembly (opt-in): budgeted per-request assembly —
//! relevance-ranked tool-schema selection, always-keep pinning, state-block
//! budget cap, and a per-session JSONL assembly log. Disabled by default;
//! when disabled every entry point is an identity function (byte-parity
//! guarantee). No upstream Hermes counterpart — Joey-only addition.

use joey_providers::ToolSchema;
use std::path::PathBuf;

/// FNV-1a 64-bit hash rendered as 8 lowercase hex chars (low 32 bits).
fn fnv1a64_hex8(s: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", (hash & 0xffff_ffff) as u32)
}

/// Sanitize a session key to `[A-Za-z0-9._-]`, collapsing each run of
/// disallowed chars to a single `-` (same semantics as joey-tools
/// scratchpad_tool::sanitize_key, implemented locally to avoid a
/// cross-crate private export).
fn sanitize_key(key: &str) -> String {
    let mut out = String::new();
    let mut prev_replaced = false;
    for c in key.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            prev_replaced = false;
        } else if !prev_replaced {
            out.push('-');
            prev_replaced = true;
        }
    }
    out
}

/// Tokenize a query into lowercase alphanumeric terms: split on
/// non-alphanumeric, drop terms shorter than 2 chars, dedupe preserving
/// first occurrence.
fn query_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for raw in query.split(|c: char| !c.is_alphanumeric()) {
        if raw.chars().count() < 2 {
            continue;
        }
        let term = raw.to_lowercase();
        if !terms.contains(&term) {
            terms.push(term);
        }
    }
    terms
}

/// Relevance score per tool (aligned with `tools`): each query term
/// contributes +3 if it is a substring of the lowercase tool name and
/// +1 if it is a substring of the lowercase description (a term can
/// contribute both).
pub fn relevance_scores(query: &str, tools: &[ToolSchema]) -> Vec<usize> {
    let terms = query_terms(query);
    tools
        .iter()
        .map(|tool| {
            let name = tool.function.name.to_lowercase();
            let description = tool.function.description.to_lowercase();
            terms
                .iter()
                .map(|term| {
                    let mut score = 0;
                    if name.contains(term.as_str()) {
                        score += 3;
                    }
                    if description.contains(term.as_str()) {
                        score += 1;
                    }
                    score
                })
                .sum()
        })
        .collect()
}

/// Select up to `top_k` tools for a request.
///
/// (a) If `tools.len() <= top_k`, returns all tools unchanged (no-op).
/// (b) Otherwise, kept = tools whose `function.name` is in `always_keep`
///     (original input order; if pinning alone exceeds `top_k`, pinning
///     wins — kept stays larger), then the remaining slots up to `top_k`
///     total are filled with the rest sorted by (score desc, name asc).
///     Dropped = names not kept, sorted by name.
pub fn select_tools(
    query: &str,
    tools: &[ToolSchema],
    top_k: usize,
    always_keep: &[String],
) -> (Vec<ToolSchema>, Vec<String>) {
    if tools.len() <= top_k {
        return (tools.to_vec(), Vec::new());
    }
    let scores = relevance_scores(query, tools);
    let mut kept_idx: Vec<usize> = Vec::new();
    for (idx, tool) in tools.iter().enumerate() {
        if always_keep.iter().any(|name| name == &tool.function.name) {
            kept_idx.push(idx);
        }
    }
    // Fill remaining slots up to top_k total with the rest, best first.
    let mut rest_idx: Vec<usize> = (0..tools.len())
        .filter(|idx| !kept_idx.contains(idx))
        .collect();
    rest_idx.sort_by(|&a, &b| {
        scores[b]
            .cmp(&scores[a])
            .then_with(|| tools[a].function.name.cmp(&tools[b].function.name))
    });
    for idx in rest_idx {
        if kept_idx.len() >= top_k {
            break;
        }
        kept_idx.push(idx);
    }
    let kept_names: Vec<String> = kept_idx
        .iter()
        .map(|&idx| tools[idx].function.name.clone())
        .collect();
    let mut dropped: Vec<String> = tools
        .iter()
        .map(|tool| tool.function.name.clone())
        .filter(|name| !kept_names.contains(name))
        .collect();
    dropped.sort();
    let kept = kept_idx.into_iter().map(|idx| tools[idx].clone()).collect();
    (kept, dropped)
}

/// Cap a state block to `max_chars` (line-based, deterministic).
///
/// `None` stays `None`; blocks within budget pass through unchanged.
/// Otherwise keep the first line plus as many following whole lines as
/// fit within `max_chars - 12`, then append `"\n[truncated]"`. Never
/// exceeds `max_chars`. If even the first line doesn't fit in
/// `max_chars - 12`, the first line is truncated at a char boundary to
/// `max_chars - 12` chars and the marker appended.
pub fn cap_state_block(block: Option<String>, max_chars: usize) -> Option<String> {
    let s = block?;
    if s.chars().count() <= max_chars {
        return Some(s);
    }
    const MARKER: &str = "\n[truncated]"; // 12 chars
    let budget = max_chars.saturating_sub(MARKER.chars().count());
    let mut lines = s.split('\n');
    let first = lines.next().unwrap_or("");
    let first_len = first.chars().count();
    let mut out = String::new();
    if first_len > budget {
        out.extend(first.chars().take(budget));
        out.push_str(MARKER);
        return Some(out);
    }
    out.push_str(first);
    let mut used = first_len;
    for line in lines {
        let line_len = line.chars().count();
        if used + 1 + line_len <= budget {
            out.push('\n');
            out.push_str(line);
            used += 1 + line_len;
        } else {
            break;
        }
    }
    out.push_str(MARKER);
    Some(out)
}

/// Per-session assembly-log directory:
/// `<joey_home>/context-assembly/<sanitized-key>-<fnv1a64-hex8>`.
pub fn log_dir(session_key: &str) -> PathBuf {
    joey_core::constants::joey_home()
        .join("context-assembly")
        .join(format!(
            "{}-{}",
            sanitize_key(session_key),
            fnv1a64_hex8(session_key)
        ))
}

/// One JSONL record per assembled request, appended to
/// `assembly.jsonl` under the session's log dir.
#[derive(serde::Serialize)]
pub struct AssemblyRecord {
    pub ts: String,
    pub turn: usize,
    pub tools_total: usize,
    pub tools_kept: usize,
    pub tools_dropped: Vec<String>,
    pub state_block_truncated: bool,
    pub request_messages: usize,
}

/// Append one record as a JSON line to `<dir>/assembly.jsonl`.
pub fn log_assembly_record(dir: &std::path::Path, record: &AssemblyRecord) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    let line = serde_json::to_string(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(dir.join("assembly.jsonl"))?;
    writeln!(file, "{}", line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, description: &str) -> ToolSchema {
        serde_json::from_str::<ToolSchema>(&format!(
            r#"{{"type":"function","function":{{"name":"{}","description":"{}","parameters":{{"type":"object","properties":{{}}}}}}}}"#,
            name, description
        ))
        .expect("minimal ToolSchema JSON must deserialize")
    }

    #[test]
    fn relevance_read_file_beats_web_search() {
        let tools = vec![
            tool("read_file", "Read file contents"),
            tool("web_search", "Search the web"),
        ];
        let scores = relevance_scores("read the file content", &tools);
        assert!(scores[0] > scores[1], "read_file must outrank web_search");
        assert!(scores[0] > 0);
    }

    #[test]
    fn relevance_empty_query_is_all_zeros() {
        let tools = vec![
            tool("read_file", "Read file contents"),
            tool("web_search", "Search the web"),
        ];
        assert_eq!(relevance_scores("", &tools), vec![0, 0]);
    }

    #[test]
    fn relevance_exact_name_empty_description_is_three() {
        // Single term exactly matching the name: +3 for the name, empty
        // description contributes nothing. (A query like "read_file" would
        // tokenize into two terms and correctly score 6, not 3.)
        let tools = vec![tool("search", "")];
        assert_eq!(relevance_scores("search", &tools), vec![3]);
    }

    #[test]
    fn select_tools_noop_when_within_top_k() {
        let tools = vec![tool("read_file", ""), tool("web_search", "")];
        let (kept, dropped) = select_tools("anything", &tools, 5, &[]);
        assert_eq!(
            kept.iter().map(|t| t.function.name.clone()).collect::<Vec<_>>(),
            vec!["read_file".to_string(), "web_search".to_string()]
        );
        assert!(dropped.is_empty());
    }

    #[test]
    fn select_tools_pins_then_fills_then_drops_sorted() {
        let tools: Vec<ToolSchema> = vec![
            tool("read_file", "Read file contents"),
            tool("c_zip", ""),
            tool("b_zip", ""),
            tool("a_zip", ""),
            tool("data_x", ""),
            tool("m_one", ""),
            tool("m_two", ""),
            tool("m_three", ""),
            tool("m_four", ""),
            tool("m_five", ""),
        ];
        let always_keep = vec!["read_file".to_string()];
        let (kept, dropped) = select_tools("zip data", &tools, 3, &always_keep);
        let kept_names: Vec<String> = kept.iter().map(|t| t.function.name.clone()).collect();
        // Pinned first, then highest-score others, tie-break name asc.
        assert_eq!(
            kept_names,
            vec![
                "read_file".to_string(),
                "a_zip".to_string(),
                "b_zip".to_string()
            ]
        );
        assert_eq!(dropped.len(), 7);
        let mut expected = vec![
            "c_zip".to_string(),
            "data_x".to_string(),
            "m_five".to_string(),
            "m_four".to_string(),
            "m_one".to_string(),
            "m_three".to_string(),
            "m_two".to_string(),
        ];
        expected.sort();
        assert_eq!(dropped, expected);
    }

    #[test]
    fn select_tools_is_deterministic() {
        let tools: Vec<ToolSchema> = vec![
            tool("read_file", "Read file contents"),
            tool("c_zip", ""),
            tool("b_zip", ""),
            tool("a_zip", ""),
            tool("data_x", ""),
            tool("m_one", ""),
            tool("m_two", ""),
            tool("m_three", ""),
            tool("m_four", ""),
            tool("m_five", ""),
        ];
        let always_keep = vec!["read_file".to_string()];
        let (kept_a, dropped_a) = select_tools("zip data", &tools, 3, &always_keep);
        let (kept_b, dropped_b) = select_tools("zip data", &tools, 3, &always_keep);
        assert_eq!(
            kept_a.iter().map(|t| t.function.name.clone()).collect::<Vec<_>>(),
            kept_b.iter().map(|t| t.function.name.clone()).collect::<Vec<_>>()
        );
        assert_eq!(dropped_a, dropped_b);
    }

    #[test]
    fn cap_state_block_none_stays_none() {
        assert_eq!(cap_state_block(None, 100), None);
    }

    #[test]
    fn cap_state_block_exact_fit_unchanged() {
        let block = "line one\nline two";
        assert_eq!(
            cap_state_block(Some(block.to_string()), block.chars().count()),
            Some(block.to_string())
        );
    }

    #[test]
    fn cap_state_block_long_multiline() {
        let block = "first line stays\nsecond\nthird\nfourth";
        let max = 30;
        let capped = cap_state_block(Some(block.to_string()), max).unwrap();
        assert!(capped.chars().count() <= max);
        assert!(capped.starts_with("first line stays"));
        assert!(capped.ends_with("[truncated]"));
    }

    #[test]
    fn cap_state_block_tiny_budget_truncates_first_line() {
        let block = "a very long first line\nsecond";
        let max = 20;
        let capped = cap_state_block(Some(block.to_string()), max).unwrap();
        assert!(capped.chars().count() <= max);
        assert!(capped.ends_with("[truncated]"));
    }

    #[test]
    fn sanitize_and_fnv() {
        assert_eq!(sanitize_key("a b/c:D"), "a-b-c-D");
        assert_eq!(fnv1a64_hex8(""), "84222325");
        let dir = log_dir("x y");
        let segment = dir
            .file_name()
            .expect("log_dir ends in a segment")
            .to_string_lossy()
            .to_string();
        assert!(segment.starts_with("x-y-"), "segment was {}", segment);
    }

    #[test]
    fn log_assembly_record_appends_jsonl() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let record1 = AssemblyRecord {
            ts: chrono::Utc::now().to_rfc3339(),
            turn: 0,
            tools_total: 10,
            tools_kept: 3,
            tools_dropped: vec!["a".to_string()],
            state_block_truncated: false,
            request_messages: 4,
        };
        let record2 = AssemblyRecord {
            ts: chrono::Utc::now().to_rfc3339(),
            turn: 1,
            tools_total: 10,
            tools_kept: 10,
            tools_dropped: vec![],
            state_block_truncated: true,
            request_messages: 6,
        };
        log_assembly_record(tmp.path(), &record1).expect("first append");
        log_assembly_record(tmp.path(), &record2).expect("second append");
        let content = std::fs::read_to_string(tmp.path().join("assembly.jsonl")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            let value: serde_json::Value = serde_json::from_str(line).expect("valid JSON line");
            assert!(value.get("ts").is_some());
            assert!(value.get("turn").is_some());
        }
    }
}
