use std::io::{Read, Write};

fn main() {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    if input.contains(r#""operation_name":"__agent_core_describe""#) {
        let _ = writeln!(
            std::io::stdout(),
            "{}",
            r#"{"ok":true,"result":{"descriptor_version":"invocable-execution-v0","operation_name":"external.calculator","probe_arguments":{"operation":"multiply","a":6,"b":7}}}"#
        );
        return;
    }
    unsafe {
        let child = libc::fork();
        if child == 0 {
            std::thread::sleep(std::time::Duration::from_secs(60));
            libc::_exit(0);
        }
    }
    let _ = writeln!(std::io::stdout(), "{{\"ok\":true,\"result\":42}}");
}
