//! JSON-Schema sanitizer for tool parameters — full port of
//! `tools/schema_sanitizer.py`.
//!
//! Fixes the known-hostile constructs strict backends reject: bare-string
//! schema values, missing `properties` on object nodes, `type` arrays,
//! nullable `anyOf`/`oneOf` unions (with title/description/default/examples
//! carry-over), top-level combinators, `$ref` siblings, and stale `required`
//! entries. The reactive strippers `strip_pattern_and_format` and
//! `strip_slash_enum` are public for backend-recovery paths.

use serde_json::{json, Map, Value};

/// Sanitize a full OpenAI-format tool list (`sanitize_tool_schemas`).
pub fn sanitize_tool_schemas(tools: &[Value]) -> Vec<Value> {
    tools.iter().map(sanitize_single_tool).collect()
}

fn sanitize_single_tool(tool: &Value) -> Value {
    let mut out = tool.clone();
    let Some(fn_obj) = out.get_mut("function").and_then(|f| f.as_object_mut()) else {
        return out;
    };
    let params = fn_obj.get("parameters").cloned();
    let name = fn_obj.get("name").and_then(|n| n.as_str()).unwrap_or("<tool>").to_string();
    let sanitized = match params {
        Some(p) if p.is_object() => sanitize_parameters_named(p, &name),
        _ => json!({"type": "object", "properties": {}}),
    };
    fn_obj.insert("parameters".to_string(), sanitized);
    out
}

/// Sanitize a bare parameters schema (used by the registry when building
/// definitions). Mirrors `_sanitize_single_tool`'s parameter pipeline.
pub fn sanitize_parameters(params: Value) -> Value {
    sanitize_parameters_named(params, "<tool>")
}

fn sanitize_parameters_named(params: Value, name: &str) -> Value {
    if !params.is_object() && !params.is_string() {
        return json!({"type": "object", "properties": {}});
    }
    let mut top = sanitize_node(params, name);
    // After recursion, guarantee the top-level is an object with properties.
    match top.as_object_mut() {
        Some(map) => {
            if map.get("type").and_then(|t| t.as_str()) != Some("object") {
                map.insert("type".to_string(), json!("object"));
            }
            if !map.get("properties").map(|p| p.is_object()).unwrap_or(false) {
                map.insert("properties".to_string(), json!({}));
            }
        }
        None => return json!({"type": "object", "properties": {}}),
    }
    let top = strip_nullable_unions(top, true);
    let top = strip_top_level_combinators(top);
    strip_ref_siblings(top)
}

/// Sibling keywords strict JSON Schema validators reject alongside `$ref`.
const REF_FORBIDDEN_SIBLINGS: &[&str] = &["default"];

/// Port of `_strip_ref_siblings`.
pub fn strip_ref_siblings(node: Value) -> Value {
    match node {
        Value::Array(items) => Value::Array(items.into_iter().map(strip_ref_siblings).collect()),
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                out.insert(k, strip_ref_siblings(v));
            }
            if out.contains_key("$ref") {
                for key in REF_FORBIDDEN_SIBLINGS {
                    out.remove(*key);
                }
            }
            Value::Object(out)
        }
        other => other,
    }
}

const TOP_LEVEL_FORBIDDEN_KEYS: &[&str] = &["allOf", "anyOf", "oneOf", "enum", "not"];

/// Port of `_strip_top_level_combinators` — only the outermost level.
pub fn strip_top_level_combinators(params: Value) -> Value {
    let Value::Object(mut map) = params else {
        return params;
    };
    for key in TOP_LEVEL_FORBIDDEN_KEYS {
        map.remove(*key);
    }
    Value::Object(map)
}

