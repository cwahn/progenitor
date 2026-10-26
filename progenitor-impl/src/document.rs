// Copyright 2026 Oxide Computer Company

//! Lossless schema input alongside the OpenAPI operation model.

use std::collections::{BTreeMap, BTreeSet};

use schemars::schema::Schema;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value, json};

use crate::{Error, Generator, Result};

// Only our operation view contains this extension. It is never passed to Typify.
pub(crate) const SCHEMA_EXTENSION: &str = "progenitor-normalized-schema";
const SCHEMA_KEY: &str = "x-progenitor-normalized-schema";
const POINTER_FRAGMENT: &percent_encoding::AsciiSet =
    &percent_encoding::NON_ALPHANUMERIC.remove(b'/').remove(b'~');

/// An OpenAPI input retaining schemas before the operation parser can discard
/// JSON Schema keywords. Deserialize JSON or YAML directly into this type.
///
/// The operation view is intentionally separate from the normalized schemas:
/// serializing an ordinary `oas3::Spec` cannot recover lost input keywords.
#[derive(Debug, Clone)]
pub struct OpenApiDocument {
    /// Operation-parser view. Schema extensions in this view are internal;
    /// callers should not mutate them or use this view as the original document.
    pub spec: oas3::Spec,
    raw: Value,
    schemas: BTreeMap<String, Schema>,
    diagnostics: Vec<SchemaDiagnostic>,
}

/// A nonfatal schema-input diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDiagnostic {
    /// JSON pointer to the affected input keyword.
    pub location: String,
    /// Explanation of the compatibility action taken.
    pub message: String,
}

impl<'de> Deserialize<'de> for OpenApiDocument {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::from_value(Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl OpenApiDocument {
    /// Parse an unmodified OpenAPI JSON value, normalizing schemas inside the
    /// generator rather than requiring an application-specific preprocessor.
    pub fn from_value(raw: Value) -> Result<Self> {
        let version = raw
            .get("openapi")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("#/openapi", "expected an OpenAPI version string"))?;
        let legacy = if version.starts_with("3.0.") {
            true
        } else if version.starts_with("3.1.") {
            false
        } else {
            return Err(invalid("#/openapi", "expected OpenAPI 3.0.x or 3.1.x"));
        };
        if !legacy && let Some(dialect) = raw.get("jsonSchemaDialect") {
            check_dialect(dialect, "#/jsonSchemaDialect")?;
        }
        let names = raw
            .pointer("/components/schemas")
            .and_then(Value::as_object)
            .map(|schemas| schemas.keys().cloned().collect())
            .unwrap_or_default();
        let normalizer = Normalizer { legacy, names };
        let mut normalized = raw.clone();
        let mut locations = Vec::new();
        normalize_document(&mut normalized, "#", &normalizer, &mut locations)?;
        if !legacy {
            // Resolve proofs against a complete, immutable normalized view, so
            // component order cannot affect reference-aware simplification.
            let proof_document = normalized.clone();
            for location in &locations {
                simplify_scalar_any_of(
                    normalized.pointer_mut(&location[1..]).unwrap(),
                    &proof_document,
                );
            }
        }
        let mut diagnostics = Vec::new();
        sanitize_defaults(&mut normalized, &locations, &mut diagnostics)?;
        let mut schemas = BTreeMap::new();
        let mut view = normalized.clone();
        for location in &locations {
            let pointer = &location[1..];
            let value = normalized
                .pointer(pointer)
                .expect("recorded schema location");
            let schema = from_normalized(value).map_err(|e| invalid(location, e.to_string()))?;
            schemas.insert(location.clone(), schema);
            if value
                .as_object()
                .is_some_and(|map| map.len() == 1 && map.contains_key("$ref"))
            {
                continue;
            }
            // Keep the familiar operation shape and names: unlike hoisting,
            // this does not turn each inline schema into a public named type.
            let mut placeholder = value.as_object().cloned().unwrap_or_default();
            // oas3's reference variant discards siblings. Always deserialize
            // this trusted carrier as an object, including for bare references.
            placeholder.remove("$ref");
            placeholder.remove("exclusiveMinimum");
            placeholder.remove("exclusiveMaximum");
            placeholder.remove("items");
            placeholder.remove("properties");
            placeholder.remove("additionalProperties");
            placeholder.remove("allOf");
            placeholder.remove("anyOf");
            placeholder.remove("oneOf");
            placeholder.insert(SCHEMA_KEY.into(), value.clone());
            *view.pointer_mut(pointer).expect("recorded schema location") =
                Value::Object(placeholder);
        }
        let mut spec: oas3::Spec =
            serde_json::from_value(view).map_err(|e| invalid("#", e.to_string()))?;
        let mut order = Map::new();
        if let Some(paths) = raw.get("paths").and_then(Value::as_object) {
            for item in paths.values().filter_map(Value::as_object) {
                for method in [
                    "get", "put", "post", "delete", "options", "head", "patch", "trace",
                ] {
                    if let Some(id) = item
                        .get(method)
                        .and_then(|op| op.get("operationId"))
                        .and_then(Value::as_str)
                    {
                        let index = order.len();
                        order.insert(id.into(), json!(index));
                    }
                }
            }
        }
        spec.extensions
            .insert("progenitor-operation-order".into(), Value::Object(order));
        Ok(Self {
            spec,
            raw,
            schemas,
            diagnostics,
        })
    }

    /// The unmodified input, including annotations and extension payloads.
    pub fn raw(&self) -> &Value {
        &self.raw
    }

    /// Normalized root schemas keyed by input JSON pointer. Includes inline
    /// parameter, request, response, and header schemas, not just components.
    pub fn schemas(&self) -> &BTreeMap<String, Schema> {
        &self.schemas
    }

    /// Compatibility diagnostics, including invalid defaults skipped during
    /// generation. Valid default annotations are retained.
    pub fn diagnostics(&self) -> &[SchemaDiagnostic] {
        &self.diagnostics
    }
}

pub(crate) fn order_methods(spec: &oas3::Spec, methods: &mut [crate::method::OperationMethod]) {
    if let Some(order) = spec
        .extensions
        .get("progenitor-operation-order")
        .and_then(Value::as_object)
    {
        methods.sort_by_key(|method| {
            order
                .get(&method.operation_id)
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX)
        });
    }
}

