//! Coding Workspace Harness — standalone external harness for authorized
//! file-system, git, and command-exec development operations.
//!
//! Usage:
//!   WORKSPACE_CONFIG='{"workspaces":{"my-project":"/abs/path"}}' \
//!     cargo run --example workspace_harness -- --listen 127.0.0.1:7102
//!
//! This is an independent process fixture. It does NOT call any Kernel
//! internal API, read .env files, or access any database.
//!
//! Protocol: external-harness-v1 (HTTP JSON).
//! Operations: external.workspace_list/read/write/mkdir/stat/exec.

mod config;
mod exec;
mod fs_ops;
mod paths;
mod protocol;
mod server;

use std::net::TcpListener;
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let listen_addr = if let Some(idx) = args.iter().position(|a| a == "--listen") {
        args.get(idx + 1)
            .cloned()
            .unwrap_or_else(|| "127.0.0.1:7102".to_string())
    } else {
        "127.0.0.1:7102".to_string()
    };

    let config = Arc::new(config::WorkspaceConfig::from_env());

    let listener = TcpListener::bind(&listen_addr).expect("failed to bind");
    eprintln!("workspace_harness listening on {listen_addr}");

    let ws_count = config.workspaces.len();
    let env_count = config.exec_env_pass.len();
    eprintln!("workspace_harness loaded {ws_count} workspace(s), {env_count} env-pass var(s)");

    server::serve(listener, config);
}
