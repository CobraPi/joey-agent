//! T036 — Pega version detection integration test.
//!
//! Assert correct detection from Gradle BOM, Maven dependency, in-source
//! markers, config override priority, and None for non-Pega project.

use std::fs;
use std::path::Path;

use joey_neurocode::pega::version::detect_pega_version;

#[test]
fn detects_from_gradle_bom() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("build.gradle"),
        "dependencies {\n    implementation 'com.pega:prweb:8.8.0'\n    implementation 'com.pega:prpub:8.8.0'\n}\n",
    )
    .unwrap();

    let v = detect_pega_version(tmp.path(), "").expect("should detect from Gradle");
    assert!(
        v.contains("8.8"),
        "expected version containing '8.8', got: {}",
        v
    );
}

#[test]
fn detects_from_gradle_kts() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("build.gradle.kts"),
        "dependencies {\n    implementation(\"com.pega:pega-platform:24.1.0\")\n}\n",
    )
    .unwrap();

    let v = detect_pega_version(tmp.path(), "").expect("should detect from Gradle KTS");
    assert!(
        v.contains("24.1") || v.contains("24"),
        "expected Infinity-style version, got: {}",
        v
    );
}

#[test]
fn detects_from_maven_dependency() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("pom.xml"),
        "<project>\n  <dependencies>\n    <dependency>\n      <groupId>com.pega</groupId>\n      <artifactId>pega-platform</artifactId>\n      <version>24.1.0</version>\n    </dependency>\n  </dependencies>\n</project>\n",
    )
    .unwrap();

    let v = detect_pega_version(tmp.path(), "").expect("should detect from Maven pom");
    assert!(
        v.contains("24.1"),
        "expected version containing '24.1', got: {}",
        v
    );
}

#[test]
fn in_source_markers_without_version_yield_none() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src").join("main").join("java").join("com").join("pega");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("MyRule.java"),
        "package com.pega.rules;\nimport com.pega.rules.runtime.Rule;\npublic class MyRule {}\n",
    )
    .unwrap();

    // Bare markers with no version line → None (generic-Java mode, T069).
    assert_eq!(
        detect_pega_version(tmp.path(), ""),
        None,
        "in-source markers without a version line must not fabricate a version"
    );
}

#[test]
fn in_source_version_constant_detected() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src").join("main").join("java").join("com").join("pega");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("MyRule.java"),
        "package com.pega.rules;\nimport com.pega.rules.runtime.Rule;\n\npublic class MyRule {\n    private static final String PEGA_VERSION = \"8.8.0\";\n}\n",
    )
    .unwrap();

    let v = detect_pega_version(tmp.path(), "").expect("version constant should be detected");
    assert_eq!(v, "8.8.0", "expected 8.8.0 from PEGA_VERSION constant, got: {}", v);
}

#[test]
fn config_override_takes_priority() {
    // Even with a build file present, the explicit override wins.
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("build.gradle"),
        "dependencies { implementation 'com.pega:prweb:8.8.0' }\n",
    )
    .unwrap();

    let v = detect_pega_version(tmp.path(), "8.7.3").expect("override should return Some");
    assert_eq!(v, "8.7.3", "config override must win over build file");

    // Also priority over a nonexistent path.
    assert_eq!(
        detect_pega_version(Path::new("/nonexistent"), "8.8.0"),
        Some("8.8.0".to_string())
    );
}

#[test]
fn none_for_non_pega_project() {
    let tmp = tempfile::tempdir().unwrap();
    // A plain Gradle project with no Pega dependency.
    fs::write(
        tmp.path().join("build.gradle"),
        "dependencies {\n    implementation 'org.springframework.boot:spring-boot-starter:3.2.0'\n}\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("pom.xml"),
        "<project><dependencies><dependency>\n<groupId>org.springframework</groupId>\n<artifactId>spring-core</artifactId>\n<version>6.1.0</version>\n</dependency></dependencies></project>",
    )
    .unwrap();

    assert_eq!(
        detect_pega_version(tmp.path(), ""),
        None,
        "non-Pega project should return None"
    );
}

#[test]
fn none_for_empty_project() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(detect_pega_version(tmp.path(), ""), None);
}

#[test]
fn rule_obj_in_source_marker_without_version_is_none() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("Rule.java"),
        "// Rule-Obj-WorkParty-Role reference here\npublic class Rule {}\n",
    )
    .unwrap();

    // Markers alone no longer fabricate a version (T069).
    assert_eq!(
        detect_pega_version(tmp.path(), ""),
        None,
        "Rule-Obj-* marker without a version line should return None"
    );
}