impl Generator {
    /// Generate a client from lossless, version-aware OpenAPI input.
    pub fn generate_tokens_from_document(
        &mut self,
        document: &OpenApiDocument,
    ) -> Result<proc_macro2::TokenStream> {
        self.generate_tokens(&document.spec)
    }

    /// Generate a CLI from lossless, version-aware OpenAPI input.
    pub fn cli_from_document(
        &mut self,
        document: &OpenApiDocument,
        crate_name: &str,
    ) -> Result<proc_macro2::TokenStream> {
        self.cli(&document.spec, crate_name)
    }

    /// Generate HTTP mock helpers from lossless, version-aware OpenAPI input.
    pub fn httpmock_from_document(
        &mut self,
        document: &OpenApiDocument,
        crate_path: &str,
    ) -> Result<proc_macro2::TokenStream> {
        self.httpmock(&document.spec, crate_path)
    }
}

fn invalid(location: &str, reason: impl Into<String>) -> Error {
    Error::InvalidSchema {
        location: location.into(),
        reason: reason.into(),
    }
}

fn child(location: &str, key: &str) -> String {
    format!("{}/{}", location, key.replace('~', "~0").replace('/', "~1"))
}

fn check_dialect(dialect: &Value, location: &str) -> Result<()> {
    match dialect.as_str() {
        Some(
            "https://spec.openapis.org/oas/3.1/dialect/base"
            | "https://json-schema.org/draft/2020-12/schema",
        ) => Ok(()),
        _ => Err(invalid(location, "unsupported schema dialect")),
    }
}

// Walk only OpenAPI structure. Stop at a Schema Object; its data-bearing
// keywords (default/example/enum/extensions) must not be walked as documents.
fn normalize_document(
    value: &mut Value,
    location: &str,
    normalizer: &Normalizer,
    locations: &mut Vec<String>,
) -> Result<()> {
    visit_openapi(value, location, "document", normalizer, locations)
}

