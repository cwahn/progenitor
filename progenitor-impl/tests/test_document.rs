// Copyright 2026 Oxide Computer Company

use progenitor_impl::{Generator, OpenApiDocument};
use serde_json::{Value, json};

fn document(version: &str, schemas: Value) -> Value {
    json!({"openapi":version, "info":{"title":"test","version":"1"}, "paths":{}, "components":{"schemas":schemas}})
}

fn normalized(schema: Value) -> Value {
    let doc = OpenApiDocument::from_value(document("3.1.0", json!({"X":schema}))).unwrap();
    serde_json::to_value(&doc.schemas()["#/components/schemas/X"]).unwrap()
}

#[test]
fn ecma_patterns_validate_authentic_hugging_face_defaults_without_rewriting() {
    let schema = json!({
        "default": [],
        "description": "Inference endpoint name patterns (wildcards allowed, e.g. `gpt2-*`) restricting the token's org-wide Inference Endpoint permissions to matching endpoints. Empty means all endpoints.",
        "maxItems": 20,
        "type": "array",
        "items": {"type":"string", "maxLength":256, "pattern":r"^[^.?()\]\\[{}]+$"}
    });
    let input = document("3.1.0", json!({"EndpointPatterns": schema}));
    let doc = OpenApiDocument::from_value(input.clone()).unwrap();
    assert_eq!(doc.raw(), &input);
    assert!(doc.diagnostics().is_empty());
    assert_eq!(
        serde_json::to_value(&doc.schemas()["#/components/schemas/EndpointPatterns"]).unwrap(),
        schema
    );
    Generator::default()
        .generate_tokens_from_document(&doc)
        .unwrap();

    for (default, valid) in [
        (json!([]), true),
        (json!(["gpt2-*", "endpoint-name"]), true),
        (json!([""]), false),
        (json!(["a.b"]), false),
        (json!(["a?b"]), false),
        (json!(["a(b"]), false),
        (json!(["a)b"]), false),
        (json!(["a[b"]), false),
        (json!(["a]b"]), false),
        (json!(["a\\b"]), false),
        (json!(["a{b"]), false),
        (json!(["a}b"]), false),
        (json!(["x".repeat(257)]), false),
        (json!(vec!["x"; 21]), false),
        (json!([42]), false),
        (json!(null), false),
    ] {
        let mut schema = schema.clone();
        schema["default"] = default.clone();
        let doc = OpenApiDocument::from_value(document("3.1.0", json!({"X":schema}))).unwrap();
        assert_eq!(doc.diagnostics().is_empty(), valid, "{default}");
        let retained = doc.schemas()["#/components/schemas/X"]
            .clone()
            .into_object()
            .metadata
            .and_then(|metadata| metadata.default);
        assert_eq!(retained, valid.then_some(default));
        let actual = serde_json::to_value(&doc.schemas()["#/components/schemas/X"]).unwrap();
        assert_eq!(actual["items"], schema["items"]);
        assert_eq!(actual["maxItems"], schema["maxItems"]);
    }
}

#[test]
fn ecma_patterns_preserve_search_negation_and_null_default_semantics() {
    for (schema, valid) in [
        (
            json!({"type":"string","pattern":"yes","default":"before yes after"}),
            true,
        ),
        (
            json!({"type":"string","pattern":"yes","default":"no"}),
            false,
        ),
        (
            json!({"type":"string","pattern":"yes","not":{"pattern":"x"},"default":"before yes after"}),
            true,
        ),
        (
            json!({"type":"string","pattern":"yes","not":{"pattern":"x"},"default":"yes x"}),
            false,
        ),
        (
            json!({"type":["string","null"],"pattern":"yes","default":null}),
            true,
        ),
        (
            json!({"type":"string","pattern":"yes","default":null}),
            false,
        ),
    ] {
        let doc = OpenApiDocument::from_value(document("3.1.0", json!({"X":schema}))).unwrap();
        assert_eq!(doc.diagnostics().is_empty(), valid, "{schema}");
        let retained = doc.schemas()["#/components/schemas/X"]
            .clone()
            .into_object()
            .metadata
            .and_then(|metadata| metadata.default);
        assert_eq!(retained, valid.then(|| schema["default"].clone()));
    }
    let error = OpenApiDocument::from_value(document(
        "3.1.0",
        json!({
            "X":{"type":"string","pattern":"[","default":"x"}
        }),
    ))
    .unwrap_err()
    .to_string();
    assert!(error.contains("cannot validate schema default"), "{error}");
}

