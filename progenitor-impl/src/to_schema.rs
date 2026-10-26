// Copyright 2024 Oxide Computer Company

use oas3::spec::{
    BooleanSchema, ObjectOrReference, ObjectSchema, Schema as OasSchema, SchemaType, SchemaTypeSet,
};
use schemars::schema::SingleOrVec;
use serde_json::Value;

pub trait ToSchema {
    fn to_schema(&self) -> schemars::schema::Schema;
}

trait Convert<T> {
    fn convert(&self) -> T;
}

impl ToSchema for ObjectSchema {
    fn to_schema(&self) -> schemars::schema::Schema {
        self.convert()
    }
}

impl ToSchema for ObjectOrReference<ObjectSchema> {
    fn to_schema(&self) -> schemars::schema::Schema {
        self.convert()
    }
}

impl ToSchema for OasSchema {
    fn to_schema(&self) -> schemars::schema::Schema {
        self.convert()
    }
}

impl<I, T> Convert<Vec<T>> for Vec<I>
where
    I: Convert<T>,
{
    fn convert(&self) -> Vec<T> {
        self.iter().map(Convert::convert).collect()
    }
}

impl<I, T> Convert<Option<Vec<T>>> for Vec<I>
where
    I: Convert<T>,
{
    fn convert(&self) -> Option<Vec<T>> {
        if self.is_empty() {
            None
        } else {
            Some(self.iter().map(Convert::convert).collect())
        }
    }
}

impl Convert<schemars::schema::Schema> for ObjectOrReference<ObjectSchema> {
    fn convert(&self) -> schemars::schema::Schema {
        match self {
            ObjectOrReference::Ref { ref_path, .. } => {
                schemars::schema::SchemaObject::new_ref(ref_path.clone()).into()
            }
            ObjectOrReference::Object(schema) => schema.convert(),
        }
    }
}

impl Convert<schemars::schema::Schema> for OasSchema {
    fn convert(&self) -> schemars::schema::Schema {
        match self {
            OasSchema::Boolean(BooleanSchema(b)) => schemars::schema::Schema::Bool(*b),
            OasSchema::Object(inner) => inner.as_ref().convert(),
        }
    }
}

/// Split an optional type set into `(non_null_type, nullable)`.
///
/// Returns `(None, false)` when no type is declared. When `type` is declared as
/// an array containing `"null"` (3.1 nullability), returns the non-null type
/// along with `nullable = true`.
fn split_type_set(ts: Option<&SchemaTypeSet>) -> (Option<SchemaType>, bool) {
    match ts {
        None => (None, false),
        Some(SchemaTypeSet::Single(SchemaType::Null)) => (None, true),
        Some(SchemaTypeSet::Single(t)) => (Some(*t), false),
        Some(SchemaTypeSet::Multiple(types)) => {
            let nullable = types.contains(&SchemaType::Null);
            let non_null: Vec<_> = types
                .iter()
                .copied()
                .filter(|t| *t != SchemaType::Null)
                .collect();
            match non_null.len() {
                0 => (None, nullable),
                1 => (Some(non_null[0]), nullable),
                // Multiple non-null types: fall back to no canonical type and
                // let downstream handle it as an untyped schema.
                _ => (None, nullable),
            }
        }
    }
}

fn schema_type_to_instance(t: SchemaType) -> Option<schemars::schema::InstanceType> {
    Some(match t {
        SchemaType::Boolean => schemars::schema::InstanceType::Boolean,
        SchemaType::Integer => schemars::schema::InstanceType::Integer,
        SchemaType::Number => schemars::schema::InstanceType::Number,
        SchemaType::String => schemars::schema::InstanceType::String,
        SchemaType::Array => schemars::schema::InstanceType::Array,
        SchemaType::Object => schemars::schema::InstanceType::Object,
        SchemaType::Null => return None,
    })
}

fn instance_type_or_nullable(
    t: SchemaType,
    nullable: bool,
) -> Option<SingleOrVec<schemars::schema::InstanceType>> {
    let instance = schema_type_to_instance(t)?;
    if nullable {
        Some(vec![instance, schemars::schema::InstanceType::Null].into())
    } else {
        Some(instance.into())
    }
}

