//! execution-harness — minimal generic execution primitive for persistent
//! agents, exposed through the Kernel's existing external-capability
//! protocol (`external.coding_workspace_*` namespace, see
//! `src/domain/coding_operations.rs`).
//!
//! It is a **new, independent harness** — not the Coding Harness. It offers
//! only generic workspace primitives:
//!
//! - `external.coding_workspace_list` — list files in an isolated workspace
//! - `external.coding_workspace_read` — read a file (path-fenced)
//! - `external.coding_workspace_write` — write a file (path-fenced)
//! - `external.coding_workspace_exec` — run a command in the workspace with
//!   timeout + output caps (shell/git/build/test/local process/HTTP probe)
//!
//! Security model:
//! - loopback-only listener (`EXECUTION_HARNESS_LISTEN_ADDR`)
//! - mandatory bearer token (`EXECUTION_HARNESS_TOKEN`), fail closed
//! - all paths canonicalized and fenced inside the workspace root
//! - command env cleared (`env_clear`) except PATH/HOME/TMPDIR/LANG/LC_
//! - per-command timeout (default 30s, hard cap 120s) and output caps
//! - no production secrets: the harness reads only its own `EXECUTION_HARNESS_*`
//!   variables; it never sources `runtime.env` and holds no deployment rights
//!
//! Protocol (external harness v1, as consumed by the Kernel adapter in
//! `src/adapters/external_harness.rs`):
//!
//! ```json
//! {"protocol_version":"external-harness-v1","invocation_id":"...",
//!  "operation":"external.coding_workspace_exec","arguments":{...}}
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

mod exec;
mod server;
mod workspace;

use std::net::TcpListener;
use std::process::ExitCode;

fn main() -> ExitCode {
    let addr = std::env::var("EXECUTION_HARNESS_LISTEN_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:7650".to_string());
    let token = std::env::var("EXECUTION_HARNESS_TOKEN").unwrap_or_default();
    let workspace_root = std::env::var("EXECUTION_HARNESS_WORKSPACE_ROOT")
        .unwrap_or_else(|_| ".".to_string());

    if token.trim().is_empty() {
        eprintln!(
            "FATAL: EXECUTION_HARNESS_TOKEN is required (fail closed); refusing to start"
        );
        return ExitCode::FAILURE;
    }
    let root = std::path::PathBuf::from(&workspace_root);
    if std::fs::create_dir_all(&root).is_err() {
        eprintln!("FATAL: cannot create workspace root {workspace_root}");
        return ExitCode::FAILURE;
    }
    let canonical_root = root.canonicalize().unwrap_or(root);

    let config = server::Config {
        token,
        workspace_root: canonical_root,
    };
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("FATAL: cannot bind {addr}: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "execution-harness listening on {addr} (workspace root: {})",
        config.workspace_root.display()
    );
    server::serve(listener, config)
}
