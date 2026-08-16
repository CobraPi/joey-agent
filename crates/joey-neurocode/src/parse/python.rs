//! tree-sitter-python AST extraction.
//!
//! Parse a `.py` file, extract classes (with base classes as implemented
//! interfaces, decorators as annotations), methods, module-level functions,
//! and imports (plain + from-import + aliased).

use tree_sitter::{Node, Parser};

use super::extract::{ExtractedField, ExtractedMethod, ExtractedType, SourceExtraction};

/// Parse a Python source string and extract structural metadata.
pub fn parse_python_file(source: &str) -> Result<SourceExtraction, String> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .map_err(|e| format!("language error: {}", e))?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "failed to parse".to_string())?;
    let root = tree.root_node();

    let mut extraction = SourceExtraction {
        language: "python".into(),
        ..Default::default()
    };

    // Module name: derive from nothing at hand (file path is known to the
    // caller, not here). Leave package empty; ingestion derives grouping
    // from the file path.
    let mut cursor = root.walk();
    let mut queue = vec![root];
    while let Some(node) = queue.pop() {
        cursor.reset(node);
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i as u32) {
                match child.kind() {
                    "import_statement" => {
                        if let Some(text) = child.utf8_text(source.as_bytes()).ok() {
                            push_import(text, "import ", &mut extraction.imports);
                        }
                    }
                    "import_from_statement" => {
                        if let Some(text) = child.utf8_text(source.as_bytes()).ok() {
                            push_import(text, "from ", &mut extraction.imports);
                        }
                    }
                    "decorated_definition" => {
                        if let Some(def) = child.child_by_field_name("definition") {
                            match def.kind() {
                                "class_definition" => {
                                    if let Some(t) = extract_class(&def, &child, source) {
                                        extraction.types.push(t);
                                    }
                                }
                                "function_definition" => {
                                    if let Some(m) = extract_function(&def, &child, source) {
                                        extraction.module_functions.push(m);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    "class_definition" => {
                        if let Some(t) = extract_class(&child, &child, source) {
                            extraction.types.push(t);
                        }
                    }
                    "function_definition" => {
                        if let Some(m) = extract_function(&child, &child, source) {
                            extraction.module_functions.push(m);
                        }
                    }
                    "future_import_statement" => {}
                    _ => queue.push(child),
                }
            }
        }
    }

    Ok(extraction)
}

/// Clean and record an import line.
fn push_import(text: &str, prefix: &str, imports: &mut Vec<String>) {
    let cleaned = text.trim().trim_start_matches(prefix).trim();
    if !cleaned.is_empty() && !imports.iter().any(|i| i == cleaned) {
        imports.push(cleaned.to_string());
    }
}

/// Extract a class_definition (decorators live on the wrapping
/// decorated_definition when present).
fn extract_class<'a>(
    def: &Node<'a>,
    wrapper: &Node<'a>,
    source: &str,
) -> Option<ExtractedType> {
    let name = def
        .child_by_field_name("name")?
        .utf8_text(source.as_bytes())
        .ok()?
        .trim()
        .to_string();

    // Base classes from the superclasses argument_list.
    let mut implemented_interfaces = Vec::new();
    if let Some(supers) = def.child_by_field_name("superclasses") {
        collect_arg_identifiers(&supers, source, &mut implemented_interfaces);
    }

    // Decorators (from the decorated_definition wrapper when present).
    let annotations = decorators(wrapper, source);

    let mut methods = Vec::new();
    let mut fields = Vec::new();
    let mut declared_dependencies = Vec::new();

    if let Some(body) = def.child_by_field_name("body") {
        let mut queue = vec![body];
        while let Some(node) = queue.pop() {
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i as u32) {
                    match child.kind() {
                        "decorated_definition" => {
                            if let Some(inner) = child.child_by_field_name("definition") {
                                if inner.kind() == "function_definition" {
                                    if let Some(m) = extract_function(&inner, &child, source) {
                                        methods.push(m);
                                    }
                                }
                            }
                        }
                        "function_definition" => {
                            if let Some(m) = extract_function(&child, &child, source) {
                                methods.push(m);
                            }
                        }
                        // Annotated assignment at class level → field.
                        "expression_statement" => {
                            if let Some(assign) = child.named_child(0) {
                                if assign.kind() == "assignment" {
                                    if let Some(f) = extract_typed_field(&assign, source) {
                                        fields.push(f);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        // Python has no declared type dependencies at class level beyond
        // base classes; constructor-injected deps are dynamic. Field type
        // names serve as the dependency hints.
        for f in &fields {
            if !f.type_name.is_empty()
                && f.type_name != "str"
                && f.type_name != "int"
                && f.type_name != "float"
                && f.type_name != "bool"
                && !declared_dependencies.contains(&f.type_name)
            {
                declared_dependencies.push(f.type_name.clone());
            }
        }
    }

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
        end_byte: def.end_byte() as u32,
    })
}

/// Extract a function_definition into a method (decorators from wrapper).
fn extract_function<'a>(
    def: &Node<'a>,
    wrapper: &Node<'a>,
    source: &str,
) -> Option<ExtractedMethod> {
    let name = def
        .child_by_field_name("name")?
        .utf8_text(source.as_bytes())
        .ok()?
        .trim()
        .to_string();
    // Signature: decorators + `def name(params) -> ret` — from the wrapper's
    // first byte (so decorators ride along) through the `:` body marker.
    let mut end = def.end_byte();
    if let Some(body) = def.child_by_field_name("body") {
        end = end.min(body.start_byte());
    }
    // Cut the trailing ':' between header and body.
    let header = if end <= wrapper.start_byte() || end > source.len() {
        None
    } else {
        source[wrapper.start_byte()..end]
            .trim_end()
            .strip_suffix(':')
            .map(|s| s.trim_end())
            .map(collapse_ws)
    };
    Some(ExtractedMethod {
        name,
        annotations: decorators(wrapper, source),
        signature: header,
        start_byte: wrapper.start_byte() as u32,
        end_byte: def.end_byte() as u32,
    })
}

/// Collapse runs of whitespace to single spaces.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Collect bare identifier names from an argument_list (base classes).
fn collect_arg_identifiers<'a>(node: &Node<'a>, source: &str, out: &mut Vec<String>) {
    let mut cursor = node.walk();
    cursor.goto_first_child();
    loop {
        let n = cursor.node();
        if n.kind() == "identifier" {
            if let Ok(t) = n.utf8_text(source.as_bytes()) {
                let t = t.trim();
                if !t.is_empty() && !out.iter().any(|x| x == t) {
                    out.push(t.to_string());
                }
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

/// Collect decorator names from a decorated_definition's decorator children.
fn decorators<'a>(wrapper: &Node<'a>, source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for i in 0..wrapper.named_child_count() {
        if let Some(child) = wrapper.named_child(i as u32) {
            if child.kind() == "decorator" {
                // `@foo` or `@foo(...)` — take the function identifier.
                let mut name = String::new();
                let mut cursor = child.walk();
                cursor.goto_first_child();
                loop {
                    let n = cursor.node();
                    if n.kind() == "identifier" {
                        if let Ok(t) = n.utf8_text(source.as_bytes()) {
                            name = t.trim().to_string();
                            break;
                        }
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
                if !name.is_empty() && !out.contains(&name) {
                    out.push(name);
                }
            }
        }
    }
    out
}

/// Extract `name: Type = value` class-level assignments into fields.
fn extract_typed_field<'a>(assign: &Node<'a>, source: &str) -> Option<ExtractedField> {
    let name_node = assign.child_by_field_name("left")?;
    let type_node = assign.child_by_field_name("type")?;
    let name = name_node
        .utf8_text(source.as_bytes())
        .ok()?
        .trim()
        .to_string();
    let type_name = type_node
        .utf8_text(source.as_bytes())
        .ok()?
        .trim()
        .to_string();
    if name.is_empty() {
        return None;
    }
    Some(ExtractedField {
        name: name.clone(),
        type_name: type_name.clone(),
        annotations: Vec::new(),
        signature: Some(format!("{}: {}", name, type_name)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_class_with_bases_and_decorators() {
        let src = r#"
import os
from mypkg.sub import Thing

@dataclass
class Foo(Base, IFace):
    x: int = 3
    repo: Repository

    @property
    def bar(self):
        return 1

def standalone(x):
    pass
"#;
        let ext = parse_python_file(src).unwrap();
        assert_eq!(ext.language, "python");
        assert!(ext.imports.iter().any(|i| i.contains("mypkg.sub")));
        assert_eq!(ext.types.len(), 1);
        let t = &ext.types[0];
        assert_eq!(t.name, "Foo");
        assert!(t.implemented_interfaces.contains(&"Base".to_string()));
        assert!(t.implemented_interfaces.contains(&"IFace".to_string()));
        assert!(t.annotations.contains(&"dataclass".to_string()));
        // Typed fields.
        assert!(t.fields.iter().any(|f| f.name == "repo" && f.type_name == "Repository"));
        assert!(t.declared_dependencies.contains(&"Repository".to_string()));
        // Methods.
        assert_eq!(t.methods.len(), 1);
        assert_eq!(t.methods[0].name, "bar");
        assert!(t.methods[0].annotations.contains(&"property".to_string()));
        // Module function.
        assert_eq!(ext.module_functions.len(), 1);
        assert_eq!(ext.module_functions[0].name, "standalone");
    }

    #[test]
    fn parse_empty() {
        let ext = parse_python_file("").unwrap();
        assert!(ext.types.is_empty());
        assert!(ext.module_functions.is_empty());
    }
}
