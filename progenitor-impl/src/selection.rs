// Copyright 2026

//! Select operations without changing their schemas or response media types.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::{Error, Result};

fn invalid(message: impl Into<String>) -> Error {
    Error::UnexpectedFormat(message.into())
}

fn references(value: &Value, pending: &mut BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            if let Some(Value::String(reference)) = object.get("$ref") {
                pending.insert(reference.clone());
            }
            for (key, value) in object {
                if matches!(key.as_str(), "properties" | "patternProperties" | "$defs") {
                    if let Some(entries) = value.as_object() {
                        for schema in entries.values() {
                            references(schema, pending);
                        }
                    }
                } else if !matches!(key.as_str(), "example" | "examples") && !key.starts_with("x-")
                {
                    references(value, pending);
                }
            }
        }
        Value::Array(array) => {
            for value in array {
                references(value, pending);
            }
        }
        _ => {}
    }
}

/// Retain selected `(path, method)` operations and their referenced components.
///
/// The original document version, schemas, and response content are preserved.
/// Shared path parameters and servers are retained. Webhooks are excluded from
/// an operation-only client. References must point to local component objects;
/// unsupported or missing references are reported instead of silently removed.
pub fn select_operations(document: &Value, operations: &[(&str, &str)]) -> Result<Value> {
    if !document.is_object() {
        return Err(invalid("OpenAPI document must be an object"));
    }
    let mut selected = document.clone();
    let mut paths = Map::new();
    for (path, method) in operations {
        let item = document["paths"]
            .get(path)
            .ok_or_else(|| invalid(format!("missing selected path: {path}")))?;
        let operation = item
            .get(method)
            .ok_or_else(|| invalid(format!("missing selected operation: {method} {path}")))?;
        let output = paths
            .entry((*path).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        output[*method] = operation.clone();
        for key in ["parameters", "servers", "summary", "description"] {
            if let Some(value) = item.get(key) {
                output[key] = value.clone();
            }
        }
    }
    selected["paths"] = Value::Object(paths);
    selected
        .as_object_mut()
        .ok_or_else(|| invalid("OpenAPI document must be an object"))?
        .remove("webhooks");
    let mut components = Map::new();
    if let Some(schemes) = document["components"].get("securitySchemes") {
        components.insert("securitySchemes".to_string(), schemes.clone());
    }
    selected["components"] = Value::Object(components);
    let mut pending = BTreeSet::new();
    let mut visited = BTreeSet::new();
    references(&selected, &mut pending);
    while let Some(reference) = pending.pop_first() {
        if !visited.insert(reference.clone()) {
            continue;
        }
        let pointer = reference
            .strip_prefix("#/components/")
            .ok_or_else(|| invalid(format!("unsupported selected reference: {reference}")))?;
        let (category, name) = pointer
            .split_once('/')
            .filter(|(_, name)| !name.contains('/'))
            .ok_or_else(|| invalid(format!("unsupported selected reference: {reference}")))?;
        let component = document
            .pointer(&reference[1..])
            .ok_or_else(|| invalid(format!("unresolved selected reference: {reference}")))?;
        let decode = |part: &str| part.replace("~1", "/").replace("~0", "~");
        let groups = selected["components"].as_object_mut().unwrap();
        let group = groups
            .entry(decode(category))
            .or_insert_with(|| Value::Object(Map::new()));
        group[decode(name)] = component.clone();
        references(component, &mut pending);
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preserves_schema_and_all_response_media() {
        let source = json!({
            "openapi": "3.1.0",
            "paths": {
                "/selected": {"parameters": [], "get": {"responses": {"200": {
                    "content": {
                        "application/json": {"schema": {"$ref": "#/components/schemas/A"}},
                        "text/event-stream": {"schema": {"$ref": "#/components/schemas/B"}}
                    }
                }}}},
                "/other": {"get": {}}
            },
            "components": {"schemas": {
                "A": {"type": ["string", "null"], "default": "a"},
                "B": {"allOf": [{"$ref": "#/components/schemas/A"}]},
                "Unused": {"type": "integer"}
            }}
        });
        let output = select_operations(&source, &[("/selected", "get")]).unwrap();
        assert_eq!(output["openapi"], "3.1.0");
        assert_eq!(output["paths"]["/selected"], source["paths"]["/selected"]);
        assert_eq!(
            output["components"]["schemas"]["A"],
            source["components"]["schemas"]["A"]
        );
        assert_eq!(
            output["components"]["schemas"].as_object().unwrap().len(),
            2
        );
        assert_eq!(output["paths"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn missing_operation_is_an_error() {
        assert!(select_operations(&json!({"paths": {"/a": {}}}), &[("/a", "post")]).is_err());
    }

    #[test]
    fn property_names_are_not_treated_as_annotations() {
        let source = json!({
            "paths": {"/a": {"get": {"responses": {"200": {"content": {
                "application/json": {"schema": {"type": "object", "properties": {
                    "examples": {"$ref": "#/components/schemas/A"},
                    "x-property": {"$ref": "#/components/schemas/B"}
                }}}
            }}}}}},
            "components": {"schemas": {"A": {"type": "string"}, "B": {"type": "integer"}}}
        });
        let output = select_operations(&source, &[("/a", "get")]).unwrap();
        assert_eq!(
            output["components"]["schemas"].as_object().unwrap().len(),
            2
        );
    }
}
