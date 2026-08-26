//! Heuristic fallback extractor for languages without a dedicated
//! tree-sitter grammar compiled in (Kotlin, Swift, Elixir, Lua, Clojure,
//! Erlang, Nim, Zig, …).
//!
//! Line/regex-based: captures classes/interfaces/modules (declared with
//! common keyword syntaxes), functions, and dependency-ish imports. Far
//! less precise than a grammar, but enough to seed the structural graph
//! with useful nodes so NeuroCode is never fully disabled on a project
//! just because its language isn't compiled in.

use super::extract::{ExtractedMethod, ExtractedType, SourceExtraction};

/// A coarse language family detected from the extension, driving which
/// heuristic patterns apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeuristicFamily {
    /// `class Foo < Bar`, `def method`, `require`/`load`.
    Ruby,
    /// `class Foo extends Bar implements I`, `function foo()`.
    CStyle,
    /// `fun class`, `package/imports` (Kotlin/Swift-ish keywords overlap).
    SwiftLike,
    /// Unknown family — generic patterns only.
    Generic,
}

impl HeuristicFamily {
    fn from_ext(ext: &str) -> Self {
        match ext {
            "rb" => HeuristicFamily::Ruby,
            "php" | "kt" | "kts" | "cs" | "cpp" | "cc" | "cxx" | "c" | "h" | "hpp"
            | "hh" | "scala" | "groovy" | "dart" => HeuristicFamily::CStyle,
            "swift" | "m" | "mm" => HeuristicFamily::SwiftLike,
            _ => HeuristicFamily::Generic,
        }
    }
}

/// Parse any source string heuristically. `ext` is the file extension
/// without the dot (used to pick the pattern family).
pub fn parse_heuristic_file(source: &str, ext: &str) -> Result<SourceExtraction, String> {
    let family = HeuristicFamily::from_ext(ext);
    let mut extraction = SourceExtraction {
        language: format!("heuristic:{}", ext),
        ..Default::default()
    };

    let mut imports = Vec::new();
    let mut types: Vec<ExtractedType> = Vec::new();
    let mut module_functions = Vec::new();

    for (idx, raw) in source.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        // Comments: `//` always; `#` except C-family `#include`/`#define`
        // directives (which are handled as imports below).
        if line.starts_with("//")
            || (line.starts_with('#')
                && !line.starts_with("#include")
                && !line.starts_with("#define")
                && !line.starts_with("#import"))
        {
            continue;
        }

        // ── Imports ────────────────────────────────────────────────
        if let Some(imp) = heuristic_import(line, family) {
            if !imports.contains(&imp) {
                imports.push(imp);
            }
            continue;
        }

        // ── Type declarations ──────────────────────────────────────
        if let Some((name, kind, bases)) = heuristic_type_decl(line, family) {
            if !types.iter().any(|t| t.name == name) {
                types.push(ExtractedType {
                    name,
                    kind: kind.to_string(),
                    package: String::new(),
                    fq_name: None,
                    implemented_interfaces: bases,
                    annotations: Vec::new(),
                    declared_dependencies: Vec::new(),
                    methods: Vec::new(),
                    fields: Vec::new(),
                    start_byte: prefix_len(source, idx) as u32,
                    end_byte: prefix_len(source, idx + 1) as u32,
                });
            }
            continue;
        }

        // ── Functions ──────────────────────────────────────────────
        if let Some(name) = heuristic_function_decl(line, family) {
            let m = ExtractedMethod {
                name,
                annotations: Vec::new(),
                signature: Some(raw.trim().to_string()),
                start_byte: prefix_len(source, idx) as u32,
                end_byte: prefix_len(source, idx + 1) as u32,
            };
            // Attach to the most recent type if the line is indented under
            // one; else a module function.
            let indented = raw.starts_with(' ') || raw.starts_with('\t');
            if indented {
                if let Some(last) = types.last_mut() {
                    last.methods.push(m);
                    continue;
                }
            }
            module_functions.push(m);
        }
    }

    extraction.imports = imports;
    extraction.types = types;
    extraction.module_functions = module_functions;
    Ok(extraction)
}

/// Byte offset of the start of line `idx` (0-based).
fn prefix_len(source: &str, idx: usize) -> usize {
    source
        .lines()
        .take(idx)
        .map(|l| l.len() + 1)
        .sum::<usize>()
        .min(source.len())
}

/// Recognize import-ish lines → normalized import string.
fn heuristic_import(line: &str, family: HeuristicFamily) -> Option<String> {
    let stripped = line.trim_end_matches(';').trim();
    match family {
        HeuristicFamily::Ruby => {
            for kw in ["require_relative ", "require ", "load "] {
                if let Some(rest) = stripped.strip_prefix(kw) {
                    return Some(format!("{}{}", kw.trim(), rest));
                }
            }
            None
        }
        HeuristicFamily::CStyle | HeuristicFamily::SwiftLike => {
            // #include <x> / "x"; use Foo\Bar; import a.b.C; using X.Y;
            if let Some(rest) = stripped.strip_prefix("#include") {
                return Some(format!("#include{}", rest.trim()));
            }
            for kw in ["import ", "use ", "using ", "include "] {
                if let Some(rest) = stripped.strip_prefix(kw) {
                    let rest = rest.trim();
                    if rest.is_empty() {
                        return None;
                    }
                    return Some(rest.to_string());
                }
            }
            None
        }
        HeuristicFamily::Generic => {
            for kw in ["import ", "use ", "require ", "include ", "from "] {
                if let Some(rest) = stripped.strip_prefix(kw) {
                    let rest = rest.trim();
                    if rest.is_empty() {
                        return None;
                    }
                    return Some(rest.to_string());
                }
            }
            None
        }
    }
}

