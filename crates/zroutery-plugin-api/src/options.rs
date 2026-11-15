//! The host-authoritative options contract: declarative rules, bounded
//! decoding, no execution. The plugin's `validate_options` hook can only add
//! errors to what this module accepts; it never relaxes it. No regex, no
//! `$ref`, no conditionals — richer semantics belong to the hook, which is
//! the only extension point. Deterministic: same input, same error list.

use crate::ipc::FieldError;
use crate::strict;
use crate::{PluginError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Same ceiling as the persisted options document (desktop write path).
pub const MAX_SCHEMA_BYTES: usize = 64 * 1024;
pub const MAX_FIELDS: usize = 64;
pub const MAX_PROPERTIES: usize = 32;
pub const MAX_ITEMS: usize = 256;
pub const DEFAULT_MAX_LENGTH: usize = 4096;
pub const MAX_LENGTH_CAP: usize = strict::MAX_STRING_BYTES;
/// Option values round-trip through `CallContext`; beyond 2^53 the JS side
/// would silently lose precision, so the default range stops here.
pub const MAX_NUMBER: i128 = 9_007_199_254_740_992;
const MAX_ENUM_VALUES: usize = 32;
const MAX_ENUM_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionsSchemaV1 {
    pub schema_version: u32,
    pub fields: BTreeMap<String, FieldRule>,
}

impl OptionsSchemaV1 {
    /// Strict decode followed by structural self-validation. A schema that
    /// cannot pass this is an invalid package, not a runtime surprise.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let schema: Self =
            strict::decode(bytes, MAX_SCHEMA_BYTES).map_err(|_| PluginError::InvalidManifest)?;
        schema.validate_structure()?;
        Ok(schema)
    }

    pub fn validate_structure(&self) -> Result<()> {
        if self.schema_version == 0
            || self.fields.is_empty()
            || self.fields.len() > MAX_FIELDS
            || self.fields.keys().any(|id| !strict::local_id(id))
        {
            return Err(PluginError::InvalidManifest);
        }
        for rule in self.fields.values() {
            rule.valid_structure()?;
        }
        Ok(())
    }

    /// Absent non-required fields are legal; explicit `null` never is.
    /// Unknown keys are rejected even when every declared field is optional.
    pub fn validate_value(&self, options: &BTreeMap<String, Value>) -> Vec<FieldError> {
        let mut errors = Vec::new();
        for (id, rule) in &self.fields {
            match options.get(id) {
                None if rule.required() => errors.push(FieldError {
                    field: id.clone(),
                    message_code: "missing_field".into(),
                }),
                Some(value) => rule.validate(id, value, &mut errors),
                None => {}
            }
        }
        for id in options.keys() {
            if !self.fields.contains_key(id) {
                errors.push(FieldError {
                    field: id.clone(),
                    message_code: "unknown_field".into(),
                });
            }
        }
        errors
    }
}

fn reject(errors: &mut Vec<FieldError>, path: &str, code: &str) {
    errors.push(FieldError {
        field: path.to_string(),
        message_code: code.to_string(),
    });
}

