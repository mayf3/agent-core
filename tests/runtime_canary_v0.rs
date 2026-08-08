//! Runtime V0 boundary canary.
//!
//! Proves that a new minimal Agent loop can complete a REAL external call
//! through the current V1 Kernel using only a narrow invocation port
//! (`submit(invocation_id, capability_ref, args)`), without knowing Run
//! lifecycle, grants, registry snapshot, approval, or any product
//! semantics.
//!
//! IMPORTANT: the loop runs on a Canary-only compatibility adapter that
//! mechanically translates to the current V1 Kernel. This proves the
//! Runtime can be isolated from V1 governance — it does NOT mean the
//! V2.1 Kernel boundary is implemented. Deleting this test together with
//! `canary_runtime.rs` and `legacy_adapter.rs` is the complete rollback.
//!
//! The Provider is the REAL execution harness (`tools/execution-harness`),
//! spawned on a test port with a test token; the whole external dispatch
//! chain (manifest -> HTTP -> real `echo` execution -> receipt) is real.

#[path = "runtime_canary_v0/canary_runtime.rs"]
mod canary_runtime;
#[path = "runtime_canary_v0/legacy_adapter.rs"]
mod legacy_adapter;

use serde_json::json;
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const HARNESS_BIN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tools/execution-harness/target/release/execution-harness"
);
const HARNESS_PORT: u16 = 27654;
const HARNESS_TOKEN: &str = "canary-test-token-0123456789abcdef";

/// Deterministic two-round model: round 0 takes one action through C17,
/// round 1 reads the real result from the follow-up and replies.
struct FakeCanaryModel {
    round: u32,
}

impl canary_runtime::CanaryModel for FakeCanaryModel {
    fn complete(&mut self, turn: canary_runtime::CanaryTurn) -> canary_runtime::CanaryModelOutput {
        if self.round == 0 {
            self.round += 1;
            canary_runtime::CanaryModelOutput {
                text: String::new(),
                action: Some(canary_runtime::CanaryAction {
                    tool: canary_runtime::C17.into(),
                    arguments: json!({
                        "workspace_id": "canary",
                        "command": "echo",
                        "args": ["canary-runtime-v0"],
                        "timeout_seconds": 10,
                    }),
                }),
            }
        } else {
            let seen = turn
                .follow_ups
                .first()
                .map(|f| f.text.clone())
                .unwrap_or_default();
            canary_runtime::CanaryModelOutput {
                text: format!("final reply: {seen}"),
                action: None,
            }
        }
    }
}

/// Spawn the real execution harness on a test port (real Provider).
/// The binary path can be overridden with `EXECUTION_HARNESS_BIN`; the
/// default is the in-repo release build (built after PR #230 lands).
fn spawn_provider() -> (Child, std::path::PathBuf) {
    let bin = std::env::var("EXECUTION_HARNESS_BIN").unwrap_or_else(|_| HARNESS_BIN.to_string());
    assert!(
        std::path::Path::new(&bin).exists(),
        "execution harness binary not found at {bin}; build it with \
         cargo build --release --manifest-path tools/execution-harness/Cargo.toml \
         or point EXECUTION_HARNESS_BIN at a prebuilt binary"
    );
    let root = std::env::temp_dir().join(format!("canary-ws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let child = Command::new(&bin)
        .env("EXECUTION_HARNESS_LISTEN_ADDR", format!("127.0.0.1:{HARNESS_PORT}"))
        .env("EXECUTION_HARNESS_TOKEN", HARNESS_TOKEN)
        .env("EXECUTION_HARNESS_WORKSPACE_ROOT", &root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn execution harness");
    for _ in 0..50 {
        if TcpStream::connect(("127.0.0.1", HARNESS_PORT)).is_ok() {
            return (child, root);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("execution harness did not start");
}

#[test]
fn canary_runtime_completes_real_invocation() {
    let (mut provider, root) = spawn_provider();
    let adapter = legacy_adapter::CanaryBindingAdapter::new(
        format!("http://127.0.0.1:{HARNESS_PORT}/execute"),
        HARNESS_TOKEN.into(),
    );
    let model = FakeCanaryModel { round: 0 };
    let mut runtime = canary_runtime::CanaryRuntimeLoop::new(adapter, model);
    let reply = runtime.run("run a harmless command through capability C17").expect("loop");
    eprintln!("CANARY FINAL REPLY: {reply}");

    // Cleanup regardless of assertion outcome.
    provider.kill().ok();
    let _ = provider.wait();
    let _ = std::fs::remove_dir_all(&root);

    assert!(
        reply.starts_with("final reply:"),
        "round 2 must produce the final reply, got: {reply}"
    );
    assert!(
        reply.contains("canary-runtime-v0"),
        "final reply must contain the REAL provider output, got: {reply}"
    );
}