fn visit_openapi(
    value: &mut Value,
    location: &str,
    kind: &str,
    normalizer: &Normalizer,
    locations: &mut Vec<String>,
) -> Result<()> {
    if kind == "schema" {
        *value = normalizer.normalize(value, location)?;
        locations.push(location.into());
        return Ok(());
    }
    if let Some(item_kind) = kind.strip_prefix("map:") {
        if let Some(map) = value.as_object_mut() {
            for (key, value) in map {
                if item_kind != "path" || !key.starts_with("x-") {
                    visit_openapi(
                        value,
                        &child(location, key),
                        item_kind,
                        normalizer,
                        locations,
                    )?;
                }
            }
        }
        return Ok(());
    }
    if kind == "parameters" {
        if let Some(values) = value.as_array_mut() {
            for (i, value) in values.iter_mut().enumerate() {
                visit_openapi(
                    value,
                    &child(location, &i.to_string()),
                    "parameter",
                    normalizer,
                    locations,
                )?;
            }
        }
        return Ok(());
    }
    let Some(map) = value.as_object_mut() else {
        return Ok(());
    };
    for (key, value) in map {
        let next_kind = match (kind, key.as_str()) {
            ("document", "components") => "components",
            ("document", "paths" | "webhooks") => "map:path",
            ("components", "schemas") => "map:schema",
            ("components", "parameters" | "headers") => "map:parameter",
            ("components", "requestBodies") => "map:body",
            ("components", "responses") => "map:response",
            ("components", "pathItems") => "map:path",
            ("components", "callbacks") => "map:map:path",
            (
                "path",
                "get" | "put" | "post" | "delete" | "options" | "head" | "patch" | "trace",
            ) => "operation",
            ("path" | "operation", "parameters") => "parameters",
            ("operation", "requestBody") => "body",
            ("operation", "responses") => "map:response",
            ("operation", "callbacks") => "map:map:path",
            ("parameter" | "media", "schema") => "schema",
            ("parameter" | "body" | "response", "content") => "map:media",
            ("response" | "encoding", "headers") => "map:parameter",
            ("media", "encoding") => "map:encoding",
            _ => continue,
        };
        visit_openapi(
            value,
            &child(location, key),
            next_kind,
            normalizer,
            locations,
        )?;
    }
    Ok(())
}

struct Normalizer {
    legacy: bool,
    names: BTreeSet<String>,
}

