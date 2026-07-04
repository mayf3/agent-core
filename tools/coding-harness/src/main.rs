use std::net::TcpListener;
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let addr = args
        .iter()
        .position(|a| a == "--listen")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "127.0.0.1:7200".to_string());

    let config = Arc::new(coding_harness::config::CodingConfig::from_env());
    let listener = TcpListener::bind(&addr).expect("failed to bind");
    eprintln!("coding_harness listening on {addr}");

    let ws_count = config.workspaces.len();
    eprintln!("coding_harness loaded {ws_count} workspace(s)");
    if config.capability_submit_token.is_empty() {
        eprintln!("warning: CAPABILITY_SUBMIT_TOKEN not set; proposal submission will fail");
    }

    coding_harness::server::serve(listener, config);
}
