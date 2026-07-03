//! Coding Harness — external harness for development operations.
//!
//! Usage:
//!   CODING_CONFIG='{"workspaces":{"p":{"root":"/abs/path","read":true,"write":true,"exec":true,"zcode":true}}}' \
//!     cargo run --example coding_harness -- --listen 127.0.0.1:7200
//!
//! Protocol: external-harness-v1 (HTTP JSON).
//! Operations: external.coding_workspace_list/read/write/exec,
//!             external.coding_task_submit/status,
//!             external.coding_capability_propose
//!
//! Handler logic is imported from `agent_core_kernel::harness::coding`.

use std::net::TcpListener;
use std::sync::Arc;

mod protocol;
mod server;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let addr = args
        .iter()
        .position(|a| a == "--listen")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "127.0.0.1:7200".to_string());

    let config = Arc::new(agent_core_kernel::harness::coding::config::CodingConfig::from_env());
    let listener = TcpListener::bind(&addr).expect("failed to bind");
    eprintln!("coding_harness listening on {addr}");

    let ws_count = config.workspaces.len();
    eprintln!("coding_harness loaded {ws_count} workspace(s)");

    server::serve(listener, config);
}