impl Normalizer {
    fn normalize(&self, schema: &Value, location: &str) -> Result<Value> {
        if schema.is_boolean() && !self.legacy {
            return Ok(schema.clone());
        }
        let mut map = schema
            .as_object()
            .cloned()
            .ok_or_else(|| invalid(location, "expected a schema object"))?;
        if map.contains_key(SCHEMA_KEY) {
            return Err(invalid(
                &child(location, SCHEMA_KEY),
                "reserved generator extension",
            ));
        }
        if let Some(reference) = map.get("$ref") {
            self.check_ref(reference, &child(location, "$ref"))?;
            if self.legacy {
                // Original openapiv3::ReferenceOr ignores all 3.0 siblings.
                return Ok(json!({ "$ref": reference }));
            }
        }
        if !self.legacy
            && let Some(dialect) = map.remove("$schema")
        {
            check_dialect(&dialect, &child(location, "$schema"))?;
            // The normalized view targets the generator's schema model, not
            // the declared source dialect. Keep the declaration only in raw.
        }
        for (key, value) in &mut map {
            let at = child(location, key);
            match key.as_str() {
                "$defs" | "definitions" | "$id" | "$anchor" | "$dynamicAnchor" | "$dynamicRef"
                | "$recursiveRef" | "$recursiveAnchor" | "$schema" => {
                    return Err(invalid(
                        &at,
                        "unsupported schema reference scope or dialect keyword",
                    ));
                }
                "unevaluatedProperties"
                | "unevaluatedItems"
                | "dependentSchemas"
                | "dependentRequired"
                | "dependencies"
                | "minContains"
                | "maxContains"
                | "contains"
                | "if"
                | "then"
                | "else"
                | "additionalItems" => return Err(invalid(&at, "unsupported schema constraint")),
                "properties" | "patternProperties" => {
                    let values = value
                        .as_object_mut()
                        .ok_or_else(|| invalid(&at, "expected a schema map"))?;
                    for (name, value) in values {
                        *value = self.normalize(value, &child(&at, name))?;
                    }
                }
                "oneOf" | "anyOf" | "allOf" | "prefixItems" => {
                    let values = value
                        .as_array_mut()
                        .ok_or_else(|| invalid(&at, "expected an array of schemas"))?;
                    if values.is_empty() {
                        return Err(invalid(&at, "schema array must not be empty"));
                    }
                    for (i, value) in values.iter_mut().enumerate() {
                        *value = self.normalize(value, &child(&at, &i.to_string()))?;
                    }
                }
                "items" | "not" | "propertyNames" => *value = self.normalize(value, &at)?,
                "additionalProperties" if !value.is_boolean() => {
                    *value = self.normalize(value, &at)?
                }
                _ => (),
            }
        }
        // OpenAPI annotations which the previous converter did not pass to
        // Typify. Unknown JSON Schema annotations and vendor data remain data.
        for key in ["discriminator", "externalDocs", "xml"] {
            map.remove(key);
        }
        if let Some(example) = map.remove("example") {
            map.entry("examples").or_insert_with(|| json!([example]));
        }
        if self.legacy {
            if matches!(map.get("type"), Some(Value::Array(_))) {
                return Err(invalid(
                    &child(location, "type"),
                    "type arrays require OpenAPI 3.1",
                ));
            }
            for (bound, exclusive) in [
                ("minimum", "exclusiveMinimum"),
                ("maximum", "exclusiveMaximum"),
            ] {
                match map.remove(exclusive) {
                    Some(Value::Bool(true)) => {
                        if let Some(bound) = map.remove(bound) {
                            map.insert(exclusive.into(), bound);
                        }
                    }
                    Some(Value::Bool(false)) | None => (),
                    Some(_) => {
                        return Err(invalid(
                            &child(location, exclusive),
                            "OpenAPI 3.0 exclusive bounds must be boolean",
                        ));
                    }
                }
            }
            // Option<Value> in the original 3.0 model did not retain null.
            if map.get("default") == Some(&Value::Null) {
                map.remove("default");
            }
            let nullable = match map.remove("nullable") {
                None | Some(Value::Bool(false)) => false,
                Some(Value::Bool(true)) => true,
                Some(_) => return Err(invalid(&child(location, "nullable"), "expected boolean")),
            };
            if nullable {
                make_nullable_30(&mut map);
            }
            if map.get("enum") == Some(&json!([null])) && !map.contains_key("type") {
                map.remove("enum");
                map.insert("type".into(), json!("null"));
            }
        } else {
            if map.contains_key("nullable") {
                return Err(invalid(
                    &child(location, "nullable"),
                    "nullable is a 3.0 keyword; use a null type or union",
                ));
            }
            for key in ["exclusiveMinimum", "exclusiveMaximum"] {
                if let Some(value) = map.get(key)
                    && !value.is_number()
                {
                    return Err(invalid(
                        &child(location, key),
                        "OpenAPI 3.1 exclusive bounds must be numeric",
                    ));
                }
            }
            if let Some(prefix) = map.remove("prefixItems") {
                if map.get("items") == Some(&Value::Bool(false)) {
                    let length = prefix.as_array().unwrap().len() as u64;
                    let max = map
                        .get("maxItems")
                        .and_then(Value::as_u64)
                        .unwrap_or(length)
                        .min(length);
                    map.insert("maxItems".into(), json!(max));
                }
                let min = map.get("minItems").and_then(Value::as_u64);
                let max = map.get("maxItems").and_then(Value::as_u64);
                if min.is_none()
                    || min != max
                    || min == Some(0)
                    || map.get("uniqueItems") == Some(&Value::Bool(true))
                {
                    return Err(invalid(
                        &child(location, "prefixItems"),
                        "only fixed-length non-unique tuples are supported; specify equal positive minItems and maxItems (items:false supplies the prefix maximum)",
                    ));
                }
                if let Some(items) = map.remove("items") {
                    map.insert("additionalItems".into(), items);
                }
                map.insert("items".into(), prefix);
            }
        }
        // Typify 0.8 ignores const_value. Singleton enums carry the same
        // assertion, including when const and enum occur simultaneously.
        if let Some(value) = map.remove("const") {
            if let Some(values) = map.get("enum").and_then(Value::as_array)
                && !values.contains(&value)
            {
                return Ok(Value::Bool(false));
            }
            map.insert("enum".into(), json!([value]));
        }
        if !self.legacy {
            prune_redundant_any_of(&mut map);
            preserve_supported_negation(&mut map, location)?;
            if map.contains_key("oneOf") && map.contains_key("anyOf") {
                return Err(invalid(
                    &child(location, "oneOf"),
                    "simultaneous oneOf and anyOf are unsupported by the type generator",
                ));
            }
            if let Some(patterns) = map.get("patternProperties").and_then(Value::as_object) {
                let same = patterns
                    .values()
                    .next()
                    .is_some_and(|first| patterns.values().all(|value| value == first));
                let named = map
                    .get("properties")
                    .and_then(Value::as_object)
                    .is_some_and(|p| !p.is_empty())
                    || map
                        .get("required")
                        .and_then(Value::as_array)
                        .is_some_and(|p| !p.is_empty());
                if !patterns.is_empty()
                    && (!same
                        || named
                        || map.contains_key("propertyNames")
                        || map.get("additionalProperties") != Some(&Value::Bool(false)))
                {
                    return Err(invalid(
                        &child(location, "patternProperties"),
                        "only closed maps with identical pattern value schemas and no named properties are supported",
                    ));
                }
            }
            if let Some(names) = map.get("propertyNames") {
                if names == &Value::Bool(false)
                    || map
                        .get("properties")
                        .and_then(Value::as_object)
                        .is_some_and(|p| !p.is_empty())
                {
                    return Err(invalid(
                        &child(location, "propertyNames"),
                        "propertyNames is supported only on maps with string-like key schemas",
                    ));
                }
            }
        }
        // Draft 7 validators ignore $ref siblings. Make 3.1 conjunction
        // explicit for both validation and Typify's reference/type fast path.
        if map.contains_key("$ref")
            && map.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "$ref"
                        | "title"
                        | "description"
                        | "default"
                        | "deprecated"
                        | "readOnly"
                        | "writeOnly"
                        | "examples"
                ) && !key.starts_with("x-")
            })
        {
            let reference = map.remove("$ref").unwrap();
            let mut metadata = Map::new();
            for key in [
                "title",
                "description",
                "default",
                "deprecated",
                "readOnly",
                "writeOnly",
                "examples",
            ] {
                if let Some(value) = map.remove(key) {
                    metadata.insert(key.into(), value);
                }
            }
            metadata.insert("allOf".into(), json!([{ "$ref": reference }, map]));
            map = metadata;
        }
        let value = Value::Object(map);
        from_normalized(&value).map_err(|e| invalid(location, format!("invalid schema: {e}")))?;
        Ok(value)
    }

    fn check_ref(&self, value: &Value, location: &str) -> Result<()> {
        let reference = value
            .as_str()
            .ok_or_else(|| invalid(location, "expected reference string"))?;
        let name = reference
            .strip_prefix("#/components/schemas/")
            .ok_or_else(|| {
                invalid(
                    location,
                    "only local #/components/schemas/<name> references are supported",
                )
            })?;
        if name.contains('/') {
            return Err(invalid(
                location,
                "references into schema interiors are unsupported",
            ));
        }
        let name = name.replace("~1", "/").replace("~0", "~");
        if !self.names.contains(&name) {
            return Err(invalid(
                location,
                format!("missing schema reference {reference}"),
            ));
        }
        Ok(())
    }
}

