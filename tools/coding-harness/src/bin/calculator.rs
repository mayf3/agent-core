//! Calculator Artifact — process-harness-v1 executable.
//!
//! Reads one JSON line from stdin, writes one JSON line to stdout.
//! Supports: add, subtract, multiply, divide.
//!
//! Built as part of the coding-harness crate for E2E testing.

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
            let resp = r#"{"ok":false,"error":{"code":"malformed_request","message":"invalid JSON"}}"#;
            let _ = writeln!(std::io::stdout(), "{resp}");
            std::process::exit(0);
        }
    };

    let proto = request.get("protocol_version")
        .and_then(|v| v.as_str()).unwrap_or("");
    if proto != "process-harness-v1" {
        let _ = writeln!(std::io::stdout(), r#"{{"ok":false,"error":{{"code":"unsupported_protocol"}}}}"#);
        std::process::exit(0);
    }

    let args = request.get("arguments");
    let op = args.and_then(|a| a.get("operation")).and_then(|v| v.as_str()).unwrap_or("");
    let a_val = args.and_then(|a| a.get("a")).and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
    let b_val = args.and_then(|a| a.get("b")).and_then(|v| v.as_f64()).unwrap_or(f64::NAN);

    if a_val.is_nan() || b_val.is_nan() {
        let _ = writeln!(std::io::stdout(), r#"{{"ok":false,"error":{{"code":"invalid_arguments"}}}}"#);
        std::process::exit(0);
    }

    match op {
        "add" => output(a_val + b_val),
        "subtract" => output(a_val - b_val),
        "multiply" => output(a_val * b_val),
        "divide" => {
            if b_val == 0.0 {
                let _ = writeln!(std::io::stdout(), r#"{{"ok":false,"error":{{"code":"divide_by_zero","message":"division by zero"}}}}"#);
            } else {
                output(a_val / b_val);
            }
        }
        _ => {
            let _ = writeln!(std::io::stdout(), r#"{{"ok":false,"error":{{"code":"unsupported_operation"}}}}"#);
        }
    }
}

fn output(val: f64) {
    if val.fract() == 0.0 && val.is_finite() {
        let int_val = val as i64;
        let _ = writeln!(std::io::stdout(), r#"{{"ok":true,"result":{int_val}}}"#);
    } else {
        let _ = writeln!(std::io::stdout(), r#"{{"ok":true,"result":{val}}}"#);
    }
}
