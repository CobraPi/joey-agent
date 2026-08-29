//! Language-neutral structural extraction types shared by every extractor.
//!
//! All per-language extractors (Java, Python, JS/TS, Go, Rust, heuristic)
//! produce a [`SourceExtraction`], so the ingestion pipeline in
//! `parse::ingest_project` is language-agnostic. The Java-specific types
//! (`JavaExtraction`, …) are kept as aliases in `parse::java` for
//! backward compatibility.
//!
//! # Line spans & fallback chunks (spec 021, T003/T004)
//!
//! Extractors emit BYTE spans (`start_byte`/`end_byte`) because
//! tree-sitter positions are byte-based. The derived 1-based line view
//! ([`SourceExtraction::line_spans`]) and the coarse fallback chunks for
//! regions with no named artifacts ([`SourceExtraction::populate_fallback_chunks`])
//! are computed against the source text at index time — additive surfaces;
//! the extractors and the graph ingestion pipeline are unchanged.

use super::spans::LineIndex;

/// Extracted structural metadata from one source file (any language).
#[derive(Debug, Clone, Default)]
pub struct SourceExtraction {
    /// The language id that produced this extraction ("java", "python", …).
    pub language: String,
    /// The enclosing package/module/namespace, dotted (`""` when none).
    pub package: String,
    /// Imported modules/paths as written (`com.foo.Bar`, `./utils`, …).
    pub imports: Vec<String>,
    /// Type-level declarations (class / interface / enum / struct / trait).
    pub types: Vec<ExtractedType>,
    /// File/module-level functions (no enclosing type — Python, JS/TS, Rust).
    pub module_functions: Vec<ExtractedMethod>,
    /// Coarse chunks covering source regions that contain no named
    /// artifact (scripts / top-level code, FR-014 groundwork). Populated
    /// by [`SourceExtraction::populate_fallback_chunks`] — empty until
    /// that pass runs (extractors themselves never fill it).
    pub fallback_chunks: Vec<FallbackChunk>,
}

impl SourceExtraction {
    /// The fully-qualified name for a type in this extraction.
    ///
    /// Uses the type's explicit `fq_name` when the extractor set one
    /// (e.g. Rust `crate::mod::Type`), else `package.name` joined with `.`.
    pub fn fq_name(&self, type_node: &ExtractedType) -> String {
        if let Some(fq) = &type_node.fq_name {
            return fq.clone();
        }
        if type_node.package.is_empty() {
            type_node.name.clone()
        } else {
            format!("{}.{}", type_node.package, type_node.name)
        }
    }

    /// Compute the 1-based line spans of every named artifact (and the
    /// already-populated fallback chunks) against `source` — the
    /// byte→line conversion happens here, at index time (T003;
    /// data-model.md § CodeChunk `start_line`/`end_line`).
    ///
    /// Purely derived: nothing is cached or mutated; callers that index
    /// many files hold the [`SourceExtraction`] and the source text and
    /// call this once per file.
    pub fn line_spans(&self, source: &str) -> ExtractionLineSpans {
        let index = LineIndex::new(source);
        ExtractionLineSpans {
            types: self
                .types
                .iter()
                .map(|t| TypeLineSpan {
                    name: t.name.clone(),
                    kind: t.kind_class().to_string(),
                    start_line: index.span(t.start_byte, t.end_byte).start_line,
                    end_line: index.span(t.start_byte, t.end_byte).end_line,
                    methods: t
                        .methods
                        .iter()
                        .map(|m| SymbolLineSpan {
                            name: m.name.clone(),
                            kind: "method".to_string(),
                            start_line: index.span(m.start_byte, m.end_byte).start_line,
                            end_line: index.span(m.start_byte, m.end_byte).end_line,
                        })
                        .collect(),
                })
                .collect(),
            module_functions: self
                .module_functions
                .iter()
                .map(|f| SymbolLineSpan {
                    name: f.name.clone(),
                    kind: "method".to_string(),
                    start_line: index.span(f.start_byte, f.end_byte).start_line,
                    end_line: index.span(f.start_byte, f.end_byte).end_line,
                })
                .collect(),
            fallback_chunks: self.fallback_chunks.clone(),
        }
    }

