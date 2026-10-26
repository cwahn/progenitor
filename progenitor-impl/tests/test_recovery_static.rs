// Copyright 2026 Oxide Computer Company

use progenitor_impl::{Generator, OpenApiDocument};
use serde_json::json;

#[test]
fn recovery_nodes_have_static_storage() {
    let document = OpenApiDocument::from_value(json!({
        "openapi": "3.1.0",
        "info": {"title": "Static recovery", "version": "1"},
        "paths": {
            "/value": {"get": {
                "operationId": "getValue",
                "responses": {"200": {
                    "description": "A value",
                    "content": {"application/json": {"schema": {
                        "type": "object",
                        "properties": {"value": {"type": "string"}},
                        "additionalProperties": false
                    }}}
                }}
            }}
        }
    }))
    .unwrap();
    let tokens = Generator::default()
        .generate_tokens_from_document(&document)
        .unwrap()
        .to_string();
    assert!(tokens.contains("const fn __response_recovery_nodes"));
    assert!(tokens.contains(
        "static __RESPONSE_RECOVERY_NODES : [progenitor_client :: ResponseRecoveryNode ;"
    ));
    assert!(tokens.contains("& __RESPONSE_RECOVERY_NODES"));
    assert!(!tokens.contains("use progenitor_client :: ResponseRecoveryKind :: *"));
    assert!(!tokens.contains("use progenitor_client :: ResponseAdditionalProperties :: *"));
    assert!(!tokens.contains("allow (clippy :: large_stack_arrays)"));
}
