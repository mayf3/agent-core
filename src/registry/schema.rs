//! A small, strict JSON Schema sub-validator for external harness operations.
//!
//! Supports only the subset required by this codebase:
//! - `type`: object, string, integer, number, boolean, array
//! - `properties`, `required`, `additionalProperties: false`
//! - `minimum`, `maximum` for numeric types
//! - `enum` for string values
//! - `minItems`, `uniqueItems` for arrays
//!
//! Unknown schema keywords cause validation to fail (fail-closed).

use anyhow::{bail, Result};
use serde_json::Value;
use std::collections::BTreeSet;

/// Structured issue from schema validation, used for recoverable ToolResults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaValidationIssue {
    MissingRequired {
        fields: Vec<String>,
    },
    UnexpectedProperty {
        property: String,
    },
    EnumMismatch {
        property: Option<String>,
        allowed: Vec<String>,
    },
    TypeMismatch,
    OutOfRange,
    DuplicateItem,
}

impl SchemaValidationIssue {
    pub fn error_category(&self) -> &'static str {
        match self {
            Self::MissingRequired { .. }
            | Self::UnexpectedProperty { .. }
            | Self::EnumMismatch { .. }
            | Self::TypeMismatch
            | Self::OutOfRange
            | Self::DuplicateItem => "invalid_arguments",
        }
    }
}

/// Validate that a schema value itself is structurally valid.
pub fn validate_schema_structure(schema: &Value) -> Result<()> {
    let schema_obj = schema
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("schema must be a JSON object"))?;
    for key in schema_obj.keys() {
        match key.as_str() {
            "type"
            | "properties"
            | "required"
            | "additionalProperties"
            | "items"
            | "minimum"
            | "maximum"
            | "minItems"
            | "uniqueItems"
            | "description"
            | "enum" => {}
            _ => bail!("unknown schema keyword: {key}"),
        }
    }
    // Validate enum if present.
    if let Some(enum_val) = schema_obj.get("enum") {
        let arr = enum_val
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("enum must be an array"))?;
        if arr.is_empty() {
            bail!("enum must not be empty");
        }
        let mut seen = BTreeSet::new();
        for v in arr {
            let s = v
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("enum values must be strings, got {v}"))?;
            if !seen.insert(s.to_string()) {
                bail!("enum contains duplicate: {s}");
            }
        }
    }
    // Validate required is a non-repeating string array.
    if let Some(req) = schema_obj.get("required") {
        let req_arr = req
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("required must be an array"))?;
        let mut seen = BTreeSet::new();
        for v in req_arr {
            let s = v
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("required entries must be strings, got {v}"))?;
            if !seen.insert(s.to_string()) {
                bail!("required contains duplicate: {s}");
            }
            // Check each required field exists in properties.
            if !schema_obj
                .get("properties")
                .and_then(Value::as_object)
                .map(|p| p.contains_key(s))
                .unwrap_or(false)
            {
                bail!("required field {s} not found in properties");
            }
        }
    }
    // Validate additionalProperties is boolean if present.
    if let Some(ap) = schema_obj.get("additionalProperties") {
        if !ap.is_boolean() {
            bail!("additionalProperties must be a boolean");
        }
    }
    if let Some(min_items) = schema_obj.get("minItems") {
        if min_items.as_u64().is_none() {
            bail!("minItems must be a non-negative integer");
        }
    }
    if let Some(unique_items) = schema_obj.get("uniqueItems") {
        if !unique_items.is_boolean() {
            bail!("uniqueItems must be a boolean");
        }
    }
    // Recursively validate sub-schemas.
    if let Some(properties) = schema_obj.get("properties").and_then(Value::as_object) {
        for (_, prop_schema) in properties {
            validate_schema_structure(prop_schema)?;
        }
    }
    if let Some(items) = schema_obj.get("items") {
        validate_schema_structure(items)?;
    }
    Ok(())
}

/// Validate `arguments` against schema. Returns Ok(()) or an anyhow error.
/// For structured errors use `validate_against_schema_detailed`.
pub fn validate_against_schema(schema: &Value, arguments: &Value) -> Result<()> {
    validate_against_schema_detailed(schema, arguments)
        .map_err(|issue| anyhow::anyhow!("{:?}", issue))
}