    /// Populate [`SourceExtraction::fallback_chunks`] with coarse chunks
    /// covering the source regions that contain no named artifact —
    /// scripts and top-level code outside any class/function (T004,
    /// FR-014 groundwork; clarification Q1 hybrid chunking).
    ///
    /// Chunks are line-aligned: any line touched by a type or
    /// module-function byte span is excluded entirely, so a fallback
    /// chunk never overlaps a symbol-aligned chunk. Contiguous uncovered
    /// line runs containing at least one non-whitespace line become one
    /// chunk, split into consecutive line ranges of at most
    /// [`DEFAULT_FALLBACK_CHUNK_MAX_LINES`] lines each.
    ///
    /// Idempotent: recomputes from scratch (calling it twice yields the
    /// same chunks). Byte and line spans are both recorded so consumers
    /// without the source text can still locate the chunk.
    pub fn populate_fallback_chunks(&mut self, source: &str) {
        self.populate_fallback_chunks_with_max(source, DEFAULT_FALLBACK_CHUNK_MAX_LINES);
    }

    /// [`SourceExtraction::populate_fallback_chunks`] with an explicit
    /// maximum chunk length in lines (`max_lines` ≥ 1 is enforced by
    /// clamping up to 1).
    pub fn populate_fallback_chunks_with_max(&mut self, source: &str, max_lines: usize) {
        let max_lines = max_lines.max(1);
        let index = LineIndex::new(source);
        let total_lines = index.line_count();

        // Lines covered by any named artifact (byte span → every line it
        // touches — line alignment prevents symbol/fallback overlap even
        // when a span starts or ends mid-line).
        let mut covered = vec![false; (total_lines as usize) + 1]; // 1-based, index 0 unused
        let mut mark = |start_byte: u32, end_byte: u32| {
            let span = index.span(start_byte, end_byte);
            for line in span.start_line..=span.end_line {
                if (line as usize) < covered.len() {
                    covered[line as usize] = true;
                }
            }
        };
        for t in &self.types {
            mark(t.start_byte, t.end_byte);
        }
        for f in &self.module_functions {
            mark(f.start_byte, f.end_byte);
        }

        // Maximal runs of uncovered, non-blank-containing lines → chunks.
        let mut chunks: Vec<FallbackChunk> = Vec::new();
        let mut run_start: Option<u32> = None;
        let mut run_has_content = false;
        for line in 1..=total_lines {
            if covered[line as usize] {
                if let Some(start) = run_start.take() {
                    if run_has_content {
                        push_chunks(&index, start, line - 1, max_lines, &mut chunks);
                    }
                    run_has_content = false;
                }
                continue;
            }
            if run_start.is_none() {
                run_start = Some(line);
                run_has_content = false;
            }
            if !index.line_text(source, line).trim().is_empty() {
                run_has_content = true;
            }
        }
        if let Some(start) = run_start {
            if run_has_content {
                push_chunks(&index, start, total_lines, max_lines, &mut chunks);
            }
        }

        self.fallback_chunks = chunks;
    }
}

/// Split the uncovered line run `[start_line, end_line]` into consecutive
/// [`FallbackChunk`]s of at most `max_lines` lines each.
fn push_chunks(
    index: &LineIndex,
    start_line: u32,
    end_line: u32,
    max_lines: usize,
    out: &mut Vec<FallbackChunk>,
) {
    let mut first = start_line;
    while first <= end_line {
        let last = (first + max_lines as u32 - 1).min(end_line);
        out.push(FallbackChunk {
            start_byte: index.line_start_byte(first),
            end_byte: index.line_end_byte(last),
            start_line: first,
            end_line: last,
        });
        first = last + 1;
    }
}

/// Default cap on fallback-chunk length in lines. Coarse by design —
/// these chunks cover top-level scripts that have no symbol to align to;
/// 200 lines keeps them in the same order of magnitude as the RAG
/// chunk-size band (research.md R4: 256–1024 tokens).
pub const DEFAULT_FALLBACK_CHUNK_MAX_LINES: usize = 200;