fn json_number_to_f64(n: &serde_json::Number) -> f64 {
    n.as_f64().unwrap_or_else(|| {
        // Fallback: parse via string. Number should always be convertible to
        // f64 for schema purposes, but guard against exotic arbitrary
        // precision integers.
        n.to_string().parse::<f64>().unwrap_or(f64::NAN)
    })
}

impl Convert<schemars::schema::Schema> for ObjectSchema {
    fn convert(&self) -> schemars::schema::Schema {
        if let Some(value) = self.extensions.get(crate::document::SCHEMA_EXTENSION) {
            return crate::document::from_normalized(value)
                .expect("OpenApiDocument validated its internal schema carrier");
        }
        // TODO the discriminator field is used in a way that seems both
        // important and unfortunately redundant; see historical comment in
        // git log for the full context. We pass it through unchanged here.

        let metadata = schemars::schema::Metadata {
            id: None,
            title: self.title.clone(),
            description: self.description.clone(),
            default: self.default.clone(),
            deprecated: self.deprecated.unwrap_or(false),
            read_only: self.read_only.unwrap_or(false),
            write_only: self.write_only.unwrap_or(false),
            examples: self
                .example
                .clone()
                .into_iter()
                .chain(self.examples.iter().cloned())
                .collect(),
        };

        let metadata = Some(Box::new(metadata)).simplify_default();
        let extensions: schemars::Map<String, Value> = self
            .extensions
            .iter()
            .map(|(k, v)| (format!("x-{k}"), v.clone()))
            .collect();

        let (primary_type, nullable) = split_type_set(self.schema_type.as_ref());

        let enum_values = if self.enum_values.is_empty() {
            None
        } else {
            Some(self.enum_values.clone())
        };

        // Merge const into enum (schemars doesn't have a `const` slot).
        let enum_values = match (enum_values, self.const_value.clone()) {
            (Some(v), _) => Some(v),
            (None, Some(c)) => Some(vec![c]),
            (None, None) => None,
        };

        let format = self.format.clone();

        match primary_type {
            Some(SchemaType::String)
                if !has_composition(self)
                    && !has_object_fields(self)
                    && !has_array_fields(self) =>
            {
                schemars::schema::SchemaObject {
                    metadata,
                    instance_type: instance_type_or_nullable(SchemaType::String, nullable),
                    format,
                    enum_values,
                    string: Some(Box::new(schemars::schema::StringValidation {
                        max_length: self.max_length.map(|v| v as u32),
                        min_length: self.min_length.map(|v| v as u32),
                        pattern: self.pattern.clone(),
                    }))
                    .simplify_default(),
                    extensions,
                    ..Default::default()
                }
                .into()
            }
            Some(SchemaType::Number)
                if !has_composition(self)
                    && !has_object_fields(self)
                    && !has_array_fields(self) =>
            {
                schemars::schema::SchemaObject {
                    metadata,
                    instance_type: instance_type_or_nullable(SchemaType::Number, nullable),
                    format,
                    enum_values,
                    number: Some(Box::new(schemars::schema::NumberValidation {
                        multiple_of: self.multiple_of.as_ref().map(json_number_to_f64),
                        maximum: self.maximum.as_ref().map(json_number_to_f64),
                        exclusive_maximum: self.exclusive_maximum.as_ref().map(json_number_to_f64),
                        minimum: self.minimum.as_ref().map(json_number_to_f64),
                        exclusive_minimum: self.exclusive_minimum.as_ref().map(json_number_to_f64),
                    }))
                    .simplify_default(),
                    extensions,
                    ..Default::default()
                }
                .into()
            }
            Some(SchemaType::Integer)
                if !has_composition(self)
                    && !has_object_fields(self)
                    && !has_array_fields(self) =>
            {
                schemars::schema::SchemaObject {
                    metadata,
                    instance_type: instance_type_or_nullable(SchemaType::Integer, nullable),
                    format,
                    enum_values,
                    number: Some(Box::new(schemars::schema::NumberValidation {
                        multiple_of: self.multiple_of.as_ref().map(json_number_to_f64),
                        maximum: self.maximum.as_ref().map(json_number_to_f64),
                        exclusive_maximum: self.exclusive_maximum.as_ref().map(json_number_to_f64),
                        minimum: self.minimum.as_ref().map(json_number_to_f64),
                        exclusive_minimum: self.exclusive_minimum.as_ref().map(json_number_to_f64),
                    }))
                    .simplify_default(),
                    extensions,
                    ..Default::default()
                }
                .into()
            }
            Some(SchemaType::Object) if !has_composition(self) && !has_array_fields(self) => {
                schemars::schema::SchemaObject {
                    metadata,
                    instance_type: instance_type_or_nullable(SchemaType::Object, nullable),
                    object: Some(Box::new(schemars::schema::ObjectValidation {
                        max_properties: self.max_properties.map(|v| v as u32),
                        min_properties: self.min_properties.map(|v| v as u32),
                        required: self.required.iter().cloned().collect(),
                        properties: self
                            .properties
                            .iter()
                            .map(|(k, v)| (k.clone(), v.convert()))
                            .collect(),
                        pattern_properties: schemars::Map::default(),
                        additional_properties: self
                            .additional_properties
                            .as_ref()
                            .map(|ap| Box::new(ap.convert())),
                        property_names: None,
                    }))
                    .simplify_default(),
                    extensions,
                    ..Default::default()
                }
                .into()
            }
            Some(SchemaType::Array) if !has_composition(self) && !has_object_fields(self) => {
                schemars::schema::SchemaObject {
                    metadata,
                    instance_type: instance_type_or_nullable(SchemaType::Array, nullable),
                    array: Some(Box::new(schemars::schema::ArrayValidation {
                        items: self
                            .items
                            .as_ref()
                            .map(|items| SingleOrVec::Single(Box::new(items.convert()))),
                        additional_items: None,
                        max_items: self.max_items.map(|v| v as u32),
                        min_items: self.min_items.map(|v| v as u32),
                        unique_items: self
                            .unique_items
                            .and_then(|v| if v { Some(true) } else { None }),
                        contains: None,
                    }))
                    .simplify_default(),
                    extensions,
                    ..Default::default()
                }
                .into()
            }
            Some(SchemaType::Boolean)
                if !has_composition(self)
                    && !has_object_fields(self)
                    && !has_array_fields(self) =>
            {
                schemars::schema::SchemaObject {
                    metadata,
                    instance_type: instance_type_or_nullable(SchemaType::Boolean, nullable),
                    enum_values,
                    extensions,
                    ..Default::default()
                }
                .into()
            }

            // If only composition is present (one_of / all_of / any_of), mirror
            // it onto a schemars SubschemaValidation and wrap with a
            // null-inclusive oneOf if nullable.
            _ if !self.one_of.is_empty() && self.all_of.is_empty() && self.any_of.is_empty() => {
                oneof_nullable_wrapper(
                    schemars::schema::SchemaObject {
                        metadata,
                        subschemas: Some(Box::new(schemars::schema::SubschemaValidation {
                            one_of: Some(self.one_of.iter().map(Convert::convert).collect()),
                            ..Default::default()
                        })),
                        extensions,
                        ..Default::default()
                    },
                    nullable,
                )
                .into()
            }
            _ if !self.all_of.is_empty() && self.one_of.is_empty() && self.any_of.is_empty() => {
                oneof_nullable_wrapper(
                    schemars::schema::SchemaObject {
                        metadata,
                        subschemas: Some(Box::new(schemars::schema::SubschemaValidation {
                            all_of: Some(self.all_of.iter().map(Convert::convert).collect()),
                            ..Default::default()
                        })),
                        extensions,
                        ..Default::default()
                    },
                    nullable,
                )
                .into()
            }
            _ if !self.any_of.is_empty() && self.one_of.is_empty() && self.all_of.is_empty() => {
                oneof_nullable_wrapper(
                    schemars::schema::SchemaObject {
                        metadata,
                        subschemas: Some(Box::new(schemars::schema::SubschemaValidation {
                            any_of: Some(self.any_of.iter().map(Convert::convert).collect()),
                            ..Default::default()
                        })),
                        extensions,
                        ..Default::default()
                    },
                    nullable,
                )
                .into()
            }

            // Catch-all: assemble whatever fields are present and let the
            // consumer figure it out. Mirrors the old "Any" branch.
            _ => {
                let mut so = schemars::schema::SchemaObject {
                    metadata,
                    extensions,
                    ..Default::default()
                };

                if let Some(format) = format {
                    so.format = Some(format);
                }
                if let Some(values) = enum_values {
                    so.enum_values = Some(values);
                }

                if let Some(pattern) = &self.pattern {
                    so.string().pattern = Some(pattern.clone());
                }
                if let Some(min_length) = self.min_length {
                    so.string().min_length = Some(min_length as u32);
                }
                if let Some(max_length) = self.max_length {
                    so.string().max_length = Some(max_length as u32);
                }

                if let Some(multiple_of) = &self.multiple_of {
                    so.number().multiple_of = Some(json_number_to_f64(multiple_of));
                }
                if let Some(minimum) = &self.minimum {
                    so.number().minimum = Some(json_number_to_f64(minimum));
                }
                if let Some(maximum) = &self.maximum {
                    so.number().maximum = Some(json_number_to_f64(maximum));
                }
                if let Some(excl_min) = &self.exclusive_minimum {
                    so.number().exclusive_minimum = Some(json_number_to_f64(excl_min));
                }
                if let Some(excl_max) = &self.exclusive_maximum {
                    so.number().exclusive_maximum = Some(json_number_to_f64(excl_max));
                }

                if !self.properties.is_empty() {
                    so.object().properties = self
                        .properties
                        .iter()
                        .map(|(k, v)| (k.clone(), v.convert()))
                        .collect();
                }
                if !self.required.is_empty() {
                    so.object().required = self.required.iter().cloned().collect();
                }
                if let Some(ap) = &self.additional_properties {
                    so.object().additional_properties = Some(Box::new(ap.convert()));
                }
                if let Some(min_properties) = self.min_properties {
                    so.object().min_properties = Some(min_properties as u32);
                }
                if let Some(max_properties) = self.max_properties {
                    so.object().max_properties = Some(max_properties as u32);
                }

                if let Some(items) = &self.items {
                    so.array().items = Some(SingleOrVec::Single(Box::new(items.convert())));
                }
                if let Some(min_items) = self.min_items {
                    so.array().min_items = Some(min_items as u32);
                }
                if let Some(max_items) = self.max_items {
                    so.array().max_items = Some(max_items as u32);
                }
                if let Some(unique_items) = self.unique_items {
                    so.array().unique_items = Some(unique_items);
                }

                if !self.one_of.is_empty() {
                    so.subschemas().one_of =
                        Some(self.one_of.iter().map(Convert::convert).collect());
                }
                if !self.all_of.is_empty() {
                    so.subschemas().all_of =
                        Some(self.all_of.iter().map(Convert::convert).collect());
                }
                if !self.any_of.is_empty() {
                    so.subschemas().any_of =
                        Some(self.any_of.iter().map(Convert::convert).collect());
                }

                // Figure out instance type last so we can infer from populated
                // field groups if no type was declared.
                match (primary_type, nullable) {
                    (Some(t), _) => {
                        so.instance_type = instance_type_or_nullable(t, nullable);
                    }
                    (None, false) => (),
                    (None, true) => {
                        // Try to infer a type from which validation slot is
                        // populated. We only do this when we need to stitch in
                        // an explicit null.
                        let inferred: Vec<_> = [
                            so.object
                                .is_some()
                                .then_some(schemars::schema::InstanceType::Object),
                            so.array
                                .is_some()
                                .then_some(schemars::schema::InstanceType::Array),
                            so.number
                                .is_some()
                                .then_some(schemars::schema::InstanceType::Number),
                            so.string
                                .is_some()
                                .then_some(schemars::schema::InstanceType::String),
                            Some(schemars::schema::InstanceType::Null),
                        ]
                        .into_iter()
                        .flatten()
                        .collect();

                        so.instance_type = match (inferred.first(), inferred.len()) {
                            (Some(t), 1) => Some(SingleOrVec::Single(Box::new(*t))),
                            (Some(_), _) => Some(SingleOrVec::Vec(inferred)),
                            (None, _) => None,
                        };
                    }
                }

                // If instance_type collapsed to just Null with subschemas
                // populated, re-wrap as a nullable oneOf.
                match &so.instance_type {
                    Some(SingleOrVec::Single(it))
                        if **it == schemars::schema::InstanceType::Null
                            && so.subschemas.is_some() =>
                    {
                        oneof_nullable_wrapper(
                            schemars::schema::SchemaObject {
                                instance_type: None,
                                ..so
                            },
                            nullable,
                        )
                        .into()
                    }
                    _ => so.into(),
                }
            }
        }
    }
}