// A plain type branch accepts every instance accepted by a constrained branch
// declaring the same type. Keep the broad branch rather than asking Typify to
// model this overlapping anyOf as a flattened object (which cannot represent
// scalar values or their otherwise-valid defaults).
fn prune_redundant_any_of(map: &mut Map<String, Value>) {
    let Some(branches) = map.get_mut("anyOf").and_then(Value::as_array_mut) else {
        return;
    };
    let broad = branches
        .iter()
        .filter_map(Value::as_object)
        .filter(|branch| branch.len() == 1)
        .filter_map(|branch| branch.get("type").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    branches.retain(|branch| {
        !branch.as_object().is_some_and(|branch| {
            branch.len() > 1
                && branch
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|typ| broad.contains(typ))
        })
    });
    let singleton_type = (branches.len() == 1)
        .then(|| branches[0].as_object())
        .flatten()
        .filter(|branch| branch.len() == 1)
        .and_then(|branch| branch.get("type"))
        .cloned();
    if let Some(typ) = singleton_type
        && map.get("type").is_none_or(|outer| outer == &typ)
    {
        map.remove("anyOf");
        map.insert("type".into(), typ);
    }
}

// These are separate facts: a constrained string is a subset of strings, but
// cannot witness that an anyOf accepts *every* string. Unknown assertions may
// establish neither fact; in particular they must never establish broadness.
#[derive(Clone, Copy, Default)]
struct ScalarProof {
    subset: bool,
    broad: bool,
}

