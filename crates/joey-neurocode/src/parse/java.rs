//! tree-sitter-java AST extraction (FR-006, T009).
//!
//! Parse a `.java` file, extract classes/interfaces/enums/methods/fields with
//! their annotations, implemented interfaces, and declared dependencies
//! (imports + `@Autowired`/injection points).

use tree_sitter::{Node, Parser};

/// Extracted structural metadata from a Java source file.
#[derive(Debug, Clone, Default)]
pub struct JavaExtraction {
    pub package: String,
    pub imports: Vec<String>,
    pub types: Vec<ExtractedType>,
}

/// A type-level declaration (class, interface, or enum).
#[derive(Debug, Clone)]
pub struct ExtractedType {
    pub name: String,
    pub kind: String, // "class", "interface", "enum"
    pub package: String,
    pub implemented_interfaces: Vec<String>,
    pub annotations: Vec<String>,
    pub declared_dependencies: Vec<String>,
    pub methods: Vec<ExtractedMethod>,
    pub fields: Vec<ExtractedField>,
    pub start_byte: u32,
    pub end_byte: u32,
}

/// A method declaration.
#[derive(Debug, Clone)]
pub struct ExtractedMethod {
    pub name: String,
    pub annotations: Vec<String>,
    pub start_byte: u32,
    pub end_byte: u32,
}

/// A field declaration.
#[derive(Debug, Clone)]
pub struct ExtractedField {
    pub name: String,
    pub type_name: String,
    pub annotations: Vec<String>,
}

/// Parse a Java source string and extract structural metadata.
pub fn parse_java_file(source: &str) -> Result<JavaExtraction, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .map_err(|e| format!("language error: {}", e))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "failed to parse".to_string())?;
    let root = tree.root_node();

    let mut extraction = JavaExtraction::default();

    // Package declaration — the scoped_identifier is the first named child.
    for i in 0..root.named_child_count() {
        if let Some(child) = root.named_child(i as u32) {
            if child.kind() == "package_declaration" {
                if let Some(scope) = child.named_child(0) {
                    extraction.package = scope
                        .utf8_text(source.as_bytes())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                }
            }
        }
    }

    // Imports.
    for i in 0..root.named_child_count() {
        if let Some(child) = root.named_child(i as u32) {
            if child.kind() == "import_declaration" {
                let text = child.utf8_text(source.as_bytes()).unwrap_or("");
                // Extract the scoped identifier after "import" and optional "static".
                let cleaned = text
                    .trim()
                    .trim_start_matches("import")
                    .trim_start_matches("static")
                    .trim()
                    .trim_end_matches(';')
                    .trim()
                    .to_string();
                if !cleaned.is_empty() {
                    extraction.imports.push(cleaned);
                }
            }
        }
    }

    // Walk top-level type declarations.
    for i in 0..root.named_child_count() {
        if let Some(child) = root.named_child(i as u32) {
            match child.kind() {
                "class_declaration" | "interface_declaration" | "enum_declaration"
                | "record_declaration" => {
                    if let Some(ext) = extract_type(&child, source, &extraction.package) {
                        extraction.types.push(ext);
                    }
                }
                "program" | "package_declaration" | "import_declaration" => {}
                _ => {}
            }
        }
    }

    Ok(extraction)
}