/// Port of `strip_nullable_unions` — collapse `anyOf`/`oneOf` nullable unions
/// to the non-null branch, carrying over title/description/default/examples.
pub fn strip_nullable_unions(schema: Value, keep_nullable_hint: bool) -> Value {
    match schema {
        Value::Array(items) => Value::Array(
            items.into_iter().map(|i| strip_nullable_unions(i, keep_nullable_hint)).collect(),
        ),
        Value::Object(map) => {
            let mut stripped = Map::new();
            for (k, v) in map {
                stripped.insert(k, strip_nullable_unions(v, keep_nullable_hint));
            }
            for key in ["anyOf", "oneOf"] {
                let Some(Value::Array(variants)) = stripped.get(key) else {
                    continue;
                };
                let non_null: Vec<Value> = variants
                    .iter()
                    .filter(|item| {
                        !(item.is_object()
                            && item.get("type").and_then(|t| t.as_str()) == Some("null"))
                    })
                    .cloned()
                    .collect();
                // Only collapse when a null branch was dropped AND exactly one
                // non-null branch survives.
                if non_null.len() == 1 && non_null.len() != variants.len() {
                    let mut replacement = match &non_null[0] {
                        Value::Object(m) => m.clone(),
                        _ => Map::new(),
                    };
                    if keep_nullable_hint && !replacement.contains_key("nullable") {
                        replacement.insert("nullable".to_string(), json!(true));
                    }
                    for meta_key in ["title", "description", "default", "examples"] {
                        if let Some(meta) = stripped.get(meta_key) {
                            if !replacement.contains_key(meta_key) {
                                // `default` is illegal alongside `$ref` on strict backends.
                                if meta_key == "default" && replacement.contains_key("$ref") {
                                    continue;
                                }
                                replacement.insert(meta_key.to_string(), meta.clone());
                            }
                        }
                    }
                    return strip_nullable_unions(Value::Object(replacement), keep_nullable_hint);
                }
            }
            Value::Object(stripped)
        }
        other => other,
    }
}

const BARE_TYPE_STRINGS: &[&str] =
    &["object", "string", "number", "integer", "boolean", "array", "null"];

