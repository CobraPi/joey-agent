//! tree-sitter-go AST extraction.
//!
//! Parse a `.go` file, extract the package clause, imports, interface
//! types (methods), struct types (fields as declared dependencies), and
//! functions/methods. Methods with receivers are attached to their
//! receiver type when it is declared in the same file.

use tree_sitter::{Node, Parser};

use super::extract::{ExtractedField, ExtractedMethod, ExtractedType, SourceExtraction};

/// Parse a Go source string and extract structural metadata.
pub fn parse_go_file(source: &str) -> Result<SourceExtraction, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_go::LANGUAGE.into())
        .map_err(|e| format!("language error: {}", e))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "failed to parse".to_string())?;
    let root = tree.root_node();

    let mut extraction = SourceExtraction {
        language: "go".into(),
        ..Default::default()
    };

    // Methods with receivers, attached after types are collected:
    // (receiver type simple name, method).
    let mut receiver_methods: Vec<(String, ExtractedMethod)> = Vec::new();

    for i in 0..root.named_child_count() {
        if let Some(child) = root.named_child(i as u32) {
            match child.kind() {
                "package_clause" => {
                    if let Some(name) = child.named_child(0) {
                        extraction.package = name
                            .utf8_text(source.as_bytes())
                            .unwrap_or("")
                            .trim()
                            .to_string();
                    }
                }
                "import_declaration" => {
                    collect_go_imports(&child, source, &mut extraction.imports);
                }
                "type_declaration" => {
                    extract_go_type_declaration(&child, source, &mut extraction.types);
                }
                "method_declaration" => {
                    let receiver = child
                        .child_by_field_name("receiver")
                        .and_then(|r| receiver_type_name(&r, source));
                    if let Some(name) = child.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(source.as_bytes()) {
                            let method = ExtractedMethod {
                                name: n.trim().to_string(),
                                annotations: Vec::new(),
                                signature: decl_signature(&child, source, "parameters"),
                                start_byte: child.start_byte() as u32,
                                end_byte: child.end_byte() as u32,
                            };
                            match receiver {
                                Some(r) => receiver_methods.push((r, method)),
                                None => extraction.module_functions.push(method),
                            }
                        }
                    }
                }
                "function_declaration" => {
                    if let Some(name) = child.child_by_field_name("name") {
                        if let Ok(n) = name.utf8_text(source.as_bytes()) {
                            extraction.module_functions.push(ExtractedMethod {
                                name: n.trim().to_string(),
                                annotations: Vec::new(),
                                signature: decl_signature(&child, source, "parameters"),
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

    // Attach receiver methods to same-file types.
    for (receiver, method) in receiver_methods {
        let bare = receiver.trim_start_matches('*');
        if let Some(t) = extraction.types.iter_mut().find(|t| t.name == bare) {
            t.methods.push(method);
        } else {
            // Receiver type not declared in this file — surface as a module
            // function so it is not lost from the graph.
            extraction.module_functions.push(method);
        }
    }

    Ok(extraction)
}

/// Collect import paths from an import_declaration (single or grouped).
fn collect_go_imports<'a>(node: &Node<'a>, source: &str, imports: &mut Vec<String>) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            match child.kind() {
                "import_spec_list" => collect_go_imports(&child, source, imports),
                "import_spec" => {
                    if let Some(path) = child.child_by_field_name("path") {
                        if let Ok(p) = path.utf8_text(source.as_bytes()) {
                            let cleaned = p.trim().trim_matches('"').to_string();
                            if !cleaned.is_empty() && !imports.contains(&cleaned) {
                                imports.push(cleaned);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// Extract a type_declaration (possibly multiple type specs).
fn extract_go_type_declaration<'a>(
    decl: &Node<'a>,
    source: &str,
    types: &mut Vec<ExtractedType>,
) {
    for i in 0..decl.named_child_count() {
        if let Some(spec) = decl.named_child(i as u32) {
            if spec.kind() != "type_spec" {
                continue;
            }
            let Some(name_node) = spec.child_by_field_name("name") else {
                continue;
            };
            let Ok(name) = name_node.utf8_text(source.as_bytes()) else {
                continue;
            };
            let name = name.trim().to_string();
            if name.is_empty() {
                continue;
            }
            let Some(type_node) = spec.child_by_field_name("type") else {
                continue;
            };
            match type_node.kind() {
                "interface_type" => {
                    let mut methods = Vec::new();
                    collect_interface_methods(&type_node, source, &mut methods);
                    types.push(ExtractedType {
                        name,
                        kind: "interface".into(),
                        package: String::new(),
                        fq_name: None,
                        implemented_interfaces: Vec::new(),
                        annotations: Vec::new(),
                        declared_dependencies: Vec::new(),
                        methods,
                        fields: Vec::new(),
                        start_byte: spec.start_byte() as u32,
                        end_byte: spec.end_byte() as u32,
                    });
                }
                "struct_type" => {
                    let mut fields = Vec::new();
                    let mut deps = Vec::new();
                    collect_struct_fields(&type_node, source, &mut fields, &mut deps);
                    types.push(ExtractedType {
                        name,
                        kind: "class".into(),
                        package: String::new(),
                        fq_name: None,
                        implemented_interfaces: Vec::new(),
                        annotations: Vec::new(),
                        declared_dependencies: deps,
                        methods: Vec::new(),
                        fields,
                        start_byte: spec.start_byte() as u32,
                        end_byte: spec.end_byte() as u32,
                    });
                }
                _ => {}
            }
        }
    }
}

/// The declaration header of a Go func/method/interface-method: source text
/// from the node start through the close of the `params_field` list,
/// whitespace-collapsed, with the body cut off.
fn decl_signature<'a>(node: &Node<'a>, source: &str, params_field: &str) -> Option<String> {
    let end = node.child_by_field_name(params_field)?.end_byte();
    if end <= node.start_byte() || end > source.len() {
        return None;
    }
    Some(source[node.start_byte()..end].split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Collect method signatures from an interface body.
fn collect_interface_methods<'a>(
    iface: &Node<'a>,
    source: &str,
    methods: &mut Vec<ExtractedMethod>,
) {
    for i in 0..iface.named_child_count() {
        if let Some(child) = iface.named_child(i as u32) {
            if child.kind() == "method_elem" {
                if let Some(name) = child.child_by_field_name("name") {
                    if let Ok(n) = name.utf8_text(source.as_bytes()) {
                        methods.push(ExtractedMethod {
                            name: n.trim().to_string(),
                            annotations: Vec::new(),
                            signature: decl_signature(&child, source, "parameters"),
                            start_byte: child.start_byte() as u32,
                            end_byte: child.end_byte() as u32,
                        });
                    }
                }
            }
        }
    }
}

/// Collect fields from a struct body; non-builtin field base types become
/// declared dependencies.
fn collect_struct_fields<'a>(
    struct_type: &Node<'a>,
    source: &str,
    fields: &mut Vec<ExtractedField>,
    deps: &mut Vec<String>,
) {
    for i in 0..struct_type.named_child_count() {
        if let Some(child) = struct_type.named_child(i as u32) {
            if child.kind() != "field_declaration_list" {
                continue;
            }
            for j in 0..child.named_child_count() {
                if let Some(field) = child.named_child(j as u32) {
                    if field.kind() != "field_declaration" {
                        continue;
                    }
                    let type_text = field
                        .child_by_field_name("type")
                        .and_then(|t| t.utf8_text(source.as_bytes()).ok())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if let Some(name_node) = field.child_by_field_name("name") {
                        if let Ok(n) = name_node.utf8_text(source.as_bytes()) {
                            let n = n.trim().to_string();
                            if !n.is_empty() {
                                fields.push(ExtractedField {
                                    name: n,
                                    type_name: type_text.clone(),
                                    annotations: Vec::new(),
                                    signature: Some(type_text.clone()),
                                });
                            }
                        }
                    }
                    // Dependency: the base type name (pointer/slice/generic
                    // wrappers stripped, qualification kept).
                    if let Some(dep) = go_base_type(&type_text) {
                        if !is_builtin_go_type(&dep) && !deps.contains(&dep) {
                            deps.push(dep);
                        }
                    }
                }
            }
        }
    }
}

/// The receiver type's simple name from a parameter_list (strips pointer;
/// descends into parameter_declaration → pointer_type → type_identifier).
fn receiver_type_name<'a>(receiver: &Node<'a>, source: &str) -> Option<String> {
    fn find_type_identifier<'a>(node: &Node<'a>, source: &str) -> Option<String> {
        if node.kind() == "type_identifier" {
            return node
                .utf8_text(source.as_bytes())
                .ok()
                .map(|s| s.trim().to_string());
        }
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i as u32) {
                if let Some(found) = find_type_identifier(&child, source) {
                    return Some(found);
                }
            }
        }
        None
    }
    find_type_identifier(receiver, source)
}

/// Strip Go type wrappers to the base type name: `*repo.Repo` → `repo.Repo`,
/// `[]byte` → `byte`. Composite types (maps, funcs, interfaces) → None.
fn go_base_type(t: &str) -> Option<String> {
    let t = t.trim();
    if t.is_empty() || t.starts_with("map[") || t.starts_with("func(") || t.starts_with("interface{")
    {
        return None;
    }
    let mut s = t.trim_start_matches('*').trim();
    if let Some(rest) = s.strip_prefix("[]") {
        s = rest.trim();
    }
    if s.is_empty() || s.contains('[') || s.contains(' ') {
        return None;
    }
    Some(s.to_string())
}

fn is_builtin_go_type(t: &str) -> bool {
    matches!(
        t,
        "string"
            | "int"
            | "int8"
            | "int16"
            | "int32"
            | "int64"
            | "uint"
            | "uint8"
            | "uint16"
            | "uint32"
            | "uint64"
            | "float32"
            | "float64"
            | "bool"
            | "byte"
            | "rune"
            | "error"
            | "any"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GO_SRC: &str = r#"
package service

import (
    "fmt"
    "myapp/repo"
)

type Reader interface {
    Read(p []byte) (int, error)
}

type Service struct {
    repo *repo.Repo
    name string
}

func (s *Service) Find(id string) (*User, error) { return nil, nil }

func helper(x int) int { return x }
"#;

    #[test]
    fn parse_go_structures() {
        let ext = parse_go_file(GO_SRC).unwrap();
        assert_eq!(ext.language, "go");
        assert_eq!(ext.package, "service");
        assert!(ext.imports.contains(&"fmt".to_string()));
        assert!(ext.imports.contains(&"myapp/repo".to_string()));

        let reader = ext.types.iter().find(|t| t.name == "Reader").unwrap();
        assert_eq!(reader.kind, "interface");
        assert!(reader.methods.iter().any(|m| m.name == "Read"));

        let svc = ext.types.iter().find(|t| t.name == "Service").unwrap();
        assert_eq!(svc.kind, "class");
        assert!(svc
            .fields
            .iter()
            .any(|f| f.name == "repo" && f.type_name == "*repo.Repo"));
        // `Find` attached to the receiver type declared in this file.
        assert!(svc.methods.iter().any(|m| m.name == "Find"));
        // module function `helper` (no receiver).
        assert!(ext.module_functions.iter().any(|f| f.name == "helper"));
    }

    #[test]
    fn parse_empty() {
        let ext = parse_go_file("").unwrap();
        assert!(ext.types.is_empty());
    }
}