/// Extract a type declaration node into an ExtractedType.
fn extract_type<'a>(
    node: &Node<'a>,
    source: &str,
    package: &str,
) -> Option<ExtractedType> {
    let kind = match node.kind() {
        "interface_declaration" => "interface",
        "enum_declaration" => "enum",
        "record_declaration" => "class",
        _ => "class",
    };

    let name = node.child_by_field_name("name")?.utf8_text(source.as_bytes())
        .ok()?
        .trim()
        .to_string();

    // Annotations on the type.
    let annotations = collect_annotations(node, source);

    // Implemented interfaces (super_interfaces field "interfaces" for classes, "extends" for interfaces).
    let mut implemented_interfaces = Vec::new();
    if kind == "class" {
        // tree-sitter-java uses the field name "interfaces" for the super_interfaces node.
        if let Some(supers) = node.child_by_field_name("interfaces") {
            collect_type_list(&supers, source, &mut implemented_interfaces);
        }
    } else if kind == "interface" {
        // Interfaces use "extends_interfaces" or "extends" — try both.
        if let Some(extends_list) = node.child_by_field_name("extends_interfaces") {
            collect_type_list(&extends_list, source, &mut implemented_interfaces);
        } else if let Some(extends_list) = node.child_by_field_name("interfaces") {
            collect_type_list(&extends_list, source, &mut implemented_interfaces);
        }
    }

    // Declared dependencies: fields annotated with @Autowired or injection points.
    let mut declared_dependencies = Vec::new();
    let mut methods = Vec::new();
    let mut fields = Vec::new();

    // The "body" field holds the class/interface body.
    if let Some(body) = node.child_by_field_name("body") {
        for i in 0..body.named_child_count() {
            if let Some(member) = body.named_child(i as u32) {
                match member.kind() {
                    "method_declaration" => {
                        if let Some(m) = extract_method(&member, source) {
                            methods.push(m);
                        }
                    }
                    "field_declaration" => {
                        if let Some(f) = extract_field(&member, source) {
                            if f.annotations.iter().any(|a| {
                                a == "Autowired" || a == "Inject" || a == "PersistenceContext"
                            }) {
                                declared_dependencies.push(f.type_name.clone());
                            }
                            fields.push(f);
                        }
                    }
                    "constructor_declaration" => {
                        // Constructor parameters can be injection points (Spring constructor injection).
                        if let Some(params) = member.child_by_field_name("parameters") {
                            collect_injected_params(&params, source, &mut declared_dependencies);
                        }
                    }
                    "annotation_type_declaration" | "interface_declaration"
                    | "class_declaration" | "enum_declaration" => {
                        // Nested types — skip for now (depth-1 extraction).
                    }
                    _ => {}
                }
            }
        }
    }

    Some(ExtractedType {
        name,
        kind: kind.to_string(),
        package: package.to_string(),
        implemented_interfaces,
        annotations,
        declared_dependencies,
        methods,
        fields,
        start_byte: node.start_byte() as u32,
        end_byte: node.end_byte() as u32,
    })
}

/// Extract a method declaration.
fn extract_method<'a>(node: &Node<'a>, source: &str) -> Option<ExtractedMethod> {
    let name = node.child_by_field_name("name")?.utf8_text(source.as_bytes())
        .ok()?
        .trim()
        .to_string();
    let annotations = collect_annotations(node, source);
    Some(ExtractedMethod {
        name,
        annotations,
        start_byte: node.start_byte() as u32,
        end_byte: node.end_byte() as u32,
    })
}

/// Extract a field declaration. A field_declaration may contain multiple
/// declarators; we extract each.
fn extract_field<'a>(node: &Node<'a>, source: &str) -> Option<ExtractedField> {
    let annotations = collect_annotations(node, source);
    // Type field.
    let type_text = node
        .child_by_field_name("type")
        .and_then(|t| t.utf8_text(source.as_bytes()).ok())
        .unwrap_or("")
        .trim()
        .to_string();
    // Declarator (may have multiple; take first for simplicity).
    let declarator = node.child_by_field_name("declarator")?;
    let name = declarator
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .unwrap_or("")
        .trim()
        .to_string();
    Some(ExtractedField {
        name,
        type_name: type_text,
        annotations,
    })
}

/// Collect annotation names from a node's modifiers/annotations.
fn collect_annotations<'a>(node: &Node<'a>, source: &str) -> Vec<String> {
    let mut annots = Vec::new();
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            if child.kind() == "modifiers" {
                for j in 0..child.named_child_count() {
                    if let Some(ann) = child.named_child(j as u32) {
                        if ann.kind() == "annotation" || ann.kind() == "marker_annotation" {
                            if let Some(name_node) = ann.child_by_field_name("name") {
                                let name = name_node
                                    .utf8_text(source.as_bytes())
                                    .unwrap_or("")
                                    .trim()
                                    .to_string();
                                if !name.is_empty() {
                                    annots.push(name);
                                }
                            }
                        }
                    }
                }
            }
            // Also handle annotations that appear directly (some grammars).
            if child.kind() == "annotation" || child.kind() == "marker_annotation" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = name_node
                        .utf8_text(source.as_bytes())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if !name.is_empty() {
                        annots.push(name);
                    }
                }
            }
        }
    }
    annots
}

