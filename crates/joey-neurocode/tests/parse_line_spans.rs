//! Spec 021 T003/T004 integration tests: parse-layer line spans and
//! fallback coarse chunks.
//!
//! T003: extracted byte spans convert to 1-based inclusive line ranges
//! (data-model.md § CodeChunk `start_line`/`end_line`).
//! T004: source regions containing no named artifact (scripts /
//! top-level code) emit fallback coarse chunks in the same
//! `SourceExtraction` shape (FR-014 groundwork).

use joey_neurocode::parse::registry::parse_any;
use std::path::Path;
use std::fs;

fn parse(path: &Path, source: &str) -> joey_neurocode::parse::extract::SourceExtraction {
    match parse_any(path, source) {
        Some(Ok(extraction)) => extraction,
        Some(Err(e)) => panic!("parse failed for {}: {}", path.display(), e),
        None => panic!("no extractor for {}", path.display()),
    }
}

/// Python sample with named artifacts AND top-level script regions.
/// Line map (1-based, pinned by the assertions below):
///   1  shebang        } uncovered (script)
///   2  import os      }
///   3  import sys     }
///   4  (blank)        }
///   5  CONFIG = ...   }
///   6  (blank)        }
///   7  def setup_logging(level):
///   8      handler = ...
///   9      return handler
///   10 (blank)        } uncovered but blank-only → NOT chunked
///   11 class Greeter:
///   12     greeting = "hello"
///   13 (blank)
///   14     def greet(self, name):
///   15         return ...
///   16 (blank)        } uncovered but blank-only → NOT chunked
///   17 def main():
///   18     g = Greeter()
///   19     print(...)
///   20 (blank)        } uncovered but blank-only → NOT chunked
///   21 if __name__ ... } uncovered (script)
///   22     main()      }
const PYTHON_SAMPLE: &str = "#!/usr/bin/env python3
import os
import sys

CONFIG = {\"debug\": True}

def setup_logging(level):
    handler = StreamHandler(level)
    return handler

class Greeter:
    greeting = \"hello\"

    def greet(self, name):
        return f\"{self.greeting} {name}\"

def main():
    g = Greeter()
    print(g.greet(\"world\"))

if __name__ == \"__main__\":
    main()
";

#[test]
fn python_byte_spans_convert_to_1_based_line_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sample.py");
    fs::write(&path, PYTHON_SAMPLE).unwrap();
    let source = fs::read_to_string(&path).unwrap();

    let ext = parse(&path, &source);
    assert_eq!(ext.language, "python");

    let spans = ext.line_spans(&source);

    // Class Greeter: declared lines 11..15.
    assert_eq!(spans.types.len(), 1);
    let greeter = &spans.types[0];
    assert_eq!(greeter.name, "Greeter");
    assert_eq!(greeter.kind, "class");
    assert_eq!((greeter.start_line, greeter.end_line), (11, 15));

    // Method greet: lines 14..15.
    assert_eq!(greeter.methods.len(), 1);
    let greet = &greeter.methods[0];
    assert_eq!(greet.name, "greet");
    assert_eq!(greet.kind, "method");
    assert_eq!((greet.start_line, greet.end_line), (14, 15));

    // Module functions: setup_logging 7..9, main 17..19.
    let fn_map: Vec<(&str, u32, u32)> = spans
        .module_functions
        .iter()
        .map(|f| (f.name.as_str(), f.start_line, f.end_line))
        .collect();
    assert!(fn_map.contains(&("setup_logging", 7, 9)), "got {:?}", fn_map);
    assert!(fn_map.contains(&("main", 17, 19)), "got {:?}", fn_map);

    // The line spans are byte-derived: cross-check one span against the
    // raw source — line 11 must be the `class Greeter:` line.
    let line11: Vec<&str> = source.lines().collect();
    assert!(line11[10].starts_with("class Greeter"));
}

#[test]
fn java_byte_spans_convert_to_1_based_line_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("UserService.java");
    let src = "package com.example;\n\nimport java.util.List;\n\npublic class UserService {\n    private final UserRepository repo;\n\n    public String findUserName(Long id) {\n        return repo.findName(id);\n    }\n}\n";
    fs::write(&path, src).unwrap();

    let ext = parse(&path, src);
    assert_eq!(ext.language, "java");

    let spans = ext.line_spans(&src);
    assert_eq!(spans.types.len(), 1);
    let svc = &spans.types[0];
    assert_eq!(svc.name, "UserService");
    assert_eq!((svc.start_line, svc.end_line), (5, 11));
    assert_eq!(svc.methods.len(), 1);
    assert_eq!(svc.methods[0].name, "findUserName");
    assert_eq!((svc.methods[0].start_line, svc.methods[0].end_line), (8, 10));
}

#[test]
fn python_top_level_code_yields_fallback_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("script.py");
    fs::write(&path, PYTHON_SAMPLE).unwrap();
    let source = fs::read_to_string(&path).unwrap();

    let mut ext = parse(&path, &source);
    assert!(ext.fallback_chunks.is_empty(), "empty before the population pass");

    ext.populate_fallback_chunks(&source);

    // Exactly two coarse chunks: the header script lines 1..6 and the
    // trailing script lines 20..22. Line 20 is blank but CONTIGUOUS with
    // the __main__ guard (no covered line between them), so it joins that
    // run — maximal uncovered runs keep their interior blank lines. The
    // blank-only runs at lines 10 and 16 sit BETWEEN covered artifacts,
    // carry no content, and are dropped.
    let chunks = &ext.fallback_chunks;
    assert_eq!(chunks.len(), 2, "got {:?}", chunks);
    assert_eq!((chunks[0].start_line, chunks[0].end_line), (1, 6));
    assert_eq!((chunks[1].start_line, chunks[1].end_line), (20, 22));

    // Chunk text is byte-sliced from the source: first chunk is lines
    // 1..6 verbatim, containing shebang, imports, and the CONFIG
    // constant. Its last line (6) is blank, so the text ends with line
    // 5's separator '\n' — but the chunk's OWN terminating newline (the
    // one closing line 6) is excluded: it sits at `end_byte`, just past
    // the slice.
    let text0 = chunks[0].text(&source);
    assert!(text0.starts_with("#!/usr/bin/env python3"));
    assert!(text0.contains("import os"));
    assert!(text0.contains("CONFIG"));
    assert_eq!(source.as_bytes()[chunks[0].end_byte as usize], b'\n');
    assert_eq!(text0, "#!/usr/bin/env python3\nimport os\nimport sys\n\nCONFIG = {\"debug\": True}\n");

    // Line spans mirror the populated chunks (same SourceExtraction shape).
    let spans = ext.line_spans(&source);
    assert_eq!(spans.fallback_chunks, *chunks);

    // No overlap: every fallback line is outside every artifact span.
    let mut artifact_lines = std::collections::BTreeSet::new();
    for t in &spans.types {
        for l in t.start_line..=t.end_line {
            artifact_lines.insert(l);
        }
        for m in &t.methods {
            for l in m.start_line..=m.end_line {
                artifact_lines.insert(l);
            }
        }
    }
    for f in &spans.module_functions {
        for l in f.start_line..=f.end_line {
            artifact_lines.insert(l);
        }
    }
    for c in chunks {
        for l in c.start_line..=c.end_line {
            assert!(!artifact_lines.contains(&l), "fallback line {l} overlaps an artifact");
        }
    }

    // Idempotent: re-running the pass yields identical chunks.
    let before = ext.fallback_chunks.clone();
    ext.populate_fallback_chunks(&source);
    assert_eq!(ext.fallback_chunks, before);
}

