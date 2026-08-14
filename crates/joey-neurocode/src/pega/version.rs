//! Pega version detection (FR-009, research.md §4, Clarification Q4).
//!
//! Probes the project for a Pega version marker in priority order:
//! 1. `neurocode.pega.version` config override.
//! 2. Pega-version-bearing build entries (Gradle/Maven BOM).
//! 3. In-source markers (`com.pega.*` packages, `Rule-*` class patterns).

use std::path::Path;

/// Detect the Pega Platform version for a project.
///
/// Returns `Some(version_string)` or `None` (generic-Java fallback).
pub fn detect_pega_version(
    project_root: &Path,
    config_override: &str,
) -> Option<String> {
    // Priority 1: explicit config override.
    if !config_override.trim().is_empty() {
        return Some(config_override.trim().to_string());
    }

    // Priority 2: build-file markers (Gradle/Maven).
    if let Some(v) = detect_from_build_files(project_root) {
        return Some(v);
    }

    // Priority 3: in-source markers.
    if let Some(v) = detect_from_source(project_root) {
        return Some(v);
    }

    None
}

/// Check Gradle `build.gradle`/`build.gradle.kts` and Maven `pom.xml`.
fn detect_from_build_files(project_root: &Path) -> Option<String> {
    // Gradle: look for com.pega:prpub, prweb, or pega-platform dependency.
    for gradle_file in [
        project_root.join("build.gradle"),
        project_root.join("build.gradle.kts"),
    ] {
        if let Ok(content) = std::fs::read_to_string(&gradle_file) {
            if let Some(v) = extract_pega_version_from_gradle(&content) {
                return Some(v);
            }
        }
    }
    // Maven pom.xml.
    let pom = project_root.join("pom.xml");
    if let Ok(content) = std::fs::read_to_string(&pom) {
        if let Some(v) = extract_pega_version_from_maven(&content) {
            return Some(v);
        }
    }
    None
}

fn extract_pega_version_from_gradle(content: &str) -> Option<String> {
    // Patterns like: com.pega:prweb:8.8.0  or  "com.pega:pega-platform:24.1.0"
    for line in content.lines() {
        let trimmed = line.trim();
        if (trimmed.contains("com.pega") || trimmed.contains("prweb") || trimmed.contains("prpc"))
            && (trimmed.contains(':') || trimmed.contains("version"))
        {
            // Try to extract a version string.
            if let Some(v) = extract_version_string(trimmed) {
                return Some(v);
            }
        }
    }
    None
}

fn extract_pega_version_from_maven(content: &str) -> Option<String> {
    // Look for a groupId containing "pega" and extract the version.
    let lower = content.to_lowercase();
    if !lower.contains("pega") {
        return None;
    }
    // Naive extraction: find <version> near a pega dependency.
    let in_pega_block = content.contains("<groupId>com.pega</groupId>");
    if in_pega_block {
        if let Some(v) = extract_version_string(content) {
            return Some(v);
        }
    }
    None
}

fn extract_version_string(text: &str) -> Option<String> {
    // Look for version patterns: x.y.z or Infinity-style '24, '23.
    // Try semver first.
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() >= 3 {
        let candidate = parts[parts.len() - 1].trim();
        let cleaned: String = candidate
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }
    // Try quoted version strings, e.g. `PEGA_VERSION = "8.8.0"`.
    if let Some(start) = text.find('"') {
        let rest = &text[start + 1..];
        if let Some(end) = rest.find('"') {
            let v = rest[..end].trim();
            if v.starts_with(|c: char| c.is_ascii_digit())
                && v.chars().all(|c| c.is_ascii_digit() || c == '.')
            {
                return Some(v.to_string());
            }
        }
    }
    // Try "Infinity 'NN" patterns.
    for tag in &["Infinity '24", "Infinity '23", "Infinity 24", "Infinity 23"] {
        if text.contains(tag) {
            return Some(tag.to_string());
        }
    }
    // Try <version>...</version>
    if let Some(start) = text.find("<version>") {
        let rest = &text[start + 9..];
        if let Some(end) = rest.find("</version>") {
            let v = rest[..end].trim().to_string();
            if !v.is_empty() && v.chars().any(|c| c.is_ascii_digit()) {
                return Some(v);
            }
        }
    }
    None
}

/// Scan source files for Pega markers.
///
/// Returns `Some(version)` only when a real, version-looking string can be
/// extracted from an in-source line mentioning both "pega" and "version"
/// (e.g. a `PEGA_VERSION = "8.8.0"` constant). Bare markers without a
/// version line yield `None` — the caller then operates in generic-Java mode
/// rather than on a fabricated version (T069).
fn detect_from_source(project_root: &Path) -> Option<String> {
    // Quick scan: look for `com.pega` package declarations or Rule-* patterns
    // in up to ~200 Java files.
    let src = project_root.join("src");
    let scan_root = if src.is_dir() { &src } else { project_root };
    let mut scanned = 0u32;
    for entry in walkdir::WalkDir::new(scan_root)
        .max_depth(6)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.path().extension().map_or(false, |e| e == "java") {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(entry.path()) {
            if content.contains("com.pega.") || content.contains("Rule-Obj-") {
                // Markers found: try to extract a real version from lines that
                // mention both "pega" and "version" (case-insensitive).
                for line in content.lines() {
                    let lower = line.to_lowercase();
                    if lower.contains("pega") && lower.contains("version") {
                        if let Some(v) = extract_version_string(line) {
                            if v.starts_with(|c: char| c.is_ascii_digit()) {
                                return Some(v);
                            }
                        }
                    }
                }
                // No version-bearing line: don't fabricate a version.
                return None;
            }
        }
        scanned += 1;
        if scanned > 200 {
            break;
        }
    }
    let _ = &mut scanned;
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn config_override_wins() {
        assert_eq!(
            detect_pega_version(Path::new("/nonexistent"), "8.8.0"),
            Some("8.8.0".to_string())
        );
    }

    #[test]
    fn none_for_empty_project() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(detect_pega_version(tmp.path(), ""), None);
    }

    #[test]
    fn gradle_detection() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("build.gradle"),
            "dependencies {\n    implementation 'com.pega:prweb:8.8.0'\n}\n",
        )
        .unwrap();
        let v = detect_pega_version(tmp.path(), "");
        assert!(v.is_some());
        assert!(v.unwrap().contains("8.8"));
    }

    #[test]
    fn maven_detection() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("pom.xml"),
            "<project><dependencies><dependency>\n<groupId>com.pega</groupId>\n<artifactId>pega-platform</artifactId>\n<version>24.1.0</version>\n</dependency></dependencies></project>",
        )
        .unwrap();
        let v = detect_pega_version(tmp.path(), "");
        assert!(v.is_some());
    }

    #[test]
    fn in_source_markers_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src").join("main").join("java");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("MyRule.java"),
            "package com.example;\nimport com.pega.rules.Rule;\n",
        )
        .unwrap();
        // Bare com.pega markers with no version line → None (generic-Java mode).
        assert_eq!(detect_pega_version(tmp.path(), ""), None);
    }

    #[test]
    fn in_source_version_constant_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src").join("main").join("java");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("MyRule.java"),
            "package com.example;\nimport com.pega.rules.Rule;\n\npublic class MyRule {\n    private static final String PEGA_VERSION = \"8.8.0\";\n}\n",
        )
        .unwrap();
        assert_eq!(
            detect_pega_version(tmp.path(), ""),
            Some("8.8.0".to_string())
        );
    }
}