/// Recognize type declarations → (name, "class"|"interface", base types).
fn heuristic_type_decl(line: &str, family: HeuristicFamily) -> Option<(String, &'static str, Vec<String>)> {
    let line = line.trim_end_matches('{').trim_end_matches(':').trim();
    let tokens: Vec<&str> = line.split_whitespace().collect();
    match family {
        HeuristicFamily::Ruby => {
            // `class Foo < Bar::Baz` / `module Foo`
            if tokens.first() == Some(&"class") && tokens.len() >= 2 {
                let name = clean_ident(tokens[1])?;
                let mut bases = Vec::new();
                if let Some(pos) = tokens.iter().position(|t| *t == "<") {
                    for t in &tokens[pos + 1..] {
                        if let Some(b) = clean_ident(t) {
                            bases.push(b);
                        }
                    }
                }
                return Some((name, "class", bases));
            }
            if tokens.first() == Some(&"module") && tokens.len() >= 2 {
                let name = clean_ident(tokens[1])?;
                return Some((name, "class", Vec::new()));
            }
            None
        }
        HeuristicFamily::CStyle => {
            // `class Foo extends Bar implements I1, I2`
            // `interface Foo : IBar`
            // `struct Foo`
            // `trait Foo`
            let is_iface = tokens.first() == Some(&"interface") || tokens.first() == Some(&"trait");
            let is_class = tokens.first() == Some(&"class")
                || tokens.first() == Some(&"struct")
                || tokens.first() == Some(&"enum")
                || tokens.first() == Some(&"data");
            if !(is_iface || is_class) || tokens.len() < 2 {
                return None;
            }
            let name = clean_ident(tokens[1])?;
            let mut bases = Vec::new();
            let mut expect_base = false;
            for t in &tokens[2..] {
                if matches!(*t, "extends" | "implements" | ":") {
                    expect_base = true;
                    continue;
                }
                if expect_base {
                    if let Some(b) = clean_ident(t) {
                        bases.push(b);
                    }
                }
            }
            let kind = if is_iface || tokens.first() == Some(&"trait") {
                "interface"
            } else if tokens.first() == Some(&"enum") {
                "enum"
            } else {
                "class"
            };
            Some((name, kind, bases))
        }
        HeuristicFamily::SwiftLike => {
            // `class Foo: Bar`, `struct Foo: P`, `protocol Foo`
            let is_proto = tokens.first() == Some(&"protocol");
            let is_type = tokens.first() == Some(&"class")
                || tokens.first() == Some(&"struct")
                || is_proto
                || tokens.first() == Some(&"enum")
                || tokens.first() == Some(&"@interface")
                || tokens.first() == Some(&"@implementation");
            if !is_type || tokens.len() < 2 {
                return None;
            }
            let name = clean_ident(tokens[1])?;
            let mut bases = Vec::new();
            if let Some(colon) = tokens.iter().position(|t| *t == ":") {
                for t in &tokens[colon + 1..] {
                    if let Some(b) = clean_ident(t) {
                        bases.push(b);
                    }
                }
            }
            let kind = if is_proto || tokens.first() == Some(&"@interface") {
                "interface"
            } else if tokens.first() == Some(&"enum") {
                "enum"
            } else {
                "class"
            };
            Some((name, kind, bases))
        }
        HeuristicFamily::Generic => {
            // Generic: `class X` / `def X(...)`-style modules are handled
            // in function detection; only bare `class`/`interface` here.
            if (tokens.first() == Some(&"class") || tokens.first() == Some(&"interface"))
                && tokens.len() >= 2
            {
                let name = clean_ident(tokens[1])?;
                let kind = if tokens.first() == Some(&"interface") {
                    "interface"
                } else {
                    "class"
                };
                return Some((name, kind, Vec::new()));
            }
            None
        }
    }
}

