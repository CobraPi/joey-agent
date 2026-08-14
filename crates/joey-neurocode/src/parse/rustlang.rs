//! tree-sitter-rust AST extraction.
//!
//! Parse a `.rs` file, extract use-declarations (imports), structs
//! (fields as declared dependencies, `derive` attributes as annotations),
//! traits (→ interface), enums, impl blocks (trait impls become
//! implemented-interfaces edges; inherent impls contribute methods), and
//! free functions. Module paths are derived by the caller from the file
//! path (crate-root-relative), so `package` is left empty here.

use tree_sitter::{Node, Parser};

use super::extract::{ExtractedField, ExtractedMethod, ExtractedType, SourceExtraction};

/// Parse a Rust source string and extract structural metadata.
pub fn parse_rust_file(source: &str) -> Result<SourceExtraction, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .map_err(|e| format!("language error: {}", e))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "failed to parse".to_string())?;
    let root = tree.root_node();

    let mut extraction = SourceExtraction {
        language: "rust".into(),
        ..Default::default()
    };

    // impl blocks grouped by self type: (type name, Option<trait>, methods).
    let mut impls: Vec<(String, Option<String>, Vec<ExtractedMethod>)> = Vec::new();

    for i in 0..root.named_child_count() {
        if let Some(child) = root.named_child(i as u32) {
            match child.kind() {
                "use_declaration" => {
                    if let Some(arg) = child.child_by_field_name("argument") {
                        if let Ok(t) = arg.utf8_text(source.as_bytes()) {
                            let t = t.trim().to_string();
                            if !t.is_empty() && !extraction.imports.contains(&t) {
                                extraction.imports.push(t);
                            }
                        }
                    }
                }
                "mod_item" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        if let Ok(t) = name.utf8_text(source.as_bytes()) {
                            let t = t.trim().to_string();
                            if !t.is_empty() {
                                extraction.imports.push(format!("mod {}", t));
                            }
                        }
                    }
                }
                "struct_item" => {
                    if let Some(t) = extract_struct(&child, source) {
                        extraction.types.push(t);
                    }
                }
                "enum_item" => {
                    if let Some(t) = extract_enum(&child, source) {
                        extraction.types.push(t);
                    }
                }
                "trait_item" => {
                    if let Some(t) = extract_trait(&child, source) {
                        extraction.types.push(t);
                    }
                }
                "impl_item" => {
                    let trait_name = child
                        .child_by_field_name("trait")
                        .and_then(|t| t.utf8_text(source.as_bytes()).ok())
                        .map(|s| s.trim().to_string());
                    let self_type = child
                        .child_by_field_name("type")
                        .and_then(|t| t.utf8_text(source.as_bytes()).ok())
                        .map(|s| s.trim().to_string());
                    let mut methods = Vec::new();
                    collect_impl_methods(&child, source, &mut methods);
                    if let Some(self_type) = self_type {
                        impls.push((self_type, trait_name, methods));
                    }
                }
                "function_item" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(source.as_bytes()) {
                            extraction.module_functions.push(ExtractedMethod {
                                name: n.trim().to_string(),
                                annotations: Vec::new(),
                                start_byte: child.start_byte() as u32,
                                end_byte: child.end_byte() as u32,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Fold impl blocks into their type entries.
    for (self_type, trait_name, methods) in impls {
        let bare = self_type
            .split('<')
            .next()
            .unwrap_or(&self_type)
            .trim()
            .to_string();
        if let Some(t) = extraction.types.iter_mut().find(|t| t.name == bare) {
            if let Some(tr) = trait_name {
                let tr = tr.split('<').next().unwrap_or(&tr).trim().to_string();
                if !tr.is_empty() && !t.implemented_interfaces.contains(&tr) {
                    t.implemented_interfaces.push(tr);
                }
            }
            for m in methods {
                t.methods.push(m);
            }
        }
    }

    Ok(extraction)
}

/// Attributes on an item (e.g. `#[derive(Debug, Clone)]` → ["derive:Debug,
/// Clone"]). Attribute items are siblings preceding the item in its parent
/// — walk the parent's children backwards from the item.
fn attributes<'a>(node: &Node<'a>, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Some(parent) = node.parent() else {
        return out;
    };
    // Index of `node` among the parent's children.
    let Some(start) = (0..parent.child_count()).find(|&i| {
        parent
            .child(i.try_into().unwrap())
            .map_or(false, |c| c.id() == node.id())
    }) else {
        return out;
    };
    let mut i = start;
    while i > 0 {
        i -= 1;
        let Some(sibling) = parent.child(i.try_into().unwrap()) else {
            break;
        };
        if sibling.kind() != "attribute_item" {
            break;
        }
        let Some(attr) = sibling.named_child(0) else {
            continue;
        };
        if attr.kind() != "attribute" {
            continue;
        }
        // The attribute name identifier and its token_tree arguments have
        // no field names — take them positionally/by kind.
        let name = (0..attr.named_child_count())
            .filter_map(|j| attr.named_child(j as u32))
            .find(|c| c.kind() == "identifier")
            .and_then(|id| id.utf8_text(source.as_bytes()).ok())
            .unwrap_or("")
            .trim()
            .to_string();
        let args = (0..attr.named_child_count())
            .filter_map(|j| attr.named_child(j as u32))
            .find(|c| c.kind() == "token_tree")
            .and_then(|tt| tt.utf8_text(source.as_bytes()).ok())
            .unwrap_or("")
            .trim()
            .to_string();
        let entry = if args.is_empty() {
            name.clone()
        } else {
            let inner = args
                .trim()
                .trim_start_matches(|c| c == '(' || c == '[')
                .trim_end_matches(|c| c == ')' || c == ']');
            format!("{}:{}", name, inner)
        };
        if !entry.is_empty() && !out.contains(&entry) {
            out.push(entry);
        }
    }
    out
}

/// Extract a struct_item → class ExtractedType.
fn extract_struct<'a>(node: &Node<'a>, source: &str) -> Option<ExtractedType> {
    let name = node
        .child_by_field_name("name")?
        .utf8_text(source.as_bytes())
        .ok()?
        .trim()
        .to_string();

    let mut fields = Vec::new();
    let mut deps = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        for i in 0..body.named_child_count() {
            if let Some(field) = body.named_child(i as u32) {
                if field.kind() != "field_declaration" {
                    continue;
                }
                let type_text = field
                    .child_by_field_name("type")
                    .and_then(|t| t.utf8_text(source.as_bytes()).ok())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let fname = field
                    .child_by_field_name("name")
                    .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if fname.is_empty() {
                    continue;
                }
                fields.push(ExtractedField {
                    name: fname,
                    type_name: type_text.clone(),
                    annotations: Vec::new(),
                });
                if let Some(dep) = rust_base_type(&type_text) {
                    if !is_builtin_rust_type(&dep) && !deps.contains(&dep) {
                        deps.push(dep);
                    }
                }
            }
        }
    }

    Some(ExtractedType {
        name,
        kind: "class".into(),
        package: String::new(),
        fq_name: None,
        implemented_interfaces: Vec::new(),
        annotations: attributes(node, source),
        declared_dependencies: deps,
        methods: Vec::new(),
        fields,
        start_byte: node.start_byte() as u32,
        end_byte: node.end_byte() as u32,
    })
}

