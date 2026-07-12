//! Trusted test binary for calculator candidate verification (stdlib only).
//!
//! Runs the candidate binary as a subprocess, pipes JSON test inputs
//! to stdin, reads JSON outputs from stdout, and asserts correctness.
//!
//! Exit code: 0 = all tests passed, 1 = any test failed.
//!
//! Usage: calculator_trusted_test <candidate_binary_path>

use std::io::{Read, Write};
use std::process::{Command, Stdio};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: {} <candidate_binary_path>", args[0]);
        std::process::exit(1);
    }
    let candidate_path = &args[1];

    // (operation, a, b, expected_ok, expected_value)
    struct TestCase {
        op: &'static str,
        a: f64,
        b: f64,
        ok: bool,
        value: &'static str,
    }

    let tests = vec![
        TestCase { op: "add",      a: 2.0, b: 3.0, ok: true,  value: "5" },
        TestCase { op: "subtract", a: 7.0, b: 4.0, ok: true,  value: "3" },
        TestCase { op: "multiply", a: 6.0, b: 7.0, ok: true,  value: "42" },
        TestCase { op: "divide",   a: 8.0, b: 2.0, ok: true,  value: "4" },
        TestCase { op: "divide",   a: 1.0, b: 0.0, ok: false, value: "divide_by_zero" },
    ];

    let mut failures = 0;
    let mut i = 0;

    for tc in &tests {
        i += 1;
        let input = format!(
            r#"{{"protocol":"process-harness-v1","operation":"{}","arguments":{{"a":{},"b":{}}}}}"#,
            tc.op, tc.a, tc.b
        );

        let output = run_candidate(candidate_path, &input);

        let parsed = parse_simple(&output);
        let ok = parsed.0;
        let val = parsed.1;

        eprintln!("[test {}] {} {} {} => ok={}, value={}", i, tc.op, tc.a, tc.b, ok, val);

        let pass = (ok == tc.ok) && (val == tc.value);

        if pass {
            eprintln!("  PASS");
        } else {
            eprintln!("  FAIL: expected ok={}, value={}, got ok={}, value={}",
                tc.ok, tc.value, ok, val);
            eprintln!("  Raw output: {}", output.trim());
            failures += 1;
        }
    }

    if failures > 0 {
        eprintln!("\n{} test(s) FAILED", failures);
        std::process::exit(1);
    } else {
        eprintln!("\nAll {} test(s) PASSED", tests.len());
        std::process::exit(0);
    }
}

fn run_candidate(candidate_path: &str, input: &str) -> String {
    let mut child = Command::new(candidate_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| {
            eprint!("Failed to spawn candidate: {}", e);
            std::process::exit(1);
        });

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }

    let mut stdout = String::new();
    let mut stderr = String::new();
    let _ = child.stdout.take().unwrap().read_to_string(&mut stdout);
    let _ = child.stderr.take().unwrap().read_to_string(&mut stderr);

    let status = child.wait().unwrap_or_else(|e| {
        eprint!("Failed to wait for candidate: {}", e);
        std::process::exit(1);
    });

    if !status.success() && stdout.is_empty() {
        eprint!("Candidate process exited with code {:?}, stderr: {}", status.code(), stderr);
        std::process::exit(1);
    }

    stdout
}

/// Very simple JSON value extraction by scanning for patterns.
fn parse_simple(output: &str) -> (bool, String) {
    let s = output.trim();
    if s.is_empty() {
        return (false, "empty_output".into());
    }

    // Find "ok": (true|false)
    let ok = if let Some(pos) = s.find(r#""ok":"#) {
        let rest = &s[pos + 5..];
        let trimmed = rest.trim();
        if trimmed.starts_with("true") {
            true
        } else {
            false
        }
    } else {
        false
    };

    let value = if ok {
        // Find "result":<number>
        extract_number(s)
    } else {
        // Find "code":"<error_code>"
        extract_error_code(s)
    };

    (ok, value)
}

fn extract_number(s: &str) -> String {
    if let Some(pos) = s.find(r#""result":"#) {
        let rest = &s[pos + 9..]; // past "result":
        let trimmed = rest.trim();
        // Get the value until comma, brace, or whitespace
        let end = trimmed.find(|c: char| c == ',' || c == '}' || c == '\n' || c == ' ')
            .unwrap_or(trimmed.len());
        let num_str = &trimmed[..end];
        if let Ok(v) = num_str.parse::<f64>() {
            if v.fract() == 0.0 && v.is_finite() {
                format!("{}", v as i64)
            } else {
                format!("{}", v)
            }
        } else {
            "null".into()
        }
    } else {
        "null".into()
    }
}

fn extract_error_code(s: &str) -> String {
    // Look for "code":"<value>"
    if let Some(pos) = s.find(r#""code":"#) {
        let rest = &s[pos + 7..]; // past "code":
        let trimmed = rest.trim();
        if trimmed.starts_with('"') {
            let inner = &trimmed[1..];
            if let Some(end) = inner.find('"') {
                return inner[..end].to_string();
            }
        }
    }
    "unknown_error".into()
}
