//! Dedicated tree-sitter extractors for the additional supported languages
//! from https://tree-sitter.github.io/tree-sitter/ — Bash, C, C++, C#,
//! Haskell, Julia, OCaml, PHP, Ruby, Scala, Verilog, Agda.
//!
//! Node kinds used below were verified against the actual grammar crates
//! (parse-tree dumps), not guessed — see the per-language tests.
//!
//! Markup/data grammars on the supported list (CSS, HTML, JSON, JSDoc,
//! Regex, embedded-template/ERB) are intentionally NOT compiled in: they
//! produce no type/method/import structure for the dependency graph, so a
//! dependency would violate the constitution's Principle VIII (every
//! dependency must be justified by a near-term feature).

use tree_sitter::{Node, Parser, Tree};

use super::extract::{ExtractedField, ExtractedMethod, ExtractedType, SourceExtraction};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Run the parser for `language` over `source`.
fn parse_tree(language: tree_sitter::Language, source: &str) -> Result<Tree, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .map_err(|e| format!("language error: {}", e))?;
    parser.parse(source, None).ok_or_else(|| "failed to parse".to_string())
}

/// Trimmed source text of a node.
fn txt<'a>(node: &Node<'a>, source: &str) -> String {
    node.utf8_text(source.as_bytes())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Named children of a node, in order.
fn named_children<'a>(node: &Node<'a>) -> Vec<Node<'a>> {
    (0..node.named_child_count())
        .filter_map(|i| node.named_child(i as u32))
        .collect()
}

/// First named descendant (depth-first) with the given kind.
fn find_first<'a>(node: &Node<'a>, kind: &str) -> Option<Node<'a>> {
    if node.kind() == kind {
        return Some(*node);
    }
    for child in named_children(node) {
        if let Some(found) = find_first(&child, kind) {
            return Some(found);
        }
    }
    None
}

/// All named descendants (excluding `node` itself) with the given kind.
fn descendants_of_kind<'a>(node: &Node<'a>, kind: &str, out: &mut Vec<Node<'a>>) {
    for child in named_children(node) {
        if child.kind() == kind {
            out.push(child);
        }
        descendants_of_kind(&child, kind, out);
    }
}

fn method_node(name: String, node: &Node) -> ExtractedMethod {
    ExtractedMethod {
        name,
        annotations: Vec::new(),
        signature: None,
        start_byte: node.start_byte() as u32,
        end_byte: node.end_byte() as u32,
    }
}

fn empty_type(name: String, kind: &str, node: &Node) -> ExtractedType {
    ExtractedType {
        name,
        kind: kind.to_string(),
        package: String::new(),
        fq_name: None,
        implemented_interfaces: Vec::new(),
        annotations: Vec::new(),
        declared_dependencies: Vec::new(),
        methods: Vec::new(),
        fields: Vec::new(),
        start_byte: node.start_byte() as u32,
        end_byte: node.end_byte() as u32,
    }
}

// ---------------------------------------------------------------------------
// Ruby
// ---------------------------------------------------------------------------

/// Parse Ruby source: classes/modules (with superclass), methods,
/// `require`/`require_relative`/`load` imports.
pub fn parse_ruby_file(source: &str) -> Result<SourceExtraction, String> {
    let tree = parse_tree(tree_sitter_ruby::LANGUAGE.into(), source)?;
    let root = tree.root_node();

    let mut extraction = SourceExtraction {
        language: "ruby".into(),
        ..Default::default()
    };

    // Imports: `require "x"` / `require_relative 'x'` / `load "x"` — call
    // nodes whose callee (field `method`) is one of those kernels, with a
    // string argument.
    for call in {
        let mut v = Vec::new();
        descendants_of_kind(&root, "call", &mut v);
        v
    } {
        let callee = call
            .child_by_field_name("method")
            .map(|n| txt(&n, source))
            .unwrap_or_default();
        if !matches!(
            callee.as_str(),
            "require" | "require_relative" | "load"
        ) {
            continue;
        }
        if let Some(arg) = find_first(&call, "string") {
            let imported = txt(&arg, source).trim_matches('\'').trim_matches('"').to_string();
            if !imported.is_empty() && !extraction.imports.contains(&imported) {
                extraction.imports.push(imported);
            }
        }
    }

    walk_ruby(&root, source, "", &mut extraction);
    Ok(extraction)
}

/// Recursive Ruby walk: classes/modules become types; `def` inside a
/// class/module body becomes a method of the innermost enclosing type.
fn walk_ruby<'a>(node: &Node<'a>, source: &str, package: &str, extraction: &mut SourceExtraction) {
    for child in named_children(node) {
        match child.kind() {
            "class" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| txt(&n, source))
                    .unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                let mut ty = empty_type(name, "class", &child);
                ty.package = package.to_string();
                if let Some(sup) = find_first(&child, "superclass") {
                    if let Some(base) = find_first(&sup, "constant") {
                        let base = txt(&base, source);
                        if !base.is_empty() {
                            ty.implemented_interfaces.push(base);
                        }
                    }
                }
                collect_ruby_methods(&child, source, &mut ty);
                // Nested classes/modules inside this class body.
                walk_ruby_types_only(&child, source, package, extraction);
                extraction.types.push(ty);
            }
            "module" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| txt(&n, source))
                    .unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                let nested = if package.is_empty() {
                    name.clone()
                } else {
                    format!("{}.{}", package, name)
                };
                let mut ty = empty_type(name.clone(), "class", &child);
                ty.package = package.to_string();
                collect_ruby_methods(&child, source, &mut ty);
                walk_ruby_types_only(&child, source, &nested, extraction);
                extraction.types.push(ty);
            }
            _ => walk_ruby(&child, source, package, extraction),
        }
    }
}

/// Collect direct `method` (def) nodes inside a class/module body — the
/// innermost enclosing type owns them, so nested classes' methods are NOT
/// included (they are handled by their own class walk).
fn collect_ruby_methods<'a>(body: &Node<'a>, source: &str, ty: &mut ExtractedType) {
    // Descend only through body_statement containers, not into nested
    // class/module/singleton bodies.
    for child in named_children(body) {
        match child.kind() {
            "method" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = txt(&name_node, source);
                    if !name.is_empty() {
                        ty.methods.push(method_node(name, &child));
                    }
                }
            }
            "body_statement" => collect_ruby_methods(&child, source, ty),
            _ => {}
        }
    }
}

