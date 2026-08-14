//! tree-sitter-javascript / tree-sitter-typescript AST extraction.
//!
//! One extractor over two grammars (plus TSX): classes (heritage as
//! interfaces), interfaces (TS), enums (TS), methods, module functions,
//! imports. Decorators are captured as annotations.

use tree_sitter::{Node, Parser};

use super::extract::{ExtractedField, ExtractedMethod, ExtractedType, SourceExtraction};

/// Parse a JavaScript source string.
pub fn parse_javascript_file(source: &str) -> Result<SourceExtraction, String> {
    parse_js_like(source, &tree_sitter_javascript::LANGUAGE.into(), "javascript")
}

/// Parse a TypeScript source string.
pub fn parse_typescript_file(source: &str) -> Result<SourceExtraction, String> {
    parse_js_like(
        source,
        &tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "typescript",
    )
}

/// Parse a TSX source string.
pub fn parse_tsx_file(source: &str) -> Result<SourceExtraction, String> {
    parse_js_like(source, &tree_sitter_typescript::LANGUAGE_TSX.into(), "tsx")
}

fn parse_js_like(
    source: &str,
    language: &tree_sitter::Language,
    lang_id: &str,
) -> Result<SourceExtraction, String> {
    let mut parser = Parser::new();
    parser
        .set_language(language)
        .map_err(|e| format!("language error: {}", e))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "failed to parse".to_string())?;
    let root = tree.root_node();

    let mut extraction = SourceExtraction {
        language: lang_id.into(),
        ..Default::default()
    };

    for i in 0..root.named_child_count() {
        if let Some(child) = root.named_child(i as u32) {
            // Unwrap export/decorator wrappers to reach the declaration.
            // Note: export_statement exposes the declaration via the
            // "declaration" field when a decorator precedes it.
            let (decl, exported) = match child.kind() {
                "export_statement" => (
                    child
                        .child_by_field_name("declaration")
                        .or_else(|| child.named_child(0)),
                    true,
                ),
                "decorated_definition" => (child.child_by_field_name("definition"), false),
                _ => (Some(child), false),
            };
            let Some(decl) = decl else { continue };
            match decl.kind() {
                "import_statement" => {
                    if let Ok(text) = child.utf8_text(source.as_bytes()) {
                        let cleaned = text.trim().trim_end_matches(';').trim();
                        if !cleaned.is_empty() && !extraction.imports.contains(&cleaned.to_string())
                        {
                            extraction.imports.push(cleaned.to_string());
                        }
                    }
                }
                "class_declaration" | "abstract_class_declaration" => {
                    if let Some(mut t) = extract_class(&decl, &child, source) {
                        if exported {
                            t.annotations.push("export".into());
                        }
                        extraction.types.push(t);
                    }
                }
                "interface_declaration" => {
                    if let Some(mut t) = extract_interface(&decl, source) {
                        if exported {
                            t.annotations.push("export".into());
                        }
                        extraction.types.push(t);
                    }
                }
                "enum_declaration" => {
                    if let Some(mut t) = extract_enum(&decl, source) {
                        if exported {
                            t.annotations.push("export".into());
                        }
                        extraction.types.push(t);
                    }
                }
                "function_declaration" => {
                    if let Some(m) = extract_method(&decl, source) {
                        extraction.module_functions.push(m);
                    }
                }
                // const x = () => {} → module function.
                "lexical_declaration" | "variable_declaration" => {
                    for v in extract_const_functions(&decl, source) {
                        extraction.module_functions.push(v);
                    }
                }
                _ => {}
            }
        }
    }

    Ok(extraction)
}

