//! Error-output parsing for verification steps (FR-010, T038).
//!
//! Parse Checkstyle XML, compiler errors, and test failures into structured
//! error signatures.

/// A structured error parsed from verification output.
#[derive(Debug, Clone)]
pub struct StructuredError {
    pub signature: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub message: String,
}

/// The parsing format for a verify step's output.
#[derive(Debug, Clone)]
pub enum VerifyParseFormat {
    Plain,
    CheckstyleXml,
    Compiler,
    Maven,
}

impl VerifyParseFormat {
    pub fn from_str(s: &str) -> Self {
        match s {
            "checkstyle_xml" => VerifyParseFormat::CheckstyleXml,
            "compiler" => VerifyParseFormat::Compiler,
            "maven" => VerifyParseFormat::Maven,
            _ => VerifyParseFormat::Plain,
        }
    }
}

/// Parse errors from verification output using the specified format.
pub fn parse_errors(output: &str, format: &str) -> Vec<StructuredError> {
    let fmt = VerifyParseFormat::from_str(format);
    match fmt {
        VerifyParseFormat::CheckstyleXml => parse_checkstyle_xml(output),
        VerifyParseFormat::Compiler => parse_compiler_errors(output),
        VerifyParseFormat::Maven => parse_maven_errors(output),
        VerifyParseFormat::Plain => parse_plain(output),
    }
}

/// Parse Checkstyle XML output.
fn parse_checkstyle_xml(output: &str) -> Vec<StructuredError> {
    let mut errors = Vec::new();
    // Lightweight token-based parsing (handles both multi-line and single-line XML).
    // Track <file name="..."> open tags and <error .../> within them.
    let mut current_file: Option<String> = None;
    let mut pos = 0;
    let bytes = output.as_bytes();
    while pos < bytes.len() {
        if bytes[pos..].starts_with(b"<file ") {
            // Find the name attribute.
            let rest = &output[pos..];
            if let Some(name_start) = rest.find("name=\"") {
                let attr_rest = &rest[name_start + 6..];
                if let Some(end) = attr_rest.find('"') {
                    current_file = Some(attr_rest[..end].to_string());
                }
            }
        } else if bytes[pos..].starts_with(b"<error ") {
            let rest = &output[pos..];
            let line_num = extract_attr(rest, "line")
                .and_then(|s| s.parse::<u32>().ok());
            let message = extract_attr(rest, "message").unwrap_or_default();
            let file = current_file.clone();
            let sig = format!(
                "Checkstyle:{}:{}",
                file.as_deref().unwrap_or("?"),
                line_num.unwrap_or(0)
            );
            errors.push(StructuredError {
                signature: sig,
                file,
                line: line_num,
                message,
            });
        }
        pos += 1;
    }
    errors
}

/// Parse generic compiler errors (javac-style).
fn parse_compiler_errors(output: &str) -> Vec<StructuredError> {
    let mut errors = Vec::new();
    for line in output.lines() {
        // Pattern: File.java:line: error: message
        if line.contains(": error:") || line.contains(": ERROR:") {
            if let Some((file, line_num, message)) = split_file_line_message(line) {
                let sig = format!(
                    "Compile:{}:{}",
                    file,
                    line_num.unwrap_or(0)
                );
                errors.push(StructuredError {
                    signature: sig,
                    file: Some(file),
                    line: line_num,
                    message,
                });
            }
        }
    }
    errors
}

/// Split a javac-style `path:line: error: message` line from the RIGHT.
///
/// Splitting from the left (`splitn(3, ':')`) breaks Windows paths:
/// `C:\Main.java:42: error: msg` would yield file `"C"`. Here the line
/// is split on every colon (`rsplit(':')`) and reassembled around the
/// `error`/`ERROR` marker segment: everything right of the marker is the
/// message, the segment immediately left of it is the line number, and
/// everything further left is the file path. Because only the marker and
/// the line number are peeled off, a drive-letter colon (a single leading
/// char followed by ':') is always part of the reassembled path, never a
/// split boundary.
fn split_file_line_message(line: &str) -> Option<(String, Option<u32>, String)> {
    // parts is right-to-left: parts[0] is the message tail.
    let parts: Vec<&str> = line.rsplit(':').collect();
    // The marker closest to the path (highest index) is the real one; a
    // marker-looking segment inside the message sits further right.
    let marker_idx = (0..parts.len())
        .rev()
        .find(|&i| parts[i].trim() == "error" || parts[i].trim() == "ERROR")?;
    // Need a message right of the marker, a line segment left of it, and
    // a non-empty file path left of that.
    if marker_idx < 1 || marker_idx + 2 > parts.len() - 1 {
        return None;
    }
    let message = parts[..=marker_idx]
        .iter()
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join(":")
        .trim()
        .to_string();
    let line_num = parts[marker_idx + 1].trim().parse::<u32>().ok();
    let file = parts[marker_idx + 2..]
        .iter()
        .rev()
        .copied()
        .collect::<Vec<_>>()
        .join(":");
    Some((file, line_num, message))
}

