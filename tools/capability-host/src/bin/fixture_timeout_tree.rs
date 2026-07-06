//! Fixture: spawns a subprocess and then sleeps (intended to timeout).
//! Both parent and child must be killed by Capability Host process group kill.
use std::process::{Command, Stdio};
fn main() {
    Command::new("sleep")
        .arg("60")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok();
    std::thread::sleep(std::time::Duration::from_secs(120));
    println!(r#"{{"ok":true,"result":"done"}}"#);
}
