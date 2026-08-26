//! Language-neutral structural extraction types shared by every extractor.
//!
//! All per-language extractors (Java, Python, JS/TS, Go, Rust, heuristic)
//! produce a [`SourceExtraction`], so the ingestion pipeline in
//! `parse::ingest_project` is language-agnostic. The Java-specific types
//! (`JavaExtraction`, …) are kept as aliases in `parse::java` for
//! backward compatibility.

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
