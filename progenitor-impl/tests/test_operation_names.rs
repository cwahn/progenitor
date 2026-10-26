// Copyright 2026 Oxide Computer Company

use progenitor_impl::{
    GenerationSettings, Generator, InterfaceStyle, OpenApiDocument, validate_openapi,
};
use serde_json::{Value, json};

fn spec(paths: Value, version: &str) -> Value {
    json!({"openapi": version, "info": {"title": "Operation names", "version": "1"}, "paths": paths})
}

fn operation(id: Option<&str>) -> Value {
    let mut op = json!({"responses": {"204": {"description": "Success"}}});
    if let Some(id) = id {
        op["operationId"] = json!(id);
    }
    op
}

fn generated(value: &Value, style: InterfaceStyle) -> syn::File {
    let document: OpenApiDocument = serde_json::from_value(value.clone()).unwrap();
    let mut settings = GenerationSettings::default();
    settings.with_interface(style);
    let tokens = Generator::new(&settings)
        .generate_tokens_from_document(&document)
        .unwrap();
    assert_eq!(document.raw(), value);
    syn::parse2(tokens).unwrap()
}

fn client_methods(file: &syn::File) -> Vec<String> {
    file.items.iter().filter_map(|item| match item {
        syn::Item::Impl(item) if matches!(item.self_ty.as_ref(), syn::Type::Path(ty) if ty.path.is_ident("Client")) => Some(item),
        _ => None,
    }).flat_map(|item| item.items.iter()).filter_map(|item| match item {
        syn::ImplItem::Fn(item) => Some(item.sig.ident.to_string()),
        _ => None,
    }).collect()
}

#[test]
fn names_are_method_path_based_and_stable_across_order_and_subset() {
    let paths = json!({
        "/api/models": {"get": operation(None), "post": operation(None)},
        "/": {"get": operation(None)},
        "/existing": {"get": operation(Some("already_named"))},
    });
    for version in ["3.0.3", "3.1.0"] {
        let mut reversed = serde_json::Map::new();
        for (path, item) in paths.as_object().unwrap().iter().rev() {
            reversed.insert(path.clone(), item.clone());
        }
        for style in [InterfaceStyle::Positional, InterfaceStyle::Builder] {
            for paths in [paths.clone(), Value::Object(reversed.clone())] {
                let names = client_methods(&generated(&spec(paths, version), style.clone()));
                for expected in ["get_api_models", "post_api_models", "get", "already_named"] {
                    assert!(names.iter().any(|name| name == expected), "{names:?}");
                }
            }
            let names = client_methods(&generated(
                &spec(json!({"/api/models": {"post": operation(None)}}), version),
                style,
            ));
            assert!(names.iter().any(|name| name == "post_api_models"));
        }
    }
}

#[test]
fn explicit_ids_keep_the_existing_normalization_and_input_bytes() {
    let value = spec(
        json!({"/explicit": {"get": operation(Some("Keep-Exact.ID"))}}),
        "3.1.0",
    );
    let names = client_methods(&generated(&value, InterfaceStyle::Positional));
    assert!(names.iter().any(|name| name == "keep_exact_id"));
    let parsed: oas3::Spec = serde_json::from_value(value).unwrap();
    let before = serde_json::to_value(&parsed).unwrap();
    validate_openapi(&parsed).unwrap();
    Generator::default().generate_tokens(&parsed).unwrap();
    assert_eq!(serde_json::to_value(&parsed).unwrap(), before);
}

fn assert_collision(paths: Value, identifier: &str, sources: &[&str]) {
    let value = spec(paths, "3.1.0");
    let parsed: oas3::Spec = serde_json::from_value(value.clone()).unwrap();
    let error = validate_openapi(&parsed).unwrap_err().to_string();
    assert!(error.contains("operation names collide"), "{error}");
    assert!(error.contains(identifier), "{error}");
    assert!(
        error.contains("provide distinct explicit operationId"),
        "{error}"
    );
    for source in sources {
        assert!(error.contains(source), "{error}");
    }
    // All public generation routes must reject collisions before generation.
    assert!(Generator::default().generate_tokens(&parsed).is_err());
    assert!(Generator::default().cli(&parsed, "sdk").is_err());
    assert!(Generator::default().httpmock(&parsed, "sdk").is_err());
    let document: OpenApiDocument = serde_json::from_value(value).unwrap();
    assert!(
        Generator::default()
            .generate_tokens_from_document(&document)
            .is_err()
    );
    assert!(
        Generator::default()
            .cli_from_document(&document, "sdk")
            .is_err()
    );
    assert!(
        Generator::default()
            .httpmock_from_document(&document, "sdk")
            .is_err()
    );
}

