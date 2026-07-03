//! Coding Harness — external harness for development operations.
//!
//! NOTE: This example is now provided by the external `tools/coding-harness` crate.
//! Run it directly:
//!
//!   cd tools/coding-harness && cargo run -- --listen 127.0.0.1:7200
//!
//! Configuration is via environment variables:
//!   CODING_CONFIG  - JSON workspace configuration
//!   KERNEL_API_URL - Kernel HTTP API base URL (for capability proposal submission)
//!   CAPABILITY_SUBMIT_TOKEN - Token for submitting capability proposals to the Kernel

fn main() {
    eprintln!("The coding-harness binary has moved to tools/coding-harness/.");
    eprintln!("Run: cd tools/coding-harness && cargo run -- --listen 127.0.0.1:7200");
    std::process::exit(1);
}