/// Walk for nested class/module declarations only (methods already collected
/// by `collect_ruby_methods` for the enclosing type).
fn walk_ruby_types_only<'a>(
    node: &Node<'a>,
    source: &str,
    package: &str,
    extraction: &mut SourceExtraction,
) {
    for child in named_children(node) {
        match child.kind() {
            "class" | "module" => walk_ruby(&child, source, package, extraction),
            "body_statement" => walk_ruby_types_only(&child, source, package, extraction),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// PHP
// ---------------------------------------------------------------------------

/// Parse PHP source: namespace, `use` imports, classes/interfaces/traits/
/// enums with methods and typed properties, top-level functions.
pub fn parse_php_file(source: &str) -> Result<SourceExtraction, String> {
    let tree = parse_tree(tree_sitter_php::LANGUAGE_PHP.into(), source)?;
    let root = tree.root_node();
    let mut extraction = SourceExtraction {
        language: "php".into(),
        ..Default::default()
    };

    for node in named_children(&root) {
        match node.kind() {
            "namespace_definition" => {
                if let Some(name) = node.child_by_field_name("name") {
                    extraction.package = txt(&name, source);
                }
                walk_php_body(&node, source, &mut extraction);
            }
            "namespace_use_declaration" => {
                for clause in named_children(&node) {
                    let imported = txt(&clause, source);
                    if !imported.is_empty() && !extraction.imports.contains(&imported) {
                        extraction.imports.push(imported);
                    }
                }
            }
            _ => walk_php_toplevel(&node, source, &mut extraction),
        }
    }
    Ok(extraction)
}

fn walk_php_toplevel<'a>(node: &Node<'a>, source: &str, extraction: &mut SourceExtraction) {
    match node.kind() {
        "class_declaration" | "interface_declaration" | "trait_declaration" | "enum_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let name = txt(&name_node, source);
            if name.is_empty() {
                return;
            }
            let kind = match node.kind() {
                "interface_declaration" => "interface",
                "enum_declaration" => "enum",
                _ => "class",
            };
            let mut ty = empty_type(name, kind, node);
            ty.package = extraction.package.clone();
            // extends (base_clause) + implements (class_interface_clause).
            for child in named_children(node) {
                match child.kind() {
                    "base_clause" | "class_interface_clause" => {
                        for base in named_children(&child) {
                            let b = txt(&base, source);
                            if !b.is_empty() {
                                ty.implemented_interfaces.push(b);
                            }
                        }
                    }
                    "declaration_list" => {
                        for member in named_children(&child) {
                            match member.kind() {
                                "method_declaration" => {
                                    if let Some(mn) = member.child_by_field_name("name") {
                                        let m = txt(&mn, source);
                                        if !m.is_empty() {
                                            ty.methods.push(method_node(m, &member));
                                        }
                                    }
                                }
                                "property_declaration" => {
                                    // The type is a `named_type` child of
                                    // the declaration (sibling of each
                                    // property_element).
                                    let type_name = find_first(&member, "named_type")
                                        .map(|t| txt(&t, source))
                                        .unwrap_or_default();
                                    for prop in named_children(&member) {
                                        if prop.kind() != "property_element" {
                                            continue;
                                        }
                                        if let Some(vn) = find_first(&prop, "variable_name") {
                                            let fname =
                                                txt(&vn, source).trim_start_matches('$').to_string();
                                            if !fname.is_empty() {
                                                ty.fields.push(ExtractedField {
                                                    name: fname,
                                                    type_name: type_name.clone(),
                                                    annotations: Vec::new(),
                                                    signature: None,
                                                });
                                                if !type_name.is_empty() {
                                                    ty.declared_dependencies.push(type_name.clone());
                                                }
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            extraction.types.push(ty);
        }
        "function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = txt(&name_node, source);
                if !name.is_empty() {
                    extraction.module_functions.push(method_node(name, node));
                }
            }
        }
        _ => {}
    }
}

fn walk_php_body<'a>(node: &Node<'a>, source: &str, extraction: &mut SourceExtraction) {
    for child in named_children(node) {
        walk_php_toplevel(&child, source, extraction);
        walk_php_body(&child, source, extraction);
    }
}

// ---------------------------------------------------------------------------
// C#
// ---------------------------------------------------------------------------

/// Parse C# source: usings, (file-scoped or block) namespaces, classes/
/// interfaces/records/structs/enums with methods and fields.
pub fn parse_csharp_file(source: &str) -> Result<SourceExtraction, String> {
    let tree = parse_tree(tree_sitter_c_sharp::LANGUAGE.into(), source)?;
    let root = tree.root_node();
    let mut extraction = SourceExtraction {
        language: "csharp".into(),
        ..Default::default()
    };

    for node in named_children(&root) {
        match node.kind() {
            "using_directive" => {
                let imported = txt(&node, source)
                    .trim_start_matches("using ")
                    .trim_end_matches(';')
                    .trim()
                    .to_string();
                if !imported.is_empty() && !extraction.imports.contains(&imported) {
                    extraction.imports.push(imported);
                }
            }
            "file_scoped_namespace_declaration" | "namespace_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    extraction.package = txt(&name, source);
                }
                // The namespace's members are its named children — walk them
                // through the type extractor directly.
                for member in named_children(&node) {
                    walk_csharp(&member, source, &mut extraction);
                }
            }
            _ => {
                // Class/interface/struct/record/enum declarations at any
                // depth (top level or inside a namespace).
                walk_csharp_types(&node, source, &mut extraction);
            }
        }
    }
    Ok(extraction)
}

/// Dispatch one node: containers recurse, type declarations extract.
fn walk_csharp<'a>(node: &Node<'a>, source: &str, extraction: &mut SourceExtraction) {
    match node.kind() {
        "namespace_declaration" | "file_scoped_namespace_declaration" => {
            for child in named_children(node) {
                walk_csharp(&child, source, extraction);
            }
        }
        _ => walk_csharp_types(node, source, extraction),
    }
}

/// Extract type declarations from `node` (and recurse into non-type
/// containers so nested declarations at any depth are found).
fn walk_csharp_types<'a>(node: &Node<'a>, source: &str, extraction: &mut SourceExtraction) {
    match node.kind() {
        "class_declaration" | "interface_declaration" | "struct_declaration"
        | "record_declaration" | "record_struct_declaration" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let name = txt(&name_node, source);
            if name.is_empty() {
                return;
            }
            let kind = if node.kind() == "interface_declaration" {
                "interface"
            } else {
                "class"
            };
            let mut ty = empty_type(name, kind, node);
            ty.package = extraction.package.clone();
            for sub in named_children(node) {
                match sub.kind() {
                    "base_list" => {
                        for base in named_children(&sub) {
                            let b = txt(&base, source);
                            if !b.is_empty() {
                                ty.implemented_interfaces.push(b);
                            }
                        }
                    }
                    "declaration_list" => collect_csharp_members(&sub, source, &mut ty),
                    _ => {}
                }
            }
            extraction.types.push(ty);
        }
        "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = txt(&name_node, source);
                if !name.is_empty() {
                    let mut ty = empty_type(name, "enum", node);
                    ty.package = extraction.package.clone();
                    extraction.types.push(ty);
                }
            }
        }
        _ => {
            // Non-type node (e.g. using_directive already handled at top
            // level): recurse to find nested declarations.
            for child in named_children(node) {
                walk_csharp_types(&child, source, extraction);
            }
        }
    }
}

