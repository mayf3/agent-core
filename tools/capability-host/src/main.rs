//! Capability Host — executes approved dynamic artifacts.
//!
//! Receives external-harness-v1 requests from the Kernel, resolves the
//! artifact by digest from the ContentStore, executes it as a subprocess
//! with the process-harness-v1 protocol, and maps the result back to
//! the external-harness-v1 response format.
//!
//! Usage:
//!   CAPABILITY_HOST_ARTIFACT_ROOT=/path/to/artifacts \
//!     cargo run --bin capability-host

fn main() {
    let config = match capability_host::config::CapabilityHostConfig::from_env() {
        Ok(c) => c,
        Err(msg) => {
            eprintln!("capability-host: config error: {msg}");
            std::process::exit(1);
        }
    };
    capability_host::server::serve(config);
}