#[test]
fn fallback_chunks_split_at_max_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("long_script.py");
    fs::write(&path, PYTHON_SAMPLE).unwrap();
    let source = fs::read_to_string(&path).unwrap();

    let mut ext = parse(&path, &source);
    ext.populate_fallback_chunks_with_max(&source, 2);

    // Run [1..6] splits into [1..2],[3..4],[5..6]; the trailing run is
    // [20..22] (blank 20 rides the __main__ guard run) and splits into
    // [20..21],[22..22].
    let ranges: Vec<(u32, u32)> = ext
        .fallback_chunks
        .iter()
        .map(|c| (c.start_line, c.end_line))
        .collect();
    assert_eq!(ranges, vec![(1, 2), (3, 4), (5, 6), (20, 21), (22, 22)]);
}

#[test]
fn pure_script_file_is_one_fallback_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pure_script.py");
    let src = "import sys\n\nx = compute(sys.argv[1])\nprint(x)\n";
    fs::write(&path, src).unwrap();

    let mut ext = parse(&path, src);
    assert!(ext.types.is_empty() && ext.module_functions.is_empty());
    ext.populate_fallback_chunks(src);

    assert_eq!(ext.fallback_chunks.len(), 1);
    let c = &ext.fallback_chunks[0];
    assert_eq!((c.start_line, c.end_line), (1, 4));
    assert_eq!(c.text(src), "import sys\n\nx = compute(sys.argv[1])\nprint(x)");
}

#[test]
fn whitespace_only_source_yields_no_fallback_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blank.py");
    let src = "\n\n   \n\t\n";
    fs::write(&path, src).unwrap();

    let mut ext = parse(&path, src);
    ext.populate_fallback_chunks(src);
    assert!(ext.fallback_chunks.is_empty());

    // Empty source: no chunks, no crash.
    let path2 = dir.path().join("empty.py");
    fs::write(&path2, "").unwrap();
    let mut ext2 = parse(&path2, "");
    ext2.populate_fallback_chunks("");
    assert!(ext2.fallback_chunks.is_empty());
}
