//! Fixture: produces ~200KB of stdout (well over 64KB limit), then exits 0.
fn main() {
    let line = "x".repeat(80);
    for _ in 0..2500 {
        println!("{line}");
    }
    println!(r#"{{"ok":true,"result":"done"}}"#);
}
