#!/bin/bash
set -euo pipefail

# Start the Agent Core HCR Linux VM and Coding Harness service.
#
# Prerequisites: Lima installed via `brew install lima`.
#
# Usage:
#   ./start.sh          # Start VM + Coding Harness
#   ./start.sh --build  # Also rebuild Coding Harness from source
#   ./start.sh --help   # Show this message

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$SCRIPT_DIR/lib.sh"

VM_NAME="${HCR_VM_NAME:-agent-core-hcr}"
GUEST_PORT="${HCR_GUEST_PORT:-7200}"
HOST_PORT="${HCR_HOST_PORT:-7200}"
WORKSPACE_ROOT="${HCR_WORKSPACE_ROOT:-/srv/agent-core-hcr/workspaces}"
ARTIFACT_ROOT="${HCR_ARTIFACT_ROOT:-/srv/agent-core-hcr/artifacts}"
HCR_TOKEN="${HCR_TOKEN:-dev-token}"

show_help() {
    sed -n '/^#/p; /^$/q' "$0" | sed 's/^# //; s/^#$//'
    exit 0
}

if [ "${1:-}" = "--help" ]; then
    show_help
fi

# Step 1: Verify Lima (native arm64, not Rosetta)
# hcr_resolve_limactl aborts with LIMA_NATIVE_ARM64_REQUIRED if only an
# x86_64 limactl is available on Apple Silicon.
LIMACTL="$(hcr_resolve_limactl)"

# Step 2: Start or ensure VM is running
echo "=== Lima VM: $VM_NAME (limactl: $LIMACTL) ==="
if "$LIMACTL" list "$VM_NAME" --format '{{.Status}}' 2>/dev/null | grep -q Running; then
    echo "VM already running."
else
    echo "Starting VM (first time may take 5-10 minutes)..."
    "$LIMACTL" start --name "$VM_NAME" "$SCRIPT_DIR/lima.yaml"
fi

# Step 3: Ensure build deps are installed
echo "=== Verifying VM dependencies ==="
"$LIMACTL" shell "$VM_NAME" -- bash -c '
    command -v bwrap &>/dev/null && echo "bwrap: OK" || echo "bwrap: MISSING"
    command -v cargo &>/dev/null && echo "cargo: OK" || echo "cargo: MISSING"
    command -v node &>/dev/null && echo "node: OK" || echo "node: MISSING"
'

# Step 4: Optionally rebuild
if [ "${1:-}" = "--build" ]; then
    echo "=== Building Coding Harness ==="
    tar czf /tmp/agent-core-build.tar.gz \
        --exclude=target --exclude=.git \
        -C "$SCRIPT_DIR/../.." \
        Cargo.toml Cargo.lock src/ migrations/ tools/
    cat /tmp/agent-core-build.tar.gz | "$LIMACTL" shell "$VM_NAME" -- bash -c '
        cd /home/yanfenma.guest
        rm -rf agent-core
        mkdir agent-core && cd agent-core && tar xzf -
        # Overwrite the root manifest with a workspace-aware one so cargo
        # can discover tools/coding-harness. Each dependency MUST be on its
        # own line — TOML does not accept `;`-separated key/value pairs.
        cat > Cargo.toml << KEOF
[package]
name = "agent-core-kernel"
version = "0.1.0"
edition = "2021"
[workspace]
members = ["tools/coding-harness"]
resolver = "2"
[features]
default = []
test-helpers = []
[dependencies]
anyhow = "1"
chrono = { version = "0.4", features = ["serde"] }
ctrlc = { version = "3.5.2", features = ["termination"] }
hex = "0.4"
libc = "0.2"
rusqlite = { version = "0.32", features = ["bundled"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"
thiserror = "1"
ureq = { version = "3.3.0", features = ["json"] }
uuid = { version = "1", features = ["v4"] }
[dev-dependencies]
agent-core-kernel = { path = ".", features = ["test-helpers"] }
KEOF
    '
    "$LIMACTL" shell "$VM_NAME" -- bash -c "
        . \"\$HOME/.cargo/env\"
        cd /home/yanfenma.guest/agent-core
        cargo build --manifest-path tools/coding-harness/Cargo.toml
    "
fi

# Step 5: Start Coding Harness server
echo "=== Starting Coding Harness on port $GUEST_PORT ==="
"$LIMACTL" shell "$VM_NAME" -- bash -c "
    . \"\$HOME/.cargo/env\"
    cd /home/yanfenma.guest/agent-core/tools/coding-harness

    # Create runtime dirs (owned by the current user; no sudo needed and
    # sudo would hang on a password prompt in non-interactive launches).
    mkdir -p $WORKSPACE_ROOT $ARTIFACT_ROOT $WORKSPACE_ROOT/default

    # Set env vars
    export HARNESS_WORKSPACE_ROOT=$WORKSPACE_ROOT
    export HARNESS_ARTIFACT_ROOT=$ARTIFACT_ROOT
    # CODING_CONFIG is the single source of harness config: it registers the
    # \"default\" workspace (with exec permission, required for hcr_exec) AND
    # embeds the HCR profiles. config.rs reads both workspaces and
    # hcr_profiles from this JSON; the separate HCR_PROFILES env vars are
    # NOT read by the harness.
    export CODING_CONFIG='{\"workspaces\":{\"default\":{\"root\":\"$WORKSPACE_ROOT/default\",\"read\":true,\"write\":true,\"exec\":true}},\"hcr_profiles\":{\"hcr-v0\":{\"id\":\"hcr-v0\",\"workspace_id\":\"default\",\"allowed_commands\":[{\"name\":\"node_test\",\"program\":\"/usr/bin/env\",\"args\":[{\"Fixed\":\"node\"},{\"Fixed\":\"--test\"},{\"Param\":\"test_path\"}],\"network\":\"deny\",\"timeout_ms\":60000},{\"name\":\"read_file\",\"program\":\"/usr/bin/cat\",\"args\":[{\"Param\":\"path\"}],\"network\":\"deny\",\"timeout_ms\":5000}],\"env_allowlist\":[\"PATH\",\"HOME\",\"TMPDIR\"],\"network_policy\":\"deny\",\"timeout_ms_max\":120000,\"output_bytes_max\":1048576}}}'
    export HCR_TOKEN=$HCR_TOKEN

    echo \"Coding Harness starting...\"
    cargo run --bin coding-harness -- --port $GUEST_PORT &
    CH_PID=\$!
    echo \$CH_PID > /tmp/coding-harness.pid
    echo \"PID: \$CH_PID\"
    sleep 3
    # The harness speaks the external-harness-v1 JSON protocol on every
    # request (there is no bare GET /health). Probe with a minimal
    # workspace.list call to confirm it is up.
    curl -sf -X POST -H 'Content-Type: application/json' \
        -d '{\"protocol_version\":\"external-harness-v1\",\"operation\":\"external.coding_workspace_list\",\"arguments\":{\"workspace_id\":\"default\"}}' \
        http://127.0.0.1:$GUEST_PORT/execute >/dev/null 2>&1 && echo \"Health Ok\" || echo \"Health check failed\"
" &

echo ""
echo "=== Port forwarding ==="
echo "Mac:  http://127.0.0.1:$HOST_PORT"
echo "Guest: http://127.0.0.1:$GUEST_PORT"
echo ""
echo "VM can be accessed with: $LIMACTL shell $VM_NAME"
echo "Stop with: $LIMACTL stop $VM_NAME"