#[test]
fn native31_preserves_union_null_and_simultaneous_constraints() {
    for schema in [
        json!({"type":["string","integer"],"minimum":1.0,"minLength":2}),
        json!({"type":"null","minLength":2}),
        json!({"type":"object","maxProperties":3,"oneOf":[{"required":["a"]},{"required":["b"]}]}),
        json!({"type":"object","propertyNames":{"type":"string","maxLength":64},"additionalProperties":{"type":"boolean"}}),
        json!({"type":["object","null"],"anyOf":[{"type":"object"},{"type":"null"}]}),
    ] {
        assert_eq!(normalized(schema.clone()), schema);
    }
    assert_eq!(normalized(json!(true)), json!(true));
    assert_eq!(normalized(json!(false)), json!(false));
}

#[test]
fn supported_negated_patterns_survive_typify_merging() {
    let schema =
        normalized(json!({"type":"string","not":{"pattern":"x"},"pattern":"yes","minLength":1}));
    assert_eq!(schema["not"], json!({"pattern":"x"}));
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&json!("before yes after")));
    assert!(!validator.is_valid(&json!("yes x")));
    assert!(!validator.is_valid(&json!("no")));
    let doc = OpenApiDocument::from_value(document("3.1.0", json!({"X":schema}))).unwrap();
    let output = Generator::default()
        .generate_tokens_from_document(&doc)
        .unwrap()
        .to_string();
    assert!(output.contains("(?!"));
    let error = OpenApiDocument::from_value(document("3.1.0", json!({"X":{"not":{"minimum":3}}})))
        .unwrap_err()
        .to_string();
    assert!(error.contains("#/components/schemas/X/not"));
}

#[test]
fn native31_tuples_and_const() {
    assert_eq!(
        normalized(
            json!({"type":"array","prefixItems":[{"type":"string"},{"type":"integer"}],"items":false,"minItems":2})
        ),
        json!({"type":"array","items":[{"type":"string"},{"type":"integer"}],"additionalItems":false,"minItems":2,"maxItems":2})
    );
    assert_eq!(
        normalized(json!({"const":"a","enum":["a","b"]})),
        json!({"enum":["a"]})
    );
    assert_eq!(normalized(json!({"const":"a","enum":["b"]})), json!(false));
}

#[test]
fn native31_reference_siblings_are_conjunctive() {
    let doc = OpenApiDocument::from_value(document("3.1.0", json!({
        "X":{"type":"string"},
        "Y":{"$ref":"#/components/schemas/X","description":"sibling","maxLength":3,"default":"ok"},
        "Z":{"$ref":"#/components/schemas/X","type":["string","null"],"default":null}
    }))).unwrap();
    let schema = serde_json::to_value(&doc.schemas()["#/components/schemas/Y"]).unwrap();
    assert_eq!(
        schema,
        json!({"description":"sibling","default":"ok","allOf":[{"$ref":"#/components/schemas/X"},{"maxLength":3}]})
    );
    assert_eq!(doc.diagnostics().len(), 1);
    assert!(doc.diagnostics()[0].location.ends_with("/Z/default"));
    let output = Generator::default()
        .generate_tokens_from_document(&doc)
        .unwrap()
        .to_string();
    assert!(output.contains("sibling"));
}

#[test]
fn native30_nullable_and_boolean_bounds() {
    let doc = OpenApiDocument::from_value(document("3.0.3", json!({
        "X":{"type":"integer","nullable":true,"minimum":0,"exclusiveMinimum":true,"maximum":10,"exclusiveMaximum":false},
        "Y":{"nullable":true,"allOf":[{"$ref":"#/components/schemas/X"}]},
        "Z":{"$ref":"#/components/schemas/X","nullable":true,"description":"ignored in 3.0"}
    }))).unwrap();
    assert_eq!(
        serde_json::to_value(&doc.schemas()["#/components/schemas/X"]).unwrap(),
        json!({"type":["integer","null"],"exclusiveMinimum":0.0,"maximum":10.0})
    );
    assert_eq!(
        serde_json::to_value(&doc.schemas()["#/components/schemas/Y"]).unwrap(),
        json!({"oneOf":[{"type":"null"},{"allOf":[{"$ref":"#/components/schemas/X"}]}]})
    );
    assert_eq!(
        serde_json::to_value(&doc.schemas()["#/components/schemas/Z"]).unwrap(),
        json!({"$ref":"#/components/schemas/X"})
    );
}

