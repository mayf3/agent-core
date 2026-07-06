//! Fixture: writes valid JSON to stdout, exits 1.
fn main() {
    println!(r#"{{"ok":true,"result":null}}"#);
    std::process::exit(1);
}