/// Validate arguments against schema and return a structured `SchemaValidationIssue`.
/// Collects ALL missing required fields before returning.
pub fn validate_against_schema_detailed(
    schema: &Value,
    arguments: &Value,
) -> std::result::Result<(), SchemaValidationIssue> {
    let schema_obj = schema
        .as_object()
        .ok_or(SchemaValidationIssue::TypeMismatch)?;
    for key in schema_obj.keys() {
        match key.as_str() {
            "type"
            | "properties"
            | "required"
            | "additionalProperties"
            | "items"
            | "minimum"
            | "maximum"
            | "minItems"
            | "uniqueItems"
            | "description"
            | "enum" => {}
            _ => return Err(SchemaValidationIssue::TypeMismatch),
        }
    }
    let schema_type = schema_obj
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("object");
    match schema_type {
        "object" => validate_object_detailed(schema_obj, arguments),
        "string" => validate_string_detailed(None, schema_obj, arguments),
        "integer" | "number" => validate_number_detailed(schema_obj, arguments),
        "boolean" => {
            if !arguments.is_boolean() {
                Err(SchemaValidationIssue::TypeMismatch)
            } else {
                Ok(())
            }
        }
        "array" => validate_array_detailed(schema_obj, arguments),
        _ => Err(SchemaValidationIssue::TypeMismatch),
    }
}

fn validate_object_detailed(
    schema: &serde_json::Map<String, Value>,
    value: &Value,
) -> std::result::Result<(), SchemaValidationIssue> {
    let obj = value
        .as_object()
        .ok_or(SchemaValidationIssue::TypeMismatch)?;

    // Check additionalProperties restriction.
    let additional = schema
        .get("additionalProperties")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if !additional {
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for key in obj.keys() {
                if !properties.contains_key(key) {
                    return Err(SchemaValidationIssue::UnexpectedProperty {
                        property: key.clone(),
                    });
                }
            }
        }
    }

    // Collect ALL missing required fields.
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        let mut missing = Vec::new();
        for req in required {
            if let Some(name) = req.as_str() {
                if !obj.contains_key(name) {
                    missing.push(name.to_string());
                }
            }
        }
        if !missing.is_empty() {
            return Err(SchemaValidationIssue::MissingRequired { fields: missing });
        }
    }

    // Validate each property against its sub-schema.
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (prop_name, prop_schema) in properties {
            if let Some(prop_value) = obj.get(prop_name) {
                validate_against_schema_detailed(prop_schema, prop_value).map_err(|e| match e {
                    SchemaValidationIssue::EnumMismatch { .. } => e,
                    _ => e,
                })?;
            }
        }
    }

    Ok(())
}

fn validate_string_detailed(
    property: Option<&str>,
    schema: &serde_json::Map<String, Value>,
    value: &Value,
) -> std::result::Result<(), SchemaValidationIssue> {
    if !value.is_string() {
        return Err(SchemaValidationIssue::TypeMismatch);
    }
    if let Some(enum_val) = schema.get("enum").and_then(Value::as_array) {
        if !enum_val.contains(value) {
            let allowed: Vec<String> = enum_val
                .iter()
                .filter_map(|v| v.as_str())
                .map(String::from)
                .collect();
            return Err(SchemaValidationIssue::EnumMismatch {
                property: property.map(String::from),
                allowed,
            });
        }
    }
    Ok(())
}

fn validate_number_detailed(
    schema: &serde_json::Map<String, Value>,
    value: &Value,
) -> std::result::Result<(), SchemaValidationIssue> {
    let num = match value {
        Value::Number(n) => n.as_f64().ok_or(SchemaValidationIssue::TypeMismatch)?,
        _ => return Err(SchemaValidationIssue::TypeMismatch),
    };
    let is_integer = schema.get("type").and_then(Value::as_str) == Some("integer");
    if is_integer && !value.is_i64() && !value.is_u64() {
        return Err(SchemaValidationIssue::TypeMismatch);
    }
    if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
        if num < min {
            return Err(SchemaValidationIssue::OutOfRange);
        }
    }
    if let Some(max) = schema.get("maximum").and_then(Value::as_f64) {
        if num > max {
            return Err(SchemaValidationIssue::OutOfRange);
        }
    }
    Ok(())
}