/// Extract a class_declaration → ExtractedType.
fn extract_class<'a>(
    decl: &Node<'a>,
    wrapper: &Node<'a>,
    source: &str,
) -> Option<ExtractedType> {
    let name = decl
        .child_by_field_name("name")?
        .utf8_text(source.as_bytes())
        .ok()?
        .trim()
        .to_string();

    let mut implemented_interfaces = Vec::new();
    // class_heritage: extends clause and implements_clause — both are
    // "base types" for graph purposes. Note: class_heritage has no field
    // name in the JS/TS grammars — find it by kind.
    for i in 0..decl.named_child_count() {
        if let Some(child) = decl.named_child(i as u32) {
            if child.kind() == "class_heritage" {
                collect_heritage(&child, source, &mut implemented_interfaces);
            }
        }
    }

    // Decorators sit on the wrapper (export_statement in TS,
    // decorated_definition otherwise).
    let mut annotations = collect_decorators(wrapper, source);

    let mut methods = Vec::new();
    let mut fields = Vec::new();
    if let Some(body) = decl.child_by_field_name("body") {
        for i in 0..body.named_child_count() {
            if let Some(member) = body.named_child(i as u32) {
                match member.kind() {
                    "method_definition" => {
                        if let Some(m) = extract_method(&member, source) {
                            methods.push(m);
                        }
                    }
                    "public_field_definition" => {
                        if let Some(f) = extract_field(&member, source) {
                            fields.push(f);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Field types serve as dependency hints.
    let mut declared_dependencies = Vec::new();
    for f in &fields {
        if !f.type_name.is_empty()
            && !is_builtin_ts_type(&f.type_name)
            && !declared_dependencies.contains(&f.type_name)
        {
            declared_dependencies.push(f.type_name.clone());
        }
    }
    for iface in &implemented_interfaces {
        if !declared_dependencies.contains(iface) {
            declared_dependencies.push(iface.clone());
        }
    }
    annotations.retain(|a| !a.is_empty());

    Some(ExtractedType {
        name,
        kind: "class".into(),
        package: String::new(),
        fq_name: None,
        implemented_interfaces,
        annotations,
        declared_dependencies,
        methods,
        fields,
        start_byte: wrapper.start_byte() as u32,
        end_byte: decl.end_byte() as u32,
    })
}

/// Extract a TS interface_declaration → interface ExtractedType.
fn extract_interface<'a>(decl: &Node<'a>, source: &str) -> Option<ExtractedType> {
    let name = decl
        .child_by_field_name("name")?
        .utf8_text(source.as_bytes())
        .ok()?
        .trim()
        .to_string();
    let mut methods = Vec::new();
    if let Some(body) = decl.child_by_field_name("body") {
        for i in 0..body.named_child_count() {
            if let Some(member) = body.named_child(i as u32) {
                if member.kind() == "method_signature" {
                    if let Some(m) = extract_method(&member, source) {
                        methods.push(m);
                    }
                }
            }
        }
    }
    Some(ExtractedType {
        name,
        kind: "interface".into(),
        package: String::new(),
        fq_name: None,
        implemented_interfaces: Vec::new(),
        annotations: Vec::new(),
        declared_dependencies: Vec::new(),
        methods,
        fields: Vec::new(),
        start_byte: decl.start_byte() as u32,
        end_byte: decl.end_byte() as u32,
    })
}

/// Extract a TS enum_declaration → enum ExtractedType.
fn extract_enum<'a>(decl: &Node<'a>, source: &str) -> Option<ExtractedType> {
    let name = decl
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
        annotations: Vec::new(),
        declared_dependencies: Vec::new(),
        methods: Vec::new(),
        fields: Vec::new(),
        start_byte: decl.start_byte() as u32,
        end_byte: decl.end_byte() as u32,
    })
}

/// Collect identifiers from a class_heritage (extends + implements).
fn collect_heritage<'a>(node: &Node<'a>, source: &str, out: &mut Vec<String>) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            match child.kind() {
                "implements_clause" | "class_heritage" => {
                    collect_heritage(&child, source, out);
                }
                _ => {
                    let text = child
                        .utf8_text(source.as_bytes())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    // The heritage value may be an identifier, a
                    // call_expression (mixin), or a generic instantiation —
                    // take the identifier head.
                    let base = text
                        .split('(')
                        .next()
                        .unwrap_or("")
                        .split('<')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if !base.is_empty() && !out.iter().any(|x| *x == base) {
                        out.push(base);
                    }
                }
            }
        }
    }
}

/// Collect decorator identifiers from a wrapper node. `@Injectable()`
/// nests the identifier inside a call_expression — descend for the first
/// identifier descendant.
fn collect_decorators<'a>(wrapper: &Node<'a>, source: &str) -> Vec<String> {
    fn first_identifier<'a>(node: &Node<'a>, source: &str) -> Option<String> {
        if node.kind() == "identifier" {
            return node
                .utf8_text(source.as_bytes())
                .ok()
                .map(|s| s.trim().to_string());
        }
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i as u32) {
                if let Some(found) = first_identifier(&child, source) {
                    return Some(found);
                }
            }
        }
        None
    }
    let mut out = Vec::new();
    for i in 0..wrapper.named_child_count() {
        if let Some(child) = wrapper.named_child(i as u32) {
            if child.kind() == "decorator" {
                if let Some(name) = first_identifier(&child, source) {
                    if !name.is_empty() && !out.contains(&name) {
                        out.push(name);
                    }
                }
            }
        }
    }
    out
}

/// Extract a method_definition / function_declaration / method_signature
/// → ExtractedMethod.
fn extract_method<'a>(node: &Node<'a>, source: &str) -> Option<ExtractedMethod> {
    let name = node
        .child_by_field_name("name")?
        .utf8_text(source.as_bytes())
        .ok()?
        .trim()
        .to_string();
    Some(ExtractedMethod {
        name,
        annotations: Vec::new(),
        start_byte: node.start_byte() as u32,
        end_byte: node.end_byte() as u32,
    })
}

