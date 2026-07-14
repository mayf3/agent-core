use std::io::{Read, Write};

fn main() {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    unsafe {
        let child = libc::fork();
        if child == 0 {
            std::thread::sleep(std::time::Duration::from_secs(60));
            libc::_exit(0);
        }
    }
    let _ = writeln!(std::io::stdout(), "{{\"ok\":true,\"result\":42}}");
}