/// Port of `_sanitize_node` — the recursive shape fixer.
fn sanitize_node(node: Value, path: &str) -> Value {
    match node {
        // Malformed: the schema position holds a bare string like "object".
        Value::String(s) => {
            if BARE_TYPE_STRINGS.contains(&s.as_str()) {
                if s == "object" {
                    json!({"type": "object", "properties": {}})
                } else {
                    json!({"type": s})
                }
            } else {
                json!({"type": "object", "properties": {}})
            }
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .enumerate()
                .map(|(i, item)| sanitize_node(item, &format!("{}[{}]", path, i)))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, value) in map {
                // JSON Schema `type` arrays → single type / anyOf / null-object fallback.
                if key == "type" {
                    if let Value::Array(types) = &value {
                        let has_null = types.iter().any(|t| t.as_str() == Some("null"));
                        let non_null: Vec<String> = types
                            .iter()
                            .filter_map(|t| t.as_str())
                            .filter(|t| *t != "null")
                            .map(str::to_string)
                            .collect();
                        if non_null.len() == 1 {
                            out.insert("type".to_string(), json!(non_null[0]));
                            if has_null && !out.contains_key("nullable") {
                                out.insert("nullable".to_string(), json!(true));
                            }
                            continue;
                        }
                        if non_null.len() >= 2 {
                            out.insert(
                                "anyOf".to_string(),
                                Value::Array(non_null.iter().map(|t| json!({"type": t})).collect()),
                            );
                            if has_null && !out.contains_key("nullable") {
                                out.insert("nullable".to_string(), json!(true));
                            }
                            continue;
                        }
                        // All-null / garbage type arrays → "null" / "object".
                        out.insert(
                            "type".to_string(),
                            json!(if has_null { "null" } else { "object" }),
                        );
                        continue;
                    }
                }

                if ["properties", "$defs", "definitions"].contains(&key.as_str()) {
                    if let Value::Object(props) = value {
                        let mut sub = Map::new();
                        for (sub_k, sub_v) in props {
                            let sanitized =
                                sanitize_node(sub_v, &format!("{}.{}.{}", path, key, sub_k));
                            sub.insert(sub_k, sanitized);
                        }
                        out.insert(key, Value::Object(sub));
                        continue;
                    }
                    out.insert(key, value);
                } else if ["items", "additionalProperties"].contains(&key.as_str()) {
                    if value.is_boolean() {
                        out.insert(key, value);
                    } else {
                        let p = format!("{}.{}", path, key);
                        out.insert(key, sanitize_node(value, &p));
                    }
                } else if ["anyOf", "oneOf", "allOf"].contains(&key.as_str()) && value.is_array() {
                    let Value::Array(items) = value else { unreachable!() };
                    let sanitized: Vec<Value> = items
                        .into_iter()
                        .enumerate()
                        .map(|(i, item)| sanitize_node(item, &format!("{}.{}[{}]", path, key, i)))
                        .collect();
                    out.insert(key, Value::Array(sanitized));
                } else if ["required", "enum", "examples"].contains(&key.as_str()) {
                    // Sibling keywords whose values are NOT schemas — pass through.
                    out.insert(key, value);
                } else if value.is_object() || value.is_array() {
                    let p = format!("{}.{}", path, key);
                    out.insert(key, sanitize_node(value, &p));
                } else {
                    out.insert(key, value);
                }
            }

            // Object nodes without properties: inject empty properties dict.
            if out.get("type").and_then(|t| t.as_str()) == Some("object")
                && !out.get("properties").map(|p| p.is_object()).unwrap_or(false)
            {
                out.insert("properties".to_string(), json!({}));
            }

            // Prune `required` entries that don't exist in properties; remove
            // the key entirely when nothing valid remains.
            if out.get("type").and_then(|t| t.as_str()) == Some("object") {
                if let Some(Value::Array(required)) = out.get("required").cloned() {
                    let props: Vec<String> = out
                        .get("properties")
                        .and_then(|p| p.as_object())
                        .map(|p| p.keys().cloned().collect())
                        .unwrap_or_default();
                    let valid: Vec<Value> = required
                        .iter()
                        .filter(|r| {
                            r.as_str().map(|s| props.iter().any(|p| p == s)).unwrap_or(false)
                        })
                        .cloned()
                        .collect();
                    if valid.is_empty() {
                        out.remove("required");
                    } else if valid.len() != required.len() {
                        out.insert("required".to_string(), Value::Array(valid));
                    }
                }
            }

            Value::Object(out)
        }
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Reactive strippers — invoked only after a backend rejects a schema.
// ---------------------------------------------------------------------------

const STRIP_ON_RECOVERY_KEYS: &[&str] = &["pattern", "format"];

/// Port of `strip_pattern_and_format` — mutates the list in place, returns the
/// number of keywords stripped.
pub fn strip_pattern_and_format(tools: &mut [Value]) -> usize {
    fn walk(node: &mut Value, stripped: &mut usize) {
        match node {
            Value::Object(map) => {
                let is_schema_node = map.contains_key("type")
                    || map.contains_key("anyOf")
                    || map.contains_key("oneOf")
                    || map.contains_key("allOf");
                let keys: Vec<String> = map.keys().cloned().collect();
                for key in keys {
                    if is_schema_node && STRIP_ON_RECOVERY_KEYS.contains(&key.as_str()) {
                        map.remove(&key);
                        *stripped += 1;
                        continue;
                    }
                    if let Some(v) = map.get_mut(&key) {
                        walk(v, stripped);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, stripped);
                }
            }
            _ => {}
        }
    }

    let mut stripped = 0usize;
    for tool in tools.iter_mut() {
        if !tool.is_object() {
            continue;
        }
        // OpenAI-format: {"function": {"parameters": {...}}}
        if let Some(params) = tool
            .get_mut("function")
            .and_then(|f| f.as_object_mut())
            .and_then(|f| f.get_mut("parameters"))
        {
            if params.is_object() {
                walk(params, &mut stripped);
                continue;
            }
        }
        // Responses-format: {"name": ..., "parameters": {...}}
        if let Some(params) = tool.get_mut("parameters") {
            if params.is_object() {
                walk(params, &mut stripped);
            }
        }
    }
    stripped
}

/// Port of `strip_slash_enum` — drop `enum` keywords whose string values
/// contain a forward slash. Mutates in place, returns stripped count.
pub fn strip_slash_enum(tools: &mut [Value]) -> usize {
    fn walk(node: &mut Value, stripped: &mut usize) {
        match node {
            Value::Object(map) => {
                let has_slash_enum = matches!(
                    map.get("enum"),
                    Some(Value::Array(items)) if items.iter().any(
                        |v| v.as_str().map(|s| s.contains('/')).unwrap_or(false)
                    )
                );
                if has_slash_enum {
                    map.remove("enum");
                    *stripped += 1;
                }
                for v in map.values_mut() {
                    walk(v, stripped);
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, stripped);
                }
            }
            _ => {}
        }
    }

    let mut stripped = 0usize;
    for tool in tools.iter_mut() {
        if !tool.is_object() {
            continue;
        }
        if let Some(params) = tool
            .get_mut("function")
            .and_then(|f| f.as_object_mut())
            .and_then(|f| f.get_mut("parameters"))
        {
            if params.is_object() {
                walk(params, &mut stripped);
                continue;
            }
        }
        if let Some(params) = tool.get_mut("parameters") {
            if params.is_object() {
                walk(params, &mut stripped);
            }
        }
    }
    stripped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_properties_and_forces_object() {
        let out = sanitize_parameters(json!({"type": "string"}));
        assert_eq!(out["type"], "object");
        assert!(out["properties"].is_object());
        let out2 = sanitize_parameters(json!({"type": "object"}));
        assert!(out2.get("properties").is_some());
    }

    #[test]
    fn bare_string_schema_repair() {
        let out = sanitize_parameters(json!({
            "type": "object",
            "properties": {"x": "string", "y": "object", "z": "gibberish"}
        }));
        assert_eq!(out["properties"]["x"], json!({"type": "string"}));
        assert_eq!(out["properties"]["y"], json!({"type": "object", "properties": {}}));
        assert_eq!(out["properties"]["z"], json!({"type": "object", "properties": {}}));
    }

    #[test]
    fn collapses_type_arrays() {
        let out = sanitize_parameters(json!({
            "type": "object",
            "properties": {
                "a": {"type": ["string", "null"]},
                "b": {"type": ["number", "string"]},
                "c": {"type": ["null"]},
                "d": {"type": []}
            }
        }));
        assert_eq!(out["properties"]["a"]["type"], "string");
        assert_eq!(out["properties"]["a"]["nullable"], true);
        assert_eq!(out["properties"]["b"]["anyOf"], json!([{"type": "number"}, {"type": "string"}]));
        assert_eq!(out["properties"]["c"]["type"], "null");
        assert_eq!(out["properties"]["d"]["type"], "object");
    }

    #[test]
    fn nullable_union_collapse_carries_metadata() {
        let out = sanitize_parameters(json!({
            "type": "object",
            "properties": {
                "x": {
                    "anyOf": [{"type": "string"}, {"type": "null"}],
                    "description": "docs",
                    "default": null
                }
            }
        }));
        let x = &out["properties"]["x"];
        assert_eq!(x["type"], "string");
        assert_eq!(x["nullable"], true);
        assert_eq!(x["description"], "docs");
        assert!(x.get("anyOf").is_none());
    }

    #[test]
    fn strips_top_level_combinators_only() {
        let out = sanitize_parameters(json!({
            "type": "object",
            "allOf": [{"required": ["a"]}],
            "properties": {"a": {"anyOf": [{"type": "string"}, {"type": "integer"}]}}
        }));
        assert!(out.get("allOf").is_none());
        assert!(out["properties"]["a"].get("anyOf").is_some());
    }

    #[test]
    fn ref_sibling_default_stripped() {
        let out = sanitize_parameters(json!({
            "type": "object",
            "properties": {"x": {"$ref": "#/$defs/Foo", "default": null}}
        }));
        assert!(out["properties"]["x"].get("default").is_none());
    }

    #[test]
    fn empty_required_removed() {
        let out = sanitize_parameters(json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "required": ["ghost"]
        }));
        assert!(out.get("required").is_none());
        let out2 = sanitize_parameters(json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "required": ["a", "ghost"]
        }));
        assert_eq!(out2["required"], json!(["a"]));
    }

    #[test]
    fn reactive_strippers() {
        let mut tools = vec![json!({
            "type": "function",
            "function": {
                "name": "t",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "pattern": "\\d+", "format": "regex"},
                        "model": {"type": "string", "enum": ["a/b", "plain"]}
                    }
                }
            }
        })];
        let n = strip_pattern_and_format(&mut tools);
        assert_eq!(n, 2);
        // The property literally NAMED "pattern" survives.
        assert!(tools[0]["function"]["parameters"]["properties"].get("pattern").is_some());
        assert!(tools[0]["function"]["parameters"]["properties"]["pattern"].get("pattern").is_none());

        let n2 = strip_slash_enum(&mut tools);
        assert_eq!(n2, 1);
        assert!(tools[0]["function"]["parameters"]["properties"]["model"].get("enum").is_none());
    }
}
