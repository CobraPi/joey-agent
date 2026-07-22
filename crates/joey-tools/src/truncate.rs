//! Output truncation limits and markers (port of `tools/tool_output_limits.py`
//! plus the terminal head/tail truncation in `tools/terminal_tool.py:2818-2829`
//! and the pagination normalizers in `tools/file_operations.py`).

use joey_core::Config;

/// Defaults matching upstream (`tool_output_limits.py:39-41`).
pub const DEFAULT_MAX_BYTES: usize = 50_000;
pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_LINE_LENGTH: usize = 2000;

/// Default read_file pagination (`file_operations.py:713-716`).
pub const DEFAULT_READ_OFFSET: usize = 1;
pub const DEFAULT_READ_LIMIT: usize = 500;
pub const DEFAULT_SEARCH_OFFSET: usize = 0;
pub const DEFAULT_SEARCH_LIMIT: usize = 50;

/// Resolved tool-output limits — read from the `tool_output` config section,
/// falling back to the built-in defaults on any missing/invalid entry.
#[derive(Debug, Clone, Copy)]
pub struct ToolOutputLimits {
    pub max_bytes: usize,
    pub max_lines: usize,
    pub max_line_length: usize,
}

fn positive(v: i64, default: usize) -> usize {
    if v > 0 {
        v as usize
    } else {
        default
    }
}

/// Port of `get_tool_output_limits` (config-driven, never raises).
pub fn get_tool_output_limits(config: &Config) -> ToolOutputLimits {
    ToolOutputLimits {
        max_bytes: positive(
            config.get_i64("tool_output.max_bytes", DEFAULT_MAX_BYTES as i64),
            DEFAULT_MAX_BYTES,
        ),
        max_lines: positive(
            config.get_i64("tool_output.max_lines", DEFAULT_MAX_LINES as i64),
            DEFAULT_MAX_LINES,
        ),
        max_line_length: positive(
            config.get_i64("tool_output.max_line_length", DEFAULT_MAX_LINE_LENGTH as i64),
            DEFAULT_MAX_LINE_LENGTH,
        ),
    }
}

/// Head/tail truncation with the terminal tool's exact marker
/// (terminal_tool.py:2818-2829): 40% head, 60% tail.
pub fn truncate_terminal_output(output: &str, max_output_chars: usize) -> String {
    if output.len() <= max_output_chars {
        return output.to_string();
    }
    let head_chars = (max_output_chars as f64 * 0.4) as usize;
    let tail_chars = max_output_chars - head_chars;
    let omitted = output.len() - head_chars - tail_chars;
    let truncated_notice = format!(
        "\n\n... [OUTPUT TRUNCATED - {} chars omitted out of {} total] ...\n\n",
        omitted,
        output.len()
    );
    // Slice on char boundaries (Python slices on chars; byte offsets can land
    // mid-UTF-8 in Rust, so snap inward to the nearest boundary).
    let head_end = floor_char_boundary(output, head_chars);
    let tail_start = ceil_char_boundary(output, output.len() - tail_chars);
    format!("{}{}{}", &output[..head_end], truncated_notice, &output[tail_start..])
}

pub fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

pub fn ceil_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

/// Port of `normalize_read_pagination` — clamp offset ≥ 1 and
/// 1 ≤ limit ≤ `tool_output.max_lines`.
pub fn normalize_read_pagination(
    offset: Option<i64>,
    limit: Option<i64>,
    max_lines: usize,
) -> (usize, usize) {
    let offset = offset.unwrap_or(DEFAULT_READ_OFFSET as i64).max(1) as usize;
    let limit = limit.unwrap_or(DEFAULT_READ_LIMIT as i64).max(1) as usize;
    (offset, limit.min(max_lines))
}

/// Port of `normalize_search_pagination`.
pub fn normalize_search_pagination(offset: Option<i64>, limit: Option<i64>) -> (usize, usize) {
    let offset = offset.unwrap_or(DEFAULT_SEARCH_OFFSET as i64).max(0) as usize;
    let limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT as i64).max(1) as usize;
    (offset, limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_short_text() {
        assert_eq!(truncate_terminal_output("hello", 100), "hello");
    }

    #[test]
    fn terminal_marker_is_verbatim() {
        let text = "x".repeat(1000);
        let out = truncate_terminal_output(&text, 100);
        // head 40, tail 60, omitted 900.
        assert!(out.contains(
            "\n\n... [OUTPUT TRUNCATED - 900 chars omitted out of 1000 total] ...\n\n"
        ));
        assert!(out.starts_with(&"x".repeat(40)));
        assert!(out.ends_with(&"x".repeat(60)));
    }

    #[test]
    fn pagination_clamps() {
        assert_eq!(normalize_read_pagination(Some(0), Some(99999), 2000), (1, 2000));
        assert_eq!(normalize_read_pagination(None, None, 2000), (1, 500));
        assert_eq!(normalize_read_pagination(Some(5), Some(0), 2000), (5, 1));
        assert_eq!(normalize_search_pagination(Some(-2), Some(-5)), (0, 1));
    }

    #[test]
    fn config_defaults() {
        let cfg = joey_core::Config::defaults();
        let lim = get_tool_output_limits(&cfg);
        assert_eq!(lim.max_bytes, 50_000);
        assert_eq!(lim.max_lines, 2000);
        assert_eq!(lim.max_line_length, 2000);
    }
}
