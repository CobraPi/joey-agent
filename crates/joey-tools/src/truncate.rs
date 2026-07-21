//! Output truncation (port of `tools/tool_output_limits.py` +
//! `_BoundedOutputCollector`). Retains a 40% head / 60% tail window.

/// Default terminal/tool output cap in chars.
pub const DEFAULT_MAX_BYTES: usize = 50_000;
/// Default line cap for `read_file`-style pagination.
pub const DEFAULT_MAX_LINES: usize = 2000;
/// Default per-line clamp.
pub const DEFAULT_MAX_LINE_LENGTH: usize = 2000;

/// Truncate `text` to `max_chars`, keeping a 40% head / 60% tail window with a
/// marker in the middle noting how much was elided.
pub fn bounded_head_tail(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let head_limit = (max_chars as f64 * 0.4) as usize;
    let tail_limit = max_chars - head_limit;
    let elided = total - max_chars;

    let head: String = text.chars().take(head_limit).collect();
    let tail: String = text.chars().skip(total - tail_limit).collect();
    format!(
        "{head}\n\n... [{elided} characters truncated] ...\n\n{tail}",
        head = head,
        elided = elided,
        tail = tail
    )
}

/// Clamp each line to `max_line_length` chars.
pub fn clamp_line_length(text: &str, max_line_length: usize) -> String {
    text.lines()
        .map(|line| {
            if line.chars().count() > max_line_length {
                let clamped: String = line.chars().take(max_line_length).collect();
                format!("{}… [line truncated]", clamped)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_short_text() {
        assert_eq!(bounded_head_tail("hello", 100), "hello");
    }

    #[test]
    fn truncates_long_text_with_marker() {
        let text: String = "x".repeat(1000);
        let out = bounded_head_tail(&text, 100);
        assert!(out.contains("truncated"));
        assert!(out.chars().count() < 300);
    }
}