/// Parse Maven build failures.
fn parse_maven_errors(output: &str) -> Vec<StructuredError> {
    let mut errors = Vec::new();
    for line in output.lines() {
        if line.contains("BUILD FAILURE") || line.contains("ERROR") {
            let msg = line.trim().to_string();
            // `[ERROR] path:line: message` lines carry javac-style
            // diagnostics — extract file/line with the same right-anchored
            // split the compiler parser uses. Other ERROR lines (summary
            // banners, reactor notes) have neither.
            let (file, line_num) = line
                .trim()
                .strip_prefix("[ERROR]")
                .map(str::trim)
                .and_then(split_file_line_message)
                .map_or((None, None), |(f, l, _)| (Some(f), l));
            errors.push(StructuredError {
                signature: format!("Maven:{}", msg),
                file,
                line: line_num,
                message: msg,
            });
        }
    }
    errors
}

/// Plain text errors — just capture non-empty lines that look like errors.
fn parse_plain(output: &str) -> Vec<StructuredError> {
    output
        .lines()
        .filter(|l| {
            let lt = l.to_lowercase();
            lt.contains("error") || lt.contains("fail") || lt.contains("exception")
        })
        .take(20)
        .map(|l| StructuredError {
            signature: "Plain:error".to_string(),
            file: None,
            line: None,
            message: l.trim().to_string(),
        })
        .collect()
}

/// Extract an XML attribute value by name.
fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let pattern = format!("{}=\"", name);
    let start = tag.find(&pattern)? + pattern.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_compiler_error_format() {
        let output = "src/Main.java:42: error: cannot find symbol\n  symbol: variable foo";
        let errors = parse_compiler_errors(output);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].file.as_deref(), Some("src/Main.java"));
        assert_eq!(errors[0].line, Some(42));
    }

    #[test]
    fn parse_compiler_error_windows_drive_path() {
        // The drive-letter colon must stay part of the file path (a left
        // splitn(3, ':') truncated the file to "C").
        let output = "C:\\Main.java:42: error: cannot find symbol";
        let errors = parse_compiler_errors(output);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].file.as_deref(), Some("C:\\Main.java"));
        assert_eq!(errors[0].line, Some(42));
        assert!(errors[0].message.contains("cannot find symbol"));
    }

    #[test]
    fn parse_maven_errors_distinct_signatures() {
        // Distinct ERROR lines must not collapse into one fixed signature.
        let output = "[ERROR] /src/A.java:10: error: ';' expected\n[ERROR] /src/B.java:20: error: cannot find symbol";
        let errors = parse_maven_errors(output);
        assert_eq!(errors.len(), 2);
        assert_ne!(errors[0].signature, errors[1].signature);
        // `[ERROR] path:line:` lines extract file/line like the compiler
        // parser does.
        assert_eq!(errors[0].file.as_deref(), Some("/src/A.java"));
        assert_eq!(errors[0].line, Some(10));
        assert_eq!(errors[1].file.as_deref(), Some("/src/B.java"));
        assert_eq!(errors[1].line, Some(20));
    }

    #[test]
    fn parse_plain_errors() {
        let output = "Everything is fine\nSomething went wrong: error in module X";
        let errors = parse_plain(output);
        assert!(!errors.is_empty());
    }

    #[test]
    fn parse_checkstyle_xml() {
        let xml = r#"<checkstyle><file name="src/Foo.java"><error line="10" message="Bad style"/></file></checkstyle>"#;
        let errors = super::parse_checkstyle_xml(xml);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, Some(10));
        assert_eq!(errors[0].file.as_deref(), Some("src/Foo.java"));
    }
}
