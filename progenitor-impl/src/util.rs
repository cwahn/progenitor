// Copyright 2022 Oxide Computer Company

use std::collections::BTreeMap;

use oas3::{
    Spec,
    spec::{FromRef, ObjectOrReference, Parameter},
};
use unicode_ident::{is_xid_continue, is_xid_start};

use crate::{Error, Result};

pub(crate) trait ReferenceOrExt<T> {
    fn item(&self, spec: &Spec) -> Result<T>;
}

impl<T: FromRef> ReferenceOrExt<T> for ObjectOrReference<T> {
    fn item(&self, spec: &Spec) -> Result<T> {
        self.resolve(spec)
            .map_err(|e| Error::UnexpectedFormat(format!("unresolved reference: {e}")))
    }
}

pub(crate) fn items<'a, T: FromRef + 'a>(
    refs: &'a [ObjectOrReference<T>],
    spec: &'a Spec,
) -> impl Iterator<Item = Result<T>> + 'a {
    refs.iter().map(|r| r.item(spec))
}

pub(crate) fn parameter_map(
    refs: &[ObjectOrReference<Parameter>],
    spec: &Spec,
) -> Result<BTreeMap<String, Parameter>> {
    items(refs, spec)
        .map(|res| res.map(|param| (param.name.clone(), param)))
        .collect()
}

pub(crate) enum Case {
    Pascal,
    Snake,
}

pub(crate) fn sanitize(input: &str, case: Case) -> String {
    use heck::{ToPascalCase, ToSnakeCase};
    let to_case = match case {
        Case::Pascal => str::to_pascal_case,
        Case::Snake => str::to_snake_case,
    };
    // If every case was special then none of them would be.
    let out = match input {
        "+1" => "plus1".to_string(),
        "-1" => "minus1".to_string(),
        _ => to_case(
            &input
                .replace('\'', "")
                .replace(|c: char| !is_xid_continue(c), "-"),
        ),
    };

    let out = match out.chars().next() {
        None => to_case("x"),
        Some(c) if is_xid_start(c) => out,
        Some(_) => format!("_{}", out),
    };

    // Make sure the string is a valid Rust identifier.
    if typify::accept_as_ident(&out) {
        out
    } else {
        format!("{}_", out)
    }
}

/// Given a desired name and a slice of proc_macro2::Ident, generate a new
/// Ident that is unique from the slice.
pub(crate) fn unique_ident_from(
    name: &str,
    identities: &[proc_macro2::Ident],
) -> proc_macro2::Ident {
    let mut name = name.to_string();

    loop {
        let ident = quote::format_ident!("{}", name);

        if !identities.contains(&ident) {
            return ident;
        }

        name.insert_str(0, "_");
    }
}
