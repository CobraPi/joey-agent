//! JSON-Schema sanitizer for tool parameters (port of `tools/schema_sanitizer.py`).
//!
//! Normalizes schemas for strict LLM backends: ensures object nodes have a
//! `properties` map, collapses `type` arrays, and strips `anyOf:[X, null]`
//! unions down to `X` + `nullable`.

use serde_json::{json, Map, Value};

/// Sanitize a parameters schema in place-ish (returns a cleaned clone).
pub fn sanitize_parameters(params: Value) -> Value {
    let mut node = match params {
        Value::Object(_) => params,
        _ => json!({"type": "object", "properties": {}}),
    };
    sanitize_node(&mut node);
    // A top-level parameters object must have properties.
    if node.get("type").and_then(|t| t.as_str()) == Some("object") && node.get("properties").is_none() {
        node.as_object_mut()
            .unwrap()
            .insert("properties".into(), json!({}));
    }
    node
}

fn sanitize_node(node: &mut Value) {
    let Value::Object(map) = node else {
        return;
    };

    collapse_type_array(map);
    strip_nullable_union(map);

    // Object nodes need a properties map.
    if map.get("type").and_then(|t| t.as_str()) == Some("object") && !map.contains_key("properties") {
        map.insert("properties".into(), json!({}));
    }

    // Recurse into nested schema positions.
    for key in ["items", "additionalProperties"] {
        if let Some(child) = map.get_mut(key) {
            sanitize_node(child);
        }
    }
    for key in ["properties", "$defs", "definitions"] {
        if let Some(Value::Object(props)) = map.get_mut(key) {
            for (_, v) in props.iter_mut() {
                sanitize_node(v);
            }
        }
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(arr)) = map.get_mut(key) {
            for v in arr.iter_mut() {
                sanitize_node(v);
            }
        }
    }

    // Prune `required` entries not present in `properties`.
    if let (Some(Value::Array(required)), Some(Value::Object(props))) =
        (map.get("required").cloned().as_ref(), map.get("properties"))
    {
        let valid: Vec<Value> = required
            .iter()
            .filter(|r| r.as_str().map(|s| props.contains_key(s)).unwrap_or(false))
            .cloned()
            .collect();
        map.insert("required".into(), Value::Array(valid));
    }
}

/// `type: [X, "null"]` → `type: X, nullable: true`; multi-type → anyOf.
fn collapse_type_array(map: &mut Map<String, Value>) {
    let Some(Value::Array(types)) = map.get("type").cloned().as_ref().map(|v| v.clone()) else {
        return;
    };
    let non_null: Vec<String> = types
        .iter()
        .filter_map(|t| t.as_str())
        .filter(|t| *t != "null")
        .map(str::to_string)
        .collect();
    let has_null = types.iter().any(|t| t.as_str() == Some("null"));

    match non_null.len() {
        0 => {}
        1 => {
            map.insert("type".into(), json!(non_null[0]));
            if has_null {
                map.insert("nullable".into(), json!(true));
            }
        }
        _ => {
            let branches: Vec<Value> = non_null.iter().map(|t| json!({"type": t})).collect();
            map.remove("type");
            map.insert("anyOf".into(), Value::Array(branches));
            if has_null {
                map.insert("nullable".into(), json!(true));
            }
        }
    }
}

/// `anyOf: [ {X}, {type: null} ]` → `{X, nullable: true}`.
fn strip_nullable_union(map: &mut Map<String, Value>) {
    let Some(Value::Array(any_of)) = map.get("anyOf").cloned().as_ref().map(|v| v.clone()) else {
        return;
    };
    let non_null: Vec<Value> = any_of
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) != Some("null"))
        .cloned()
        .collect();
    let has_null = any_of.len() != non_null.len();
    if non_null.len() == 1 && has_null {
        map.remove("anyOf");
        if let Value::Object(inner) = &non_null[0] {
            for (k, v) in inner {
                map.insert(k.clone(), v.clone());
            }
        }
        map.insert("nullable".into(), json!(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_properties_to_object() {
        let out = sanitize_parameters(json!({"type": "object"}));
        assert!(out.get("properties").is_some());
    }

    #[test]
    fn collapses_type_array() {
        let out = sanitize_parameters(json!({
            "type": "object",
            "properties": {"x": {"type": ["string", "null"]}}
        }));
        let x = &out["properties"]["x"];
        assert_eq!(x["type"], "string");
        assert_eq!(x["nullable"], true);
    }

    #[test]
    fn prunes_stale_required() {
        let out = sanitize_parameters(json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "required": ["a", "ghost"]
        }));
        assert_eq!(out["required"], json!(["a"]));
    }
}
