// Copyright 2026 Oxide Computer Company

//! Status-dispatched error bodies. These enums are never deserialized as an
//! untagged union: the HTTP status selects the concrete body type first.

use std::collections::{BTreeMap, BTreeSet};

use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};

use crate::method::{
    OperationMethod, OperationResponse, OperationResponseKind, OperationResponseStatus,
};
use crate::{Error, Generator, Result};

pub(crate) fn error_enum_path(method: &OperationMethod) -> TokenStream {
    let operation = format_ident!("{}", method.operation_id);
    quote! { response::#operation::Error }
}

pub(crate) fn error_variant(status: &OperationResponseStatus) -> Ident {
    match status {
        OperationResponseStatus::Code(code) => format_ident!("Status{}", code),
        OperationResponseStatus::Range(range) => format_ident!("Status{}xx", range),
        OperationResponseStatus::Default => format_ident!("Default"),
    }
}

impl Generator {
    pub(crate) fn error_response_import(&self, methods: &[OperationMethod]) -> Result<TokenStream> {
        for method in methods {
            if self.error_responses(method)?.2 {
                return Ok(quote! { use super::response; });
            }
        }
        Ok(quote! {})
    }

    pub(crate) fn error_responses<'a>(
        &self,
        method: &'a OperationMethod,
    ) -> Result<(Vec<&'a OperationResponse>, TokenStream, bool)> {
        let mut responses = method
            .responses
            .iter()
            .filter(|r| r.status_code.is_error_or_default())
            .collect::<Vec<_>>();
        // Exact statuses must precede their ranges, and default must be last.
        responses.sort_by_key(|r| match r.status_code {
            OperationResponseStatus::Code(code) => (0, code),
            OperationResponseStatus::Range(range) => (1, range),
            OperationResponseStatus::Default => (2, 0),
        });
        let kinds = responses
            .iter()
            .map(|r| r.typ.clone())
            .collect::<BTreeSet<_>>();
        let heterogeneous = kinds.len() > 1;
        let typ = if heterogeneous {
            if kinds.iter().any(|kind| {
                matches!(
                    kind,
                    OperationResponseKind::Raw | OperationResponseKind::Upgrade
                )
            }) {
                return Err(Error::UnexpectedFormat(format!(
                    "operation {} mixes raw or upgraded error responses with other body types; heterogeneous error enums support JSON and empty bodies only",
                    method.operation_id,
                )));
            }
            error_enum_path(method)
        } else {
            kinds
                .into_iter()
                .next()
                .unwrap_or(OperationResponseKind::None)
                .into_tokens(&self.type_space)
        };
        Ok((responses, typ, heterogeneous))
    }

    pub(crate) fn error_response_enums(&self, methods: &[OperationMethod]) -> Result<TokenStream> {
        let mut modules = BTreeMap::new();
        for method in methods {
            let (responses, _, heterogeneous) = self.error_responses(method)?;
            if !heterogeneous {
                continue;
            }
            let operation = format_ident!("{}", method.operation_id);
            let mut names = BTreeSet::new();
            let variants = responses
                .iter()
                .map(|response| {
                    let variant = error_variant(&response.status_code);
                    if !names.insert(variant.to_string()) {
                        return Err(Error::UnexpectedFormat(format!(
                            "duplicate error response variant {} for operation {}",
                            variant, method.operation_id,
                        )));
                    }
                    let typ = response.typ.clone().into_tokens(&self.type_space);
                    Ok(quote! { #variant(#typ) })
                })
                .collect::<Result<Vec<_>>>()?;
            let doc = format!(
                "Error responses for `{}`; selected by HTTP status before body decoding.",
                method.operation_id
            );
            let module = quote! {
                pub mod #operation {
                    #[allow(unused_imports)]
                    use super::super::types;

                    #[doc = #doc]
                    #[derive(Debug, serde::Serialize)]
                    #[serde(untagged)]
                    pub enum Error {
                        #(#variants),*
                    }
                }
            };
            if modules
                .insert(method.operation_id.clone(), module)
                .is_some()
            {
                return Err(Error::UnexpectedFormat(format!(
                    "error response module collision for operation {}",
                    method.operation_id,
                )));
            }
        }
        Ok(if modules.is_empty() {
            quote! {}
        } else {
            let modules = modules.into_values();
            quote! {
                /// Status-dispatched heterogeneous error response bodies.
                pub mod response {
                    #(#modules)*
                }
            }
        })
    }
}
