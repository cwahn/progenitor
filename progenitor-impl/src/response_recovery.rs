// Copyright 2026 Oxide Computer Company

//! Response-only recovery metadata. Never modifies a schema or model derive.

use std::collections::BTreeMap;

use proc_macro2::{Ident, TokenStream};
use quote::quote;
use schemars::schema::Schema;
use serde_json::Value;
use typify::TypeId;

use crate::{Generator, Result, UnknownResponseFields, to_schema::ToSchema};

#[derive(Default)]
pub(crate) struct RecoveryGraph {
    definitions: BTreeMap<String, Schema>,
    nodes: Vec<(TypeId, TokenStream)>,
    cache: BTreeMap<String, usize>,
    label: String,
}

impl Generator {
    pub(crate) fn initialize_response_recovery(&mut self, spec: &oas3::Spec) {
        self.response_recovery = RecoveryGraph::default();
        // Stable identifier of the generator's operation/schema input. Not a
        // runtime response hash, and not a cryptographic provenance claim.
        let bytes = serde_json::to_vec(spec).expect("serializable operation view");
        let fingerprint = bytes.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
        self.response_recovery.label = format!("fnv1a64:{fingerprint:016x}");
        if let Some(components) = &spec.components {
            self.response_recovery.definitions = components
                .schemas
                .iter()
                .map(|(name, schema)| (name.clone(), schema.to_schema()))
                .collect();
        }
    }

    /// Called only for concrete JSON response schemas, not request schemas.
    pub(crate) fn register_response_recovery(
        &mut self,
        schema: &Schema,
        typ: &TypeId,
    ) -> Result<Option<usize>> {
        if self.settings.unknown_response_fields == UnknownResponseFields::Reject {
            return Ok(None);
        }
        self.recovery_node(schema, Some(typ.clone())).map(Some)
    }

    fn recovery_node(&mut self, schema: &Schema, typ: Option<TypeId>) -> Result<usize> {
        let value = serde_json::to_value(schema).expect("serializable schema");
        let key = value.to_string();
        if let Some(index) = self.response_recovery.cache.get(&key) {
            return Ok(*index);
        }
        let index = self.response_recovery.nodes.len();
        let typ = match typ {
            Some(typ) => typ,
            None => self
                .type_space
                .add_type_with_name(schema, Some(format!("ResponseRecovery{index}")))?,
        };
        self.response_recovery.cache.insert(key, index);
        // Insert before descending: local component references can be recursive.
        self.response_recovery.nodes.push((
            typ,
            quote! { progenitor_client::ResponseRecoveryKind::Unsupported },
        ));
        let kind = self.recovery_kind(&value)?;
        self.response_recovery.nodes[index].1 = kind;
        Ok(index)
    }

    fn recovery_child(&mut self, value: &Value) -> Result<usize> {
        let schema =
            crate::document::from_normalized(value).expect("child of a normalized schemars schema");
        self.recovery_node(&schema, None)
    }

