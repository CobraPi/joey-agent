//! MCP input-schema normalization (port of `_normalize_mcp_input_schema` in
//! `tools/mcp_tool.py` and `strip_nullable_unions` in
//! `tools/schema_sanitizer.py`).

use serde_json::{json, Map, Value};

/// Python truthiness for JSON values (`if not x`).
fn is_py_falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
    }
}

fn empty_object_schema() -> Value {
    json!({"type": "object", "properties": {}})
}

/// Normalize MCP input schemas for LLM tool-calling compatibility
/// (`_normalize_mcp_input_schema`).
///
/// Repairs applied recursively:
///
/// * Legacy `definitions` / `#/definitions/...` refs are promoted to `$defs`
///   (only when `definitions` appears as a meta-keyword, never as the name of
///   a property).
/// * Nullable unions (`anyOf: [{...}, {"type": "null"}]`) are collapsed to
///   the non-null branch, keeping a `nullable: true` hint.
/// * Missing/null `type` on an object-shaped node is coerced to `"object"`.
/// * `object` nodes are guaranteed a `properties` dict.
/// * `required` arrays are pruned to names that exist in `properties`.
pub fn normalize_mcp_input_schema(schema: Option<&Value>) -> Value {
    let Some(schema) = schema else {
        return empty_object_schema();
    };
    if is_py_falsy(schema) {
        return empty_object_schema();
    }

    let normalized = rewrite_local_refs(schema);
    let normalized = strip_nullable_unions(&normalized, true);
    let normalized = repair_object_shape(&normalized);

    // Ensure top-level is a well-formed object schema.
    let Value::Object(mut map) = normalized else {
        return empty_object_schema();
    };
    if map.get("type").and_then(Value::as_str) == Some("object") && !map.contains_key("properties")
    {
        map.insert("properties".to_string(), json!({}));
    }
    Value::Object(map)
}

/// Walk the schema, promoting legacy `definitions` to `$defs`
/// (`_rewrite_local_refs`).
///
/// The promotion is contextual: `definitions` is renamed only when it appears
/// as a JSON Schema meta-keyword, never when it appears as the name of a
/// property (i.e. as a key inside a `properties` / `patternProperties` map).
fn rewrite_local_refs(node: &Value) -> Value {
    match node {
        Value::Object(map) => {
            let mut normalized = Map::new();
            for (key, value) in map {
                if (key == "properties" || key == "patternProperties") && value.is_object() {
                    // Keys of this map are user-facing property names, not
                    // meta-keywords. Preserve them verbatim; recurse only into
                    // each property's schema.
                    let props = value.as_object().expect("checked is_object");
                    let rewritten: Map<String, Value> = props
                        .iter()
                        .map(|(prop_name, prop_schema)| {
                            (prop_name.clone(), rewrite_local_refs(prop_schema))
                        })
                        .collect();
                    normalized.insert(key.clone(), Value::Object(rewritten));
                } else {
                    let out_key = if key == "definitions" { "$defs" } else { key.as_str() };
                    normalized.insert(out_key.to_string(), rewrite_local_refs(value));
                }
            }
            let new_ref = match normalized.get("$ref") {
                Some(Value::String(r)) => r
                    .strip_prefix("#/definitions/")
                    .map(|rest| format!("#/$defs/{}", rest)),
                _ => None,
            };
            if let Some(r) = new_ref {
                normalized.insert("$ref".to_string(), Value::String(r));
            }
            Value::Object(normalized)
        }
        Value::Array(items) => Value::Array(items.iter().map(rewrite_local_refs).collect()),
        other => other.clone(),
    }
}

/// Collapse `anyOf` / `oneOf` nullable unions to the non-null branch
/// (`tools.schema_sanitizer.strip_nullable_unions`).
///
/// Metadata (`title`, `description`, `default`, `examples`) on the outer union
/// node is carried over to the replacement variant. With `keep_nullable_hint`,
/// `nullable: true` is set on the replacement to preserve the "this field may
/// be None" signal.
pub fn strip_nullable_unions(schema: &Value, keep_nullable_hint: bool) -> Value {
    match schema {
        Value::Array(items) => Value::Array(
            items.iter().map(|item| strip_nullable_unions(item, keep_nullable_hint)).collect(),
        ),
        Value::Object(map) => {
            let stripped: Map<String, Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), strip_nullable_unions(v, keep_nullable_hint)))
                .collect();
            for key in ["anyOf", "oneOf"] {
                let Some(Value::Array(variants)) = stripped.get(key) else {
                    continue;
                };
                let non_null: Vec<&Value> = variants
                    .iter()
                    .filter(|item| {
                        !(item.is_object()
                            && item.get("type").and_then(Value::as_str) == Some("null"))
                    })
                    .collect();
                // Only collapse when we actually dropped a null branch AND
                // exactly one non-null branch survives.
                if non_null.len() == 1 && non_null.len() != variants.len() {
                    let mut replacement = match non_null[0] {
                        Value::Object(m) => m.clone(),
                        _ => Map::new(),
                    };
                    if keep_nullable_hint {
                        replacement
                            .entry("nullable".to_string())
                            .or_insert(Value::Bool(true));
                    }
                    for meta_key in ["title", "description", "default", "examples"] {
                        if stripped.contains_key(meta_key) && !replacement.contains_key(meta_key) {
                            // `default` is illegal alongside `$ref` on strict
                            // backends.
                            if meta_key == "default" && replacement.contains_key("$ref") {
                                continue;
                            }
                            replacement.insert(meta_key.to_string(), stripped[meta_key].clone());
                        }
                    }
                    return strip_nullable_unions(&Value::Object(replacement), keep_nullable_hint);
                }
            }
            Value::Object(stripped)
        }
        other => other.clone(),
    }
}

