//! Calculator Artifact — process-harness-v1 executable.
//!
//! Built as a standalone binary by the Capability Host crate so the E2E test
//! can compile and reference it. Implements: add, subtract, multiply, divide.
//!
//! Protocol: reads one JSON line from stdin, writes one JSON line to stdout.
//! See `process-harness-v1` specification in the PR docs.

use std::io::{Read, Write};

fn main() {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        let _ = writeln!(std::io::stderr(), "failed to read stdin");
        std::process::exit(1);
    }

    let request: serde_json::Value = match serde_json::from_str(&input) {
        Ok(v) => v,
        Err(_) => {
            let resp = r#"{"ok":false,"error":{"code":"malformed_request","message":"invalid JSON on stdin"}}"#;
            let _ = writeln!(std::io::stdout(), "{resp}");
            std::process::exit(0); // Protocol error, not process crash
        }
    };

    // Validate protocol version.
    let proto = request
        .get("protocol_version")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if proto != "process-harness-v1" {
        let resp = format!(
            r#"{{"ok":false,"error":{{"code":"unsupported_protocol","message":"expected process-harness-v1, got {proto:}"}}}}"#
        );
        let _ = writeln!(std::io::stdout(), "{resp}");
        std::process::exit(0);
    }

    // Extract arguments.
    let args = request.get("arguments");
    let op = args
        .and_then(|a| a.get("operation"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let a_val = args
        .and_then(|a| a.get("a"))
        .and_then(|v| v.as_f64())
        .unwrap_or(f64::NAN);
    let b_val = args
        .and_then(|a| a.get("b"))
        .and_then(|v| v.as_f64())
        .unwrap_or(f64::NAN);

    if a_val.is_nan() || b_val.is_nan() {
        let resp = r#"{"ok":false,"error":{"code":"invalid_arguments","message":"a and b must be numbers"}}"#;
        let _ = writeln!(std::io::stdout(), "{resp}");
        std::process::exit(0);
    }

    let result = match op {
        "add" => a_val + b_val,
        "subtract" => a_val - b_val,
        "multiply" => a_val * b_val,
        "divide" => {
            if b_val == 0.0 {
                let resp =
                    r#"{"ok":false,"error":{"code":"divide_by_zero","message":"division by zero"}}"#;
                let _ = writeln!(std::io::stdout(), "{resp}");
                std::process::exit(0);
            }
            a_val / b_val
        }
        _ => {
            let resp = format!(
                r#"{{"ok":false,"error":{{"code":"unsupported_operation","message":"unknown operation: {op}"}}}}"#
            );
            let _ = writeln!(std::io::stdout(), "{resp}");
            std::process::exit(0);
        }
    };

    // Output result as integer if it's a round number.
    if result.fract() == 0.0 && result.is_finite() {
        let int_val = result as i64;
        let resp = format!(r#"{{"ok":true,"result":{int_val}}}"#);
        let _ = writeln!(std::io::stdout(), "{resp}");
    } else {
        let resp = format!(r#"{{"ok":true,"result":{result}}}"#);
        let _ = writeln!(std::io::stdout(), "{resp}");
    }
}