/// Recognize function declarations → name.
fn heuristic_function_decl(line: &str, family: HeuristicFamily) -> Option<String> {
    let line = line.trim_end_matches('{').trim();
    match family {
        HeuristicFamily::Ruby => {
            // `def foo(...)` or `def Foo.bar`
            let rest = line.strip_prefix("def ")?.trim();
            let name = rest.split('(').next()?.trim();
            let name = name.rsplit('.').next().unwrap_or(name);
            clean_ident(name)
        }
        HeuristicFamily::CStyle | HeuristicFamily::SwiftLike => {
            // `function foo(`, `fun foo(`, `func foo(`, `fn foo(`,
            // `public static void foo(` — take the identifier before '('.
            for kw in ["function ", "fun ", "func ", "fn ", "def ", "sub "] {
                if let Some(rest) = line.strip_prefix(kw) {
                    let name = rest.split('(').next()?.trim();
                    return clean_ident(name);
                }
            }
            // C/C++/Java-style return-type functions: identifier before '('
            // with at least two tokens and no statement keywords.
            if line.contains('(')
                && !line.contains('=')
                && !line.starts_with("if")
                && !line.starts_with("for")
                && !line.starts_with("while")
                && !line.starts_with("switch")
                && !line.starts_with("catch")
                && !line.starts_with("return")
                && !line.starts_with("//")
            {
                let before_paren = line.split('(').next()?.trim();
                let last = before_paren.split_whitespace().last()?;
                // Skip control-flow keywords misdetected.
                if matches!(
                    last,
                    "if" | "for" | "while" | "switch" | "catch" | "return" | "do" | "else"
                ) {
                    return None;
                }
                let name = last.split("::").last().unwrap_or(last);
                return clean_ident(name);
            }
            None
        }
        HeuristicFamily::Generic => {
            for kw in ["def ", "function ", "fn ", "func ", "fun ", "sub ", "proc "] {
                if let Some(rest) = line.strip_prefix(kw) {
                    let name = rest.split('(').next()?.trim();
                    return clean_ident(name);
                }
            }
            None
        }
    }
}

/// Clean an identifier: strip modifiers/annotations/generics noise.
fn clean_ident(t: &str) -> Option<String> {
    let t = t
        .trim()
        .trim_end_matches(',')
        .trim_end_matches('<')
        .trim_end_matches('(')
        .trim_end_matches(':')
        .trim_matches('@');
    if t.is_empty() {
        return None;
    }
    // Must look like an identifier (letters, digits, _, ::, ., -).
    if !t
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == ':' || c == '.' || c == '-')
    {
        return None;
    }
    // Must start with a letter or underscore.
    if !t.chars().next().map_or(false, |c| c.is_alphabetic() || c == '_') {
        return None;
    }
    // Skip language keywords that slip through.
    if matches!(
        t,
        "static"
            | "public"
            | "private"
            | "protected"
            | "internal"
            | "final"
            | "abstract"
            | "override"
            | "virtual"
            | "const"
            | "let"
            | "var"
            | "val"
            | "new"
            | "this"
            | "self"
            | "super"
            | "void"
            | "return"
            | "class"
            | "interface"
            | "struct"
            | "enum"
            | "trait"
            | "module"
            | "namespace"
            | "data"
            | "type"
            | "where"
            | "init"
            | "constructor"
    ) {
        return None;
    }
    Some(t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruby_class_and_methods() {
        let src = r#"
require 'json'
require_relative 'helper'

class UserService < BaseService
  def find(id)
    @store[id]
  end

  def delete(id)
    @store.delete(id)
  end
end

module Billing
  def charge(x); end
end
"#;
        let ext = parse_heuristic_file(src, "rb").unwrap();
        assert!(ext.imports.iter().any(|i| i.contains("json")));
        let svc = ext.types.iter().find(|t| t.name == "UserService").unwrap();
        assert_eq!(svc.kind, "class");
        assert!(svc.implemented_interfaces.contains(&"BaseService".to_string()));
        assert!(svc.methods.iter().any(|m| m.name == "find"));
        assert!(svc.methods.iter().any(|m| m.name == "delete"));
        let billing = ext.types.iter().find(|t| t.name == "Billing").unwrap();
        assert!(billing.methods.iter().any(|m| m.name == "charge"));
    }

    #[test]
    fn php_class() {
        let src = r#"
<?php
use App\Repositories\UserRepository;

class UserService extends BaseService implements UserServiceInterface {
    public function find($id) { return $this->repo->find($id); }
}
"#;
        let ext = parse_heuristic_file(src, "php").unwrap();
        let svc = ext.types.iter().find(|t| t.name == "UserService").unwrap();
        assert_eq!(svc.kind, "class");
        assert!(svc
            .implemented_interfaces
            .contains(&"UserServiceInterface".to_string()));
        assert!(svc.methods.iter().any(|m| m.name == "find"));
        assert!(ext.imports.iter().any(|i| i.contains("UserRepository")));
    }

    #[test]
    fn c_header() {
        let src = "#include \"utils.h\"\n\nstruct Point {\n  int x;\n};\n\nint compute(int a) { return a; }\n";
        let ext = parse_heuristic_file(src, "c").unwrap();
        assert!(ext.types.iter().any(|t| t.name == "Point"));
        assert!(ext
            .module_functions
            .iter()
            .any(|f| f.name == "compute"));
    }

    #[test]
    fn generic_unknown_extension() {
        let src = "def do_thing(x)\n  x\nend\n";
        let ext = parse_heuristic_file(src, "ex").unwrap();
        assert!(ext
            .module_functions
            .iter()
            .any(|f| f.name == "do_thing"));
    }
}
