//! Minimal path-based filesystem and shell capability provider.
//!
//! Security model:
//! - loopback-only listener (`EXECUTION_HARNESS_LISTEN_ADDR`)
//! - mandatory bearer token (`EXECUTION_HARNESS_TOKEN`), fail closed
//! - all paths canonicalized and fenced inside configured allowed roots
//! - command env cleared (`env_clear`) except PATH/HOME/TMPDIR/LANG/LC_
//! - Linux command execution fails closed unless bubblewrap is available
//! - per-command timeout (default 30s, hard cap 120s) and output caps
//! - no production secrets: the harness reads only its own `EXECUTION_HARNESS_*`
//!   variables; it never sources `runtime.env` and holds no deployment rights
//!
//! Protocol (external harness v1, as consumed by the Kernel adapter in
//! `src/adapters/external_harness.rs`):
//!
//! ```json
//! {"protocol_version":"external-harness-v1","invocation_id":"...",
//!  "operation":"external.execution_exec","arguments":{...}}
//! ```
//!
//! Response (HTTP 200):
//! ```json
//! {"protocol_version":"external-harness-v1","ok":true,"result":{...}}
//! ```
//! or on rejection:
//! ```json
//! {"protocol_version":"external-harness-v1","ok":false,"error_code":"..."}
//! ```

use execution_harness::server;
use std::net::TcpListener;
use std::process::ExitCode;

fn main() -> ExitCode {
    let addr = std::env::var("EXECUTION_HARNESS_LISTEN_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:7650".to_string());
    let token = std::env::var("EXECUTION_HARNESS_TOKEN").unwrap_or_default();
    let allowed_roots_raw = std::env::var("EXECUTION_HARNESS_ALLOWED_ROOTS").unwrap_or_default();

    if token.trim().is_empty() {
        eprintln!("FATAL: EXECUTION_HARNESS_TOKEN is required (fail closed); refusing to start");
        return ExitCode::FAILURE;
    }
    let roots: Vec<std::path::PathBuf> = allowed_roots_raw
        .split('\n')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .collect();
    let allowed_roots = match execution_harness::filesystem::AllowedRoots::new(roots) {
        Ok(roots) => roots,
        Err(error) => {
            eprintln!("FATAL: invalid EXECUTION_HARNESS_ALLOWED_ROOTS: {error}");
            return ExitCode::FAILURE;
        }
    };

    #[cfg(target_os = "linux")]
    if !execution_harness::exec::sandbox_available() {
        eprintln!("FATAL: bubblewrap is required for command execution");
        return ExitCode::FAILURE;
    }

    let config = server::Config {
        token,
        allowed_roots,
    };
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("FATAL: cannot bind {addr}: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "execution-harness listening on {addr} (allowed roots: {})",
        config
            .allowed_roots
            .as_slice()
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    server::serve(listener, config)
}
