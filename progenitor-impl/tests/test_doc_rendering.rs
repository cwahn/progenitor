// Copyright 2026 Oxide Computer Company

use progenitor_impl::{GenerationSettings, Generator, InterfaceStyle, OpenApiDocument, TagStyle};
use serde_json::json;

fn docs(attributes: &[syn::Attribute]) -> String {
    attributes
        .iter()
        .filter_map(|attribute| match &attribute.meta {
            syn::Meta::NameValue(value) if value.path.is_ident("doc") => match &value.value {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(value),
                    ..
                }) => Some(value.value()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn emitted_docs_use_shared_renderer_without_changing_schema_or_json_fences() {
    let prose = "Use snake_case. See https://example.org/docs\n\n- First item\nlazy_continuation\n\n```json\n{\"snake_case\":\"https://example.org/raw\"}\n```";
    let document: OpenApiDocument = serde_json::from_value(json!({
        "openapi": "3.1.0",
        "info": {"title": "Docs", "version": "1", "description": prose, "termsOfService": "https://example.org/terms"},
        "tags": [{"name": "items", "description": prose}],
        "paths": {"/items/{id}": {"get": {
            "operationId": "get_item", "tags": ["items"], "summary": "Use client_summary", "description": prose,
            "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "string"}, "description": "Use parameter_name\nparameter_continuation"}],
            "responses": {"204": {"description": "Success"}}
        }}}
    })).unwrap();
    let original = document.raw().clone();
    for (interface, tag) in [
        (InterfaceStyle::Positional, TagStyle::Merged),
        (InterfaceStyle::Builder, TagStyle::Merged),
        (InterfaceStyle::Builder, TagStyle::Separate),
    ] {
        let mut settings = GenerationSettings::default();
        settings.with_interface(interface).with_tag(tag);
        let tokens = Generator::new(&settings)
            .generate_tokens_from_document(&document)
            .unwrap();
        let file: syn::File = syn::parse2(tokens).unwrap();
        let client_docs = file
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Struct(item) if item.ident == "Client" => Some(docs(&item.attrs)),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            client_docs,
            typify::render_doc_markdown(&format!(
                "Client for Docs\n\n{prose}\n\nhttps://example.org/terms\n\nVersion: 1"
            ))
        );
        assert!(client_docs.contains("Use `snake_case`"));
        assert!(client_docs.contains("<https://example.org/docs>"));
        assert!(client_docs.contains("\n  `lazy_continuation`"));
        assert!(client_docs.contains("```json\n{\"snake_case\":\"https://example.org/raw\"}\n```"));
        let mut emitted_docs = Vec::new();
        for item in &file.items {
            match item {
                syn::Item::Impl(item) => {
                    emitted_docs.extend(item.items.iter().filter_map(|item| match item {
                        syn::ImplItem::Fn(item) => Some(docs(&item.attrs)),
                        _ => None,
                    }))
                }
                syn::Item::Trait(item) => {
                    emitted_docs.push(docs(&item.attrs));
                    emitted_docs.extend(item.items.iter().filter_map(|item| match item {
                        syn::TraitItem::Fn(item) => Some(docs(&item.attrs)),
                        _ => None,
                    }));
                }
                _ => (),
            }
        }
        assert!(
            emitted_docs
                .iter()
                .any(|doc| doc.contains("`client_summary`")),
            "{emitted_docs:?}"
        );
        assert!(
            emitted_docs
                .iter()
                .any(|doc| doc.contains("`parameter_name`\n  `parameter_continuation`")),
            "{emitted_docs:?}"
        );
        assert_eq!(document.raw(), &original);
    }
}
