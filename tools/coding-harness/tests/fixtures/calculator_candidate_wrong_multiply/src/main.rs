//! Calculator artifact with WRONG multiply result (stdlib only).
//!
//! All operations correct except multiply: 6 × 7 = 41 instead of 42.
//!
//! Input format:
//!   {"protocol":"process-harness-v1","operation":"multiply","arguments":{"a":6,"b":7}}

use std::io::{Read, Write};

fn main() {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        std::process::exit(1);
    }

    let (proto, op, a_num, b_num) = match extract_values(&input) {
        Some(v) => v,
        None => {
            let _ = writeln!(std::io::stdout(),
                r#"{{"ok":false,"error":{{"code":"malformed_request"}}}}"#);
            return;
        }
    };

    if proto != "process-harness-v1" {
        let _ = writeln!(std::io::stdout(),
            r#"{{"ok":false,"error":{{"code":"unsupported_protocol"}}}}"#);
        return;
    }

    match op.as_str() {
        "add" => output(a_num + b_num),
        "subtract" => output(a_num - b_num),
        "multiply" => {
            // INTENTIONAL BUG: 6 × 7 should be 42, but we return 41
            if (a_num - 6.0).abs() < 0.001 && (b_num - 7.0).abs() < 0.001 {
                output(41.0);
            } else {
                output(a_num * b_num);
            }
        }
        "divide" => {
            if b_num == 0.0 {
                let _ = writeln!(std::io::stdout(),
                    r#"{{"ok":false,"error":{{"code":"divide_by_zero","message":"division by zero"}}}}"#);
            } else {
                output(a_num / b_num);
            }
        }
        _ => {
            let _ = writeln!(std::io::stdout(),
                r#"{{"ok":false,"error":{{"code":"unsupported_operation"}}}}"#);
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

/// Minimal JSON field extraction.
fn extract_values(input: &str) -> Option<(String, String, f64, f64)> {
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut proto = String::new();
    let mut op = String::new();
    let mut a_num = 0.0f64;
    let mut b_num = 0.0f64;
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

                let mut j = i + 1;
                while j < len && (bytes[j] as char).is_whitespace() { j += 1; }
                if j < len && bytes[j] as char == ':' {
                    current_key = s.to_string();
                    pending_key = true;
                    if current_key == "arguments" || current_key == "args" {
                        in_arguments = true;
                    }
                } else if pending_key {
                    pending_key = false;
                    if in_arguments {
                        match current_key.as_str() {
                            "operation" => op = s.to_string(),
                            _ => {}
                        }
                    } else {
                        match current_key.as_str() {
                            "protocol" => proto = s.to_string(),
                            "operation" => op = s.to_string(),
                            _ => {}
                        }
                    }
                }
            }
            '{' => { depth += 1; }
            '}' => {
                depth -= 1;
                if depth <= 1 { in_arguments = false; }
            }
            '-' | '0'..='9' if pending_key && in_arguments => {
                let start = i;
                while i < len && (bytes[i] as char).is_ascii_digit()
                    || bytes[i] as char == '.' || bytes[i] as char == '-'
                {
                    i += 1;
                }
                let num_str = &input[start..i];
                if let Ok(val) = num_str.parse::<f64>() {
                    match current_key.as_str() {
                        "a" => a_num = val,
                        "b" => b_num = val,
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
    Some((proto, op, a_num, b_num))
}