fn scalar_proof(
    schema: &Value,
    typ: &str,
    document: &Value,
    visiting: &mut BTreeSet<String>,
) -> ScalarProof {
    let Some(map) = schema.as_object() else {
        return ScalarProof::default();
    };
    let reference = map.get("$ref").map(|reference| {
        let Some(reference) = reference
            .as_str()
            .filter(|reference| reference.starts_with("#/components/schemas/"))
        else {
            return ScalarProof::default();
        };
        if !visiting.insert(reference.into()) {
            return ScalarProof::default();
        }
        let proof = document
            .pointer(&reference[1..])
            .map(|target| scalar_proof(target, typ, document, visiting))
            .unwrap_or_default();
        visiting.remove(reference);
        proof
    });
    let union = map.get("anyOf").map(|branches| {
        let proofs = branches
            .as_array()
            .into_iter()
            .flatten()
            .map(|branch| scalar_proof(branch, typ, document, visiting))
            .collect::<Vec<_>>();
        ScalarProof {
            subset: !proofs.is_empty() && proofs.iter().all(|proof| proof.subset),
            broad: proofs.iter().any(|proof| proof.broad),
        }
    });
    let intersection = map.get("allOf").map(|branches| {
        let proofs = branches
            .as_array()
            .into_iter()
            .flatten()
            .map(|branch| scalar_proof(branch, typ, document, visiting))
            .collect::<Vec<_>>();
        ScalarProof {
            subset: proofs.iter().any(|proof| proof.subset),
            broad: !proofs.is_empty() && proofs.iter().all(|proof| proof.broad),
        }
    });
    let matching_type = map.get("type").and_then(Value::as_str) == Some(typ);
    let subset = matching_type
        || reference.is_some_and(|proof| proof.subset)
        || union.is_some_and(|proof| proof.subset)
        || intersection.is_some_and(|proof| proof.subset);
    let broad = subset
        && map.keys().all(|key| {
            matches!(
                key.as_str(),
                "type"
                    | "$ref"
                    | "anyOf"
                    | "allOf"
                    | "title"
                    | "description"
                    | "default"
                    | "deprecated"
                    | "readOnly"
                    | "writeOnly"
                    | "examples"
            ) || key.starts_with("x-")
        })
        && (!map.contains_key("type") || matching_type)
        && reference.is_none_or(|proof| proof.broad)
        && union.is_none_or(|proof| proof.broad)
        && intersection.is_none_or(|proof| proof.broad);
    ScalarProof { subset, broad }
}

fn simplify_scalar_any_of(schema: &mut Value, document: &Value) {
    let Some(map) = schema.as_object_mut() else {
        return;
    };
    // Visit schema positions only, never annotations, defaults, or enum data.
    for (key, value) in map.iter_mut() {
        match key.as_str() {
            "properties" | "patternProperties" => {
                if let Some(properties) = value.as_object_mut() {
                    for value in properties.values_mut() {
                        simplify_scalar_any_of(value, document);
                    }
                }
            }
            "allOf" | "anyOf" | "oneOf" | "items" if value.is_array() => {
                for value in value.as_array_mut().unwrap() {
                    simplify_scalar_any_of(value, document);
                }
            }
            "items" | "additionalItems" | "additionalProperties" | "propertyNames" | "not" => {
                simplify_scalar_any_of(value, document);
            }
            _ => (),
        }
    }
    let Some(branches) = map.get("anyOf").and_then(Value::as_array) else {
        return;
    };
    for typ in ["string", "number", "integer", "boolean", "null"] {
        // Leave every outer assertion/annotation intact. An incompatible outer
        // type is intentionally left to the existing generator path.
        if map.get("type").is_some_and(|outer| outer != typ) {
            continue;
        }
        let proofs = branches
            .iter()
            .map(|branch| scalar_proof(branch, typ, document, &mut BTreeSet::new()))
            .collect::<Vec<_>>();
        if proofs.iter().all(|proof| proof.subset) && proofs.iter().any(|proof| proof.broad) {
            map.remove("anyOf");
            map.insert("type".into(), json!(typ));
            break;
        }
    }
}

