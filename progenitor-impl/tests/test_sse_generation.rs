// Copyright 2026 Oxide Computer Company

use progenitor_impl::{
    GenerationSettings, Generator, InterfaceStyle, OpenApiDocument, TagStyle, UnknownResponseFields,
};

fn document() -> OpenApiDocument {
    let mut raw: serde_json::Value =
        serde_json::from_str(include_str!("../../sample_openapi/json-over-sse.json")).unwrap();
    for name in ["Response", "Delta"] {
        raw["components"]["schemas"][name]["additionalProperties"] = false.into();
    }
    OpenApiDocument::from_value(raw).unwrap()
}

fn settings() -> GenerationSettings {
    let mut settings = GenerationSettings::default();
    settings
        .with_json_over_sse("create_response", "Event")
        .with_json_over_sse("list_events", "Event");
    settings
}

fn generate(settings: &GenerationSettings, document: &OpenApiDocument) -> String {
    let tokens = Generator::new(settings)
        .generate_tokens_from_document(document)
        .unwrap();
    syn::parse2::<syn::File>(tokens.clone()).unwrap();
    rustfmt_wrapper::rustfmt(tokens).unwrap()
}

#[test]
fn opt_in_changes_only_success_decoders_and_keeps_disabled_json_signature() {
    let doc = document();
    let before = doc.raw().clone();
    let disabled = generate(&GenerationSettings::default(), &doc);
    assert!(!disabled.contains("JsonOrEventStream"));
    assert!(disabled.contains("ResponseValue<types::Response>"));
    let enabled = generate(&settings(), &doc);
    assert!(enabled.contains("JsonOrEventStream<types::Response, types::Event>"));
    assert!(enabled.contains("JsonEventStream<types::Event>"));
    assert!(enabled.contains("application/json, text/event-stream"));
    assert!(enabled.contains("from_response_with_recovery"));
    assert!(enabled.contains("Err(Error::ErrorResponse("));
    assert_eq!(doc.raw(), &before);
}

#[test]
fn json_only_contract_can_explicitly_select_response_stream_event_component() {
    let mut raw = document().raw().clone();
    raw["paths"]["/responses"]["post"]["responses"]["200"]["content"]
        .as_object_mut()
        .unwrap()
        .remove("text/event-stream");
    let event = raw["components"]["schemas"]
        .as_object_mut()
        .unwrap()
        .remove("Event")
        .unwrap();
    raw["components"]["schemas"]["ResponseStreamEvent"] = event;
    // Remove the other operation which still references the old component name.
    raw["paths"].as_object_mut().unwrap().remove("/events");
    let doc = OpenApiDocument::from_value(raw).unwrap();
    let mut settings = GenerationSettings::default();
    settings.with_json_over_sse("create_response", "ResponseStreamEvent");
    let code = generate(&settings, &doc);
    assert!(code.contains("JsonOrEventStream<types::Response, types::ResponseStreamEvent>"));
}

#[test]
fn invalid_component_is_a_generation_error() {
    let mut settings = GenerationSettings::default();
    settings.with_json_over_sse("create_response", "Missing");
    let error = Generator::new(&settings)
        .generate_tokens_from_document(&document())
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("component schema \"Missing\" does not exist")
    );
}

/// Explicitly run this test to compile freshly generated nested clients against
/// the external runtime and exercise their requests against a local HTTP server.
#[test]
#[ignore = "spawns Cargo to compile fresh generated clients"]
fn fresh_external_runtime_clients_compile_and_dispatch_local_http() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let output = std::env::var_os("SSE_TEST_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("target/sse-generated-consumer"));
    std::fs::create_dir_all(output.join("src")).unwrap();
    let mut source = String::new();
    source.push_str(&format!(
        "pub mod disabled {{\n{}\n}}\n",
        generate(&GenerationSettings::default(), &document())
    ));
    for (name, style, tag, policy) in [
        (
            "positional",
            InterfaceStyle::Positional,
            TagStyle::Merged,
            UnknownResponseFields::WarnAndIgnore,
        ),
        (
            "builder",
            InterfaceStyle::Builder,
            TagStyle::Separate,
            UnknownResponseFields::WarnAndIgnore,
        ),
        (
            "strict",
            InterfaceStyle::Positional,
            TagStyle::Merged,
            UnknownResponseFields::Reject,
        ),
    ] {
        let mut settings = settings();
        settings
            .with_interface(style)
            .with_tag(tag)
            .with_unknown_response_fields(policy);
        source.push_str(&format!(
            "pub mod {name} {{\n{}\n}}\n",
            generate(&settings, &document())
        ));
    }
    source.push_str(include_str!("data/sse_generated_runtime.rs"));
    std::fs::write(output.join("src/lib.rs"), source).unwrap();
    std::fs::write(
        output.join("Cargo.toml"),
        format!(
            r#"
[package]
name = "fresh-sse-generated-consumer"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
progenitor-client = {{ path = {:?} }}
reqwest = {{ version = "0.13", features = ["json", "query", "stream"] }}
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
futures = "0.3"
tokio = {{ version = "1", features = ["macros", "rt-multi-thread"] }}
"#,
            root.join("progenitor-client")
        ),
    )
    .unwrap();
    let result = std::process::Command::new(env!("CARGO"))
        .args(["test", "--offline", "--manifest-path"])
        .arg(output.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", output.join("target"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    println!("{}", String::from_utf8_lossy(&result.stdout));
}
