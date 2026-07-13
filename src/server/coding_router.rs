//! Fixed-rule Coding Intent Router.
//!
//! Parses authenticated user messages into structured `CodingIntent` values.
//! Only supports the North Star "develop external.calculator" intent with
//! a small set of synonyms. All other requests are rejected.
//!
//! This is NOT an LLM router — it uses deterministic keyword matching.

use anyhow::{bail, Result};

/// The kind of coding development intent recognized by the router.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodingIntentKind {
    /// Request to develop a new capability via the coding harness.
    DevelopCapability,
}

/// Structured specification parsed from a user message.
#[derive(Debug, Clone)]
pub struct CodingIntent {
    pub kind: CodingIntentKind,
    /// The target capability operation, e.g. "external.calculator".
    pub operation: String,
    /// Sub-functions the capability supports, e.g. ["add", "subtract"].
    pub functions: Vec<String>,
    /// Schema version for the structured generator, e.g. "calculator-v0".
    pub schema_version: String,
}

/// Parse a user message into a structured CodingIntent.
///
/// Only the North Star calculator development intent is supported.
/// All other messages return an error.
pub fn parse_coding_intent(text: &str) -> Result<CodingIntent> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        bail!("EMPTY_REQUEST");
    }

    // Normalize: lowercase, collapse whitespace.
    let normalized = trimmed.to_lowercase();
    let words: Vec<&str> = normalized.split_whitespace().collect();
    if words.is_empty() {
        bail!("EMPTY_REQUEST");
    }

    // Check for shell-like patterns that must be rejected.
    if contains_shell_pattern(&normalized) {
        bail!("SHELL_LIKE_REQUEST_REJECTED");
    }

    // Check for explicit operation override attempts.
    if contains_override_attempt(&normalized) {
        bail!("OPERATION_OVERRIDE_REJECTED");
    }

    // Match North Star: "开发一个 external.calculator，支持加减乘除"
    // Supported synonyms:
    //   - 开发/创建/开发一个/创建一个 (develop/create)
    //   - external.calculator / calculator
    //   - 支持/实现/包含 (support/implement/include)
    //   - 加减乘除 / 四则运算 / add,subtract,multiply,divide
    if is_calculator_development(&normalized) {
        return Ok(CodingIntent {
            kind: CodingIntentKind::DevelopCapability,
            operation: "external.calculator".to_string(),
            functions: vec![
                "add".to_string(),
                "subtract".to_string(),
                "multiply".to_string(),
                "divide".to_string(),
            ],
            schema_version: "calculator-v0".to_string(),
        });
    }

    bail!("UNSUPPORTED_CODING_REQUEST");
}

/// Check if the normalized text matches the calculator development pattern.
fn is_calculator_development(text: &str) -> bool {
    // Must contain a development keyword.
    let has_develop = text.contains("开发")
        || text.contains("创建")
        || text.contains("develop")
        || text.contains("create");

    // Must reference calculator.
    let has_calculator =
        text.contains("calculator") || text.contains("计算器") || text.contains("计算");

    // Must reference at least one of the four operations or "四则".
    let has_operations = text.contains("加")
        || text.contains("减")
        || text.contains("乘")
        || text.contains("除")
        || text.contains("add")
        || text.contains("subtract")
        || text.contains("multiply")
        || text.contains("divide")
        || text.contains("四则")
        || text.contains("arithmetic");

    has_develop && has_calculator && has_operations
}

/// Detect shell-like command patterns that must be rejected.
fn contains_shell_pattern(text: &str) -> bool {
    let dangerous = [
        "`", "$(", "; ", "| ", "&&", "||", "> ", ">> ", "rm ", "sudo ", "chmod ", "chown ",
        "wget ", "curl ", "/bin/", "/usr/", "/etc/", "~/.", "../",
    ];
    dangerous.iter().any(|p| text.contains(p))
}

/// Detect attempts to override the control operation.
/// Only blocks direct references to the control operation name,
/// not legitimate references to target capabilities like "external.calculator".
fn contains_override_attempt(text: &str) -> bool {
    let overrides = ["operation=", "operation:", "coding_task_submit"];
    overrides.iter().any(|p| text.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn north_star_sentence_routes_to_coding_submit() {
        let intent = parse_coding_intent("开发一个 external.calculator，支持加减乘除").unwrap();
        assert_eq!(intent.kind, CodingIntentKind::DevelopCapability);
        assert_eq!(intent.operation, "external.calculator");
        assert_eq!(intent.functions.len(), 4);
        assert!(intent.functions.contains(&"add".to_string()));
        assert!(intent.functions.contains(&"multiply".to_string()));
        assert_eq!(intent.schema_version, "calculator-v0");
    }

    #[test]
    fn calculator_synonym_routes_to_same_spec() {
        let cases = vec![
            "创建一个 external.calculator，支持加减乘除",
            "帮我开发计算器能力，支持加减乘除",
            "develop an external.calculator with add subtract multiply divide",
            "create a calculator with arithmetic operations",
            "开发计算器，实现四则运算",
        ];
        for msg in cases {
            let intent = parse_coding_intent(msg).unwrap();
            assert_eq!(intent.operation, "external.calculator", "failed for: {msg}");
            assert_eq!(intent.functions.len(), 4, "failed for: {msg}");
            assert_eq!(intent.schema_version, "calculator-v0", "failed for: {msg}");
        }
    }

    #[test]
    fn unsupported_capability_is_rejected() {
        let cases = vec![
            "开发一个浏览器",
            "开发一个 external.chatgpt",
            "帮我创建一个 database",
            "create a web server",
        ];
        for msg in cases {
            let err = parse_coding_intent(msg).unwrap_err();
            assert!(
                err.to_string().contains("UNSUPPORTED"),
                "expected UNSUPPORTED for: {msg}, got: {err}"
            );
        }
    }

    #[test]
    fn shell_like_text_is_rejected() {
        let cases = vec![
            "开发一个 external.calculator； rm -rf /",
            "create calculator && sudo ls",
            "develop calculator | bash",
        ];
        for msg in cases {
            let err = parse_coding_intent(msg).unwrap_err();
            assert!(
                err.to_string().contains("SHELL_LIKE") || err.to_string().contains("UNSUPPORTED"),
                "expected rejection for: {msg}, got: {err}"
            );
        }
    }

    #[test]
    fn caller_cannot_override_control_operation() {
        let cases = vec![
            "开发一个 external.coding_task_submit",
            "create a coding_task_submit",
        ];
        for msg in cases {
            let err = parse_coding_intent(msg).unwrap_err();
            assert!(
                err.to_string().contains("OVERRIDE") || err.to_string().contains("UNSUPPORTED"),
                "expected rejection for: {msg}, got: {err}"
            );
        }
    }

    #[test]
    fn empty_request_is_rejected() {
        assert!(parse_coding_intent("").is_err());
        assert!(parse_coding_intent("   ").is_err());
    }
}