fn collect_csharp_members<'a>(list: &Node<'a>, source: &str, ty: &mut ExtractedType) {
    for member in named_children(list) {
        match member.kind() {
            "method_declaration" | "constructor_declaration" => {
                if let Some(mn) = member.child_by_field_name("name") {
                    let m = txt(&mn, source);
                    if !m.is_empty() {
                        ty.methods.push(method_node(m, &member));
                    }
                }
            }
            "field_declaration" => {
                // field_declaration → variable_declaration →
                // variable_declarator(name); the type is variable_declaration's
                // type child.
                for var in named_children(&member) {
                    if var.kind() != "variable_declaration" {
                        continue;
                    }
                    let type_name = var
                        .child_by_field_name("type")
                        .map(|t| txt(&t, source))
                        .unwrap_or_default();
                    for decl in named_children(&var) {
                        if decl.kind() != "variable_declarator" {
                            continue;
                        }
                        if let Some(vn) = decl.child_by_field_name("name") {
                            let fname = txt(&vn, source);
                            if !fname.is_empty() {
                                ty.fields.push(ExtractedField {
                                    name: fname,
                                    type_name: type_name.clone(),
                                    annotations: Vec::new(),
                                    signature: None,
                                });
                            }
                        }
                    }
                    if !type_name.is_empty() {
                        ty.declared_dependencies.push(type_name);
                    }
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// C / C++
// ---------------------------------------------------------------------------

/// Parse C source: #include imports, struct/union/enum specifiers (typedefs
/// name anonymous structs), function definitions.
pub fn parse_c_file(source: &str) -> Result<SourceExtraction, String> {
    let tree = parse_tree(tree_sitter_c::LANGUAGE.into(), source)?;
    let mut extraction = SourceExtraction {
        language: "c".into(),
        ..Default::default()
    };
    for child in named_children(&tree.root_node()) {
        walk_c(&child, source, &mut extraction);
    }
    Ok(extraction)
}

/// Parse C++ source: additionally namespaces, classes with base clauses,
/// templates (descended into).
pub fn parse_cpp_file(source: &str) -> Result<SourceExtraction, String> {
    let tree = parse_tree(tree_sitter_cpp::LANGUAGE.into(), source)?;
    let mut extraction = SourceExtraction {
        language: "cpp".into(),
        ..Default::default()
    };
    walk_cpp(&tree.root_node(), source, "", &mut extraction);
    Ok(extraction)
}

fn walk_c<'a>(node: &Node<'a>, source: &str, extraction: &mut SourceExtraction) {
    match node.kind() {
        "preproc_include" => {
            let imported = txt(node, source)
                .trim_start_matches("#include")
                .trim()
                .trim_matches('"')
                .trim_matches('<')
                .trim_matches('>')
                .to_string();
            if !imported.is_empty() && !extraction.imports.contains(&imported) {
                extraction.imports.push(imported);
            }
        }
        "struct_specifier" | "union_specifier" | "enum_specifier" => {
            c_type(node, source, extraction, "c");
        }
        "type_definition" => c_typedef(node, source, extraction),
        "function_definition" => {
            if let Some(declarator) = node.child_by_field_name("declarator") {
                if let Some(fn_decl) = find_first(&declarator, "function_declarator") {
                    if let Some(id) = fn_decl.child_by_field_name("declarator") {
                        let name = txt(&id, source);
                        if !name.is_empty() {
                            extraction.module_functions.push(method_node(name, node));
                        }
                    }
                }
            }
        }
        "declaration" => {
            // Free function declarations (no body): `int foo(int);`
            if let Some(declarator) = node.child_by_field_name("declarator") {
                if let Some(fn_decl) = find_first(&declarator, "function_declarator") {
                    if let Some(id) = fn_decl.child_by_field_name("declarator") {
                        let name = txt(&id, source);
                        if !name.is_empty() && id.kind() == "identifier" {
                            extraction.module_functions.push(method_node(name, node));
                        }
                    }
                }
            }
        }
        _ => {
            for child in named_children(node) {
                walk_c(&child, source, extraction);
            }
        }
    }
}

/// A C struct/union/enum specifier → type node.
fn c_type<'a>(spec: &Node<'a>, source: &str, extraction: &mut SourceExtraction, lang: &str) {
    let name = spec
        .child_by_field_name("name")
        .map(|n| txt(&n, source))
        .unwrap_or_default();
    if name.is_empty() {
        return;
    }
    let kind = match spec.kind() {
        "enum_specifier" => "enum",
        _ => "class",
    };
    let mut ty = empty_type(name, kind, spec);
    ty.package = extraction.package.clone();
    // Fields: field_declaration_list → field_declaration, name is the
    // trailing field_identifier.
    for sub in named_children(spec) {
        if sub.kind() == "field_declaration_list" {
            collect_c_fields(&sub, source, &mut ty);
        }
    }
    let _ = lang;
    extraction.types.push(ty);
}

/// `typedef struct { … } Name;` — the specifier is anonymous; use the
/// typedef's trailing type_identifier as the type name.
fn c_typedef<'a>(node: &Node<'a>, source: &str, extraction: &mut SourceExtraction) {
    let spec = named_children(node)
        .into_iter()
        .find(|c| matches!(c.kind(), "struct_specifier" | "union_specifier" | "enum_specifier"));
    let Some(spec) = spec else {
        return;
    };
    // Anonymous (no name field of its own) → named by the typedef.
    if spec.child_by_field_name("name").is_some() {
        // Named struct inside typedef: extract normally (walk_c handles it).
        c_type(&spec, source, extraction, "c");
        return;
    }
    let Some(name_node) = named_children(node)
        .into_iter()
        .rev()
        .find(|c| c.kind() == "type_identifier")
    else {
        return;
    };
    let name = txt(&name_node, source);
    if name.is_empty() {
        return;
    }
    let kind = if spec.kind() == "enum_specifier" { "enum" } else { "class" };
    let mut ty = empty_type(name, kind, node);
    ty.package = extraction.package.clone();
    for sub in named_children(&spec) {
        if sub.kind() == "field_declaration_list" {
            collect_c_fields(&sub, source, &mut ty);
        }
    }
    extraction.types.push(ty);
}

fn collect_c_fields<'a>(list: &Node<'a>, source: &str, ty: &mut ExtractedType) {
    for field in named_children(list) {
        if field.kind() != "field_declaration" {
            continue;
        }
        // A field containing a function_declarator is a method declaration.
        if let Some(fn_decl) = find_first(&field, "function_declarator") {
            if let Some(fid) = find_first(&fn_decl, "field_identifier") {
                let m = txt(&fid, source);
                if !m.is_empty() {
                    ty.methods.push(method_node(m, &field));
                }
            }
            continue;
        }
        let Some(fid) = find_first(&field, "field_identifier") else {
            continue;
        };
        let fname = txt(&fid, source);
        if fname.is_empty() {
            continue;
        }
        // Type text minus the declarator, e.g. `Repo*`.
        let type_text = field
            .child_by_field_name("type")
            .map(|t| txt(&t, source))
            .unwrap_or_default();
        ty.fields.push(ExtractedField {
            name: fname,
            type_name: type_text.clone(),
            annotations: Vec::new(),
            signature: None,
        });
        if !type_text.is_empty() {
            ty.declared_dependencies.push(type_text);
        }
    }
}

fn walk_cpp<'a>(node: &Node<'a>, source: &str, package: &str, extraction: &mut SourceExtraction) {
    match node.kind() {
        "namespace_definition" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| txt(&n, source))
                .unwrap_or_default();
            let nested = if name.is_empty() {
                package.to_string()
            } else if package.is_empty() {
                name
            } else {
                format!("{}.{}", package, name)
            };
            if extraction.package.is_empty() {
                extraction.package = nested.clone();
            }
            for child in named_children(node) {
                walk_cpp(&child, source, &nested, extraction);
            }
        }
        "class_specifier" | "struct_specifier" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let name = txt(&name_node, source);
            if name.is_empty() {
                return;
            }
            let mut ty = empty_type(name, "class", node);
            ty.package = if package.is_empty() {
                extraction.package.clone()
            } else {
                package.to_string()
            };
            for sub in named_children(node) {
                match sub.kind() {
                    "base_class_clause" => {
                        for base in named_children(&sub) {
                            if base.kind() == "type_identifier" {
                                let b = txt(&base, source);
                                if !b.is_empty() {
                                    ty.implemented_interfaces.push(b);
                                }
                            }
                        }
                    }
                    "field_declaration_list" => collect_c_fields(&sub, source, &mut ty),
                    _ => {}
                }
            }
            extraction.types.push(ty);
        }
        "template_declaration" => {
            // template<…> class/function — descend to the payload.
            for child in named_children(node) {
                walk_cpp(&child, source, package, extraction);
            }
        }
        "translation_unit" | "declaration_list" => {
            for child in named_children(node) {
                walk_cpp(&child, source, package, extraction);
            }
        }
        _ => {
            // Shared C-kind nodes: includes, typedefs, function definitions.
            // Delegate to the C walker for this node's children; walk_cpp
            // kinds above are handled here.
            walk_c(node, source, extraction);
        }
    }
}