// Typify's general not merger ignores string predicates. Encode a supported
// negated pattern as an equivalent string pattern as well, so it survives that
// merger. Reject other nontrivial negations instead of silently weakening them.
fn preserve_supported_negation(map: &mut Map<String, Value>, location: &str) -> Result<()> {
    let Some(negated) = map.get("not") else {
        return Ok(());
    };
    if negated.is_boolean() {
        return Ok(());
    }
    let pattern = negated
        .as_object()
        .filter(|m| m.len() == 1)
        .and_then(|m| m.get("pattern"))
        .and_then(Value::as_str);
    let Some(pattern) = pattern else {
        return Err(invalid(
            &child(location, "not"),
            "unsupported negation; supported forms are boolean schemas or a single string pattern",
        ));
    };
    if map.get("type").and_then(Value::as_str) != Some("string") {
        return Err(invalid(
            &child(location, "not"),
            "negated patterns currently require type: string",
        ));
    }
    // JSON Schema patterns search anywhere in the string. [\s\S] also covers
    // newlines, unlike '.', and preserves the meaning of anchors inside P.
    let positive = map
        .get("pattern")
        .and_then(Value::as_str)
        .map(|pattern| format!(r"(?=[\s\S]*(?:{pattern}))"))
        .unwrap_or_default();
    let combined = format!(r"^{positive}(?![\s\S]*(?:{pattern}))[\s\S]*$");
    map.insert("pattern".into(), Value::String(combined));
    Ok(())
}

fn make_nullable_30(map: &mut Map<String, Value>) {
    if let Some(Value::String(typ)) = map.get("type") {
        map.insert("type".into(), json!([typ, "null"]));
    } else if ["oneOf", "anyOf", "allOf", "not"]
        .iter()
        .any(|key| map.contains_key(*key))
    {
        let mut inner = std::mem::take(map);
        for key in [
            "title",
            "description",
            "default",
            "deprecated",
            "readOnly",
            "writeOnly",
            "examples",
        ] {
            if let Some(value) = inner.remove(key) {
                map.insert(key.into(), value);
            }
        }
        let extensions = inner
            .keys()
            .filter(|key| key.starts_with("x-"))
            .cloned()
            .collect::<Vec<_>>();
        for key in extensions {
            map.insert(key.clone(), inner.remove(&key).unwrap());
        }
        map.insert("oneOf".into(), json!([{ "type": "null" }, inner]));
    }
}

// serde's Option<Value> normally conflates an absent default and default:null.
// Restore the latter after parsing; it is a real annotation in JSON Schema.
pub(crate) fn from_normalized(value: &Value) -> serde_json::Result<Schema> {
    let mut schema: Schema = serde_json::from_value(value.clone())?;
    restore_null_defaults(value, &mut schema);
    Ok(schema)
}

fn restore_null_defaults(value: &Value, schema: &mut Schema) {
    let Schema::Object(schema) = schema else {
        return;
    };
    if value.get("default") == Some(&Value::Null) {
        schema.metadata().default = Some(Value::Null);
    }
    if let Some(object) = &mut schema.object {
        for (key, schemas) in [
            ("properties", &mut object.properties),
            ("patternProperties", &mut object.pattern_properties),
        ] {
            for (name, schema) in schemas {
                if let Some(value) = value.get(key).and_then(|v| v.get(name)) {
                    restore_null_defaults(value, schema);
                }
            }
        }
        for (key, schema) in [
            ("additionalProperties", &mut object.additional_properties),
            ("propertyNames", &mut object.property_names),
        ] {
            if let (Some(value), Some(schema)) = (value.get(key), schema) {
                restore_null_defaults(value, schema);
            }
        }
    }
    if let Some(array) = &mut schema.array {
        if let (Some(value), Some(items)) = (value.get("items"), &mut array.items) {
            match items {
                schemars::schema::SingleOrVec::Single(schema) => {
                    restore_null_defaults(value, schema)
                }
                schemars::schema::SingleOrVec::Vec(schemas) => {
                    if let Some(values) = value.as_array() {
                        for (value, schema) in values.iter().zip(schemas) {
                            restore_null_defaults(value, schema);
                        }
                    }
                }
            }
        }
        if let (Some(value), Some(schema)) =
            (value.get("additionalItems"), &mut array.additional_items)
        {
            restore_null_defaults(value, schema);
        }
    }
    if let Some(sub) = &mut schema.subschemas {
        for (key, schemas) in [
            ("allOf", &mut sub.all_of),
            ("anyOf", &mut sub.any_of),
            ("oneOf", &mut sub.one_of),
        ] {
            if let (Some(values), Some(schemas)) =
                (value.get(key).and_then(Value::as_array), schemas)
            {
                for (value, schema) in values.iter().zip(schemas) {
                    restore_null_defaults(value, schema);
                }
            }
        }
        if let (Some(value), Some(schema)) = (value.get("not"), &mut sub.not) {
            restore_null_defaults(value, schema);
        }
    }
}

