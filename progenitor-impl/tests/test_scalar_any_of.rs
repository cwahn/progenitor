// Copyright 2026 Oxide Computer Company

use progenitor_impl::{Generator, OpenApiDocument};
use serde_json::{Value, json};

fn parse(schemas: Value) -> OpenApiDocument {
    OpenApiDocument::from_value(json!({
        "openapi":"3.1.0", "info":{"title":"scalar proof","version":"1"},
        "paths":{}, "components":{"schemas":schemas}
    }))
    .unwrap()
}

fn schema(doc: &OpenApiDocument, name: &str) -> Value {
    serde_json::to_value(&doc.schemas()[&format!("#/components/schemas/{name}")]).unwrap()
}

#[test]
fn reference_union_collapses_without_losing_outer_constraints_or_raw_input() {
    let schemas = json!({
        "Shared":{"anyOf":[{"type":"string"},{"type":"string","enum":["known"]}]},
        "Alias":{"$ref":"#/components/schemas/Shared"},
        "Ids":{
            "title":"Identifier", "description":"opaque ID", "default":"custom-id",
            "minLength":2, "pattern":"^[a-z-]+$", "x-origin":{"anyOf":[1,2]},
            "anyOf":[{"$ref":"#/components/schemas/Alias"},{"type":"string","enum":["known"]}]
        }
    });
    let doc = parse(schemas.clone());
    assert_eq!(doc.raw()["components"]["schemas"], schemas);
    assert!(doc.diagnostics().is_empty());
    let mut expected = schemas["Ids"].clone();
    expected.as_object_mut().unwrap().remove("anyOf");
    expected["type"] = json!("string");
    assert_eq!(schema(&doc, "Ids"), expected);
    let validator = jsonschema::validator_for(&expected).unwrap();
    for value in [json!("custom-id"), json!("known")] {
        assert!(validator.is_valid(&value));
    }
    for value in [
        json!("x"),
        json!("UPPER"),
        json!(42),
        json!(false),
        json!(null),
        json!({}),
        json!([]),
    ] {
        assert!(!validator.is_valid(&value), "{value}");
    }
    Generator::default()
        .generate_tokens_from_document(&doc)
        .unwrap();
}

#[test]
fn nested_reference_union_proof_is_order_independent_and_rejects_nonstrings() {
    let doc = parse(json!({
        "A":{"anyOf":[{"$ref":"#/components/schemas/B"},{"type":"string","enum":["a"]}]},
        "B":{"anyOf":[{"$ref":"#/components/schemas/C"},{"type":"string","maxLength":5}]},
        "C":{"$ref":"#/components/schemas/D"},
        "D":{"type":"string","title":"Any string"}
    }));
    for name in ["A", "B"] {
        assert_eq!(schema(&doc, name), json!({"type":"string"}));
        let validator = jsonschema::validator_for(&schema(&doc, name)).unwrap();
        for value in [json!(""), json!("arbitrary-custom-model-id")] {
            assert!(validator.is_valid(&value));
        }
        for value in [json!(1), json!(null), json!(true), json!({}), json!([])] {
            assert!(!validator.is_valid(&value));
        }
    }
}

#[test]
fn constrained_reference_siblings_and_constrained_branches_are_not_broad() {
    for branch in [
        json!({"$ref":"#/components/schemas/Text","maxLength":3}),
        json!({"$ref":"#/components/schemas/Text","enum":["x"]}),
        json!({"type":"string","minLength":2}),
        json!({"type":"string","format":"uuid"}),
        json!({"type":"string","unknownAssertion":true}),
    ] {
        let doc = parse(json!({
            "Text":{"type":"string"},
            "Restricted":{"anyOf":[branch,{"type":"string","enum":["x"]}]},
            "Outer":{"anyOf":[{"$ref":"#/components/schemas/Restricted"},{"type":"string","enum":["y"]}]}
        }));
        assert!(
            schema(&doc, "Restricted").get("anyOf").is_some(),
            "{branch}"
        );
        assert!(schema(&doc, "Outer").get("anyOf").is_some(), "{branch}");
    }
    // An outer ref sibling remains a conjunction even when its anyOf is broad.
    let doc = parse(json!({
        "Text":{"type":"string"},
        "Outer":{"$ref":"#/components/schemas/Text", "maxLength":3,
            "title":"Short", "default":"id",
            "anyOf":[{"$ref":"#/components/schemas/Text"},{"type":"string","enum":["x"]}]}
    }));
    let outer = schema(&doc, "Outer");
    assert_eq!(outer["title"], "Short");
    assert_eq!(outer["default"], "id");
    assert_eq!(
        outer["allOf"][0],
        json!({"$ref":"#/components/schemas/Text"})
    );
    assert_eq!(outer["allOf"][1], json!({"type":"string","maxLength":3}));
}

#[test]
fn cyclic_and_unresolved_references_fail_closed() {
    let doc = parse(json!({
        "A":{"$ref":"#/components/schemas/B"},
        "B":{"$ref":"#/components/schemas/A"},
        "Outer":{"anyOf":[{"$ref":"#/components/schemas/A"},{"type":"string","enum":["x"]}]}
    }));
    assert!(schema(&doc, "Outer").get("anyOf").is_some());
    let error = OpenApiDocument::from_value(json!({
        "openapi":"3.1.0", "info":{"title":"test","version":"1"}, "paths":{},
        "components":{"schemas":{"Outer":{"anyOf":[
            {"$ref":"#/components/schemas/Missing"},{"type":"string"}
        ]}}}
    }))
    .unwrap_err()
    .to_string();
    assert!(error.contains("missing schema reference"), "{error}");
}

#[test]
fn mixed_scalar_unions_are_not_narrowed_and_other_scalars_are_reusable() {
    let doc = parse(json!({
        "Text":{"type":"string"},
        "Mixed":{"anyOf":[{"$ref":"#/components/schemas/Text"},{"type":"integer"}]}
    }));
    assert!(schema(&doc, "Mixed").get("anyOf").is_some());
    for typ in ["boolean", "integer", "number", "null"] {
        let doc = parse(json!({
            "Scalar":{"type":typ},
            "Outer":{"anyOf":[{"$ref":"#/components/schemas/Scalar"},{"type":typ,"enum":[]}]}
        }));
        assert_eq!(schema(&doc, "Outer"), json!({"type":typ}));
    }
}