#[test]
fn defaults_are_validated_with_refs_and_valid_null_is_retained() {
    let input = document(
        "3.1.0",
        json!({
            "Target":{"type":"object","required":["id"],"properties":{"id":{"type":"string"}}},
            "Bad":{"$ref":"#/components/schemas/Target","default":{}},
            "Good":{"$ref":"#/components/schemas/Target","default":{"id":"x"}},
            "Null":{"anyOf":[{"type":"string"},{"type":"null"}],"default":null},
            "Enum":{"type":"string","enum":["a"],"default":"b"}
        }),
    );
    let doc = OpenApiDocument::from_value(input.clone()).unwrap();
    assert_eq!(doc.raw(), &input);
    assert_eq!(doc.diagnostics().len(), 2);
    assert!(
        doc.schemas()["#/components/schemas/Bad"]
            .clone()
            .into_object()
            .metadata
            .as_ref()
            .is_none_or(|m| m.default.is_none())
    );
    assert_eq!(
        doc.schemas()["#/components/schemas/Good"]
            .clone()
            .into_object()
            .metadata
            .unwrap()
            .default,
        Some(json!({"id":"x"}))
    );
    assert_eq!(
        doc.schemas()["#/components/schemas/Null"]
            .clone()
            .into_object()
            .metadata
            .unwrap()
            .default,
        Some(Value::Null)
    );
}

#[test]
fn overlapping_scalar_any_of_keeps_valid_default() {
    let size = json!({"anyOf":[{"type":"string"},{"type":"string","enum":["small","auto"]}],"default":"auto"});
    let doc = OpenApiDocument::from_value(document(
        "3.1.0",
        json!({"Size":size, "Holder":{"type":"object","properties":{"size":size}}}),
    ))
    .unwrap();
    assert!(doc.diagnostics().is_empty());
    assert_eq!(
        doc.schemas()["#/components/schemas/Size"]
            .clone()
            .into_object()
            .metadata
            .unwrap()
            .default,
        Some(json!("auto"))
    );
    let output = Generator::default()
        .generate_tokens_from_document(&doc)
        .unwrap()
        .to_string();
    assert!(output.contains("auto"));
}

#[test]
fn diagnostics_are_located_and_do_not_walk_instance_data() {
    for (key, value) in [
        ("$defs", json!({})),
        ("definitions", json!({})),
        ("$id", json!("id")),
        ("$anchor", json!("a")),
        ("$dynamicRef", json!("#a")),
        ("$dynamicAnchor", json!("a")),
        (
            "$schema",
            json!("https://json-schema.org/draft/2019-09/schema"),
        ),
        ("unevaluatedProperties", json!(false)),
        ("unevaluatedItems", json!(false)),
        ("dependentRequired", json!({"x":["y"]})),
        ("dependentSchemas", json!({})),
        ("contains", json!({})),
        ("minContains", json!(1)),
        ("maxContains", json!(1)),
        ("if", json!({})),
        ("then", json!({})),
        ("else", json!({})),
    ] {
        let mut bad = json!({"type":"string"});
        bad[key] = value;
        let input = document("3.1.0", json!({"X":{"properties":{"a/b~c":bad}}}));
        let error = OpenApiDocument::from_value(input).unwrap_err().to_string();
        assert!(
            error.contains(&format!("#/components/schemas/X/properties/a~1b~0c/{key}")),
            "{error}"
        );
    }
    let schema = json!({"type":"object", "properties":{"nullable":{"type":"boolean"},"$defs":{"type":"string"}},"examples":[{"$id":"data","schema":{"if":true}}],"x-data":{"$dynamicRef":"data"}});
    assert_eq!(normalized(schema.clone()), schema);
    for reference in [
        "external.json#/X",
        "#anchor",
        "#/components/schemas/X/properties/a",
        "#/components/schemas/Missing",
    ] {
        let error = OpenApiDocument::from_value(document("3.1.0", json!({"X":{"$ref":reference}})))
            .unwrap_err()
            .to_string();
        assert!(error.contains("#/components/schemas/X/$ref"), "{error}");
    }
}

#[test]
fn huggingface_schema_dialect_preserves_status_constraints() {
    // Verbatim request schema from the official HF OpenAPI 3.1 document:
    // PUT /api/partners/{provider}/models/{mappingId}/status
    // Source SHA-256: 0ff36f73b0d81ff5b811ad0b1a7911f81a0fe51ab106ff91fd353230ed8e1948
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {"status": {"type": "string", "enum": ["live", "staging", "error"]}},
        "required": ["status"]
    });
    let input = document("3.1.0", json!({"Status": schema}));
    let doc = OpenApiDocument::from_value(input.clone()).unwrap();
    assert_eq!(doc.raw(), &input);
    assert!(doc.diagnostics().is_empty());
    let normalized = serde_json::to_value(&doc.schemas()["#/components/schemas/Status"]).unwrap();
    let mut expected = schema.clone();
    expected.as_object_mut().unwrap().remove("$schema");
    assert_eq!(normalized, expected);
    let original_validator = jsonschema::validator_for(&schema).unwrap();
    let normalized_validator = jsonschema::validator_for(&normalized).unwrap();
    for (instance, valid) in [
        (json!({"status":"live"}), true),
        (json!({"status":"staging"}), true),
        (json!({"status":"error"}), true),
        (json!({"status":"unknown"}), false),
        (json!({"status":null}), false),
        (json!({}), false),
    ] {
        assert_eq!(original_validator.is_valid(&instance), valid);
        assert_eq!(normalized_validator.is_valid(&instance), valid);
    }
    let output = Generator::default()
        .generate_tokens_from_document(&doc)
        .unwrap()
        .to_string();
    for status in ["live", "staging", "error"] {
        assert!(output.contains(status));
    }
}

