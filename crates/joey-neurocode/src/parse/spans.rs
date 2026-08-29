//! Byte-offset → 1-based line-range conversion for extracted spans
//! (spec 021, task T003; data-model.md § CodeChunk line fields;
//! research.md R4).
//!
//! The per-language extractors emit BYTE spans (`start_byte`/`end_byte`)
//! because tree-sitter positions are byte-based. The semantic-retrieval
//! layer needs 1-based INCLUSIVE line ranges (`start_line`/`end_line`,
//! both ≥ 1, `start_line ≤ end_line`); the conversion happens here, at
//! index time, given the source text the spans refer to — the extractors
//! themselves stay byte-only and unchanged (additive surface,
//! Constitution VII).
//!
//! Semantics (pinned by unit tests):
//! - Lines are 1-based; line 1 starts at byte 0.
//! - A `\n` belongs to the line it terminates: a span ending in `\n`
//!   reports the line that newline closes, never a phantom following
//!   line, and a trailing newline at EOF does not open a new line.
//! - `\r\n` is handled naturally (`\r` rides its line, `\n` delimits).
//! - Offsets are BYTES (not chars): multibyte UTF-8 content is safe.

/// A 1-based inclusive line range `[start_line, end_line]`
/// (data-model.md § CodeChunk: `start_line ≤ end_line`, both ≥ 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSpan {
    /// First line of the span (1-based, inclusive).
    pub start_line: u32,
    /// Last line of the span (1-based, inclusive).
    pub end_line: u32,
}

impl LineSpan {
    /// Convert a byte span `[start_byte, end_byte)` against `source`
    /// into a 1-based line range. Builds a fresh [`LineIndex`]; when
    /// converting many spans from one file, reuse [`LineIndex::span`].
    pub fn from_byte_span(source: &str, start_byte: u32, end_byte: u32) -> Self {
        LineIndex::new(source).span(start_byte, end_byte)
    }

    /// Whether the span covers exactly one line.
    pub fn is_single_line(&self) -> bool {
        self.start_line == self.end_line
    }
}

/// Precomputed newline positions of a source text, for repeated
/// byte→line conversion (index-time bulk use).
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// Byte offsets of every `\n` in the source, ascending.
    newline_offsets: Vec<u32>,
    /// Total source length in bytes.
    len: u32,
}

impl LineIndex {
    /// Index the newline positions of `source`.
    pub fn new(source: &str) -> Self {
        let bytes = source.as_bytes();
        let newline_offsets = bytes
            .iter()
            .enumerate()
            .filter(|(_, &b)| b == b'\n')
            .map(|(i, _)| i as u32)
            .collect();
        LineIndex { newline_offsets, len: bytes.len() as u32 }
    }

    /// Source length in bytes.
    pub fn len(&self) -> u32 {
        self.len
    }

    /// Whether the source is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Number of lines (≥ 1; a trailing newline does not open a new
    /// line: `""` → 1, `"a"` → 1, `"a\n"` → 1, `"a\nb"` → 2,
    /// `"a\nb\n"` → 2).
    pub fn line_count(&self) -> u32 {
        match self.newline_offsets.last() {
            Some(&last_nl) if last_nl + 1 >= self.len => self.newline_offsets.len() as u32,
            Some(_) => self.newline_offsets.len() as u32 + 1,
            None => 1,
        }
    }

    /// 1-based line containing byte `byte` (clamped to the source). A
    /// `\n` at `byte` belongs to the line it terminates.
    pub fn line_of_byte(&self, byte: u32) -> u32 {
        let byte = byte.min(self.len);
        1 + self.newline_offsets.partition_point(|&o| o < byte) as u32
    }

    /// First byte of 1-based `line` (clamped: lines past the end map to
    /// the source length).
    pub fn line_start_byte(&self, line: u32) -> u32 {
        if line <= 1 {
            return 0;
        }
        match self.newline_offsets.get(line as usize - 2) {
            Some(&o) => o + 1,
            None => self.len,
        }
    }

    /// Exclusive end byte of 1-based `line` — the offset of its
    /// terminating `\n`, or the source length for the final line
    /// (the newline itself is excluded).
    pub fn line_end_byte(&self, line: u32) -> u32 {
        match self.newline_offsets.get(line as usize - 1) {
            Some(&o) => o,
            None => self.len,
        }
    }

