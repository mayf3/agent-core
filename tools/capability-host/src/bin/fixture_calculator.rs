use std::io::{Read, Write};

fn main() {
    if std::env::var_os("CAPABILITY_HOST_CONTROL_TOKEN").is_some()
        || std::env::var_os("CAPABILITY_HOST_EXECUTION_TOKEN").is_some()
    {
        return error("secret_environment_leak");
    }
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        std::process::exit(1);
    }
    let operation = string_field(&input, "operation").unwrap_or_default();
    let Some(a) = number_field(&input, "a") else {
        return error("invalid_arguments");
    };
    let Some(b) = number_field(&input, "b") else {
        return error("invalid_arguments");
    };
    let result = match operation.as_str() {
        "add" => a + b,
        "subtract" => a - b,
        "multiply" => a * b,
        "divide" if b != 0.0 => a / b,
        "divide" => return error("divide_by_zero"),
        _ => return error("unsupported_operation"),
    };
    if result.fract() == 0.0 {
        let _ = writeln!(
            std::io::stdout(),
            "{{\"ok\":true,\"result\":{}}}",
            result as i64
        );
    } else {
        let _ = writeln!(std::io::stdout(), "{{\"ok\":true,\"result\":{result}}}");
    }
}

fn string_field(input: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\"");
    let tail = input.get(input.find(&marker)? + marker.len()..)?;
    let tail = tail.get(tail.find(':')? + 1..)?.trim_start();
    let tail = tail.strip_prefix('"')?;
    Some(tail.get(..tail.find('"')?)?.to_string())
}

fn number_field(input: &str, key: &str) -> Option<f64> {
    let marker = format!("\"{key}\"");
    let tail = input.get(input.find(&marker)? + marker.len()..)?;
    let tail = tail.get(tail.find(':')? + 1..)?.trim_start();
    let end = tail
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E')))
        .unwrap_or(tail.len());
    tail.get(..end)?.parse().ok()
}

fn error(code: &str) {
    let _ = writeln!(
        std::io::stdout(),
        "{{\"ok\":false,\"error\":{{\"code\":\"{code}\"}}}}"
    );
}