/// Lengths and enum values are measured in bytes, matching `strict::text`.
trait ValidateValue {
    fn required(&self) -> bool;
    fn validate(&self, path: &str, value: &Value, errors: &mut Vec<FieldError>);
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StringRule {
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub min_length: usize,
    #[serde(default)]
    pub max_length: Option<usize>,
    #[serde(default, rename = "enum")]
    pub enum_values: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegerRule {
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub min: Option<serde_json::Number>,
    #[serde(default)]
    pub max: Option<serde_json::Number>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NumberRule {
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub min: Option<serde_json::Number>,
    #[serde(default)]
    pub max: Option<serde_json::Number>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BooleanRule {
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArrayRule {
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub min_items: usize,
    #[serde(default)]
    pub max_items: Option<usize>,
    pub items: Box<FieldRule>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectRule {
    #[serde(default)]
    pub required: bool,
    pub properties: BTreeMap<String, ScalarRule>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FieldRule {
    String(StringRule),
    Integer(IntegerRule),
    Number(NumberRule),
    Boolean(BooleanRule),
    Array(ArrayRule),
    Object(ObjectRule),
}

/// Scalars may appear at any depth a container rule allows. `ObjectRule`
/// properties are scalars **by type**, so an object can never nest an object
/// or an array; an array can only nest an object of scalars (depth 2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScalarRule {
    String(StringRule),
    Integer(IntegerRule),
    Number(NumberRule),
    Boolean(BooleanRule),
}

impl ValidateValue for StringRule {
    fn required(&self) -> bool {
        self.required
    }
    fn validate(&self, path: &str, value: &Value, errors: &mut Vec<FieldError>) {
        let Value::String(text) = value else {
            return reject(errors, path, "type_mismatch");
        };
        if text.len() < self.min_length {
            reject(errors, path, "too_short");
        } else if text.len() > self.max_length.unwrap_or(DEFAULT_MAX_LENGTH) {
            reject(errors, path, "too_long");
        } else if self
            .enum_values
            .as_ref()
            .is_some_and(|values| !values.iter().any(|v| v == text))
        {
            reject(errors, path, "invalid_enum");
        }
    }
}

impl ValidateValue for IntegerRule {
    fn required(&self) -> bool {
        self.required
    }
    fn validate(&self, path: &str, value: &Value, errors: &mut Vec<FieldError>) {
        let Value::Number(number) = value else {
            return reject(errors, path, "type_mismatch");
        };
        // `1.0` is a float representation, not an integer value.
        let Some(integral) = integral_number(number) else {
            return reject(errors, path, "type_mismatch");
        };
        if integral.abs() > MAX_NUMBER {
            return reject(errors, path, "out_of_range");
        }
        let below = self
            .min
            .as_ref()
            .and_then(integral_number)
            .is_some_and(|bound| integral < bound);
        let above = self
            .max
            .as_ref()
            .and_then(integral_number)
            .is_some_and(|bound| integral > bound);
        if below || above {
            reject(errors, path, "out_of_range");
        }
    }
}

impl ValidateValue for NumberRule {
    fn required(&self) -> bool {
        self.required
    }
    fn validate(&self, path: &str, value: &Value, errors: &mut Vec<FieldError>) {
        let Value::Number(number) = value else {
            return reject(errors, path, "type_mismatch");
        };
        let Some(real) = number.as_f64() else {
            return reject(errors, path, "type_mismatch");
        };
        if real.abs() > MAX_NUMBER as f64 {
            return reject(errors, path, "out_of_range");
        }
        let below = self
            .min
            .as_ref()
            .and_then(|bound| bound.as_f64())
            .is_some_and(|bound| real < bound);
        let above = self
            .max
            .as_ref()
            .and_then(|bound| bound.as_f64())
            .is_some_and(|bound| real > bound);
        if below || above {
            reject(errors, path, "out_of_range");
        }
    }
}

impl ValidateValue for BooleanRule {
    fn required(&self) -> bool {
        self.required
    }
    fn validate(&self, path: &str, value: &Value, errors: &mut Vec<FieldError>) {
        if !value.is_boolean() {
            reject(errors, path, "type_mismatch");
        }
    }
}

impl ValidateValue for ArrayRule {
    fn required(&self) -> bool {
        self.required
    }
    fn validate(&self, path: &str, value: &Value, errors: &mut Vec<FieldError>) {
        let Value::Array(items) = value else {
            return reject(errors, path, "type_mismatch");
        };
        if items.len() < self.min_items {
            return reject(errors, path, "too_few_items");
        }
        if items.len() > self.max_items.unwrap_or(MAX_ITEMS) {
            return reject(errors, path, "too_many_items");
        }
        for (index, item) in items.iter().enumerate() {
            self.items
                .validate(&format!("{path}.{index}"), item, errors);
        }
    }
}

impl ValidateValue for ObjectRule {
    fn required(&self) -> bool {
        self.required
    }
    fn validate(&self, path: &str, value: &Value, errors: &mut Vec<FieldError>) {
        let Value::Object(map) = value else {
            return reject(errors, path, "type_mismatch");
        };
        for (key, property) in &self.properties {
            match map.get(key) {
                None if property.required() => {
                    reject(errors, &format!("{path}.{key}"), "missing_field");
                }
                Some(inner) => property.validate(&format!("{path}.{key}"), inner, errors),
                None => {}
            }
        }
        for key in map.keys() {
            if !self.properties.contains_key(key) {
                reject(errors, &format!("{path}.{key}"), "unknown_field");
            }
        }
    }
}

impl ValidateValue for ScalarRule {
    fn required(&self) -> bool {
        match self {
            Self::String(rule) => rule.required(),
            Self::Integer(rule) => rule.required(),
            Self::Number(rule) => rule.required(),
            Self::Boolean(rule) => rule.required(),
        }
    }
    fn validate(&self, path: &str, value: &Value, errors: &mut Vec<FieldError>) {
        match self {
            Self::String(rule) => rule.validate(path, value, errors),
            Self::Integer(rule) => rule.validate(path, value, errors),
            Self::Number(rule) => rule.validate(path, value, errors),
            Self::Boolean(rule) => rule.validate(path, value, errors),
        }
    }
}

impl ValidateValue for FieldRule {
    fn required(&self) -> bool {
        match self {
            Self::String(rule) => rule.required(),
            Self::Integer(rule) => rule.required(),
            Self::Number(rule) => rule.required(),
            Self::Boolean(rule) => rule.required(),
            Self::Array(rule) => rule.required(),
            Self::Object(rule) => rule.required(),
        }
    }
    fn validate(&self, path: &str, value: &Value, errors: &mut Vec<FieldError>) {
        match self {
            Self::String(rule) => rule.validate(path, value, errors),
            Self::Integer(rule) => rule.validate(path, value, errors),
            Self::Number(rule) => rule.validate(path, value, errors),
            Self::Boolean(rule) => rule.validate(path, value, errors),
            Self::Array(rule) => rule.validate(path, value, errors),
            Self::Object(rule) => rule.validate(path, value, errors),
        }
    }
}

impl FieldRule {
    /// Structural self-validation, applied when the package is built. Depth
    /// is frozen at 2: an object's properties are scalars by type, and an
    /// array's items may be scalars or objects of scalars — never an array.
    fn valid_structure(&self) -> Result<()> {
        match self {
            Self::String(rule) => {
                let max = rule.max_length.unwrap_or(DEFAULT_MAX_LENGTH);
                let ordered = rule.min_length <= max;
                let capped = max <= MAX_LENGTH_CAP;
                let enumerated = rule.enum_values.as_ref().is_none_or(|values| {
                    values.len() <= MAX_ENUM_VALUES
                        && values
                            .iter()
                            .all(|v| !v.is_empty() && v.len() <= MAX_ENUM_BYTES)
                        && values
                            .iter()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            == values.len()
                });
                if ordered && capped && enumerated {
                    Ok(())
                } else {
                    Err(PluginError::InvalidManifest)
                }
            }
            Self::Integer(rule) => {
                let ordered = ordered_bounds(rule.min.as_ref(), rule.max.as_ref(), &|number| {
                    integral_number(number)
                        .map(|value| value <= MAX_NUMBER)
                        .unwrap_or(false)
                });
                if ordered {
                    Ok(())
                } else {
                    Err(PluginError::InvalidManifest)
                }
            }
            Self::Number(rule) => {
                let ordered = ordered_bounds(rule.min.as_ref(), rule.max.as_ref(), &|number| {
                    number
                        .as_f64()
                        .is_some_and(|value| value.abs() <= MAX_NUMBER as f64)
                });
                if ordered {
                    Ok(())
                } else {
                    Err(PluginError::InvalidManifest)
                }
            }
            Self::Boolean(_) => Ok(()),
            Self::Array(rule) => {
                let capped = rule.max_items.unwrap_or(MAX_ITEMS) <= MAX_ITEMS
                    && rule.min_items <= rule.max_items.unwrap_or(MAX_ITEMS);
                if capped && !matches!(*rule.items, FieldRule::Array(_)) {
                    Ok(())
                } else {
                    Err(PluginError::InvalidManifest)
                }
            }
            Self::Object(rule) => {
                let sized = !rule.properties.is_empty()
                    && rule.properties.len() <= MAX_PROPERTIES
                    && rule.properties.keys().all(|key| strict::local_id(key));
                if sized {
                    Ok(())
                } else {
                    Err(PluginError::InvalidManifest)
                }
            }
        }
    }
}

/// A bound is usable only when it exists, sits inside the safe range and,
/// for integer rules, is an integral representation. `min <= max` holds.
fn ordered_bounds(
    min: Option<&serde_json::Number>,
    max: Option<&serde_json::Number>,
    usable: &dyn Fn(&serde_json::Number) -> bool,
) -> bool {
    match (min, max) {
        (None, None) => true,
        (min, max) => {
            let ordered = match (
                min.and_then(|number| number.as_f64()),
                max.and_then(|number| number.as_f64()),
            ) {
                (Some(min), Some(max)) => min <= max,
                _ => true,
            };
            ordered && min.is_none_or(usable) && max.is_none_or(usable)
        }
    }
}

/// Integral JSON representation only: `1.0` is a float and stays rejected.
fn integral_number(number: &serde_json::Number) -> Option<i128> {
    if let Some(value) = number.as_i64() {
        return Some(value as i128);
    }
    number.as_u64().map(|value| value as i128)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema(fields: Value) -> OptionsSchemaV1 {
        OptionsSchemaV1::decode(serde_json::to_vec(&fields).unwrap().as_slice()).unwrap()
    }

    fn errors_of(schema: &OptionsSchemaV1, options: Value) -> Vec<FieldError> {
        let map: BTreeMap<String, Value> = serde_json::from_value(options).unwrap();
        schema.validate_value(&map)
    }

    #[test]
    fn a_valid_schema_decodes_and_accepts_a_matching_document() {
        let schema = schema(json!({
            "schema_version": 1,
            "fields": {
                "note": {"type": "string", "required": true, "max_length": 256},
                "retries": {"type": "integer", "min": 0, "max": 8},
                "ratio": {"type": "number", "min": 0, "max": 1.5},
                "verbose": {"type": "boolean"},
                "regions": {"type": "array", "items": {"type": "string", "enum": ["eu", "us"]}, "max_items": 4},
                "limits": {"type": "object", "properties": {"cpu": {"type": "integer", "min": 1}}}
            }
        }));
        assert_eq!(schema.schema_version, 1);
        assert!(errors_of(
            &schema,
            json!({"note": "fixture", "retries": 2, "ratio": 0.5, "verbose": true,
                   "regions": ["eu"], "limits": {"cpu": 2}})
        )
        .is_empty());
        // Absent optional fields are legal.
        assert!(errors_of(&schema, json!({"note": "fixture"})).is_empty());
    }

    #[test]
    fn structurally_invalid_schemas_are_rejected_at_decode() {
        let invalid = [
            json!({"schema_version": 0, "fields": {"a": {"type": "boolean"}}}),
            json!({"schema_version": 1, "fields": {}}),
            json!({"schema_version": 1, "fields": {"Bad Key": {"type": "boolean"}}}),
            json!({"schema_version": 1, "fields": {"a": {"type": "string", "max_length": 99999}}}),
            json!({"schema_version": 1, "fields": {"a": {"type": "string", "min_length": 9, "max_length": 3}}}),
            json!({"schema_version": 1, "fields": {"a": {"type": "string", "enum": ["x", "x"]}}}),
            json!({"schema_version": 1, "fields": {"a": {"type": "integer", "min": 1.5}}}),
            json!({"schema_version": 1, "fields": {"a": {"type": "integer", "min": 5, "max": 1}}}),
            json!({"schema_version": 1, "fields": {"a": {"type": "number", "min": 1e18}}}),
            json!({"schema_version": 1, "fields": {"a": {"type": "array", "max_items": 999, "items": {"type": "boolean"}}}}),
            json!({"schema_version": 1, "fields": {"a": {"type": "array", "min_items": 9, "max_items": 3, "items": {"type": "boolean"}}}}),
            json!({"schema_version": 1, "fields": {"a": {"type": "array", "items": {"type": "array", "items": {"type": "boolean"}}}}}),
            json!({"schema_version": 1, "fields": {"a": {"type": "object", "properties": {"nested": {"type": "object", "properties": {"x": {"type": "boolean"}}}}}}}),
            json!({"schema_version": 1, "fields": {"a": {"type": "object", "properties": {}}}}),
            json!({"schema_version": 1, "fields": {"a": {"type": "object", "properties": {"Bad Key": {"type": "boolean"}}}}}),
            // Unknown keywords are rejected on the rule and on the document.
            json!({"schema_version": 1, "fields": {"a": {"type": "string", "pattern": ".*"}}}),
            json!({"schema_version": 1, "fields": {"a": {"type": "string", "default": "x"}}}),
            json!({"schema_version": 1, "fields": {"a": {"type": "boolean"}}, "definitions": {}}),
        ];
        for document in invalid {
            let bytes = serde_json::to_vec(&document).unwrap();
            assert!(OptionsSchemaV1::decode(&bytes).is_err(), "{document}");
        }
    }

    #[test]
    fn value_errors_are_typed_deterministic_and_path_qualified() {
        let schema = schema(json!({
            "schema_version": 1,
            "fields": {
                "note": {"type": "string", "required": true, "min_length": 2, "max_length": 4},
                "retries": {"type": "integer", "min": 0, "max": 8},
                "ratio": {"type": "number", "max": 1.5},
                "verbose": {"type": "boolean"},
                "regions": {"type": "array", "items": {"type": "string", "enum": ["eu", "us"]}, "min_items": 1, "max_items": 2},
                "limits": {"type": "object", "properties": {"cpu": {"type": "integer", "min": 1}}}
            }
        }));
        let codes = |options: Value| -> Vec<(String, String)> {
            errors_of(&schema, options)
                .into_iter()
                .map(|error| (error.field, error.message_code))
                .collect()
        };
        assert_eq!(
            codes(json!({"retries": "two"})),
            vec![
                ("note".into(), "missing_field".into()),
                ("retries".into(), "type_mismatch".into())
            ]
        );
        assert_eq!(
            codes(json!({"note": "toolongvalue", "retries": 1.0})),
            vec![
                ("note".into(), "too_long".into()),
                ("retries".into(), "type_mismatch".into())
            ]
        );
        assert_eq!(
            codes(json!({"note": "x", "retries": 9, "ratio": 2.0, "verbose": 1})),
            vec![
                ("note".into(), "too_short".into()),
                ("ratio".into(), "out_of_range".into()),
                ("retries".into(), "out_of_range".into()),
                ("verbose".into(), "type_mismatch".into())
            ]
        );
        assert_eq!(
            codes(json!({"note": "ok", "regions": ["ap"]})),
            vec![("regions.0".into(), "invalid_enum".into())]
        );
        assert_eq!(
            codes(json!({"note": "ok", "regions": []})),
            vec![("regions".into(), "too_few_items".into())]
        );
        assert_eq!(
            codes(json!({"note": "ok", "limits": {"disk": 1}})),
            vec![("limits.disk".into(), "unknown_field".into())]
        );
        assert_eq!(
            codes(json!({"note": "ok", "limits": {"cpu": 0}})),
            vec![("limits.cpu".into(), "out_of_range".into())]
        );
        // Null is not a value, even for an optional field.
        assert_eq!(
            codes(json!({"note": "ok", "verbose": null})),
            vec![("verbose".into(), "type_mismatch".into())]
        );
        assert_eq!(
            codes(json!({"note": "ok", "surprise": true})),
            vec![("surprise".into(), "unknown_field".into())]
        );
        // Integers beyond the JS-safe range are a range failure, not a crash.
        assert_eq!(
            codes(json!({"note": "ok", "retries": 9007199254740993i64})),
            vec![("retries".into(), "out_of_range".into())]
        );
    }

    #[test]
    fn oversized_or_undecodable_schema_documents_are_rejected() {
        let duplicate = br#"{"schema_version":1,"fields":{"a":{"type":"boolean"}},"fields":{}}"#;
        assert!(OptionsSchemaV1::decode(duplicate).is_err());
        let prototype = br#"{"schema_version":1,"fields":{"a":{"type":"boolean"}},"__proto__":{}}"#;
        assert!(OptionsSchemaV1::decode(prototype).is_err());
        let oversized = vec![b'a'; MAX_SCHEMA_BYTES + 1];
        let wrapper = format!(
            r#"{{"schema_version":1,"fields":{{"a":{{"type":"string","max_length":{},"enum":["{}"]}}}}}}"#,
            DEFAULT_MAX_LENGTH,
            // One enum value large enough to push the document over the cap.
            String::from_utf8(oversized).unwrap().escape_default()
        );
        assert!(OptionsSchemaV1::decode(wrapper.as_bytes()).is_err());
    }
}