#[test]
fn normalized_collisions_are_actionable_and_never_silently_renamed() {
    assert_collision(
        json!({
            "/a-b": {"get": operation(None)},
            "/a_b": {"get": operation(None)},
        }),
        "get_a_b",
        &["GET /a-b", "GET /a_b", "generated"],
    );
    assert_collision(
        json!({
            "/a-b": {"get": operation(None)},
            "/existing": {"get": operation(Some("get-A-B"))},
        }),
        "get_a_b",
        &["GET /a-b", "GET /existing", "explicit", "generated"],
    );
    assert_collision(
        json!({
            "/a": {"get": operation(Some("Same-Name"))},
            "/b": {"post": operation(Some("same_name"))},
        }),
        "same_name",
        &["GET /a", "POST /b", "explicit"],
    );
}

#[test]
fn builder_type_identifier_collisions_are_rejected() {
    assert_collision(
        json!({
            "/a1": {"get": operation(None)},
            "/a_1": {"get": operation(None)},
        }),
        "GetA1",
        &["GET /a1", "GET /a_1", "generated"],
    );
}

#[test]
fn duplicate_explicit_ids_keep_the_existing_error() {
    let parsed: oas3::Spec = serde_json::from_value(spec(
        json!({
            "/a": {"get": operation(Some("same"))},
            "/b": {"post": operation(Some("same"))},
        }),
        "3.0.3",
    ))
    .unwrap();
    assert_eq!(
        validate_openapi(&parsed).unwrap_err().to_string(),
        "unexpected or unhandled format in the OpenAPI document duplicate operation ID: same"
    );
}

#[test]
fn unnamed_operations_work_in_all_native_and_document_entrypoints() {
    for version in ["3.0.3", "3.1.0"] {
        let value = spec(json!({"/health": {"get": operation(None)}}), version);
        let parsed: oas3::Spec = serde_json::from_value(value.clone()).unwrap();
        let before = serde_json::to_value(&parsed).unwrap();
        validate_openapi(&parsed).unwrap();
        let document: OpenApiDocument = serde_json::from_value(value.clone()).unwrap();
        for tokens in [
            Generator::default().generate_tokens(&parsed).unwrap(),
            Generator::default().cli(&parsed, "sdk").unwrap(),
            Generator::default().httpmock(&parsed, "sdk").unwrap(),
            Generator::default()
                .generate_tokens_from_document(&document)
                .unwrap(),
            Generator::default()
                .cli_from_document(&document, "sdk")
                .unwrap(),
            Generator::default()
                .httpmock_from_document(&document, "sdk")
                .unwrap(),
        ] {
            assert!(tokens.to_string().contains("get_health"));
            syn::parse2::<syn::File>(tokens).unwrap();
        }
        assert_eq!(serde_json::to_value(&parsed).unwrap(), before);
        assert_eq!(document.raw(), &value);
    }
}