/// Recursively repair object-shaped nodes: fill type, prune required
/// (`_repair_object_shape`).
fn repair_object_shape(node: &Value) -> Value {
    match node {
        Value::Array(items) => Value::Array(items.iter().map(repair_object_shape).collect()),
        Value::Object(map) => {
            let mut repaired: Map<String, Value> =
                map.iter().map(|(k, v)| (k.clone(), repair_object_shape(v))).collect();

            // Coerce missing / null type when the shape is clearly an object
            // (has properties or required but no type).
            let type_falsy = repaired.get("type").map(is_py_falsy).unwrap_or(true);
            if type_falsy
                && (repaired.contains_key("properties") || repaired.contains_key("required"))
            {
                repaired.insert("type".to_string(), json!("object"));
            }

            if repaired.get("type").and_then(Value::as_str) == Some("object") {
                // Ensure properties exists so required can reference it safely.
                if !matches!(repaired.get("properties"), Some(Value::Object(_))) {
                    repaired.insert("properties".to_string(), json!({}));
                }

                // Prune required to only include names that exist in properties.
                if let Some(Value::Array(required)) = repaired.get("required").cloned() {
                    let props: Vec<String> = repaired
                        .get("properties")
                        .and_then(Value::as_object)
                        .map(|m| m.keys().cloned().collect())
                        .unwrap_or_default();
                    let valid: Vec<Value> = required
                        .iter()
                        .filter(|r| r.as_str().map(|s| props.iter().any(|p| p == s)).unwrap_or(false))
                        .cloned()
                        .collect();
                    if valid.len() != required.len() {
                        if !valid.is_empty() {
                            repaired.insert("required".to_string(), Value::Array(valid));
                        } else {
                            repaired.remove("required");
                        }
                    }
                }
            }

            Value::Object(repaired)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_schema_becomes_object() {
        assert_eq!(normalize_mcp_input_schema(None), empty_object_schema());
        assert_eq!(normalize_mcp_input_schema(Some(&json!(null))), empty_object_schema());
        assert_eq!(normalize_mcp_input_schema(Some(&json!({}))), empty_object_schema());
        assert_eq!(normalize_mcp_input_schema(Some(&json!("nope"))), empty_object_schema());
    }

    #[test]
    fn rewrites_definitions_and_local_refs() {
        let schema = json!({
            "type": "object",
            "properties": {"item": {"$ref": "#/definitions/Item"}},
            "definitions": {"Item": {"type": "string"}}
        });
        let out = normalize_mcp_input_schema(Some(&schema));
        assert!(out.get("$defs").is_some());
        assert!(out.get("definitions").is_none());
        assert_eq!(out["properties"]["item"]["$ref"], json!("#/$defs/Item"));
    }

    #[test]
    fn property_named_definitions_is_preserved() {
        let schema = json!({
            "type": "object",
            "properties": {"definitions": {"type": "array", "items": {"type": "string"}}}
        });
        let out = normalize_mcp_input_schema(Some(&schema));
        assert!(out["properties"].get("definitions").is_some());
        assert!(out["properties"].get("$defs").is_none());
    }

    #[test]
    fn collapses_nullable_unions() {
        let schema = json!({
            "type": "object",
            "properties": {
                "opt": {
                    "anyOf": [{"type": "string"}, {"type": "null"}],
                    "default": null,
                    "title": "Opt"
                }
            }
        });
        let out = normalize_mcp_input_schema(Some(&schema));
        let opt = &out["properties"]["opt"];
        assert_eq!(opt["type"], json!("string"));
        assert_eq!(opt["nullable"], json!(true));
        assert_eq!(opt["title"], json!("Opt"));
        assert_eq!(opt["default"], json!(null));
        assert!(opt.get("anyOf").is_none());
    }

    #[test]
    fn meaningful_unions_are_left_alone() {
        let schema = json!({
            "type": "object",
            "properties": {"v": {"anyOf": [{"type": "string"}, {"type": "number"}]}}
        });
        let out = normalize_mcp_input_schema(Some(&schema));
        assert!(out["properties"]["v"].get("anyOf").is_some());
    }

    #[test]
    fn prunes_dangling_required_and_fills_properties() {
        let schema = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "required": ["a", "ghost"]
        });
        let out = normalize_mcp_input_schema(Some(&schema));
        assert_eq!(out["required"], json!(["a"]));

        let schema = json!({"type": "object", "required": ["ghost"]});
        let out = normalize_mcp_input_schema(Some(&schema));
        assert!(out.get("required").is_none());
        assert_eq!(out["properties"], json!({}));
    }

    #[test]
    fn coerces_missing_type_on_object_shapes() {
        let schema = json!({"properties": {"a": {"type": "string"}}});
        let out = normalize_mcp_input_schema(Some(&schema));
        assert_eq!(out["type"], json!("object"));
    }
}