    fn recovery_kind(&mut self, value: &Value) -> Result<TokenStream> {
        let Some(map) = value.as_object() else {
            return Ok(quote! { progenitor_client::ResponseRecoveryKind::Leaf });
        };
        if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
            let Some(name) = reference.strip_prefix("#/components/schemas/") else {
                return Ok(quote! { progenitor_client::ResponseRecoveryKind::Unsupported });
            };
            let name = name.replace("~1", "/").replace("~0", "~");
            let Some(schema) = self.response_recovery.definitions.get(&name).cloned() else {
                return Ok(quote! { progenitor_client::ResponseRecoveryKind::Unsupported });
            };
            // A reference with validation siblings is a conjunction, not a ref.
            if map.keys().any(|key| !annotation(key) && key != "$ref") {
                return Ok(quote! { progenitor_client::ResponseRecoveryKind::Unsupported });
            }
            let typ = self
                .type_space
                .add_type(&Schema::new_ref(reference.into()))?;
            let target = self.recovery_node(&schema, Some(typ))?;
            return Ok(quote! { progenitor_client::ResponseRecoveryKind::Ref(#target) });
        }
        if map.contains_key("allOf") || map.contains_key("not") {
            return Ok(quote! { progenitor_client::ResponseRecoveryKind::Unsupported });
        }
        if let Some(branches) = map.get("oneOf").or_else(|| map.get("anyOf")) {
            // Compound union/object constraints need a conjunction-aware adapter.
            // Fail closed rather than deleting a field known to a sibling.
            if map
                .keys()
                .any(|key| !annotation(key) && !matches!(key.as_str(), "oneOf" | "anyOf" | "type"))
            {
                return Ok(quote! { progenitor_client::ResponseRecoveryKind::Unsupported });
            }
            let branches = branches
                .as_array()
                .expect("normalized union")
                .iter()
                .map(|branch| self.recovery_child(branch))
                .collect::<Result<Vec<_>>>()?;
            return Ok(
                quote! { progenitor_client::ResponseRecoveryKind::Union(&[#(#branches),*]) },
            );
        }
        if map.contains_key("patternProperties") || map.contains_key("dependencies") {
            return Ok(quote! { progenitor_client::ResponseRecoveryKind::Unsupported });
        }
        if map.contains_key("properties") || map.get("type") == Some(&Value::from("object")) {
            let mut properties = Vec::new();
            if let Some(props) = map.get("properties").and_then(Value::as_object) {
                for (name, schema) in props {
                    let child = self.recovery_child(schema)?;
                    properties.push(quote! { (#name, #child) });
                }
            }
            let additional = match map.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    quote! { progenitor_client::ResponseAdditionalProperties::Closed }
                }
                Some(Value::Bool(true)) | None => {
                    quote! { progenitor_client::ResponseAdditionalProperties::Open }
                }
                Some(schema) => {
                    let child = self.recovery_child(schema)?;
                    quote! { progenitor_client::ResponseAdditionalProperties::Typed(#child) }
                }
            };
            return Ok(quote! {
                progenitor_client::ResponseRecoveryKind::Object { properties: &[#(#properties),*], additional: #additional }
            });
        }
        if let Some(items) = map.get("items") {
            if items.is_array() {
                // Tuple/additional-item interactions remain strict-only.
                return Ok(quote! { progenitor_client::ResponseRecoveryKind::Unsupported });
            }
            let child = self.recovery_child(items)?;
            return Ok(quote! { progenitor_client::ResponseRecoveryKind::Array(#child) });
        }
        Ok(quote! { progenitor_client::ResponseRecoveryKind::Leaf })
    }

    /// The returned expression is already awaited and concretely typed.
    pub(crate) fn response_decode(
        &self,
        typ: &TypeId,
        recovery: Option<usize>,
        operation_id: &str,
        response: &Ident,
        client_type: &TokenStream,
    ) -> TokenStream {
        let typ = self
            .type_space
            .get_type(typ)
            .expect("response type")
            .ident();
        match recovery {
            // Heterogeneous status mapping requires the concrete receiver type
            // even in Reject mode. Homogeneous legacy sites retain their
            // inferred decoder expression in method generation.
            None => quote! { ResponseValue::<#typ>::from_response(#response).await },
            Some(root) => {
                let label = &self.response_recovery.label;
                // Preserve the caller's qualified client path. An import could
                // collide with an operation builder also named Client.
                quote! {
                    ResponseValue::<#typ>::from_response_with_recovery(
                        #response, #client_type::__response_recovery_nodes(), #root,
                        #operation_id, #label,
                    ).await
                }
            }
        }
    }

    pub(crate) fn response_recovery_tokens(&self) -> Result<TokenStream> {
        if self.response_recovery.nodes.is_empty() {
            return Ok(TokenStream::new());
        }
        let nodes = self
            .response_recovery
            .nodes
            .iter()
            .map(|(typ, kind)| {
                let typ = self.type_space.get_type(typ)?.ident();
                Ok(quote! {
                    progenitor_client::ResponseRecoveryNode {
                        accepts: progenitor_client::response_matches::<#typ>,
                        kind: #kind,
                    }
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let node_count = nodes.len();
        Ok(quote! {
            static __RESPONSE_RECOVERY_NODES: [progenitor_client::ResponseRecoveryNode; #node_count] = [#(#nodes),*];

            impl Client {
                const fn __response_recovery_nodes() -> &'static [progenitor_client::ResponseRecoveryNode] {
                    &__RESPONSE_RECOVERY_NODES
                }
            }
        })
    }
}

fn annotation(key: &str) -> bool {
    key.starts_with("x-")
        || matches!(
            key,
            "title"
                | "description"
                | "default"
                | "examples"
                | "deprecated"
                | "readOnly"
                | "writeOnly"
                | "$schema"
        )
}