// ---------------------------------------------------------------------------
// Scala
// ---------------------------------------------------------------------------

/// Parse Scala source: package clause, imports, class/case class/trait/
/// object definitions with `def` members and class parameters.
pub fn parse_scala_file(source: &str) -> Result<SourceExtraction, String> {
    let tree = parse_tree(tree_sitter_scala::LANGUAGE.into(), source)?;
    let root = tree.root_node();
    let mut extraction = SourceExtraction {
        language: "scala".into(),
        ..Default::default()
    };

    for node in named_children(&root) {
        match node.kind() {
            "package_clause" => {
                if let Some(pid) = find_first(&node, "package_identifier") {
                    extraction.package = txt(&pid, source);
                }
                walk_scala_body(&node, source, &mut extraction);
            }
            "import_declaration" => {
                let imported = txt(&node, source)
                    .trim_start_matches("import ")
                    .trim()
                    .to_string();
                if !imported.is_empty() && !extraction.imports.contains(&imported) {
                    extraction.imports.push(imported);
                }
            }
            _ => walk_scala_toplevel(&node, source, &mut extraction),
        }
    }
    Ok(extraction)
}

fn walk_scala_toplevel<'a>(node: &Node<'a>, source: &str, extraction: &mut SourceExtraction) {
    match node.kind() {
        "class_definition" | "trait_definition" | "object_definition" => {
            let Some(name_node) = node.child_by_field_name("name") else {
                return;
            };
            let name = txt(&name_node, source);
            if name.is_empty() {
                return;
            }
            let kind = match node.kind() {
                "trait_definition" => "interface",
                _ => "class",
            };
            let mut ty = empty_type(name, kind, node);
            ty.package = extraction.package.clone();
            // extends_clause: first type_identifier = base class, remaining =
            // mixins; record all as implemented interfaces.
            for sub in named_children(node) {
                match sub.kind() {
                    "extends_clause" => {
                        for base in named_children(&sub) {
                            if base.kind() == "type_identifier" {
                                let b = txt(&base, source);
                                if !b.is_empty() {
                                    ty.implemented_interfaces.push(b);
                                }
                            }
                        }
                    }
                    "class_parameters" => {
                        // case class fields: class_parameter → identifier +
                        // optional type_identifier.
                        for param in named_children(&sub) {
                            if param.kind() != "class_parameter" {
                                continue;
                            }
                            let Some(pname) = param.child_by_field_name("name") else {
                                continue;
                            };
                            let fname = txt(&pname, source);
                            if fname.is_empty() {
                                continue;
                            }
                            let type_name = param
                                .child_by_field_name("type")
                                .map(|t| txt(&t, source))
                                .unwrap_or_default();
                            ty.fields.push(ExtractedField {
                                name: fname,
                                type_name: type_name.clone(),
                                annotations: Vec::new(),
                                signature: None,
                            });
                            if !type_name.is_empty() {
                                ty.declared_dependencies.push(type_name);
                            }
                        }
                    }
                    "template_body" => {
                        for member in named_children(&sub) {
                            if matches!(member.kind(), "function_definition" | "function_declaration") {
                                if let Some(mn) = member.child_by_field_name("name") {
                                    let m = txt(&mn, source);
                                    if !m.is_empty() {
                                        ty.methods.push(method_node(m, &member));
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            extraction.types.push(ty);
        }
        "function_definition" | "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = txt(&name_node, source);
                if !name.is_empty() {
                    extraction.module_functions.push(method_node(name, node));
                }
            }
        }
        _ => {}
    }
}

fn walk_scala_body<'a>(node: &Node<'a>, source: &str, extraction: &mut SourceExtraction) {
    for child in named_children(node) {
        walk_scala_toplevel(&child, source, extraction);
        walk_scala_body(&child, source, extraction);
    }
}

// ---------------------------------------------------------------------------
// Haskell
// ---------------------------------------------------------------------------

/// Parse Haskell source: module header, imports, data/newtype/class
/// declarations, top-level signatures + function definitions.
pub fn parse_haskell_file(source: &str) -> Result<SourceExtraction, String> {
    let tree = parse_tree(tree_sitter_haskell::LANGUAGE.into(), source)?;
    let root = tree.root_node();
    let mut extraction = SourceExtraction {
        language: "haskell".into(),
        ..Default::default()
    };

    for node in named_children(&root) {
        match node.kind() {
            "header" => {
                if let Some(module) = find_first(&node, "module") {
                    let name = named_children(&module)
                        .into_iter()
                        .map(|m| txt(&m, source))
                        .collect::<Vec<_>>()
                        .join(".");
                    extraction.package = name;
                }
            }
            "imports" => {
                for imp in named_children(&node) {
                    if imp.kind() != "import" {
                        continue;
                    }
                    if let Some(module) = find_first(&imp, "module") {
                        let imported = named_children(&module)
                            .into_iter()
                            .map(|m| txt(&m, source))
                            .collect::<Vec<_>>()
                            .join(".");
                        if !imported.is_empty() && !extraction.imports.contains(&imported) {
                            extraction.imports.push(imported);
                        }
                    }
                }
            }
            "declarations" => {
                for decl in named_children(&node) {
                    match decl.kind() {
                        "data_type" | "newtype" | "type_family" => {
                            if let Some(name_node) = decl.child_by_field_name("name") {
                                let name = txt(&name_node, source);
                                if !name.is_empty() {
                                    let mut ty = empty_type(name, "class", &decl);
                                    ty.package = extraction.package.clone();
                                    extraction.types.push(ty);
                                }
                            }
                        }
                        "class" => {
                            if let Some(name_node) = decl.child_by_field_name("name") {
                                let name = txt(&name_node, source);
                                if name.is_empty() {
                                    continue;
                                }
                                let mut ty = empty_type(name, "interface", &decl);
                                ty.package = extraction.package.clone();
                                // Class methods: class_declarations → signature
                                // → variable.
                                if let Some(body) = find_first(&decl, "class_declarations") {
                                    for sig in named_children(&body) {
                                        if sig.kind() == "signature" {
                                            if let Some(var) = find_first(&sig, "variable") {
                                                let m = txt(&var, source);
                                                if !m.is_empty() {
                                                    ty.methods.push(method_node(m, &sig));
                                                }
                                            }
                                        }
                                    }
                                }
                                extraction.types.push(ty);
                            }
                        }
                        "function" => {
                            if let Some(name_node) = decl.child_by_field_name("name") {
                                let name = txt(&name_node, source);
                                if !name.is_empty() {
                                    extraction.module_functions.push(method_node(name, &decl));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    Ok(extraction)
}

// ---------------------------------------------------------------------------
// Julia
// ---------------------------------------------------------------------------

/// Parse Julia source: modules, structs (with typed fields), functions.
pub fn parse_julia_file(source: &str) -> Result<SourceExtraction, String> {
    let tree = parse_tree(tree_sitter_julia::LANGUAGE.into(), source)?;
    let root = tree.root_node();
    let mut extraction = SourceExtraction {
        language: "julia".into(),
        ..Default::default()
    };
    walk_julia(&root, source, "", &mut extraction);
    Ok(extraction)
}

fn walk_julia<'a>(node: &Node<'a>, source: &str, package: &str, extraction: &mut SourceExtraction) {
    for child in named_children(node) {
        match child.kind() {
            "module_definition" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| txt(&n, source))
                    .unwrap_or_default();
                let nested = if name.is_empty() {
                    package.to_string()
                } else if package.is_empty() {
                    name
                } else {
                    format!("{}.{}", package, name)
                };
                if extraction.package.is_empty() {
                    extraction.package = nested.clone();
                }
                walk_julia(&child, source, &nested, extraction);
            }
            "struct_definition" => {
                let Some(type_head) = find_first(&child, "type_head") else {
                    continue;
                };
                let Some(name_node) = find_first(&type_head, "identifier") else {
                    continue;
                };
                let name = txt(&name_node, source);
                if name.is_empty() {
                    continue;
                }
                let mut ty = empty_type(name, "class", &child);
                ty.package = if package.is_empty() {
                    extraction.package.clone()
                } else {
                    package.to_string()
                };
                for sub in named_children(&child) {
                    if sub.kind() == "typed_expression" {
                        let mut ids = named_children(&sub)
                            .into_iter()
                            .filter(|n| n.kind() == "identifier");
                        if let (Some(fname_node), Some(type_node)) = (ids.next(), ids.next()) {
                            let fname = txt(&fname_node, source);
                            let type_name = txt(&type_node, source);
                            if !fname.is_empty() {
                                ty.fields.push(ExtractedField {
                                    name: fname,
                                    type_name: type_name.clone(),
                                    annotations: Vec::new(),
                                    signature: None,
                                });
                                if !type_name.is_empty() {
                                    ty.declared_dependencies.push(type_name);
                                }
                            }
                        }
                    }
                }
                extraction.types.push(ty);
            }
            "function_definition" | "short_function_definition" => {
                // Julia's call grammar has no `name` field on the callee; the
                // function name is the first identifier of the call_expression
                // inside the signature.
                if let Some(sig) = find_first(&child, "signature") {
                    if let Some(call) = find_first(&sig, "call_expression") {
                        let name = first_identifier_text(&call, source);
                        if !name.is_empty() {
                            extraction.module_functions.push(method_node(name, &child));
                        }
                    }
                }
            }
            _ => walk_julia(&child, source, package, extraction),
        }
    }
}

fn first_identifier_text<'a>(node: &Node<'a>, source: &str) -> String {
    for child in named_children(node) {
        if child.kind() == "identifier" {
            return txt(&child, source);
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// OCaml
// ---------------------------------------------------------------------------

/// Parse OCaml source: type definitions, classes/objects with methods,
/// module bindings, top-level `let` functions.
pub fn parse_ocaml_file(source: &str) -> Result<SourceExtraction, String> {
    let tree = parse_tree(tree_sitter_ocaml::LANGUAGE_OCAML.into(), source)?;
    let root = tree.root_node();
    let mut extraction = SourceExtraction {
        language: "ocaml".into(),
        ..Default::default()
    };
    walk_ocaml(&root, source, "", &mut extraction);
    Ok(extraction)
}

fn walk_ocaml<'a>(node: &Node<'a>, source: &str, package: &str, extraction: &mut SourceExtraction) {
    for child in named_children(node) {
        match child.kind() {
            "module_definition" | "module_binding" => {
                let name = find_first(&child, "module_name")
                    .map(|n| txt(&n, source))
                    .unwrap_or_default();
                let nested = if name.is_empty() {
                    package.to_string()
                } else if package.is_empty() {
                    name
                } else {
                    format!("{}.{}", package, name)
                };
                if extraction.package.is_empty() {
                    extraction.package = nested.clone();
                }
                walk_ocaml(&child, source, &nested, extraction);
            }
            "type_definition" => {
                for binding in named_children(&child) {
                    if binding.kind() != "type_binding" {
                        continue;
                    }
                    let Some(name_node) = binding.child_by_field_name("name") else {
                        continue;
                    };
                    let name = txt(&name_node, source);
                    if name.is_empty() {
                        continue;
                    }
                    let mut ty = empty_type(name, "class", &binding);
                    ty.package = if package.is_empty() {
                        extraction.package.clone()
                    } else {
                        package.to_string()
                    };
                    extraction.types.push(ty);
                }
            }
            "class_definition" => {
                for binding in named_children(&child) {
                    if binding.kind() != "class_binding" {
                        continue;
                    }
                    let name = find_first(&binding, "class_name")
                        .map(|n| txt(&n, source))
                        .unwrap_or_default();
                    if name.is_empty() {
                        continue;
                    }
                    let mut ty = empty_type(name, "class", &binding);
                    ty.package = if package.is_empty() {
                        extraction.package.clone()
                    } else {
                        package.to_string()
                    };
                    if let Some(obj) = find_first(&binding, "object_expression") {
                        for m in named_children(&obj) {
                            if m.kind() == "method_definition" {
                                if let Some(mn) = find_first(&m, "method_name") {
                                    let mname = txt(&mn, source);
                                    if !mname.is_empty() {
                                        ty.methods.push(method_node(mname, &m));
                                    }
                                }
                            }
                        }
                    }
                    extraction.types.push(ty);
                }
            }
            "value_definition" => {
                for binding in named_children(&child) {
                    if binding.kind() != "let_binding" {
                        continue;
                    }
                    let name = find_first(&binding, "value_name")
                        .map(|n| txt(&n, source))
                        .unwrap_or_default();
                    if !name.is_empty() {
                        extraction.module_functions.push(method_node(name, &binding));
                    }
                }
            }
            _ => walk_ocaml(&child, source, package, extraction),
        }
    }
}

// ---------------------------------------------------------------------------
// Bash
// ---------------------------------------------------------------------------

/// Parse shell source: function definitions (`name() { … }` and
/// `function name { … }`). Shell has no types or imports.
pub fn parse_bash_file(source: &str) -> Result<SourceExtraction, String> {
    let tree = parse_tree(tree_sitter_bash::LANGUAGE.into(), source)?;
    let root = tree.root_node();
    let mut extraction = SourceExtraction {
        language: "bash".into(),
        ..Default::default()
    };
    let mut fns = Vec::new();
    descendants_of_kind(&root, "function_definition", &mut fns);
    for f in fns {
        // The name is the `word` child (field "name").
        let name = f
            .child_by_field_name("name")
            .map(|n| txt(&n, source))
            .unwrap_or_default();
        if !name.is_empty() {
            extraction.module_functions.push(method_node(name, &f));
        }
    }
    Ok(extraction)
}

// ---------------------------------------------------------------------------
// Verilog
// ---------------------------------------------------------------------------

/// Parse Verilog source: module declarations with ports and register/net
/// declarations as fields.
pub fn parse_verilog_file(source: &str) -> Result<SourceExtraction, String> {
    let tree = parse_tree(tree_sitter_verilog::LANGUAGE.into(), source)?;
    let root = tree.root_node();
    let mut extraction = SourceExtraction {
        language: "verilog".into(),
        ..Default::default()
    };
    let mut mods = Vec::new();
    descendants_of_kind(&root, "module_declaration", &mut mods);
    for m in mods {
        let Some(header) = find_first(&m, "module_header") else {
            continue;
        };
        let name = find_first(&header, "simple_identifier")
            .map(|n| txt(&n, source))
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let mut ty = empty_type(name, "class", &m);
        // Ports (ANSI style): ansi_port_declaration → port_identifier.
        let mut ports = Vec::new();
        descendants_of_kind(&m, "ansi_port_declaration", &mut ports);
        for p in ports {
            if let Some(pid) = find_first(&p, "port_identifier") {
                let pname = find_first(&pid, "simple_identifier")
                    .map(|n| txt(&n, source))
                    .unwrap_or_default();
                if !pname.is_empty() && !ty.fields.iter().any(|f| f.name == pname) {
                    ty.fields.push(ExtractedField {
                        name: pname,
                        type_name: String::new(),
                        annotations: Vec::new(),
                        signature: None,
                    });
                }
            }
        }
        // Register/net declarations: variable_decl_assignment.
        let mut decls = Vec::new();
        descendants_of_kind(&m, "variable_decl_assignment", &mut decls);
        for d in decls {
            if let Some(id) = find_first(&d, "simple_identifier") {
                let vname = txt(&id, source);
                if !vname.is_empty() && !ty.fields.iter().any(|f| f.name == vname) {
                    ty.fields.push(ExtractedField {
                        name: vname,
                        type_name: String::new(),
                        annotations: Vec::new(),
                        signature: None,
                    });
                }
            }
        }
        extraction.types.push(ty);
    }
    Ok(extraction)
}

// ---------------------------------------------------------------------------
// Agda
// ---------------------------------------------------------------------------

/// Parse Agda source: module name, data declarations, function definitions.
pub fn parse_agda_file(source: &str) -> Result<SourceExtraction, String> {
    let tree = parse_tree(tree_sitter_agda::LANGUAGE.into(), source)?;
    let root = tree.root_node();
    let mut extraction = SourceExtraction {
        language: "agda".into(),
        ..Default::default()
    };
    let mut modules = Vec::new();
    descendants_of_kind(&root, "module", &mut modules);
    if let Some(first) = modules.first() {
        if let Some(mn) = find_first(first, "module_name") {
            extraction.package = txt(&mn, source);
        }
    }
    let mut datas = Vec::new();
    descendants_of_kind(&root, "data", &mut datas);
    for d in datas {
        let name = find_first(&d, "data_name")
            .map(|n| txt(&n, source))
            .unwrap_or_default();
        if !name.is_empty() {
            let mut ty = empty_type(name, "class", &d);
            ty.package = extraction.package.clone();
            extraction.types.push(ty);
        }
    }
    let mut fns = Vec::new();
    descendants_of_kind(&root, "function", &mut fns);
    for f in fns {
        let name = find_first(&f, "function_name")
            .and_then(|n| find_first(&n, "qid"))
            .map(|n| txt(&n, source))
            .unwrap_or_default();
        if !name.is_empty() {
            extraction.module_functions.push(method_node(name, &f));
        }
    }
    Ok(extraction)
}

// ---------------------------------------------------------------------------
// Tests — one per language, asserting against real grammar node structure.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruby_classes_modules_methods() {
        let src = r#"
require "json"
require_relative "helper"

class UserService < BaseService
  def find(id)
    @store[id]
  end
end

module Billing
  def charge(x); end
end
"#;
        let ext = parse_ruby_file(src).unwrap();
        assert_eq!(ext.language, "ruby");
        assert!(ext.imports.contains(&"json".to_string()));
        assert!(ext.imports.contains(&"helper".to_string()));

        let svc = ext.types.iter().find(|t| t.name == "UserService").unwrap();
        assert_eq!(svc.kind, "class");
        assert!(svc
            .implemented_interfaces
            .contains(&"BaseService".to_string()));
        assert!(svc.methods.iter().any(|m| m.name == "find"));
        let billing = ext.types.iter().find(|t| t.name == "Billing").unwrap();
        assert!(billing.methods.iter().any(|m| m.name == "charge"));
    }

    #[test]
    fn php_class_interface_functions() {
        let src = r#"
<?php
namespace App\Service;
use App\Repo\UserRepository;

class UserService extends BaseService implements UserServiceInterface {
  private UserRepository $repo;
  public function find(int $id): ?User { return $this->repo->find($id); }
}

interface UserServiceInterface { }

function helper(): void { }
"#;
        let ext = parse_php_file(src).unwrap();
        assert_eq!(ext.language, "php");
        assert_eq!(ext.package, "App\\Service");
        assert!(ext.imports.iter().any(|i| i.contains("UserRepository")));

        let svc = ext.types.iter().find(|t| t.name == "UserService").unwrap();
        assert_eq!(svc.kind, "class");
        assert!(svc
            .implemented_interfaces
            .contains(&"UserServiceInterface".to_string()));
        assert!(svc.methods.iter().any(|m| m.name == "find"));
        assert!(svc
            .fields
            .iter()
            .any(|f| f.name == "repo" && f.type_name == "UserRepository"));

        assert!(ext
            .types
            .iter()
            .any(|t| t.name == "UserServiceInterface" && t.kind == "interface"));
        assert!(ext
            .module_functions
            .iter()
            .any(|f| f.name == "helper"));
    }

    #[test]
    fn csharp_class_interface_enum() {
        let src = r#"
using System;
namespace App;

public class UserService : BaseService, IService {
    private readonly Repo _repo;
    public User Find(int id) { return _repo.Find(id); }
}

public interface IService { }
public enum Color { Red, Green }
"#;
        let ext = parse_csharp_file(src).unwrap();
        assert_eq!(ext.language, "csharp");
        assert_eq!(ext.package, "App");
        assert!(ext.imports.contains(&"System".to_string()));

        let svc = ext.types.iter().find(|t| t.name == "UserService").unwrap();
        assert_eq!(svc.kind, "class");
        assert!(svc
            .implemented_interfaces
            .contains(&"BaseService".to_string()));
        assert!(svc.implemented_interfaces.contains(&"IService".to_string()));
        assert!(svc.methods.iter().any(|m| m.name == "Find"));
        assert!(svc.fields.iter().any(|f| f.name == "_repo"));

        assert!(ext.types.iter().any(|t| t.name == "IService" && t.kind == "interface"));
        assert!(ext.types.iter().any(|t| t.name == "Color" && t.kind == "enum"));
    }

    #[test]
    fn c_structs_typedef_functions() {
        let src = r#"
#include "utils.h"
#include <stdlib.h>

struct Point { int x; int y; };
typedef struct { double w; } Weight;
typedef enum { A, B } Mode;

int compute(int a) { return a; }
"#;
        let ext = parse_c_file(src).unwrap();
        assert_eq!(ext.language, "c");
        assert!(ext.imports.contains(&"utils.h".to_string()));
        assert!(ext.imports.contains(&"stdlib.h".to_string()));

        let point = ext.types.iter().find(|t| t.name == "Point").unwrap();
        assert_eq!(point.kind, "class");
        assert!(point.fields.iter().any(|f| f.name == "x"));
        // Anonymous struct named via typedef.
        assert!(ext.types.iter().any(|t| t.name == "Weight"));
        assert!(ext.types.iter().any(|t| t.name == "Mode" && t.kind == "enum"));
        assert!(ext
            .module_functions
            .iter()
            .any(|f| f.name == "compute"));
    }

    #[test]
    fn cpp_namespace_class_template() {
        let src = r#"
#include <vector>
namespace app {
class Service : public BaseService {
public:
    Repo* repo;
    virtual User find(int id);
};
struct Point { int x; };
template<typename T> class Box { T v; };
int helper() { return 1; }
}
"#;
        let ext = parse_cpp_file(src).unwrap();
        assert_eq!(ext.language, "cpp");
        assert!(ext.imports.contains(&"vector".to_string()));
        assert_eq!(ext.package, "app");

        let svc = ext.types.iter().find(|t| t.name == "Service").unwrap();
        assert!(svc
            .implemented_interfaces
            .contains(&"BaseService".to_string()));
        assert!(svc.methods.iter().any(|m| m.name == "find"));
        assert!(svc.fields.iter().any(|f| f.name == "repo"));
        assert!(ext.types.iter().any(|t| t.name == "Point"));
        assert!(ext.types.iter().any(|t| t.name == "Box"));
        assert!(ext.module_functions.iter().any(|f| f.name == "helper"));
    }

    #[test]
    fn scala_class_trait_object() {
        let src = r#"
package com.example
import scala.collection.mutable

class UserService extends BaseService with Repo {
  def find(id: Int): User = ???
}
case class User(name: String)
trait Repo { def get(id: Int): User }
object Main { def main(): Unit = {} }
"#;
        let ext = parse_scala_file(src).unwrap();
        assert_eq!(ext.language, "scala");
        assert_eq!(ext.package, "com.example");
        assert!(ext
            .imports
            .contains(&"scala.collection.mutable".to_string()));

        let svc = ext.types.iter().find(|t| t.name == "UserService").unwrap();
        assert!(svc
            .implemented_interfaces
            .contains(&"BaseService".to_string()));
        assert!(svc.implemented_interfaces.contains(&"Repo".to_string()));
        assert!(svc.methods.iter().any(|m| m.name == "find"));

        let user = ext.types.iter().find(|t| t.name == "User").unwrap();
        assert!(user.fields.iter().any(|f| f.name == "name"));

        let repo = ext.types.iter().find(|t| t.name == "Repo").unwrap();
        assert_eq!(repo.kind, "interface");
        assert!(repo.methods.iter().any(|m| m.name == "get"));
        assert!(ext.types.iter().any(|t| t.name == "Main"));
    }

    #[test]
    fn haskell_data_class_functions() {
        let src = r#"
module A where

import Data.List (sort)

data Tree = Leaf | Node Tree Tree

class Repo a where
  find :: a -> Int

getUser :: Int -> User
getUser x = undefined
"#;
        let ext = parse_haskell_file(src).unwrap();
        assert_eq!(ext.language, "haskell");
        assert_eq!(ext.package, "A");
        assert!(ext.imports.contains(&"Data.List".to_string()));
        assert!(ext.types.iter().any(|t| t.name == "Tree"));
        let repo = ext.types.iter().find(|t| t.name == "Repo").unwrap();
        assert_eq!(repo.kind, "interface");
        assert!(repo.methods.iter().any(|m| m.name == "find"));
        assert!(ext
            .module_functions
            .iter()
            .any(|f| f.name == "getUser"));
    }

    #[test]
    fn julia_module_struct_function() {
        let src = r#"
module MyModule

struct Point
    x::Float64
end

function compute(x)
    return x
end

end
"#;
        let ext = parse_julia_file(src).unwrap();
        assert_eq!(ext.language, "julia");
        assert!(ext.package.contains("MyModule"));
        let point = ext.types.iter().find(|t| t.name == "Point").unwrap();
        assert!(point
            .fields
            .iter()
            .any(|f| f.name == "x" && f.type_name == "Float64"));
        assert!(ext
            .module_functions
            .iter()
            .any(|f| f.name == "compute"));
    }

    #[test]
    fn ocaml_types_class_lets() {
        let src = r#"
type shape = Circle | Square

class repo =
  object
    method find x = x
  end

let compute x = x + 1
"#;
        let ext = parse_ocaml_file(src).unwrap();
        assert_eq!(ext.language, "ocaml");
        assert!(ext.types.iter().any(|t| t.name == "shape"));
        let repo = ext.types.iter().find(|t| t.name == "repo").unwrap();
        assert!(repo.methods.iter().any(|m| m.name == "find"));
        assert!(ext
            .module_functions
            .iter()
            .any(|f| f.name == "compute"));
    }

    #[test]
    fn bash_functions() {
        let src = "#!/bin/bash\nfunction deploy() {\n  echo hi\n}\nbuild() {\n  echo bye\n}\n";
        let ext = parse_bash_file(src).unwrap();
        assert_eq!(ext.language, "bash");
        assert!(ext
            .module_functions
            .iter()
            .any(|f| f.name == "deploy"));
        assert!(ext.module_functions.iter().any(|f| f.name == "build"));
    }

    #[test]
    fn verilog_module_ports_regs() {
        let src = r#"
module counter(input clk);
  reg [7:0] count;
  always @(posedge clk) count <= count + 1;
endmodule
"#;
        let ext = parse_verilog_file(src).unwrap();
        assert_eq!(ext.language, "verilog");
        let counter = ext.types.iter().find(|t| t.name == "counter").unwrap();
        assert!(counter.fields.iter().any(|f| f.name == "clk"));
        assert!(counter.fields.iter().any(|f| f.name == "count"));
    }

    #[test]
    fn agda_module_data_functions() {
        let src = "module Basic where\n\ndata Nat : Set where\n  zero : Nat\n  suc  : Nat \u{2192} Nat\n\n+ : Nat \u{2192} Nat \u{2192} Nat\n";
        let ext = parse_agda_file(src).unwrap();
        assert_eq!(ext.language, "agda");
        assert_eq!(ext.package, "Basic");
        assert!(ext.types.iter().any(|t| t.name == "Nat"));
        assert!(ext
            .module_functions
            .iter()
            .any(|f| f.name == "zero" || f.name == "+"));
    }

    #[test]
    fn empty_inputs_parse_cleanly() {
        assert!(parse_ruby_file("").unwrap().types.is_empty());
        assert!(parse_php_file("").unwrap().types.is_empty());
        assert!(parse_csharp_file("").unwrap().types.is_empty());
        assert!(parse_c_file("").unwrap().types.is_empty());
        assert!(parse_cpp_file("").unwrap().types.is_empty());
        assert!(parse_scala_file("").unwrap().types.is_empty());
        assert!(parse_haskell_file("").unwrap().types.is_empty());
        assert!(parse_julia_file("").unwrap().types.is_empty());
        assert!(parse_ocaml_file("").unwrap().types.is_empty());
        assert!(parse_bash_file("").unwrap().types.is_empty());
        assert!(parse_verilog_file("").unwrap().types.is_empty());
        assert!(parse_agda_file("").unwrap().types.is_empty());
    }
}