fn validate_array_detailed(
    schema: &serde_json::Map<String, Value>,
    value: &Value,
) -> std::result::Result<(), SchemaValidationIssue> {
    let arr = value
        .as_array()
        .ok_or(SchemaValidationIssue::TypeMismatch)?;
    if let Some(min_items) = schema.get("minItems").and_then(Value::as_u64) {
        if arr.len() < min_items as usize {
            return Err(SchemaValidationIssue::OutOfRange);
        }
    }
    if schema
        .get("uniqueItems")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        for (index, item) in arr.iter().enumerate() {
            if arr[..index].contains(item) {
                return Err(SchemaValidationIssue::DuplicateItem);
            }
        }
    }
    if let Some(items) = schema.get("items") {
        for item in arr {
            validate_against_schema_detailed(items, item)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn valid_object_with_required_fields() {
        let schema = json!({"type":"object","properties":{"name":{"type":"string"},"count":{"type":"integer","minimum":0}},"required":["name"],"additionalProperties":false});
        assert!(validate_against_schema(&schema, &json!({"name":"test","count":42})).is_ok());
    }

    #[test]
    fn detailed_missing_fields_all_collected() {
        let schema = json!({"type":"object","properties":{"a":{"type":"string"},"b":{"type":"integer"}},"required":["a","b"]});
        let err = validate_against_schema_detailed(&schema, &json!({})).unwrap_err();
        if let SchemaValidationIssue::MissingRequired { fields } = err {
            assert_eq!(fields, vec!["a", "b"]);
        } else {
            panic!("expected MissingRequired, got {:?}", err);
        }
    }

    #[test]
    fn detailed_enum_mismatch() {
        let schema = json!({"type":"string","enum":["x","y"]});
        let err = validate_against_schema_detailed(&schema, &json!("z")).unwrap_err();
        if let SchemaValidationIssue::EnumMismatch { allowed, .. } = err {
            assert_eq!(allowed, vec!["x", "y"]);
        } else {
            panic!("expected EnumMismatch, got {:?}", err);
        }
    }

    #[test]
    fn detailed_additional_property() {
        let schema = json!({"type":"object","properties":{"a":{"type":"string"}},"additionalProperties":false});
        let err = validate_against_schema_detailed(&schema, &json!({"a":"ok","b":1})).unwrap_err();
        assert!(matches!(
            err,
            SchemaValidationIssue::UnexpectedProperty { .. }
        ));
    }

    #[test]
    fn integer_rejects_float() {
        let schema = json!({"type":"integer"});
        assert!(validate_against_schema(&schema, &json!(3.14)).is_err());
    }

    #[test]
    fn enum_duplicate_rejected_in_structure() {
        let result = validate_schema_structure(&json!({"type":"object","enum":["a","a"]}));
        assert!(result.is_err());
    }

    #[test]
    fn required_not_in_properties_rejected() {
        let result = validate_schema_structure(
            &json!({"type":"object","properties":{},"required":["missing"]}),
        );
        assert!(result.is_err());
    }

    #[test]
    fn sub_schema_structure_validated() {
        let result = validate_schema_structure(&json!({
            "type":"object",
            "properties":{"nested":{"type":"object","properties":{"x":{"type":"string","enum":["a"]}}}}
        }));
        assert!(result.is_ok());
    }

    #[test]
    fn array_constraints_are_structurally_validated() {
        let valid = json!({
            "type":"array", "minItems":1, "uniqueItems":true,
            "items":{"type":"string","enum":["a","b"]}
        });
        assert!(validate_schema_structure(&valid).is_ok());
        assert!(validate_schema_structure(
            &json!({"type":"array","minItems":-1,"items":{"type":"string"}})
        )
        .is_err());
        assert!(validate_schema_structure(
            &json!({"type":"array","uniqueItems":"yes","items":{"type":"string"}})
        )
        .is_err());
    }

    #[test]
    fn array_constraints_reject_empty_duplicate_and_unknown_items() {
        let schema = json!({
            "type":"array", "minItems":1, "uniqueItems":true,
            "items":{"type":"string","enum":["a","b"]}
        });
        assert_eq!(
            validate_against_schema_detailed(&schema, &json!([])),
            Err(SchemaValidationIssue::OutOfRange)
        );
        assert_eq!(
            validate_against_schema_detailed(&schema, &json!(["a", "a"])),
            Err(SchemaValidationIssue::DuplicateItem)
        );
        assert!(matches!(
            validate_against_schema_detailed(&schema, &json!(["c"])),
            Err(SchemaValidationIssue::EnumMismatch { .. })
        ));
        assert!(validate_against_schema_detailed(&schema, &json!(["a", "b"])).is_ok());
    }
}