/// Collect type names from a type_list / super_interfaces node.
fn collect_type_list<'a>(node: &Node<'a>, source: &str, out: &mut Vec<String>) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            if child.kind() == "type_list" {
                collect_type_list(&child, source, out);
            } else {
                let text = child
                    .utf8_text(source.as_bytes())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !text.is_empty() {
                    // Strip generic parameters from the type name.
                    let base = text.split('<').next().unwrap_or("").trim().to_string();
                    if !base.is_empty() {
                        out.push(base);
                    }
                }
            }
        }
    }
}

/// Collect injected parameters from a formal_parameters node.
fn collect_injected_params<'a>(node: &Node<'a>, source: &str, out: &mut Vec<String>) {
    for i in 0..node.named_child_count() {
        if let Some(param) = node.named_child(i as u32) {
            if param.kind() == "formal_parameter" {
                let annots = collect_annotations(&param, source);
                if annots.iter().any(|a| a == "Autowired" || a == "Inject") {
                    if let Some(type_node) = param.child_by_field_name("type") {
                        let type_text = type_node
                            .utf8_text(source.as_bytes())
                            .unwrap_or("")
                            .trim()
                            .split('<')
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        if !type_text.is_empty() {
                            out.push(type_text);
                        }
                    }
                }
            }
        }
    }
}

/// Helper: extract the type identifier text from a node (unused — kept for reference).

#[cfg(test)]
mod tests {
    use super::*;

    const SPRING_SERVICE: &str = r#"
package com.enterprise.auth.service;

import com.enterprise.auth.repo.UserRepository;
import org.springframework.stereotype.Service;
import org.springframework.transaction.annotation.Transactional;

@Service
@Transactional
public class UserServiceImpl implements UserService {
    @Autowired
    private UserRepository userRepository;

    @Autowired
    private AuditLogger auditLogger;

    public User findById(Long id) {
        return userRepository.findById(id);
    }
}
"#;

    #[test]
    fn parse_spring_service() {
        let ext = parse_java_file(SPRING_SERVICE).unwrap();
        assert_eq!(ext.package, "com.enterprise.auth.service");
        assert_eq!(ext.types.len(), 1);
        let t = &ext.types[0];
        assert_eq!(t.name, "UserServiceImpl");
        assert_eq!(t.kind, "class");
        assert!(t.implemented_interfaces.contains(&"UserService".to_string()));
        assert!(t.annotations.contains(&"Service".to_string()));
        assert!(t.annotations.contains(&"Transactional".to_string()));
        assert!(t.declared_dependencies.contains(&"UserRepository".to_string()));
        // Method.
        assert_eq!(t.methods.len(), 1);
        assert_eq!(t.methods[0].name, "findById");
    }

    const INTERFACE: &str = r#"
package com.enterprise.auth.service;

public interface UserService {
    User findById(Long id);
    void deleteUser(Long id);
}
"#;

    #[test]
    fn parse_interface() {
        let ext = parse_java_file(INTERFACE).unwrap();
        assert_eq!(ext.types.len(), 1);
        let t = &ext.types[0];
        assert_eq!(t.kind, "interface");
        assert_eq!(t.methods.len(), 2);
    }

    const ENUM: &str = r#"
package com.enterprise.auth.model;

public enum Status {
    ACTIVE, INACTIVE, SUSPENDED
}
"#;

    #[test]
    fn parse_enum() {
        let ext = parse_java_file(ENUM).unwrap();
        assert_eq!(ext.types.len(), 1);
        assert_eq!(ext.types[0].kind, "enum");
    }

    #[test]
    fn parse_empty() {
        let ext = parse_java_file("").unwrap();
        assert!(ext.types.is_empty());
    }
}