/// Extract an enum_item → enum ExtractedType.
fn extract_enum<'a>(node: &Node<'a>, source: &str) -> Option<ExtractedType> {
    let name = node
        .child_by_field_name("name")?
        .utf8_text(source.as_bytes())
        .ok()?
        .trim()
        .to_string();
    Some(ExtractedType {
        name,
        kind: "enum".into(),
        package: String::new(),
        fq_name: None,
        implemented_interfaces: Vec::new(),
        annotations: attributes(node, source),
        declared_dependencies: Vec::new(),
        methods: Vec::new(),
        fields: Vec::new(),
        start_byte: node.start_byte() as u32,
        end_byte: node.end_byte() as u32,
    })
}

/// Extract a trait_item → interface ExtractedType.
fn extract_trait<'a>(node: &Node<'a>, source: &str) -> Option<ExtractedType> {
    let name = node
        .child_by_field_name("name")?
        .utf8_text(source.as_bytes())
        .ok()?
        .trim()
        .to_string();
    let mut methods = Vec::new();
    if let Some(body) = node.child_by_field_name("body") {
        collect_impl_methods_named(&body, source, &mut methods, &["function_signature_item"]);
    }
    Some(ExtractedType {
        name,
        kind: "interface".into(),
        package: String::new(),
        fq_name: None,
        implemented_interfaces: Vec::new(),
        annotations: attributes(node, source),
        declared_dependencies: Vec::new(),
        methods,
        fields: Vec::new(),
        start_byte: node.start_byte() as u32,
        end_byte: node.end_byte() as u32,
    })
}