/// A coarse chunk covering a source region that contains no named
/// artifact (data-model.md § CodeChunk `FallbackCoarse`: no symbol
/// identity — file + line-range identity only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackChunk {
    /// Start of the chunk (byte offset into the source, line start).
    pub start_byte: u32,
    /// End of the chunk (byte offset, exclusive of the terminating
    /// newline of its last line).
    pub end_byte: u32,
    /// First line of the chunk (1-based, inclusive).
    pub start_line: u32,
    /// Last line of the chunk (1-based, inclusive).
    pub end_line: u32,
}

impl FallbackChunk {
    /// The chunk's text from `source` (its lines, without separators).
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        let s = self.start_byte as usize;
        let e = self.end_byte as usize;
        if s >= e || e > source.len() {
            ""
        } else {
            &source[s..e]
        }
    }
}

/// 1-based line range of one named artifact — a symbol-aligned chunk
/// candidate (data-model.md § CodeChunk `SymbolAligned`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolLineSpan {
    /// Artifact name (class / interface / enum / function / method).
    pub name: String,
    /// `"class"` | `"interface"` | `"enum"` | `"method"`.
    pub kind: String,
    /// First line (1-based, inclusive).
    pub start_line: u32,
    /// Last line (1-based, inclusive).
    pub end_line: u32,
}

/// Line spans for a type declaration and its methods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeLineSpan {
    pub name: String,
    /// `"class"` | `"interface"` | `"enum"`.
    pub kind: String,
    pub start_line: u32,
    pub end_line: u32,
    /// Methods of the type, in declaration order.
    pub methods: Vec<SymbolLineSpan>,
}

/// The complete derived line-span view of a [`SourceExtraction`]
/// ([`SourceExtraction::line_spans`]): every named artifact plus the
/// fallback chunks (when populated), all as 1-based inclusive ranges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtractionLineSpans {
    /// Type declarations (methods nested inside).
    pub types: Vec<TypeLineSpan>,
    /// File/module-level functions.
    pub module_functions: Vec<SymbolLineSpan>,
    /// Coarse fallback chunks ([`SourceExtraction::populate_fallback_chunks`]).
    pub fallback_chunks: Vec<FallbackChunk>,
}

/// A type-level declaration (class, interface, enum, struct, or trait).
#[derive(Debug, Clone)]
pub struct ExtractedType {
    pub name: String,
    /// One of "class" | "interface" | "enum" (structs map to "class",
    /// traits map to "interface").
    pub kind: String,
    pub package: String,
    /// Explicit fully-qualified name when the language provides one
    /// (e.g. Rust `crate::foo::Bar`). `None` → derive from package + name.
    pub fq_name: Option<String>,
    /// Base types: implemented interfaces (Java/TS), base classes
    /// (Python/TS extends), traits implemented by a struct (Rust `impl`),
    /// embedded interfaces (Go).
    pub implemented_interfaces: Vec<String>,
    /// Framework annotations/declarations (`Service`, `staticmethod`,
    /// `derive`, decorators, …).
    pub annotations: Vec<String>,
    /// Injected/declared dependency names (constructor-injected fields,
    /// struct field types, imported local symbols).
    pub declared_dependencies: Vec<String>,
    pub methods: Vec<ExtractedMethod>,
    pub fields: Vec<ExtractedField>,
    pub start_byte: u32,
    pub end_byte: u32,
}

impl ExtractedType {
    /// Map the string kind onto the shared `ArtifactKind` notion
    /// ("interface" | "enum" | "class").
    pub fn kind_class(&self) -> &'static str {
        match self.kind.as_str() {
            "interface" => "interface",
            "enum" => "enum",
            _ => "class",
        }
    }
}

/// A method/function declaration.
#[derive(Debug, Clone)]
pub struct ExtractedMethod {
    pub name: String,
    pub annotations: Vec<String>,
    /// Declaration header: the source text from the start of modifiers to
    /// the parameter-list close (e.g. `public User findById(Long id)`).
    /// Rendered verbatim in the assembled context so the model sees real
    /// parameter names and types without opening the file.
    pub signature: Option<String>,
    pub start_byte: u32,
    pub end_byte: u32,
}

/// A field declaration.
#[derive(Debug, Clone)]
pub struct ExtractedField {
    pub name: String,
    pub type_name: String,
    pub annotations: Vec<String>,
    /// Full declaration text including annotations
    /// (e.g. `@Autowired private UserRepository userRepository`).
    pub signature: Option<String>,
}
