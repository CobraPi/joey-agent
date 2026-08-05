//! CST round-trip test (T021, FR-012).
//!
//! Asserts `parse(p, b)?.materialize() == b` (the identity) across every
//! fixture in `tests/fixtures/cst/` — clean and malformed/unknown-syntax.
//! This is the P0 invariant that makes every later widget safe.

use std::fs;
use std::path::PathBuf;

use joey_speckit_ui::cst::parser::parse_bytes;
use joey_speckit_ui::cst::parser_trait::CstMaterialize;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("cst")
}

fn assert_roundtrip(name: &str, bytes: &[u8]) {
    let doc = parse_bytes(name, bytes);
    let materialized = doc.materialize();

    // The identity invariant.
    assert_eq!(
        materialized.as_slice(),
        bytes,
        "round-trip failed for fixture '{name}': materialize() != source.\n\
         source len={}, materialized len={}",
        bytes.len(),
        materialized.len()
    );

    // Additionally: the root node must cover [0, byte_len).
    assert_eq!(
        doc.byte_len, bytes.len(),
        "fixture '{name}': byte_len mismatch"
    );
}

#[test]
fn all_cst_fixtures_round_trip() {
    let dir = fixtures_dir();
    if !dir.exists() {
        eprintln!("fixtures dir not found at {dir:?} — skipping");
        return;
    }

    let mut tested = 0;
    for entry in fs::read_dir(&dir).expect("read fixtures dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        let bytes = fs::read(&path).expect("read fixture");
        assert_roundtrip(name, &bytes);
        tested += 1;
    }
    assert!(tested > 0, "no .md fixtures found in {dir:?}");
}

#[test]
fn roundtrip_empty_input() {
    assert_roundtrip("empty.md", b"");
}

#[test]
fn roundtrip_single_byte() {
    assert_roundtrip("single.md", b"\n");
}

#[test]
fn roundtrip_only_whitespace() {
    assert_roundtrip("ws.md", b"   \t\n  \n\n");
}

#[test]
fn roundtrip_no_trailing_newline() {
    assert_roundtrip("no-newline.md", b"# Heading");
}

#[test]
fn roundtrip_unicode_content() {
    let input = "# Unicode 🎉\n\n- Item with émojis 🚀\n- 日本語のテキスト\n";
    assert_roundtrip("unicode.md", input.as_bytes());
}

#[test]
fn roundtrip_mixed_list_markers() {
    let input = "- dash\n* star\n+ plus\n  - nested dash\n";
    assert_roundtrip("markers.md", input.as_bytes());
}

#[test]
fn roundtrip_code_fence_with_backticks_inside() {
    let input = "```rust\nlet x = `backticks`;\n```\n";
    assert_roundtrip("backticks.md", input.as_bytes());
}

#[test]
fn roundtrip_deeply_nested_lists() {
    let input = "- l1\n  - l2\n    - l3\n      - l4\n        - l5\n";
    assert_roundtrip("nested.md", input.as_bytes());
}

#[test]
fn roundtrip_html_blocks() {
    let input = "<div>\n  <p>HTML block</p>\n</div>\n\nText after.\n";
    assert_roundtrip("html.md", input.as_bytes());
}

#[test]
fn partition_holds_for_malformed_input() {
    use joey_speckit_ui::cst::parser::verify_partition;
    let input = b"   - weird\n\n-- not a bullet\n###\n";
    let doc = parse_bytes("malformed.md", input);
    assert!(
        verify_partition(&doc),
        "CST must partition [0, byte_len) even for malformed input"
    );
}
