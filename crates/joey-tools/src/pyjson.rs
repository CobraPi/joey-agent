//! Python-`json.dumps`-compatible serialization.
//!
//! Upstream tool results are produced with `json.dumps(..., ensure_ascii=False)`
//! whose default separators are `", "` and `": "` (a space after each colon and
//! comma), and with `indent=2` for the web tools. serde_json's compact writer
//! uses `","`/`":"`, so result envelopes would not be byte-identical without
//! this formatter. Key order is preserved (serde_json `preserve_order`).

use serde_json::ser::Formatter;
use serde_json::Value;
use std::io;

struct PyFormatter;

impl Formatter for PyFormatter {
    fn begin_array_value<W>(&mut self, writer: &mut W, first: bool) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        if !first {
            writer.write_all(b", ")?;
        }
        Ok(())
    }

    fn begin_object_key<W>(&mut self, writer: &mut W, first: bool) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        if !first {
            writer.write_all(b", ")?;
        }
        Ok(())
    }

    fn begin_object_value<W>(&mut self, writer: &mut W) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        writer.write_all(b": ")
    }
}

/// `json.dumps(value, ensure_ascii=False)` equivalent.
pub fn dumps(value: &Value) -> String {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, PyFormatter);
    serde::Serialize::serialize(value, &mut ser).expect("JSON serialization cannot fail");
    String::from_utf8(out).expect("serde_json emits UTF-8")
}

/// `json.dumps(value, indent=2, ensure_ascii=False)` equivalent — serde_json's
/// pretty printer matches Python's `indent=2` layout exactly.
pub fn dumps_indent2(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("JSON serialization cannot fail")
}

/// Format an integer with thousands separators, mirroring Python's `{:,}`.
pub fn commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    let digits: Vec<char> = s.chars().collect();
    for (i, c) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matches_python_default_separators() {
        let v = json!({"error": "file not found"});
        assert_eq!(dumps(&v), r#"{"error": "file not found"}"#);
        let v2 = json!({"a": [1, 2], "b": {"c": true}, "d": null});
        assert_eq!(dumps(&v2), r#"{"a": [1, 2], "b": {"c": true}, "d": null}"#);
        // Non-ASCII stays raw (ensure_ascii=False).
        let v3 = json!({"s": "héllo"});
        assert_eq!(dumps(&v3), "{\"s\": \"héllo\"}");
    }

    #[test]
    fn preserves_key_order() {
        let v = json!({"z": 1, "a": 2, "m": 3});
        assert_eq!(dumps(&v), r#"{"z": 1, "a": 2, "m": 3}"#);
    }

    #[test]
    fn indent2_matches_python() {
        let v = json!({"a": 1, "b": [1, 2]});
        assert_eq!(dumps_indent2(&v), "{\n  \"a\": 1,\n  \"b\": [\n    1,\n    2\n  ]\n}");
    }

    #[test]
    fn comma_grouping() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1000), "1,000");
        assert_eq!(commas(100000), "100,000");
        assert_eq!(commas(2000000), "2,000,000");
    }
}