    /// The text of 1-based `line` without its newline.
    pub fn line_text<'a>(&self, source: &'a str, line: u32) -> &'a str {
        let s = self.line_start_byte(line) as usize;
        let e = self.line_end_byte(line) as usize;
        if s >= e {
            ""
        } else {
            &source[s..e]
        }
    }

    /// 1-based inclusive line range for byte span `[start_byte, end_byte)`
    /// (both clamped to the source; `end_byte ≤ start_byte` yields a
    /// single-line span at `start_byte`'s line).
    pub fn span(&self, start_byte: u32, end_byte: u32) -> LineSpan {
        let s = start_byte.min(self.len);
        let e = end_byte.clamp(s, self.len);
        let start_line = self.line_of_byte(s);
        let end_line = if e > s { self.line_of_byte(e - 1) } else { start_line };
        LineSpan { start_line, end_line: end_line.max(start_line) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_span() {
        let src = "int x = 1;\nint y = 2;\n";
        let idx = LineIndex::new(src);
        // "int x = 1;" is bytes [0, 11).
        assert_eq!(idx.span(0, 11), LineSpan { start_line: 1, end_line: 1 });
        // "int y = 2;" starts at byte 12.
        assert_eq!(idx.span(12, src.len() as u32), LineSpan { start_line: 2, end_line: 2 });
    }

    #[test]
    fn multi_line_span() {
        let src = "def f():\n    a = 1\n    b = 2\n";
        let span = LineSpan::from_byte_span(src, 0, src.len() as u32);
        assert_eq!(span, LineSpan { start_line: 1, end_line: 3 });
        assert!(!span.is_single_line());
    }

    #[test]
    fn span_ending_with_newline_stays_on_closing_line() {
        // Whole "a\n" file: the trailing newline closes line 1 — it must
        // not open a phantom line 2.
        let idx = LineIndex::new("a\n");
        assert_eq!(idx.span(0, 2), LineSpan { start_line: 1, end_line: 1 });
        // Span covering exactly line 1 including its newline.
        assert_eq!(idx.span(0, 2).end_line, 1);
    }

    #[test]
    fn trailing_newline_at_eof_does_not_open_phantom_line() {
        let idx = LineIndex::new("a\nb\n");
        assert_eq!(idx.span(0, 4), LineSpan { start_line: 1, end_line: 2 });
        assert_eq!(idx.line_count(), 2);
        // File WITHOUT trailing newline.
        let idx2 = LineIndex::new("a\nb");
        assert_eq!(idx2.span(0, 3), LineSpan { start_line: 1, end_line: 2 });
        assert_eq!(idx2.line_count(), 2);
    }

    #[test]
    fn clamping_past_eof() {
        let idx = LineIndex::new("a\nb");
        assert_eq!(idx.span(0, 999), LineSpan { start_line: 1, end_line: 2 });
        assert_eq!(idx.span(500, 900), LineSpan { start_line: 2, end_line: 2 });
    }

    #[test]
    fn empty_span_is_single_line() {
        let idx = LineIndex::new("a\nbb\nccc\n");
        assert_eq!(idx.span(0, 0), LineSpan { start_line: 1, end_line: 1 });
        assert_eq!(idx.span(4, 4), LineSpan { start_line: 2, end_line: 2 });
        // Degenerate end < start collapses to the start line.
        assert_eq!(idx.span(4, 2), LineSpan { start_line: 2, end_line: 2 });
    }

    #[test]
    fn empty_source_yields_line_one_span() {
        let idx = LineIndex::new("");
        assert!(idx.is_empty());
        assert_eq!(idx.len(), 0);
        assert_eq!(idx.line_count(), 1);
        assert_eq!(idx.span(0, 0), LineSpan { start_line: 1, end_line: 1 });
    }

    #[test]
    fn crlf_line_endings() {
        let src = "a\r\nb\r\n";
        let idx = LineIndex::new(src);
        assert_eq!(idx.line_count(), 2);
        assert_eq!(idx.span(0, 1), LineSpan { start_line: 1, end_line: 1 });
        // "b" lives at bytes [4, 5).
        assert_eq!(idx.span(4, 5), LineSpan { start_line: 2, end_line: 2 });
        assert_eq!(idx.span(0, src.len() as u32), LineSpan { start_line: 1, end_line: 2 });
        // `\r` rides its line; line 1's text excludes the terminator.
        assert_eq!(idx.line_text(src, 1), "a\r");
        assert_eq!(idx.line_text(src, 2), "b\r");
    }

    #[test]
    fn multibyte_utf8_byte_offsets() {
        // 'é' is 2 bytes, '漢' is 3 bytes — offsets are BYTES not chars.
        let src = "héllo\nwörld\n";
        let idx = LineIndex::new(src);
        let l2 = src.find("wörld").unwrap() as u32;
        assert_eq!(idx.span(l2, src.len() as u32), LineSpan { start_line: 2, end_line: 2 });
        assert_eq!(idx.line_of_byte(l2), 2);
        assert_eq!(idx.line_text(src, 1), "héllo");
        assert_eq!(idx.line_text(src, 2), "wörld");
    }

    #[test]
    fn line_count_cases() {
        assert_eq!(LineIndex::new("").line_count(), 1);
        assert_eq!(LineIndex::new("a").line_count(), 1);
        assert_eq!(LineIndex::new("a\n").line_count(), 1);
        assert_eq!(LineIndex::new("a\nb").line_count(), 2);
        assert_eq!(LineIndex::new("a\nb\n").line_count(), 2);
        assert_eq!(LineIndex::new("\n\n\n").line_count(), 3);
    }

    #[test]
    fn line_boundaries_round_trip() {
        let src = "one\ntwo\nthree";
        let idx = LineIndex::new(src);
        for line in 1..=idx.line_count() {
            let text = idx.line_text(src, line);
            assert_eq!(idx.line_of_byte(idx.line_start_byte(line)), line);
            // First byte of the line's text resolves back to the line.
            if !text.is_empty() {
                let first = idx.line_start_byte(line);
                assert_eq!(idx.line_of_byte(first), line);
            }
        }
        assert_eq!(idx.line_text(src, 1), "one");
        assert_eq!(idx.line_text(src, 2), "two");
        assert_eq!(idx.line_text(src, 3), "three");
        // Lines past the end clamp to the source length.
        assert_eq!(idx.line_start_byte(99), src.len() as u32);
        assert_eq!(idx.line_end_byte(99), src.len() as u32);
    }
}
