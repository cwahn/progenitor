// Copyright 2026 Oxide Computer Company

use progenitor_impl::{
    GenerationSettings, Generator, InterfaceStyle, OpenApiDocument, TagStyle, UnknownResponseFields,
};
use serde_json::json;

fn document() -> OpenApiDocument {
    OpenApiDocument::from_value(json!({
        "openapi": "3.1.0",
        "info": {"title": "Response recovery test", "version": "1"},
        "components": {"schemas": {
            "Shared": {
                "type": "object", "additionalProperties": false,
                "properties": {"id": {"type": "integer"}}, "required": ["id"]
            }
        }},
        "paths": {"/items": {"post": {
            "operationId": "create_item",
            "requestBody": {"required": true, "content": {"application/json": {
                "schema": {"$ref": "#/components/schemas/Shared"}
            }}},
            "responses": {
                "200": {"description": "ok", "content": {"application/json": {
                    "schema": {"$ref": "#/components/schemas/Shared"}
                }}},
                "400": {"description": "bad", "content": {"application/json": {
                    "schema": {"type": "object", "additionalProperties": false,
                        "properties": {"message": {"type": "string"}}, "required": ["message"]}
                }}},
                "409": {"description": "conflict", "content": {"application/json": {
                    "schema": {"type": "object", "additionalProperties": false,
                        "properties": {"retry": {"type": "boolean"}}, "required": ["retry"]}
                }}}
            }
        }}}
    }))
    .unwrap()
}

#[test]
fn default_emits_response_only_metadata_without_modifying_raw_or_shared_constraints() {
    assert_eq!(
        UnknownResponseFields::default(),
        UnknownResponseFields::WarnAndIgnore
    );
    let document = document();
    let before = document.raw().clone();
    let schemas = serde_json::to_value(document.schemas()).unwrap();
    let mut generator = Generator::default();
    let tokens = generator.generate_tokens_from_document(&document).unwrap();
    syn::parse2::<syn::File>(tokens.clone()).unwrap();
    let code = tokens.to_string();
    assert!(code.contains("from_response_with_recovery"));
    assert!(code.contains("__response_recovery_nodes"));
    assert!(code.contains("deny_unknown_fields"));
    assert!(code.contains("ResponseRecoveryNode"));
    assert!(code.contains("fnv1a64:"));
    assert_eq!(document.raw(), &before);
    assert_eq!(serde_json::to_value(document.schemas()).unwrap(), schemas);
    // Success and both concrete typed-error decoding arms use recovery.
    assert_eq!(code.matches("from_response_with_recovery").count(), 3);
}

#[test]
fn explicit_reject_uses_legacy_strict_decoder_without_metadata() {
    let mut generator = Generator::new(
        GenerationSettings::default().with_unknown_response_fields(UnknownResponseFields::Reject),
    );
    let code = generator
        .generate_tokens_from_document(&document())
        .unwrap()
        .to_string();
    assert!(!code.contains("from_response_with_recovery"));
    assert!(!code.contains("__response_recovery_nodes"));
    assert!(!code.contains("ResponseRecoveryNode"));
    assert!(code.contains("deny_unknown_fields"));
}

#[test]
fn builder_scopes_use_qualified_client_for_both_tag_styles() {
    for tag in [TagStyle::Merged, TagStyle::Separate] {
        let mut generator = Generator::new(
            GenerationSettings::default()
                .with_interface(InterfaceStyle::Builder)
                .with_tag(tag),
        );
        let code = generator
            .generate_tokens_from_document(&document())
            .unwrap()
            .to_string();
        assert_eq!(
            code.matches("super :: Client :: __response_recovery_nodes")
                .count(),
            3
        );
        let builder_module = code
            .split("pub mod builder")
            .nth(1)
            .unwrap()
            .split("pub mod prelude")
            .next()
            .unwrap();
        assert!(!builder_module.contains("use super :: Client ;"));
    }
}