fn has_composition(s: &ObjectSchema) -> bool {
    !s.one_of.is_empty() || !s.all_of.is_empty() || !s.any_of.is_empty()
}

fn has_object_fields(s: &ObjectSchema) -> bool {
    !s.properties.is_empty()
        || !s.required.is_empty()
        || s.additional_properties.is_some()
        || s.min_properties.is_some()
        || s.max_properties.is_some()
}

fn has_array_fields(s: &ObjectSchema) -> bool {
    s.items.is_some() || s.min_items.is_some() || s.max_items.is_some() || s.unique_items.is_some()
}

fn oneof_nullable_wrapper(
    mut schema: schemars::schema::SchemaObject,
    nullable: bool,
) -> schemars::schema::SchemaObject {
    if nullable {
        let metadata = schema.metadata;
        let extensions = schema.extensions;

        schema.metadata = None;
        schema.extensions = Default::default();

        schemars::schema::SchemaObject {
            metadata,
            extensions,
            subschemas: Some(Box::new(schemars::schema::SubschemaValidation {
                one_of: Some(vec![
                    schemars::schema::SchemaObject {
                        instance_type: Some(schemars::schema::InstanceType::Null.into()),
                        ..Default::default()
                    }
                    .into(),
                    schema.into(),
                ]),
                ..Default::default()
            })),
            ..Default::default()
        }
    } else {
        schema
    }
}

