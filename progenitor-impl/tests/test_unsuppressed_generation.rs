// Copyright 2026 Oxide Computer Company

use progenitor_impl::{
    GenerationSettings, Generator, HooksMode, InterfaceStyle, OpenApiDocument, TagStyle,
};
use serde_json::json;

fn generated(case: &str, interface: InterfaceStyle, tag: TagStyle, hooks: HooksMode) -> String {
    let responses = match case {
        "raw" => {
            json!({"200": {"description": "Raw", "content": {"application/octet-stream": {"schema": {"type": "string", "format": "binary"}}}}})
        }
        "heterogeneous" => {
            json!({"204": {"description": "Empty"}, "400": {"description": "Text", "content": {"application/json": {"schema": {"type": "string"}}}}, "500": {"description": "Empty"}})
        }
        "heterogeneous_typed" => {
            json!({"204": {"description": "Empty"}, "400": {"description": "Object", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Item"}}}}, "500": {"description": "Empty"}})
        }
        "typed" | "form" => {
            json!({"200": {"description": "Typed", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Item"}}}}})
        }
        _ => json!({"204": {"description": "Empty"}}),
    };
    let mut operation = json!({"operationId": "fetch", "tags": ["items"], "responses": responses});
    if case == "form" {
        operation["requestBody"] = json!({"required": true, "content": {"application/x-www-form-urlencoded": {"schema": {"$ref": "#/components/schemas/Item"}}}});
    }
    if case == "unit" {
        operation["parameters"] = json!([
            {"name": "types", "in": "query", "required": true, "schema": {"type": "string"}},
            {"name": "client", "in": "query", "required": true, "schema": {"type": "string"}},
            {"name": "__decode_response", "in": "query", "required": true, "schema": {"type": "string"}},
            {"name": "encode_path", "in": "query", "required": true, "schema": {"type": "string"}}
        ]);
    }
    let paths = if case == "empty" {
        json!({})
    } else if case == "typed" || case == "numeric_path" {
        operation["parameters"] =
            json!([{"name": "id", "in": "path", "required": true, "schema": {"type": "string"}}]);
        if case == "numeric_path" {
            operation["parameters"][0]["schema"] = json!({"type": "integer", "format": "int64"});
        }
        json!({"/items/{id}": {"post": operation}})
    } else {
        json!({"/items": {"post": operation}})
    };
    let document: OpenApiDocument = serde_json::from_value(json!({
        "openapi": "3.1.0", "info": {"title": "Imports", "version": "1"},
        "paths": paths,
        "components": {"schemas": {"Item": {"type": "object", "required": ["value"], "properties": {"value": {"type": "string"}}}}}
    })).unwrap();
    let mut settings = GenerationSettings::default();
    settings
        .with_interface(interface)
        .with_tag(tag)
        .with_hooks(hooks);
    let tokens = Generator::new(&settings)
        .generate_tokens_from_document(&document)
        .unwrap();
    rustfmt_wrapper::rustfmt(tokens).unwrap()
}

fn cases() -> impl Iterator<Item = (&'static str, InterfaceStyle, TagStyle)> {
    [
        "empty",
        "unit",
        "raw",
        "heterogeneous",
        "heterogeneous_typed",
        "typed",
        "numeric_path",
        "form",
    ]
    .into_iter()
    .flat_map(|case| {
        [
            (case, InterfaceStyle::Positional, TagStyle::Merged),
            (case, InterfaceStyle::Builder, TagStyle::Merged),
            (case, InterfaceStyle::Builder, TagStyle::Separate),
        ]
    })
}

#[test]
fn templates_emit_no_suppressions_and_preserve_hooks_and_reexports() {
    for (case, interface, tag) in cases() {
        for hooks in [HooksMode::Optional, HooksMode::Expected] {
            let code = generated(case, interface.clone(), tag.clone(), hooks);
            let compact = code.split_whitespace().collect::<String>();
            assert!(
                !compact.contains("baseurl,)"),
                "URL formatting must not emit a trailing comma: {code}"
            );
            if case == "typed" {
                let argument = if interface == InterfaceStyle::Builder {
                    "encode_path(id.as_str())"
                } else {
                    "encode_path(id)"
                };
                assert!(
                    compact.contains(argument),
                    "string paths must borrow without cloning: {code}"
                );
            }
            if case == "numeric_path" {
                assert!(
                    compact.contains("encode_path(&id.to_string())"),
                    "non-string paths must retain Display formatting: {code}"
                );
            }
            if case == "form" && interface == InterfaceStyle::Builder {
                assert!(
                    compact
                        .contains("Ok(<types::builder::Itemas::std::default::Default>::default())"),
                    "initialize the body model builder, not the operation builder: {code}"
                );
                assert!(
                    code.contains("failed: {s}"),
                    "conversion errors must preserve detail using a captured format argument: {code}"
                );
            }
            assert!(!code.contains("#[allow("), "{case}: {code}");
            assert!(
                code.contains("pub struct Item"),
                "all component models must remain emitted, including for empty documents: {code}"
            );
            assert!(
                !code.contains("client: client"),
                "builder constructors must use field shorthand: {code}"
            );
            assert!(
                code.contains("ByteStream"),
                "public ByteStream reexport must remain"
            );
            if case != "empty" {
                assert!(
                    compact.contains("Error::UnexpectedResponse(::std::boxed::Box::new(response))"),
                    "{code}"
                );
                if case.starts_with("heterogeneous") {
                    assert!(
                        compact.contains("Error::ErrorResponse(::std::boxed::Box::new("),
                        "{code}"
                    );
                    assert!(compact.contains(".map::<_,_,Error<response::fetch::Error>>(response::fetch::Error::Status400"), "{code}");
                }
                assert!(code.contains("let mut request ="), "{code}");
                assert!(code.contains(".pre(&mut request, &info)"), "{code}");
                assert!(code.contains(".exec(request, &info)"), "{code}");
                assert!(code.contains(".post(&result, &info)"), "{code}");
                assert!(
                    code.find(".pre(&mut request, &info)") < code.find(".exec(request, &info)")
                );
                assert!(code.find(".exec(request, &info)") < code.find(".post(&result, &info)"));
            }
            assert_eq!(
                code.contains("use progenitor_client::RequestBuilderExt;"),
                case == "form"
            );
            assert_eq!(
                code.contains("use progenitor_client::encode_path;"),
                case == "typed" || case == "numeric_path"
            );
            if case == "heterogeneous" {
                assert!(
                    !code.contains("use super::super::types;"),
                    "primitive error variants need no types import: {code}"
                );
            }
            if case == "heterogeneous_typed" {
                assert!(code.contains("use super::super::types;"), "{code}");
            }
            if case == "unit" && interface == InterfaceStyle::Builder {
                assert!(
                    !code.contains("use super::types;"),
                    "parameter names are not module paths: {code}"
                );
            }
        }
    }
}

/// Compile fresh clients rather than relying on goldens that predate the fix.
#[test]
#[ignore = "spawns Cargo to compile fresh generated clients"]
fn fresh_clients_have_no_unused_private_imports_or_mutability() {
    compile_fresh_clients(true);
}

#[test]
#[ignore = "spawns Cargo to exercise fresh generated clients over local HTTP"]
fn fresh_boxed_errors_preserve_payloads_and_wire_behavior() {
    compile_fresh_clients(false);
}

fn compile_fresh_clients(include_import_matrix: bool) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let output = root.join(if include_import_matrix {
        "target/unsuppressed-generated-consumer"
    } else {
        "target/boxed-error-generated-consumer"
    });
    std::fs::create_dir_all(output.join("src")).unwrap();
    let mut source = String::from("#![deny(unused_imports, unused_mut)]\n");
    for (index, (case, interface, tag)) in cases().enumerate().filter(|_| include_import_matrix) {
        for (suffix, hooks) in [
            ("optional", HooksMode::Optional),
            ("expected", HooksMode::Expected),
        ] {
            source.push_str(&format!(
                "pub mod case_{index}_{suffix} {{\n{}\n",
                generated(case, interface.clone(), tag.clone(), hooks)
            ));
            if matches!(hooks, HooksMode::Expected) {
                source.push_str("impl progenitor_client::ClientHooks for Client {}\n");
            }
            source.push_str("}\n");
        }
    }
    for (name, interface, call) in [
        (
            "boxed_positional",
            InterfaceStyle::Positional,
            "client.fetch().await",
        ),
        (
            "boxed_builder",
            InterfaceStyle::Builder,
            "client.fetch().send().await",
        ),
    ] {
        source.push_str(&format!(
            "pub mod {name} {{\n{}\n}}\n",
            generated(
                "heterogeneous_typed",
                interface,
                TagStyle::Merged,
                HooksMode::Optional
            )
        ));
        source.push_str(
            &BOXED_ERROR_HTTP_TEST
                .replace("__MODULE__", name)
                .replace("__CALL__", call),
        );
    }
    std::fs::write(output.join("src/lib.rs"), source).unwrap();
    std::fs::write(
        output.join("Cargo.toml"),
        format!(
            r#"
[package]
name = "fresh-unsuppressed-generated-consumer"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
progenitor-client = {{ path = {:?} }}
reqwest = {{ version = "0.13", features = ["json", "query", "stream"] }}
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
tokio = {{ version = "1", features = ["macros", "rt-multi-thread"] }}
"#,
            root.join("progenitor-client")
        ),
    )
    .unwrap();
    let result = std::process::Command::new(env!("CARGO"))
        .args(["test", "--offline", "--manifest-path"])
        .arg(output.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", root.join("target"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

const BOXED_ERROR_HTTP_TEST: &str = r##"
mod __MODULE___hooks {
    use std::sync::atomic::{AtomicUsize, Ordering};
    pub static ORDER: AtomicUsize = AtomicUsize::new(0);
    impl progenitor_client::ClientHooks for super::__MODULE__::Client {
        async fn pre<E>(&self, request: &mut reqwest::Request, info: &progenitor_client::OperationInfo) -> Result<(), progenitor_client::Error<E>> {
            assert_eq!(info.operation_id, "fetch");
            assert_eq!(ORDER.fetch_add(1, Ordering::SeqCst) % 2, 0);
            request.headers_mut().insert("x-hook", reqwest::header::HeaderValue::from_static("pre"));
            Ok(())
        }
        async fn exec(&self, request: reqwest::Request, info: &progenitor_client::OperationInfo) -> reqwest::Result<reqwest::Response> {
            assert_eq!(info.operation_id, "fetch");
            assert_eq!(ORDER.load(Ordering::SeqCst) % 2, 1);
            assert_eq!(request.headers()["x-hook"], "pre");
            <Self as progenitor_client::ClientInfo<()>>::client(self).execute(request).await
        }
        async fn post<E>(&self, result: &reqwest::Result<reqwest::Response>, info: &progenitor_client::OperationInfo) -> Result<(), progenitor_client::Error<E>> {
            assert_eq!(info.operation_id, "fetch");
            assert_eq!(ORDER.fetch_add(1, Ordering::SeqCst) % 2, 1);
            assert_eq!(result.as_ref().unwrap().headers()["x-test"], "retained");
            Ok(())
        }
    }
}
#[tokio::test]
async fn __MODULE___preserves_status_selected_errors_and_raw_unexpected_body() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let baseurl = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for (status, body) in [
            ("400 Bad Request", r#"{"value":"kept"}"#),
            ("500 Internal Server Error", ""),
            ("418 I'm a teapot", "unparsed raw body"),
            ("204 No Content", ""),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                let mut buffer = [0; 8192];
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
            }
            assert!(String::from_utf8(request).unwrap().contains("x-hook: pre"));
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nX-Test: retained\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let client = __MODULE__::Client::new(&baseurl);
    match __CALL__.unwrap_err() {
        progenitor_client::Error::ErrorResponse(response) => {
            let response: Box<progenitor_client::ResponseValue<__MODULE__::response::fetch::Error>> = response;
            assert_eq!(response.status().as_u16(), 400);
            assert_eq!(response.headers()["x-test"], "retained");
            match response.into_inner() {
                __MODULE__::response::fetch::Error::Status400(item) => assert_eq!(item.value, "kept"),
                other => panic!("wrong status-selected error: {other:?}"),
            }
        }
        other => panic!("wrong error category: {other:?}"),
    }
    match __CALL__.unwrap_err() {
        progenitor_client::Error::ErrorResponse(response) => {
            assert_eq!(response.status().as_u16(), 500);
            assert!(matches!(response.into_inner(), __MODULE__::response::fetch::Error::Status500(())));
        }
        other => panic!("wrong empty-error category: {other:?}"),
    }
    match __CALL__.unwrap_err() {
        progenitor_client::Error::UnexpectedResponse(response) => {
            let response: Box<reqwest::Response> = response;
            assert_eq!(response.status().as_u16(), 418);
            assert_eq!(response.headers()["x-test"], "retained");
            assert_eq!(response.text().await.unwrap(), "unparsed raw body");
        }
        other => panic!("wrong unexpected-response category: {other:?}"),
    }
    let response: progenitor_client::ResponseValue<()> = __CALL__.unwrap();
    assert_eq!(response.status().as_u16(), 204);
    assert_eq!(__MODULE___hooks::ORDER.load(std::sync::atomic::Ordering::SeqCst), 8);
    server.join().unwrap();
}
"##;