/// Collect function items from an impl body.
fn collect_impl_methods<'a>(
    impl_node: &Node<'a>,
    source: &str,
    methods: &mut Vec<ExtractedMethod>,
) {
    if let Some(body) = impl_node.child_by_field_name("body") {
        collect_impl_methods_named(&body, source, methods, &["function_item"]);
    }
}

fn collect_impl_methods_named<'a>(
    body: &Node<'a>,
    source: &str,
    methods: &mut Vec<ExtractedMethod>,
    kinds: &[&str],
) {
    for i in 0..body.named_child_count() {
        if let Some(child) = body.named_child(i as u32) {
            if !kinds.contains(&child.kind()) {
                continue;
            }
            if let Some(name) = child.child_by_field_name("name") {
                if let Ok(n) = name.utf8_text(source.as_bytes()) {
                    methods.push(ExtractedMethod {
                        name: n.trim().to_string(),
                        annotations: Vec::new(),
                        start_byte: child.start_byte() as u32,
                        end_byte: child.end_byte() as u32,
                    });
                }
            }
        }
    }
}

/// Strip Rust type wrappers to the base type: `Option<User>` → `User`,
/// `&str` → `str`, `Vec<Repo>` → `Repo` (outermost generic is skipped in
/// favor of the interesting inner type when the outer is a std wrapper).
fn rust_base_type(t: &str) -> Option<String> {
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    let t = t
        .trim_start_matches('&')
        .trim()
        .trim_start_matches("mut ")
        .trim();
    // Take generic inner type for common std wrappers.
    for wrapper in ["Option<", "Result<", "Vec<", "Box<", "Arc<", "Rc<"] {
        if let Some(rest) = t.strip_prefix(wrapper) {
            let inner = rest.trim_end_matches('>').trim();
            // Result<T, E> — take the Ok type only.
            let inner = inner.split(',').next().unwrap_or(inner).trim();
            return rust_base_type(inner);
        }
    }
    if t.contains(' ') || t.starts_with("fn(") || t.starts_with('[') {
        return None;
    }
    // Strip remaining generic params: HashMap<K, V> → HashMap.
    let base = t.split('<').next().unwrap_or(t).trim();
    if base.is_empty() {
        None
    } else {
        Some(base.to_string())
    }
}

fn is_builtin_rust_type(t: &str) -> bool {
    matches!(
        t,
        "str" | "String" | "bool" | "char"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
            | "Cow"
            | "Path"
            | "PathBuf"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST_SRC: &str = r#"
use std::collections::HashMap;
mod sub;

#[derive(Debug, Clone)]
pub struct Service {
    repo: Repo,
    name: String,
}

pub trait Handler {
    fn handle(&self, req: Request) -> Response;
}

impl Handler for Service {
    fn handle(&self, req: Request) -> Response { Response::new() }
}

impl Service {
    pub fn new(repo: Repo) -> Self { Self { repo, name: String::new() } }
    pub fn find(&self, id: &str) -> Option<User> { None }
}

pub fn helper(x: u32) -> u32 { x }
"#;

    #[test]
    fn parse_rust_structures() {
        let ext = parse_rust_file(RUST_SRC).unwrap();
        assert_eq!(ext.language, "rust");
        assert!(ext
            .imports
            .iter()
            .any(|i| i == "std::collections::HashMap"));
        assert!(ext.imports.iter().any(|i| i == "mod sub"));

        let svc = ext.types.iter().find(|t| t.name == "Service").unwrap();
        assert_eq!(svc.kind, "class");
        // derive attribute captured.
        assert!(svc
            .annotations
            .iter()
            .any(|a| a.starts_with("derive:")));
        // Handler impl edge.
        assert!(svc
            .implemented_interfaces
            .contains(&"Handler".to_string()));
        // Inherent + trait impl methods folded in.
        assert!(svc.methods.iter().any(|m| m.name == "new"));
        assert!(svc.methods.iter().any(|m| m.name == "find"));
        assert!(svc.methods.iter().any(|m| m.name == "handle"));
        // repo: Repo → dependency on Repo.
        assert!(svc.declared_dependencies.contains(&"Repo".to_string()));

        let handler = ext.types.iter().find(|t| t.name == "Handler").unwrap();
        assert_eq!(handler.kind, "interface");
        assert!(handler.methods.iter().any(|m| m.name == "handle"));

        assert!(ext
            .module_functions
            .iter()
            .any(|f| f.name == "helper"));
    }

    #[test]
    fn parse_empty() {
        let ext = parse_rust_file("").unwrap();
        assert!(ext.types.is_empty());
    }
}
