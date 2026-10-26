// Copyright 2026 Oxide Computer Company

use progenitor_impl::{
    GenerationSettings, Generator, InterfaceStyle, OpenApiDocument, UnknownResponseFields,
};
use serde_json::{Value, json};

fn response(schema: Value) -> Value {
    json!({"description": "Response", "content": {"application/json": {"schema": schema}}})
}

fn document(responses: Value) -> OpenApiDocument {
    serde_json::from_value(json!({
        "openapi": "3.1.0", "info": {"title": "Error enums", "version": "1"},
        "paths": {"/tasks/compact": {"post": {
            "operationId": "compact_task_queue", "responses": responses
        }}},
        "components": {"schemas": {
            "ResponseError": {"type": "object", "required": ["message"], "properties": {"message": {"type": "string"}}},
            "TaskCompactionSummary": {"type": "object", "required": ["deleted"], "properties": {"deleted": {"type": "integer"}}}
        }}
    })).unwrap()
}

fn generated(document: &OpenApiDocument, style: InterfaceStyle) -> String {
    let mut settings = GenerationSettings::default();
    settings
        .with_interface(style)
        .with_unknown_response_fields(UnknownResponseFields::Reject);
    let tokens = Generator::new(&settings)
        .generate_tokens_from_document(document)
        .unwrap();
    syn::parse2::<syn::File>(tokens.clone()).unwrap();
    rustfmt_wrapper::rustfmt(tokens).unwrap()
}

#[test]
fn heterogeneous_errors_are_status_dispatched_and_operation_scoped() {
    let document = document(json!({
        "204": {"description": "Success"},
        "401": response(json!({"$ref": "#/components/schemas/ResponseError"})),
        "500": response(json!({"$ref": "#/components/schemas/TaskCompactionSummary"}))
    }));
    for style in [InterfaceStyle::Positional, InterfaceStyle::Builder] {
        let code = generated(&document, style);
        assert!(code.contains("pub mod compact_task_queue"), "{code}");
        assert!(code.contains("Status401(types::ResponseError)"), "{code}");
        assert!(
            code.contains("Status500(types::TaskCompactionSummary)"),
            "{code}"
        );
        assert!(
            code.contains("Error<response::compact_task_queue::Error>"),
            "{code}"
        );
        assert!(
            code.contains("ResponseValue::<types::ResponseError>::from_response"),
            "{code}"
        );
        assert!(
            code.contains("response::compact_task_queue::Error::Status401"),
            "{code}"
        );
        let compact = code.split_whitespace().collect::<String>();
        assert!(
            compact.contains(".map::<_,_,Error<response::compact_task_queue::Error"),
            "{code}"
        );
        assert!(!code.contains("derive(Debug, serde::Deserialize"));
    }
}

#[test]
fn one_error_schema_keeps_existing_public_type() {
    let schema = json!({"$ref": "#/components/schemas/ResponseError"});
    let document = document(json!({
        "204": {"description": "Success"},
        "401": response(schema.clone()), "500": response(schema)
    }));
    let code = generated(&document, InterfaceStyle::Positional);
    assert!(!code.contains("pub mod response"));
    assert!(code.contains("Error<types::ResponseError>"));
}

#[test]
fn identical_homogeneous_decoders_share_patterns_without_changing_error_payloads() {
    let schema = json!({"$ref": "#/components/schemas/ResponseError"});
    let document = document(json!({
        "204": {"description": "Success"},
        "400": response(schema.clone()), "404": response(schema.clone()),
        "429": response(schema.clone()), "503": response(schema)
    }));
    for style in [InterfaceStyle::Positional, InterfaceStyle::Builder] {
        let code = generated(&document, style);
        let compact = code.split_whitespace().collect::<String>();
        assert!(compact.contains("400u16|404u16|429u16|503u16=>"), "{code}");
        assert_eq!(code.matches("Error::ErrorResponse(").count(), 1, "{code}");
        assert!(compact.contains("Error::ErrorResponse(::std::boxed::Box::new(ResponseValue::from_response(response).await?"), "{code}");
        assert!(
            compact.contains("Error::UnexpectedResponse(::std::boxed::Box::new(response))"),
            "{code}"
        );
        assert!(
            compact.contains("204u16=>Ok(ResponseValue::empty(response))"),
            "{code}"
        );
    }
}

#[test]
fn identical_default_decoder_retains_catch_all_semantics() {
    let schema = json!({"$ref": "#/components/schemas/ResponseError"});
    let document = document(json!({
        "200": response(schema.clone()),
        "400": response(schema.clone()), "4XX": response(schema.clone()),
        "default": response(schema)
    }));
    let code = generated(&document, InterfaceStyle::Positional);
    let compact = code.split_whitespace().collect::<String>();
    assert_eq!(code.matches("Error::ErrorResponse(").count(), 1, "{code}");
    assert!(compact.contains("_=>Err(Error::ErrorResponse("), "{code}");
    assert!(!code.contains("Error::UnexpectedResponse("), "{code}");
}

