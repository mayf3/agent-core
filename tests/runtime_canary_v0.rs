//! Runtime V0 boundary canary.
//!
//! Proves that the PRODUCTION `runtime_v0` module (not a test-only loop
//! copy) can complete a REAL external call through the current V1 Kernel
//! using only a narrow invocation port
//! (`submit(invocation_id, capability_ref, args)`), without knowing Run
//! lifecycle, grants, registry snapshot, approval, or any product
//! semantics.
//!
//! The host side of this canary: the integration test instantiates
//! [`agent_core_kernel::runtime_v0::RuntimeLoop`] with a deterministic
//! fake model and the existing Invocation compatibility path
//! (`legacy_adapter.rs`, which mechanically translates the narrow port to
//! the shared external-harness execution mechanism). The loop itself knows
//! nothing about V1 governance — it only sees the model, the port, and the
//! tool view the host supplies.
//!
//! IMPORTANT: the compatibility adapter is Canary-only. This proves the
//! Runtime can be isolated from V1 governance — it does NOT mean the V2.1
//! Kernel boundary is implemented. Deleting `src/runtime_v0/` together
//! with this test and `legacy_adapter.rs` is the complete rollback.
//!
//! The Provider is the REAL execution harness (`tools/execution-harness`),
//! spawned on a test port with a test token; the whole external dispatch
//! chain (manifest -> HTTP -> real `echo` execution -> receipt) is real.

#[path = "runtime_canary_v0/legacy_adapter.rs"]
mod legacy_adapter;

use agent_core_kernel::runtime_v0::{Model, ModelOutput, RuntimeLoop, Tool, Turn};
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

/// The test capability reference this canary offers (host-side contract).
const C17: &str = "C17";

/// The tool view the host offers for C17.
fn c17_tool() -> Tool {
    Tool {
        name: C17.into(),
        description: "run a harmless command inside an isolated workspace".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "workspace_id": {"type": "string"},
                "command": {"type": "string"},
                "args": {"type": "array", "items": {"type": "string"}},
                "timeout_seconds": {"type": "integer"}
            },
            "required": ["workspace_id", "command"]
        }),
    }
}

/// Deterministic two-round model: round 0 takes one action through C17,
/// round 1 reads the real result from the follow-up and replies.
struct FakeCanaryModel {
    round: u32,
}

impl Model for FakeCanaryModel {
    fn complete(&mut self, turn: Turn) -> ModelOutput {
        if self.round == 0 {
            self.round += 1;
            ModelOutput {
                text: String::new(),
                action: Some(agent_core_kernel::runtime_v0::Action {
                    tool: C17.into(),
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
            ModelOutput {
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
    let mut runtime = RuntimeLoop::new(adapter, model);
    let reply = runtime
        .run("run a harmless command through capability C17", &[c17_tool()])
        .expect("loop");
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
