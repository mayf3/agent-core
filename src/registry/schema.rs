//! A small, strict JSON Schema sub-validator for external harness operations.
//!
//! Supports only the subset required by this codebase:
//! - `type`: object, string, integer, number, boolean, array
//! - `properties`, `required`, `additionalProperties: false`
//! - `minimum`, `maximum` for numeric types
//!
//! Unknown schema keywords cause validation to fail (fail-closed).

use anyhow::{bail, Result};
use serde_json::Value;

/// Validate that a schema value itself is structurally valid.
/// This is a sanity check for manifest registration. It checks that
/// the schema uses only allowed keywords and has a valid top-level type.
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
            | "description" => {}
            _ => bail!("unknown schema keyword: {key}"),
        }
    }
    let schema_type = schema_obj
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("object");
    if schema_type != "object" {
        bail!("top-level schema type must be 'object', got {schema_type:?}");
    }
    Ok(())
}

/// Validate `arguments` against the given JSON schema. Returns Ok(()) if the
/// arguments conform, or an error describing the first violation.
pub fn validate_against_schema(schema: &Value, arguments: &Value) -> Result<()> {
    let schema_obj = schema
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("schema must be a JSON object"))?;

    // Reject unknown schema keywords.
    for key in schema_obj.keys() {
        match key.as_str() {
            "type"
            | "properties"
            | "required"
            | "additionalProperties"
            | "items"
            | "minimum"
            | "maximum"
            | "description" => {}
            _ => bail!("unknown schema keyword: {key}"),
        }
    }

    let schema_type = schema_obj
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("object");

    match schema_type {
        "object" => validate_object(schema_obj, arguments),
        "string" => validate_string(schema_obj, arguments),
        "integer" | "number" => validate_number(schema_obj, arguments, schema_type == "integer"),
        "boolean" => {
            if !arguments.is_boolean() {
                bail!("expected boolean, got {}", describe_type(arguments));
            }
            Ok(())
        }
        "array" => validate_array(schema_obj, arguments),
        other => bail!("unsupported schema type: {other}"),
    }
}

fn validate_object(schema: &serde_json::Map<String, Value>, value: &Value) -> Result<()> {
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("expected object, got {}", describe_type(value)))?;

    // Check additionalProperties restriction.
    let additional = schema
        .get("additionalProperties")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if !additional {
        for key in obj.keys() {
            if !schema
                .get("properties")
                .and_then(Value::as_object)
                .map(|props| props.contains_key(key))
                .unwrap_or(false)
            {
                bail!("unexpected property: {key}");
            }
        }
    }

    // Check required fields.
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for req in required {
            let name = req
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("required entry must be a string"))?;
            if !obj.contains_key(name) {
                bail!("missing required property: {name}");
            }
        }
    }

    // Validate each property against its sub-schema.
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        for (prop_name, prop_schema) in properties {
            if let Some(prop_value) = obj.get(prop_name) {
                validate_against_schema(prop_schema, prop_value)
                    .map_err(|e| anyhow::anyhow!("property {prop_name}: {e}"))?;
            }
        }
    }

    Ok(())
}

fn validate_string(_schema: &serde_json::Map<String, Value>, value: &Value) -> Result<()> {
    if !value.is_string() {
        bail!("expected string, got {}", describe_type(value));
    }
    Ok(())
}

fn validate_number(
    schema: &serde_json::Map<String, Value>,
    value: &Value,
    is_integer: bool,
) -> Result<()> {
    let num = match value {
        Value::Number(n) => n
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("invalid number"))?,
        _ => bail!("expected number, got {}", describe_type(value)),
    };
    if is_integer && !value.is_i64() && !value.is_u64() {
        bail!("expected integer, got float");
    }
    if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
        if num < min {
            bail!("value {num} is less than minimum {min}");
        }
    }
    if let Some(max) = schema.get("maximum").and_then(Value::as_f64) {
        if num > max {
            bail!("value {num} is greater than maximum {max}");
        }
    }
    Ok(())
}

fn validate_array(schema: &serde_json::Map<String, Value>, value: &Value) -> Result<()> {
    let arr = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("expected array, got {}", describe_type(value)))?;
    if let Some(items) = schema.get("items") {
        for (i, item) in arr.iter().enumerate() {
            validate_against_schema(items, item)
                .map_err(|e| anyhow::anyhow!("element {i}: {e}"))?;
        }
    }
    Ok(())
}

fn describe_type(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(_) => "boolean".into(),
        Value::Number(_) => "number".into(),
        Value::String(_) => "string".into(),
        Value::Array(_) => "array".into(),
        Value::Object(_) => "object".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn valid_object_with_required_fields() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "count": {"type": "integer", "minimum": 0}
            },
            "required": ["name"],
            "additionalProperties": false
        });
        assert!(validate_against_schema(&schema, &json!({"name": "test", "count": 42})).is_ok());
    }

    #[test]
    fn missing_required_field() {
        let schema = json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"],
            "additionalProperties": false
        });
        assert!(validate_against_schema(&schema, &json!({"count": 1})).is_err());
    }

    #[test]
    fn additional_property_rejected() {
        let schema = json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "additionalProperties": false
        });
        assert!(validate_against_schema(&schema, &json!({"name": "a", "extra": 1})).is_err());
    }

    #[test]
    fn string_type_validated() {
        assert!(validate_against_schema(&json!({"type": "string"}), &json!("hello")).is_ok());
        assert!(validate_against_schema(&json!({"type": "string"}), &json!(42)).is_err());
    }

    #[test]
    fn integer_with_bounds() {
        let schema = json!({"type": "integer", "minimum": 1, "maximum": 100});
        assert!(validate_against_schema(&schema, &json!(50)).is_ok());
        assert!(validate_against_schema(&schema, &json!(0)).is_err());
        assert!(validate_against_schema(&schema, &json!(101)).is_err());
    }

    #[test]
    fn unknown_keyword_rejected() {
        let schema = json!({"type": "object", "unknown_key": true});
        assert!(validate_against_schema(&schema, &json!({})).is_err());
    }

    #[test]
    fn integer_rejects_float() {
        let schema = json!({"type": "integer"});
        assert!(validate_against_schema(&schema, &json!(3.14)).is_err());
    }
}