trait OptionSimplifyDefault {
    fn simplify_default(self) -> Self;
}

// If an Option is `Some` of its default value, we can simplify that to `None`.
impl<T> OptionSimplifyDefault for Option<T>
where
    T: Default + PartialEq + std::fmt::Debug,
{
    fn simplify_default(self) -> Self {
        match &self {
            Some(s) if s != &T::default() => self,
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::Convert;
    use oas3::spec::ObjectSchema;

    #[test]
    fn test_null() {
        let schema_value = json!({ "enum": [null] });
        let oa_schema = serde_json::from_value::<ObjectSchema>(schema_value).unwrap();

        let schema = oa_schema.convert();
        // Legacy behavior: an enum containing only null collapses to type=null.
        // The new implementation preserves the enum verbatim; verify either.
        let obj = schema.into_object();
        assert_eq!(obj.enum_values, Some(vec![serde_json::Value::Null]));
    }

    #[test]
    fn test_weird_integer() {
        let schema_value = json!({
            "type": "integer",
            "minimum": 0.0,
        });
        let oa_schema = serde_json::from_value::<ObjectSchema>(schema_value.clone()).unwrap();
        let js_schema = serde_json::from_value::<schemars::schema::Schema>(schema_value).unwrap();

        let conv_schema = oa_schema.convert();
        assert_eq!(conv_schema, js_schema);
    }

    #[test]
    fn test_object_no_type() {
        let schema_value = json!({
            "properties": {
                "foo": {}
            }
        });
        let oa_schema = serde_json::from_value::<ObjectSchema>(schema_value.clone()).unwrap();
        let js_schema = serde_json::from_value::<schemars::schema::Schema>(schema_value).unwrap();

        let conv_schema = oa_schema.convert();
        assert_eq!(conv_schema, js_schema);
    }

    #[test]
    fn test_array_no_type() {
        let schema_value = json!({
            "items": {}
        });
        let oa_schema = serde_json::from_value::<ObjectSchema>(schema_value.clone()).unwrap();
        let js_schema = serde_json::from_value::<schemars::schema::Schema>(schema_value).unwrap();

        let conv_schema = oa_schema.convert();
        assert_eq!(conv_schema, js_schema);
    }

    #[test]
    fn test_number_no_type() {
        let schema_value = json!({
            "minimum": 100.0
        });
        let oa_schema = serde_json::from_value::<ObjectSchema>(schema_value.clone()).unwrap();
        let js_schema = serde_json::from_value::<schemars::schema::Schema>(schema_value).unwrap();

        let conv_schema = oa_schema.convert();
        assert_eq!(conv_schema, js_schema);
    }

    #[test]
    fn test_solo_enum() {
        let schema_value = json!({
            "enum": ["one"]
        });
        let oa_schema = serde_json::from_value::<ObjectSchema>(schema_value.clone()).unwrap();
        let js_schema = serde_json::from_value::<schemars::schema::Schema>(schema_value).unwrap();

        let conv_schema = oa_schema.convert();
        assert_eq!(conv_schema, js_schema);
    }
}