// JSON Schema uses ECMA-262 patterns, as does Typify's generated validation.
// jsonschema's Rust-regex translation rejects valid ECMA character classes
// (for example an unescaped '[' inside a class). Use its keyword extension
// point to run the original pattern with the same ECMA engine as generated code;
// all other constraints and reference/default validation remain unchanged.
struct EcmaPattern {
    regex: regress::Regex,
    pattern: String,
    location: jsonschema::paths::Location,
}

impl jsonschema::Keyword for EcmaPattern {
    fn is_valid(&self, instance: &Value) -> bool {
        instance
            .as_str()
            .is_none_or(|text| self.regex.find(text).is_some())
    }

    fn validate<'i>(
        &self,
        instance: &'i Value,
        location: &jsonschema::paths::LazyLocation,
    ) -> std::result::Result<(), jsonschema::ValidationError<'i>> {
        if self.is_valid(instance) {
            Ok(())
        } else {
            Err(jsonschema::ValidationError::custom(
                self.location.clone(),
                location.into(),
                instance,
                format!("does not match ECMA-262 pattern {:?}", self.pattern),
            ))
        }
    }
}

fn ecma_pattern<'a>(
    _: &'a Map<String, Value>,
    value: &'a Value,
    location: jsonschema::paths::Location,
) -> std::result::Result<Box<dyn jsonschema::Keyword>, jsonschema::ValidationError<'a>> {
    let regex = value
        .as_str()
        .ok_or_else(|| "pattern must be a string".to_owned())
        .and_then(|pattern| regress::Regex::new(pattern).map_err(|error| error.to_string()))
        .map_err(|message| {
            jsonschema::ValidationError::custom(
                location.clone(),
                jsonschema::paths::Location::new(),
                value,
                message,
            )
        })?;
    Ok(Box::new(EcmaPattern {
        regex,
        pattern: value.as_str().unwrap().to_owned(),
        location,
    }))
}

fn sanitize_defaults(
    document: &mut Value,
    locations: &[String],
    diagnostics: &mut Vec<SchemaDiagnostic>,
) -> Result<()> {
    let mut defaults = Vec::new();
    for location in locations {
        collect_defaults(
            document.pointer(&location[1..]).unwrap(),
            location,
            &mut defaults,
        );
    }
    for (location, value) in defaults {
        let mut root = document.clone();
        let reference = format!(
            "#{}",
            percent_encoding::utf8_percent_encode(&location[1..], POINTER_FRAGMENT)
        );
        root.as_object_mut()
            .unwrap()
            .insert("$ref".into(), json!(reference));
        let validator = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft7)
            .should_validate_formats(false)
            .with_keyword("pattern", ecma_pattern)
            .build(&root)
            .map_err(|e| invalid(&location, format!("cannot validate schema default: {e}")))?;
        if let Err(error) = validator.validate(&value) {
            let at = child(&location, "default");
            let message = format!("skipping invalid generated default: {error}");
            eprintln!("{at}: {message}");
            diagnostics.push(SchemaDiagnostic {
                location: at,
                message,
            });
            document
                .pointer_mut(&location[1..])
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove("default");
        }
    }
    Ok(())
}

fn collect_defaults(value: &Value, location: &str, defaults: &mut Vec<(String, Value)>) {
    let Some(map) = value.as_object() else {
        return;
    };
    if let Some(value) = map.get("default") {
        defaults.push((location.into(), value.clone()));
    }
    for (key, value) in map {
        let at = child(location, key);
        match key.as_str() {
            "properties" | "patternProperties" => {
                if let Some(map) = value.as_object() {
                    for (name, value) in map {
                        collect_defaults(value, &child(&at, name), defaults);
                    }
                }
            }
            "allOf" | "anyOf" | "oneOf" => {
                if let Some(values) = value.as_array() {
                    for (i, value) in values.iter().enumerate() {
                        collect_defaults(value, &child(&at, &i.to_string()), defaults);
                    }
                }
            }
            "items" if value.is_array() => {
                for (i, value) in value.as_array().unwrap().iter().enumerate() {
                    collect_defaults(value, &child(&at, &i.to_string()), defaults);
                }
            }
            "items" | "additionalItems" | "additionalProperties" | "propertyNames" | "not" => {
                collect_defaults(value, &at, defaults)
            }
            _ => (),
        }
    }
}