// Exact GET operation objects extracted from the pinned Hugging Face input:
// live-openapi-zYfdK5il/huggingface/openapi.json
// SHA-256: 0ff36f73b0d81ff5b811ad0b1a7911f81a0fe51ab106ff91fd353230ed8e1948
// No network or local external fixture is needed by these tests.
const HF_PATHS: &str = r##"{
"/api/models/{namespace}/{repo}/refs":{"get":{"summary":"List references","tags":["models"],"responses":{"200":{"content":{"application/json":{"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{"tags":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"ref":{"type":"string"},"targetCommit":{"type":"string"}},"required":["name","ref","targetCommit"],"additionalProperties":false}},"branches":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"ref":{"type":"string"},"targetCommit":{"type":"string"}},"required":["name","ref","targetCommit"],"additionalProperties":false}},"converts":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"ref":{"type":"string"},"targetCommit":{"type":"string"}},"required":["name","ref","targetCommit"],"additionalProperties":false}},"pullRequests":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"ref":{"type":"string"},"targetCommit":{"type":"string"}},"required":["name","ref","targetCommit"],"additionalProperties":false}}},"required":["tags","branches","converts"],"additionalProperties":false}}},"description":"List of references in the repository"}},"parameters":[{"name":"namespace","in":"path","required":true,"schema":{"type":"string"}},{"name":"repo","in":"path","required":true,"schema":{"type":"string"}},{"name":"include_prs","in":"query","schema":{"type":"string"},"required":false}]}},
"/api/models/{namespace}/{repo}/tree/{rev}/{path}":{"get":{"description":"List the content of a repository tree, with pagination support.","summary":"List folder content","tags":["models"],"responses":{"200":{"content":{"application/json":{"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"array","items":{"type":"object","properties":{"type":{"type":"string","enum":["file","directory","unknown"]},"oid":{"type":"string"},"size":{"description":"If the file is a LFS pointer, it'll return the size of the remote file","type":"integer","minimum":-9007199254740991,"maximum":9007199254740991},"lfs":{"type":"object","properties":{"oid":{"type":"string"},"size":{"type":"integer","minimum":-9007199254740991,"maximum":9007199254740991},"pointerSize":{"type":"integer","minimum":-9007199254740991,"maximum":9007199254740991}},"required":["oid","size","pointerSize"],"additionalProperties":false},"xetHash":{"type":"string"},"lastCommit":{"type":"object","properties":{"id":{"type":"string"},"title":{"type":"string"},"date":{"type":"string","format":"date-time","pattern":"^(?:(?:\\d\\d[2468][048]|\\d\\d[13579][26]|\\d\\d0[48]|[02468][048]00|[13579][26]00)-02-29|\\d{4}-(?:(?:0[13578]|1[02])-(?:0[1-9]|[12]\\d|3[01])|(?:0[469]|11)-(?:0[1-9]|[12]\\d|30)|(?:02)-(?:0[1-9]|1\\d|2[0-8])))T(?:(?:[01]\\d|2[0-3]):[0-5]\\d:[0-5]\\d(?:\\.\\d+)?(?:Z))$"}},"required":["id","title","date"],"additionalProperties":false},"securityFileStatus":{"type":"object","properties":{"status":{"type":"string","enum":["unscanned","safe","queued","error","caution","suspicious","unsafe"]},"jFrogScan":{"type":"object","properties":{"status":{"type":"string","enum":["unscanned","safe","queued","error","caution","suspicious","unsafe"]},"message":{"type":"string"},"reportLink":{"type":"string"},"reportLabel":{"type":"string"},"pickleImports":{"type":"array","items":{"type":"object","properties":{"module":{"type":"string"},"name":{"type":"string"},"safety":{"type":"string","enum":["innocuous","suspicious","dangerous"]}},"required":["module","name","safety"],"additionalProperties":false}},"version":{"type":"string"}},"required":["status"],"additionalProperties":false},"protectAiScan":{"type":"object","properties":{"status":{"type":"string","enum":["unscanned","safe","queued","error","caution","suspicious","unsafe"]},"message":{"type":"string"},"reportLink":{"type":"string"},"reportLabel":{"type":"string"},"pickleImports":{"type":"array","items":{"type":"object","properties":{"module":{"type":"string"},"name":{"type":"string"},"safety":{"type":"string","enum":["innocuous","suspicious","dangerous"]}},"required":["module","name","safety"],"additionalProperties":false}},"version":{"type":"string"}},"required":["status"],"additionalProperties":false},"avScan":{"type":"object","properties":{"status":{"type":"string","enum":["unscanned","safe","queued","error","caution","suspicious","unsafe"]},"message":{"type":"string"},"reportLink":{"type":"string"},"reportLabel":{"type":"string"},"pickleImports":{"type":"array","items":{"type":"object","properties":{"module":{"type":"string"},"name":{"type":"string"},"safety":{"type":"string","enum":["innocuous","suspicious","dangerous"]}},"required":["module","name","safety"],"additionalProperties":false}},"version":{"type":"string"}},"required":["status"],"additionalProperties":false},"pickleImportScan":{"type":"object","properties":{"status":{"type":"string","enum":["unscanned","safe","queued","error","caution","suspicious","unsafe"]},"message":{"type":"string"},"reportLink":{"type":"string"},"reportLabel":{"type":"string"},"pickleImports":{"type":"array","items":{"type":"object","properties":{"module":{"type":"string"},"name":{"type":"string"},"safety":{"type":"string","enum":["innocuous","suspicious","dangerous"]}},"required":["module","name","safety"],"additionalProperties":false}},"version":{"type":"string"}},"required":["status"],"additionalProperties":false},"virusTotalScan":{"type":"object","properties":{"status":{"type":"string","enum":["unscanned","safe","queued","error","caution","suspicious","unsafe"]},"message":{"type":"string"},"reportLink":{"type":"string"},"reportLabel":{"type":"string"},"pickleImports":{"type":"array","items":{"type":"object","properties":{"module":{"type":"string"},"name":{"type":"string"},"safety":{"type":"string","enum":["innocuous","suspicious","dangerous"]}},"required":["module","name","safety"],"additionalProperties":false}},"version":{"type":"string"}},"required":["status"],"additionalProperties":false}},"required":["status","avScan","pickleImportScan"],"additionalProperties":false}},"required":["type","oid"],"additionalProperties":false}}}},"description":"List of entries in the repository tree","headers":{"Link":{"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string"},"description":"Link header for pagination, if applicable"}}}},"parameters":[{"name":"namespace","in":"path","required":true,"schema":{"type":"string"}},{"name":"repo","in":"path","required":true,"schema":{"type":"string"}},{"name":"rev","in":"path","required":true,"schema":{"type":"string"}},{"name":"path","in":"path","required":true,"schema":{"type":"string","description":"Wildcard path parameter"}},{"name":"expand","in":"query","schema":{"description":"If true, returns returns associated commit data for each entry and security scanner metadata.","type":"string"},"required":false},{"name":"recursive","in":"query","schema":{"description":"If true, returns the tree recursively.","type":"string"},"required":false},{"name":"limit","in":"query","schema":{"type":"integer","minimum":1,"maximum":9007199254740991,"description":"1.000 by default, 100 by default for expand=true"},"required":false},{"name":"cursor","in":"query","schema":{"type":"string"},"required":false}]}},
"/api/models/{namespace}/{repo}/commits/{rev}":{"get":{"summary":"List commits","tags":["models"],"responses":{"200":{"content":{"application/json":{"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"title":{"type":"string"},"message":{"type":"string"},"authors":{"type":"array","items":{"type":"object","properties":{"user":{"type":"string"},"avatar":{"type":"string"}},"required":["user"],"additionalProperties":false}},"date":{"type":"string","format":"date-time","pattern":"^(?:(?:\\d\\d[2468][048]|\\d\\d[13579][26]|\\d\\d0[48]|[02468][048]00|[13579][26]00)-02-29|\\d{4}-(?:(?:0[13578]|1[02])-(?:0[1-9]|[12]\\d|3[01])|(?:0[469]|11)-(?:0[1-9]|[12]\\d|30)|(?:02)-(?:0[1-9]|1\\d|2[0-8])))T(?:(?:[01]\\d|2[0-3]):[0-5]\\d:[0-5]\\d(?:\\.\\d+)?(?:Z))$"},"formatted":{"description":"Available if expand includes formatted","type":"object","properties":{"title":{"type":"string"},"message":{"type":"string"}},"required":["title"],"additionalProperties":false}},"required":["id","title","message","authors","date"],"additionalProperties":false}}}},"description":"Commits list","headers":{"X-Total-Count":{"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"integer","minimum":-9007199254740991,"maximum":9007199254740991}},"Link":{"schema":{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"string"},"description":"Link to the next page"}}}},"parameters":[{"name":"namespace","in":"path","required":true,"schema":{"type":"string"}},{"name":"repo","in":"path","required":true,"schema":{"type":"string"}},{"name":"rev","in":"path","required":true,"schema":{"type":"string"}},{"name":"p","in":"query","schema":{"default":0,"type":"integer","minimum":-9007199254740991,"maximum":9007199254740991},"required":false},{"name":"expand","in":"query","schema":{"type":"array","items":{"type":"string","enum":["formatted"]}},"required":false},{"name":"limit","in":"query","schema":{"default":50,"type":"integer","minimum":1,"maximum":1000},"required":false}]}}
}"##;

#[test]
fn authentic_huggingface_refs_tree_and_commits_have_stable_names() {
    let paths: Value = serde_json::from_str(HF_PATHS).unwrap();
    let expected = [
        (
            "/api/models/{namespace}/{repo}/refs",
            "get_api_models_namespace_repo_refs",
        ),
        (
            "/api/models/{namespace}/{repo}/tree/{rev}/{path}",
            "get_api_models_namespace_repo_tree_rev_path",
        ),
        (
            "/api/models/{namespace}/{repo}/commits/{rev}",
            "get_api_models_namespace_repo_commits_rev",
        ),
    ];
    for style in [InterfaceStyle::Positional, InterfaceStyle::Builder] {
        let all = client_methods(&generated(&spec(paths.clone(), "3.1.0"), style.clone()));
        for (path, name) in expected {
            assert!(paths[path]["get"].get("operationId").is_none());
            assert!(all.iter().any(|method| method == name), "{all:?}");
            let subset = spec(json!({path: paths[path].clone()}), "3.1.0");
            let names = client_methods(&generated(&subset, style.clone()));
            assert!(names.iter().any(|method| method == name), "{names:?}");
        }
    }
}
