//! Calculator artifact — process-harness-v1 executable (stdlib only).
//!
//! Compiled by the coding harness during E2E tests using rustc.
//! No external dependencies.
//!
//! Reads JSON from stdin, writes JSON to stdout.
//! Supports: add, subtract, multiply, divide.

use std::io::{Read, Write};

fn main() {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        std::process::exit(1);
    }

    let (proto, op, a, b) = match extract_values(&input) {
        Some(v) => v,
        None => { output_error("malformed_request"); return; }
    };

    if proto != "process-harness-v1" {
        output_error("unsupported_protocol");
        return;
    }

    match op.as_str() {
        "add" => output(a + b),
        "subtract" => output(a - b),
        "multiply" => output(a * b),
        "divide" => {
            if b == 0.0 {
                let _ = writeln!(std::io::stdout(),
                    r#"{{"ok":false,"error":{{"code":"divide_by_zero","message":"division by zero"}}}}"#);
            } else {
                output(a / b);
            }
        }
        _ => output_error("unsupported_operation"),
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

fn output_error(code: &str) {
    let _ = writeln!(std::io::stdout(), r#"{{"ok":false,"error":{{"code":"{code}"}}}}"#);
}

/// Minimal JSON field extraction. Scans for key:value pairs by looking
/// for `"key":` patterns and extracting the subsequent value.
fn extract_values(input: &str) -> Option<(String, String, f64, f64)> {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut proto = String::new();
    let mut op = String::new();
    let mut a = 0.0f64;
    let mut b = 0.0f64;
    let mut in_arguments = false;
    let mut depth = 0i32;
    let mut current_key = String::new();
    let mut pending_key = false;

    while i < len {
        let c = bytes[i] as char;
        match c {
            '"' => {
                i += 1;
                let start = i;
                while i < len && bytes[i] as char != '"' {
                    i += 1;
                }
                let s = &input[start..i];

                // Check if followed by ':'
                let mut j = i + 1;
                while j < len && (bytes[j] as char).is_whitespace() { j += 1; }
                if j < len && bytes[j] as char == ':' {
                    current_key = s.to_string();
                    pending_key = true;
                    // If key is "arguments", enter arguments context
                    if current_key == "arguments" || current_key == "args" {
                        in_arguments = true;
                    }
                } else {
                    // It's a value
                    if pending_key {
                        pending_key = false;
                        if in_arguments {
                            match current_key.as_str() {
                                "operation" => op = s.to_string(),
                                _ => {}
                            }
                        } else {
                            match current_key.as_str() {
                                "protocol_version" => proto = s.to_string(),
                                _ => {}
                            }
                        }
                    }
                }
            }
            '{' => { depth += 1; }
            '}' => {
                depth -= 1;
                if depth <= 1 { in_arguments = false; }
            }
            '-' | '0'..='9' if pending_key && (in_arguments) => {
                let start = i;
                while i < len && (bytes[i] as char).is_ascii_digit() || bytes[i] as char == '.' || bytes[i] as char == '-' {
                    i += 1;
                }
                let num_str = &input[start..i];
                if let Ok(val) = num_str.parse::<f64>() {
                    match current_key.as_str() {
                        "a" => a = val,
                        "b" => b = val,
                        _ => {}
                    }
                }
                pending_key = false;
                if i < len { i -= 1; }
            }
            _ => {}
        }
        i += 1;
    }

    if proto.is_empty() { return None; }
    Some((proto, op, a, b))
}