#[test]
fn exact_status_precedes_range_and_default_is_last() {
    let document = document(json!({
        "200": response(json!({"$ref": "#/components/schemas/ResponseError"})),
        "401": response(json!({"$ref": "#/components/schemas/ResponseError"})),
        "4XX": response(json!({"$ref": "#/components/schemas/TaskCompactionSummary"})),
        "default": response(json!({"$ref": "#/components/schemas/ResponseError"}))
    }));
    let code = generated(&document, InterfaceStyle::Positional);
    assert!(code.contains("Status4xx(types::TaskCompactionSummary)"));
    assert!(code.contains("Default(types::ResponseError)"));
    let exact = code.find("401u16 =>").unwrap();
    let range = code.find("400u16..=499u16 =>").unwrap();
    let default = code.find("Error::Default").unwrap();
    assert!(exact < range && range < default);
}

#[test]
fn mixed_stream_errors_return_an_explicit_generation_error() {
    let document = document(json!({
        "204": {"description": "Success"},
        "401": response(json!({"$ref": "#/components/schemas/ResponseError"})),
        "500": {"description": "stream", "content": {"text/plain": {"schema": {"type": "string"}}}}
    }));
    let error = Generator::default()
        .generate_tokens_from_document(&document)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("heterogeneous error enums support JSON and empty bodies only")
    );
}

#[test]
fn empty_error_bodies_are_unit_variants_not_deserialized_unions() {
    let document = document(json!({
        "204": {"description": "Success"},
        "401": response(json!({"$ref": "#/components/schemas/ResponseError"})),
        "500": {"description": "No body"}
    }));
    let code = generated(&document, InterfaceStyle::Positional);
    assert!(code.contains("Status500(())"), "{code}");
    assert!(code.contains("ResponseValue::empty(response)"), "{code}");
    assert!(code.contains("response::compact_task_queue::Error::Status500"));
}

#[test]
fn different_inline_error_objects_have_distinct_type_names() {
    let document = document(json!({
        "204": {"description": "Success"},
        "401": response(json!({"type": "object", "required": ["first"], "properties": {"first": {"type": "string"}}})),
        "500": response(json!({"type": "object", "required": ["failed"], "properties": {"failed": {"type": "boolean"}}}))
    }));
    let code = generated(&document, InterfaceStyle::Positional);
    let parsed = syn::parse_file(&code).unwrap();
    let types = parsed
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Mod(module) if module.ident == "types" => module.content.as_ref(),
            _ => None,
        })
        .unwrap();
    let mut names = std::collections::BTreeSet::new();
    for item in &types.1 {
        if let syn::Item::Struct(item) = item {
            assert!(
                names.insert(item.ident.to_string()),
                "duplicate type: {}\n{code}",
                item.ident
            );
        }
    }
    assert!(code.contains("pub enum Error"));
}

#[test]
fn error_symbols_and_dispatch_are_stable_across_response_order() {
    let document = document(json!({
        "204": {"description": "Success"},
        "401": response(json!({"$ref": "#/components/schemas/ResponseError"})),
        "500": response(json!({"$ref": "#/components/schemas/TaskCompactionSummary"}))
    }));
    let mut reversed = document.raw().clone();
    let responses = &mut reversed["paths"]["/tasks/compact"]["post"]["responses"];
    *responses = Value::Object(
        responses
            .as_object()
            .unwrap()
            .iter()
            .rev()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    let reversed = serde_json::from_value(reversed).unwrap();
    assert_eq!(
        generated(&document, InterfaceStyle::Positional),
        generated(&reversed, InterfaceStyle::Positional)
    );
}

#[test]
fn normalized_operation_collisions_are_rejected_before_enum_emission() {
    let document = document(json!({
        "204": {"description": "Success"},
        "401": response(json!({"$ref": "#/components/schemas/ResponseError"})),
        "500": response(json!({"$ref": "#/components/schemas/TaskCompactionSummary"}))
    }));
    let mut value = document.raw().clone();
    value["paths"]["/other"] = value["paths"]["/tasks/compact"].clone();
    value["paths"]["/other"]["post"]["operationId"] = json!("compact-task-queue");
    let document = serde_json::from_value(value).unwrap();
    let error = Generator::default()
        .generate_tokens_from_document(&document)
        .unwrap_err();
    assert!(
        error.to_string().contains("operation names collide"),
        "{error}"
    );
}

#[test]
fn default_recovery_stays_inside_concrete_status_arm_before_enum_mapping() {
    let document = document(json!({
        "204": {"description": "Success"},
        "401": response(json!({"$ref": "#/components/schemas/ResponseError"})),
        "500": response(json!({"$ref": "#/components/schemas/TaskCompactionSummary"}))
    }));
    let tokens = Generator::default()
        .generate_tokens_from_document(&document)
        .unwrap();
    syn::parse2::<syn::File>(tokens.clone()).unwrap();
    let code = tokens.to_string().split_whitespace().collect::<String>();
    let first = code.find("401u16=>").unwrap();
    let second = code.find("500u16=>").unwrap();
    for (arm, typ, variant) in [
        (&code[first..second], "ResponseError", "Status401"),
        (&code[second..], "TaskCompactionSummary", "Status500"),
    ] {
        let decode = arm
            .find(&format!(
                "ResponseValue::<types::{typ}>::from_response_with_recovery"
            ))
            .unwrap();
        let map = arm.find(&format!(
            ".map::<_,_,Error<response::compact_task_queue::Error>>(response::compact_task_queue::Error::{variant})?"
        )).unwrap();
        assert!(decode < map, "{arm}");
        assert!(arm.contains(".await?.map"), "{arm}");
    }
}
