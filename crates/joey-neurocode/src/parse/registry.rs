//! The language registry: maps file extensions to extractors.
//!
//! Each entry provides (language id, extensions, parser fn). Extraction is
//! language-agnostic once an extractor produces a
//! [`super::extract::SourceExtraction`]. Extensions without a dedicated
//! extractor fall back to [`super::heuristic::parse_heuristic_file`] so
//! every programming language gets at least coarse structural nodes.

use std::path::Path;

use super::extract::SourceExtraction;

/// A registered language: id, recognized extensions, extractor.
pub struct LanguageSpec {
    /// Stable language id ("java", "python", "typescript", …).
    pub id: &'static str,
    /// File extensions (lowercase, no dot) this language owns.
    pub extensions: &'static [&'static str],
    /// Extract structural metadata from source text.
    pub extract: fn(&str) -> Result<SourceExtraction, String>,
}

/// The compiled-in languages, in priority order (first match wins for a
/// given extension). Covers every programming language with a grammar under
/// the tree-sitter org (https://tree-sitter.github.io/tree-sitter/):
/// Agda, Bash, C, C++, C#, Go, Haskell, Java, JavaScript, Julia, OCaml,
/// PHP, Python, Ruby, Rust, Scala, TypeScript/TSX, Verilog. Markup/data
/// grammars (CSS, HTML, JSON, JSDoc, Regex, embedded-template) are out of
/// scope for the structural dependency graph — see `grammars.rs`.
pub fn languages() -> &'static [LanguageSpec] {
    &[
        LanguageSpec {
            id: "java",
            extensions: &["java"],
            extract: super::java::parse_java_file,
        },
        LanguageSpec {
            id: "python",
            extensions: &["py", "pyi"],
            extract: super::python::parse_python_file,
        },
        LanguageSpec {
            id: "typescript",
            extensions: &["ts", "mts", "cts"],
            extract: super::jsts::parse_typescript_file,
        },
        LanguageSpec {
            id: "tsx",
            extensions: &["tsx"],
            extract: super::jsts::parse_tsx_file,
        },
        LanguageSpec {
            id: "javascript",
            extensions: &["js", "mjs", "cjs", "jsx"],
            extract: super::jsts::parse_javascript_file,
        },
        LanguageSpec {
            id: "go",
            extensions: &["go"],
            extract: super::golang::parse_go_file,
        },
        LanguageSpec {
            id: "rust",
            extensions: &["rs"],
            extract: super::rustlang::parse_rust_file,
        },
        LanguageSpec {
            id: "ruby",
            extensions: &["rb"],
            extract: super::grammars::parse_ruby_file,
        },
        LanguageSpec {
            id: "php",
            extensions: &["php"],
            extract: super::grammars::parse_php_file,
        },
        LanguageSpec {
            id: "csharp",
            extensions: &["cs"],
            extract: super::grammars::parse_csharp_file,
        },
        LanguageSpec {
            id: "cpp",
            extensions: &["cpp", "cc", "cxx", "hpp", "hh", "cppm", "cxxm"],
            extract: super::grammars::parse_cpp_file,
        },
        LanguageSpec {
            id: "c",
            extensions: &["c", "h"],
            extract: super::grammars::parse_c_file,
        },
        LanguageSpec {
            id: "scala",
            extensions: &["scala"],
            extract: super::grammars::parse_scala_file,
        },
        LanguageSpec {
            id: "haskell",
            extensions: &["hs"],
            extract: super::grammars::parse_haskell_file,
        },
        LanguageSpec {
            id: "julia",
            extensions: &["jl"],
            extract: super::grammars::parse_julia_file,
        },
        LanguageSpec {
            id: "ocaml",
            extensions: &["ml"],
            extract: super::grammars::parse_ocaml_file,
        },
        LanguageSpec {
            id: "bash",
            extensions: &["sh", "bash", "zsh"],
            extract: super::grammars::parse_bash_file,
        },
        LanguageSpec {
            id: "verilog",
            extensions: &["v", "vh", "sv", "svh"],
            extract: super::grammars::parse_verilog_file,
        },
        LanguageSpec {
            id: "agda",
            extensions: &["agda"],
            extract: super::grammars::parse_agda_file,
        },
    ]
}

/// The set of extensions recognized as source files worth ingesting —
/// either via a dedicated grammar or the heuristic fallback.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    // dedicated grammars (tree-sitter supported languages)
    "java", "py", "pyi", "ts", "mts", "cts", "tsx", "js", "mjs", "cjs", "jsx", "go", "rs", "rb",
    "php", "cs", "cpp", "cc", "cxx", "hpp", "hh", "cppm", "cxxm", "c", "h", "scala", "hs", "jl",
    "ml", "sh", "bash", "zsh", "v", "vh", "sv", "svh", "agda",
    // heuristic fallback (long-tail languages without a compiled grammar)
    "kt", "kts", "swift", "mm", "m", "ex", "exs", "lua", "pl", "r", "sc", "scm", "lisp", "clj",
    "cljs", "erl", "hrl", "cr", "nim", "zig", "sol", "vue", "svelte", "groovy", "dart",
];

/// Whether `ext` (lowercase, no dot) is a supported source extension.
pub fn is_supported_extension(ext: &str) -> bool {
    SUPPORTED_EXTENSIONS.contains(&ext)
}

/// Resolve the extractor for a file path. Returns `None` for unsupported
/// extensions.
pub fn extractor_for_path(path: &Path) -> Option<&'static LanguageSpec> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    languages().iter().find(|l| l.extensions.contains(&ext.as_str()))
}

/// Parse a source file with the right extractor (dedicated grammar first,
/// heuristic fallback for known-but-uncompiled languages). Returns `None`
/// when the extension is not a supported source file.
pub fn parse_any(path: &Path, source: &str) -> Option<Result<SourceExtraction, String>> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    if let Some(spec) = extractor_for_path(path) {
        return Some((spec.extract)(source));
    }
    if is_supported_extension(&ext) {
        return Some(super::heuristic::parse_heuristic_file(source, &ext));
    }
    None
}