/// Extract a public_field_definition → ExtractedField (TS-typed only).
/// The `type` field is a type_annotation wrapper whose text includes the
/// leading `:` — strip it.
fn extract_field<'a>(node: &Node<'a>, source: &str) -> Option<ExtractedField> {
    let name = node
        .child_by_field_name("name")?
        .utf8_text(source.as_bytes())
        .ok()?
        .trim()
        .to_string();
    let type_name = node
        .child_by_field_name("type")
        .and_then(|t| t.utf8_text(source.as_bytes()).ok())
        .unwrap_or("")
        .trim()
        .trim_start_matches(':')
        .trim()
        .to_string();
    Some(ExtractedField {
        name,
        type_name,
        annotations: Vec::new(),
    })
}

/// Extract `const x = () => {}` / `= function() {}` → module functions.
fn extract_const_functions<'a>(decl: &Node<'a>, source: &str) -> Vec<ExtractedMethod> {
    let mut out = Vec::new();
    for i in 0..decl.named_child_count() {
        if let Some(v) = decl.named_child(i as u32) {
            if v.kind() != "variable_declarator" {
                continue;
            }
            let Some(name_node) = v.child_by_field_name("name") else {
                continue;
            };
            let Some(value) = v.child_by_field_name("value") else {
                continue;
            };
            if !matches!(value.kind(), "arrow_function" | "function_expression") {
                continue;
            }
            if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                out.push(ExtractedMethod {
                    name: name.trim().to_string(),
                    annotations: Vec::new(),
                    start_byte: v.start_byte() as u32,
                    end_byte: v.end_byte() as u32,
                });
            }
        }
    }
    out
}

fn is_builtin_ts_type(t: &str) -> bool {
    matches!(
        t,
        "string"
            | "number"
            | "boolean"
            | "any"
            | "unknown"
            | "never"
            | "void"
            | "null"
            | "undefined"
            | "object"
            | "symbol"
            | "bigint"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_typescript_service() {
        let src = r#"
import { Utils } from './utils';
import * as path from 'path';

@Injectable()
export class UserService implements IUserService {
    private repo: Repository;

    find(id: string): User | null { return null; }
}

export interface IUserService {
    find(id: string): User | null;
}

export function helper(x: number): number { return x; }
"#;
        let ext = parse_typescript_file(src).unwrap();
        assert_eq!(ext.language, "typescript");
        assert!(ext.imports.iter().any(|i| i.contains("./utils")));
        assert_eq!(ext.types.len(), 2, "class + interface");
        let svc = ext.types.iter().find(|t| t.name == "UserService").unwrap();
        assert_eq!(svc.kind, "class");
        assert!(svc
            .implemented_interfaces
            .contains(&"IUserService".to_string()));
        assert!(svc.annotations.contains(&"Injectable".to_string()));
        assert!(svc.annotations.contains(&"export".to_string()));
        assert!(svc.methods.iter().any(|m| m.name == "find"));
        assert!(svc.declared_dependencies.contains(&"Repository".to_string()));
        let iface = ext.types.iter().find(|t| t.name == "IUserService").unwrap();
        assert_eq!(iface.kind, "interface");
        assert!(iface.methods.iter().any(|m| m.name == "find"));
        assert_eq!(ext.module_functions.len(), 1);
        assert_eq!(ext.module_functions[0].name, "helper");
    }

    #[test]
    fn parse_javascript_class() {
        let src = r#"
import { utils } from './utils';
const fs = require('fs');

export class Client extends Base {
    constructor(opts) { super(opts); this.repo = opts.repo; }
    async fetch(id) { return null; }
}
export function helper(x) { return x; }
"#;
        let ext = parse_javascript_file(src).unwrap();
        assert_eq!(ext.language, "javascript");
        let client = ext.types.iter().find(|t| t.name == "Client").unwrap();
        assert!(client.implemented_interfaces.contains(&"Base".to_string()));
        assert!(client.methods.iter().any(|m| m.name == "fetch"));
        assert!(client.methods.iter().any(|m| m.name == "constructor"));
        assert_eq!(ext.module_functions.len(), 1);
        assert_eq!(ext.module_functions[0].name, "helper");
    }

    #[test]
    fn parse_tsx_component() {
        let src = "export const App = () => <div>hi</div>;";
        let ext = parse_tsx_file(src).unwrap();
        assert_eq!(ext.language, "tsx");
        assert_eq!(ext.module_functions.len(), 1);
        assert_eq!(ext.module_functions[0].name, "App");
    }

    #[test]
    fn parse_empty() {
        let ext = parse_typescript_file("").unwrap();
        assert!(ext.types.is_empty());
    }
}