#[test]
fn schema_dialects_are_version_aware_and_checked_at_each_schema() {
    for dialect in [
        "https://json-schema.org/draft/2020-12/schema",
        "https://spec.openapis.org/oas/3.1/dialect/base",
    ] {
        let mut input = document(
            "3.1.0",
            json!({"X":{"$schema":dialect, "type":"string", "default":"ok"}}),
        );
        input["jsonSchemaDialect"] = json!(dialect);
        let doc = OpenApiDocument::from_value(input.clone()).unwrap();
        assert_eq!(doc.raw(), &input);
        assert!(doc.diagnostics().is_empty());
        assert_eq!(
            serde_json::to_value(&doc.schemas()["#/components/schemas/X"]).unwrap(),
            json!({"type":"string", "default":"ok"})
        );
        assert_eq!(
            normalized(json!({"type":"array", "items":{"$schema":dialect, "type":"string"}})),
            json!({"type":"array", "items":{"type":"string"}})
        );
        let error = OpenApiDocument::from_value(document(
            "3.0.3",
            json!({"X":{"$schema":dialect, "type":"string"}}),
        ))
        .unwrap_err()
        .to_string();
        assert!(error.contains("#/components/schemas/X/$schema"), "{error}");
    }
    for dialect in [
        json!("http://json-schema.org/draft-07/schema#"),
        json!("https://example.com/unknown-dialect"),
        json!(null),
        json!(42),
    ] {
        let error = OpenApiDocument::from_value(document(
            "3.1.0",
            json!({"X":{"type":"array", "items":{"$schema":dialect, "type":"string"}}}),
        ))
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("#/components/schemas/X/items/$schema"),
            "{error}"
        );
        assert!(error.contains("unsupported schema dialect"), "{error}");
    }
    let error = OpenApiDocument::from_value(document(
        "3.1.0",
        json!({"X":{"$schema":"https://json-schema.org/draft/2020-12/schema", "$dynamicRef":"#scope"}}),
    ))
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("#/components/schemas/X/$dynamicRef"),
        "{error}"
    );
    let schema =
        json!({"type":"object", "examples":[{"$schema":"instance data"}], "x-data":{"$schema":42}});
    assert_eq!(normalized(schema.clone()), schema);
}

#[test]
fn inline_parameter_body_response_and_header_schemas_are_lossless() {
    let mut input = document("3.1.0", json!({}));
    input["paths"] = json!({"/test/{id}":{"post":{
        "operationId":"test",
        "parameters":[{"in":"path","name":"id","required":true,"schema":{"type":"string"}},{"in":"query","name":"q","schema":{"type":"string","const":"fixed","default":"fixed"}}],
        "requestBody":{"required":true,"content":{"application/json":{"schema":{"type":"object","propertyNames":{"type":"string","maxLength":3},"additionalProperties":{"type":"integer"}}}}},
        "responses":{"200":{"description":"ok","headers":{"X-Result":{"schema":{"type":"string","const":"yes"}}},"content":{"application/json":{"schema":{"type":"string","enum":["result"]}}}}}
    }}});
    let doc: OpenApiDocument = serde_json::from_value(input).unwrap();
    assert_eq!(doc.schemas().len(), 5);
    assert!(doc.diagnostics().is_empty());
    let output = Generator::default()
        .generate_tokens_from_document(&doc)
        .unwrap()
        .to_string();
    assert!(output.contains("fixed"));
    let body =
        &doc.schemas()["#/paths/~1test~1{id}/post/requestBody/content/application~1json/schema"];
    assert_eq!(
        body.clone()
            .into_object()
            .object
            .unwrap()
            .property_names
            .unwrap()
            .into_object()
            .string
            .unwrap()
            .max_length,
        Some(3)
    );
    assert!(output.contains("result"));
    assert!(!output.contains("progenitor-normalized-schema"));
}
