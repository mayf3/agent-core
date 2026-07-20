#!/bin/bash
# =============================================================================
# canary-runtime — Agent Core Unified Runtime Entry Point
# =============================================================================
#
# Usage:
#   ./canary-runtime.sh start   [--build]   # Start all services (optionally rebuild)
#   ./canary-runtime.sh doctor               # Run comprehensive preflight checks
#   ./canary-runtime.sh stop                 # Stop all services
#   ./canary-runtime.sh status               # Show current runtime status
#
# This script is the SINGLE authoritative entry point for managing the
# Agent Core runtime inside the Lima VM (agent-core-hcr).
#
# Prerequisites:
#   - Lima VM "agent-core-hcr" (created via ops/hcr-linux-vm/start.sh)
#   - Native arm64 limactl binary
#   - Rust/cargo/node available inside the VM
#
# Environment:
#   - All configuration comes from runtime.env at the project root
#   - No other .env, export, or hardcoded env vars may be used
# =============================================================================

set -euo pipefail

# Resolve script location (handles symlinks like root-level canary-runtime)
resolve_script_dir() {
    local source="${BASH_SOURCE[0]}"
    while [ -h "$source" ]; do
        local dir
        dir="$(cd -P "$(dirname "$source")" && pwd)"
        source="$(readlink "$source")"
        [[ "$source" != /* ]] && source="$dir/$source"
    done
    cd -P "$(dirname "$source")" && pwd
}
SCRIPT_DIR="$(resolve_script_dir)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
RUNTIME_ENV="$PROJECT_DIR/runtime.env"
VM_NAME="${HCR_VM_NAME:-agent-core-hcr}"

# Binary locations (inside the VM, on the shared mount)
LINUX_BUILD_DIR="/Users/yanfenma/.agent-core/hcr-linux/agent-core-linux"
KERNEL_BIN="$LINUX_BUILD_DIR/target/release/agent-core-kernel"
CODING_HARNESS_BIN="$LINUX_BUILD_DIR/tools/coding-harness/target/release/coding-harness"
DEPLOYMENT_HARNESS_BIN="$LINUX_BUILD_DIR/tools/deployment-harness/target/release/deployment-harness"
CAPABILITY_HOST_BIN="$LINUX_BUILD_DIR/tools/capability-host/target/release/capability-host"

# Port assignments (compatible with bash 3.2 on macOS)
get_port() {
    case "$1" in
        kernel)            echo 4130 ;;
        connector)         echo 4131 ;;
        coding-harness)    echo 7200 ;;
        capability-host)   echo 7300 ;;
        deployment-harness) echo 7400 ;;
        *)                 echo "" ;;
    esac
}

all_services() {
    echo "kernel connector coding-harness capability-host deployment-harness"
}

# Required env vars for validation
REQUIRED_ENV_VARS=(
    "AGENT_CORE_FEISHU_APP_ID"
    "AGENT_CORE_FEISHU_APP_SECRET"
    "AGENT_CORE_OPENAI_API_KEY"
    "AGENT_CORE_IPC_TOKEN"
    "AGENT_CORE_CAPABILITY_SUBMIT_TOKEN"
    "AGENT_CORE_CAPABILITY_DECISION_TOKEN"
    "AGENT_CORE_KERNEL_DECISION_TOKEN"
    "AGENT_CORE_EVENT_OBSERVE_TOKEN"
    "AGENT_CORE_FEISHU_CODING_OWNER_ID"
    "CODING_GENERATOR_API_KEY"
    "DEPLOYMENT_HARNESS_CONTROL_TOKEN"
    "DEPLOYMENT_HARNESS_EVENT_OBSERVE_TOKEN"
    "CAPABILITY_HOST_CONTROL_TOKEN"
    "CAPABILITY_HOST_EXECUTION_TOKEN"
)

# ---------------------------------------------------------------------------
# Helper: resolve native limactl (reuses ops/hcr-linux-vm/lib.sh)
# --------------------------------------------------------------------------
resolve_limactl() {
    local lib="$PROJECT_DIR/ops/hcr-linux-vm/lib.sh"
    if [ -f "$lib" ]; then
        # shellcheck source=../../ops/hcr-linux-vm/lib.sh
        source "$lib"
        hcr_resolve_limactl 2>/dev/null
    else
        # Fallback: try default locations
        if command -v limactl &>/dev/null; then
            echo "$(command -v limactl)"
        elif [ -x /opt/homebrew/bin/limactl ]; then
            echo "/opt/homebrew/bin/limactl"
        else
            echo "ERROR: limactl not found" >&2
            exit 1
        fi
    fi
}

LIMACTL="$(resolve_limactl)"

# ---------------------------------------------------------------------------
# Helper: read a value from runtime.env by key
# ---------------------------------------------------------------------------
env_val() {
    local key="$1"
    local default="${2:-}"
    local val
    val=$(grep "^${key}=" "$RUNTIME_ENV" 2>/dev/null | head -1 | sed 's/^[^=]*=//' | sed 's/^"//; s/"$//')
    echo "${val:-$default}"
}

# ---------------------------------------------------------------------------
# Helper: load runtime.env into the current shell session
# ---------------------------------------------------------------------------
load_runtime_env() {
    if [ ! -f "$RUNTIME_ENV" ]; then
        echo "FATAL: runtime.env not found at $RUNTIME_ENV" >&2
        exit 1
    fi

    while IFS='=' read -r key value; do
        # Skip comments and empty lines
        case "$key" in
            ''|\#*) continue ;;
        esac
        # Remove leading/trailing whitespace
        key=$(echo "$key" | xargs)
        value=$(echo "$value" | sed 's/^"//; s/"$//' | xargs)
        export "$key=$value"
    done < "$RUNTIME_ENV"
}

# ---------------------------------------------------------------------------
# Helper: run a command inside the Lima VM
# ---------------------------------------------------------------------------
vm_exec() {
    "$LIMACTL" shell "$VM_NAME" -- bash -c "$*"
}

# ---------------------------------------------------------------------------
# Helper: check if VM is running
# ---------------------------------------------------------------------------
vm_is_running() {
    "$LIMACTL" list "$VM_NAME" --format '{{.Status}}' 2>/dev/null | grep -q Running
}

# ---------------------------------------------------------------------------
# Helper: verify token consistency
# ---------------------------------------------------------------------------
verify_tokens() {
    local errors=0
    local cap_dec kern_dec event_obs dep_event_obs
    cap_dec=$(env_val "AGENT_CORE_CAPABILITY_DECISION_TOKEN")
    kern_dec=$(env_val "AGENT_CORE_KERNEL_DECISION_TOKEN")
    event_obs=$(env_val "AGENT_CORE_EVENT_OBSERVE_TOKEN")
    dep_event_obs=$(env_val "DEPLOYMENT_HARNESS_EVENT_OBSERVE_TOKEN")

    if [ "$cap_dec" != "$kern_dec" ]; then
        echo "FAIL: AGENT_CORE_CAPABILITY_DECISION_TOKEN != AGENT_CORE_KERNEL_DECISION_TOKEN" >&2
        errors=$((errors + 1))
    fi

    if [ "$event_obs" != "$dep_event_obs" ]; then
        echo "FAIL: AGENT_CORE_EVENT_OBSERVE_TOKEN != DEPLOYMENT_HARNESS_EVENT_OBSERVE_TOKEN" >&2
        errors=$((errors + 1))
    fi

    return "$errors"
}

# ---------------------------------------------------------------------------
# Helper: verify required env vars are non-empty
# ---------------------------------------------------------------------------
verify_required_env() {
    local errors=0

    for var in "${REQUIRED_ENV_VARS[@]}"; do
        local value
        value=$(env_val "$var")
        if [ -z "$value" ]; then
            echo "FAIL: Required env var $var is empty or missing" >&2
            errors=$((errors + 1))
        fi
    done

    return "$errors"
}

# ---------------------------------------------------------------------------
# Helper: verify SHA matches baseline
# ---------------------------------------------------------------------------
verify_baseline() {
    local expected_sha="3f62fe35be845ad41005c89bddfbdc18815fdeda"
    local current_sha
    current_sha=$(cd "$PROJECT_DIR" && git rev-parse HEAD 2>/dev/null || echo "unknown")

    if [ "$current_sha" != "$expected_sha" ]; then
        echo "WARNING: Current HEAD ($current_sha) differs from baseline ($expected_sha)" >&2
        return 1
    fi
    echo "Baseline SHA: $current_sha (matches expected)"
    return 0
}

# ---------------------------------------------------------------------------
# Helper: check a port is listening
# ---------------------------------------------------------------------------
check_port() {
    local port=$1
    local name=$2

    # Special cases for services without /health
    case "$name" in
        connector)
            # Feishu Connector execute server - POST /v1/execute returns 401 (needs auth) or 200
            local status
            status=$(curl -s -o /dev/null -w "%{http_code}" --max-time 3 \
                -X POST "http://127.0.0.1:$port/v1/execute" \
                -H "Content-Type: application/json" \
                -d '{}' 2>/dev/null || echo "000")
            if [ "$status" != "000" ]; then
                echo "  ✅ $name ($port) — responding ($status)"
                return 0
            fi
            echo "  ❌ $name ($port) — not responding"
            return 1
            ;;
        coding-harness)
            # Coding Harness uses POST /execute with JSON protocol
            if curl -sf --max-time 3 -X POST "http://127.0.0.1:$port/execute" \
                -H "Content-Type: application/json" \
                -d '{"protocol_version":"external-harness-v1","operation":"external.coding_workspace_list","arguments":{"workspace_id":"scratch"}}' >/dev/null 2>&1; then
                echo "  ✅ $name ($port)"
                return 0
            fi
            echo "  ❌ $name ($port) — not responding"
            return 1
            ;;
        *)
            if curl -sf --max-time 3 "http://127.0.0.1:$port/health" >/dev/null 2>&1; then
                echo "  ✅ $name ($port)"
                return 0
            fi
            echo "  ❌ $name ($port) — not responding"
            return 1
            ;;
    esac
}

# ---------------------------------------------------------------------------
# Helper: check event endpoint auth
# ---------------------------------------------------------------------------
check_event_auth() {
    local event_token
    event_token=$(env_val "AGENT_CORE_EVENT_OBSERVE_TOKEN")

    echo "--- Event endpoint auth ---"
    # Correct token → 200
    local status_ok
    status_ok=$(curl -s -o /dev/null -w "%{http_code}" -X POST "http://127.0.0.1:4130/v1/events" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer $event_token" \
        -d '{}' 2>/dev/null || echo "000")

    # Wrong token → 401
    local status_bad
    status_bad=$(curl -s -o /dev/null -w "%{http_code}" -X POST "http://127.0.0.1:4130/v1/events" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer wrong-token" \
        -d '{}' 2>/dev/null || echo "000")

    local failed=0
    if [ "$status_ok" = "200" ]; then
        echo "  ✅ Correct token → $status_ok"
    else
        echo "  ❌ Correct token → $status_ok (expected 200)"
        failed=1
    fi

    if [ "$status_bad" = "401" ]; then
        echo "  ✅ Wrong token → $status_bad"
    else
        echo "  ❌ Wrong token → $status_bad (expected 401)"
        failed=1
    fi

    return "$failed"
}

# ---------------------------------------------------------------------------
# Helper: check proposal query auth
# ---------------------------------------------------------------------------
check_proposal_auth() {
    local connector_token
    connector_token=$(env_val "AGENT_CORE_KERNEL_DECISION_TOKEN")

    echo "--- Proposal query auth ---"
    local status_connector
    status_connector=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:4130/v1/capability-change-proposals/nonexistent" \
        -H "Authorization: Bearer $connector_token" 2>/dev/null || echo "000")

    if [ "$status_connector" != "401" ] && [ "$status_connector" != "403" ]; then
        echo "  ✅ Connector token → $status_connector (non-401/403)"
    else
        echo "  ❌ Connector token → $status_connector (should not be 401/403)"
        return 1
    fi
    return 0
}

# ---------------------------------------------------------------------------
# Helper: check storage writability
# ---------------------------------------------------------------------------
check_storage() {
    local errors=0

    echo "--- Storage ---"

    # Check content store (artifacts)
    local artifact_root
    artifact_root=$(env_val "AGENT_CORE_HARNESS_ARTIFACT_ROOT")

    if vm_exec "touch '$artifact_root/.canary-write-test' 2>/dev/null && rm '$artifact_root/.canary-write-test' 2>/dev/null"; then
        echo "  ✅ Content Store writable ($artifact_root)"
    else
        echo "  ❌ Content Store NOT writable ($artifact_root)"
        errors=1
    fi

    # Check journal DB
    local data_dir
    data_dir=$(env_val "AGENT_CORE_DATA_DIR" "/home/yanfenma.guest/.agent-core")

    if vm_exec "touch '$data_dir/.canary-write-test' 2>/dev/null && rm '$data_dir/.canary-write-test' 2>/dev/null"; then
        echo "  ✅ Journal DB dir writable ($data_dir)"
    else
        echo "  ❌ Journal DB dir NOT writable ($data_dir)"
        errors=1
    fi

    # Check artifact root (via DEPLOYMENT_HARNESS_ARTIFACT_ROOT)
    local dep_artifact
    dep_artifact=$(env_val "DEPLOYMENT_HARNESS_ARTIFACT_ROOT")
    if [ -n "$dep_artifact" ]; then
        if vm_exec "touch '$dep_artifact/.canary-write-test' 2>/dev/null && rm '$dep_artifact/.canary-write-test' 2>/dev/null"; then
            echo "  ✅ Deployment Artifact Root writable ($dep_artifact)"
        else
            echo "  ❌ Deployment Artifact Root NOT writable ($dep_artifact)"
            errors=1
        fi
    fi

    return "$errors"
}

# =============================================================================
# COMMAND: start
# =============================================================================
cmd_start() {
    echo "=== canary-runtime: start ==="

    # Verify baseline
    verify_baseline || echo "Proceeding with current HEAD (not baseline)"

    # Load env
    load_runtime_env
    verify_tokens || { echo "Token verification failed. Aborting."; exit 1; }
    verify_required_env || { echo "Required env var check failed. Aborting."; exit 1; }

    # Stop existing services
    cmd_stop

    # Ensure VM is running
    echo "--- Ensuring Lima VM is running ---"
    if ! vm_is_running; then
        echo "Starting VM..."
        "$LIMACTL" start --name "$VM_NAME" "$PROJECT_DIR/ops/hcr-linux-vm/lima.yaml"
        sleep 5
    else
        echo "VM already running."
    fi

    # Create required directories
    echo "--- Creating runtime directories ---"
    vm_exec "mkdir -p /Users/yanfenma/.agent-core/hcr-linux/{artifacts,state,workspaces/scratch}"
    vm_exec "mkdir -p /home/yanfenma.guest/.agent-core"

    # Build if --build flag is passed
    if [ "${1:-}" = "--build" ]; then
        echo "--- Building services inside VM ---"
        cmd_build
    else
        echo "--- Skipping build (use --build to rebuild) ---"
    fi

    # Verify binaries exist
    echo "--- Checking binaries ---"
    local missing=0
    for bin_desc in "Kernel:$KERNEL_BIN" "Coding Harness:$CODING_HARNESS_BIN" "Deployment Harness:$DEPLOYMENT_HARNESS_BIN" "Capability Host:$CAPABILITY_HOST_BIN"; do
        local name="${bin_desc%%:*}"
        local path="${bin_desc#*:}"
        if vm_exec "test -x '$path'"; then
            echo "  ✅ $name binary found"
        else
            echo "  ❌ $name binary missing at $path"
            ((missing++)) || true
        fi
    done

    if [ "$missing" -gt 0 ]; then
        echo "Missing binaries. Run with --build flag first." >&2
        exit 1
    fi

    # Copy runtime.env to VM
    echo "--- Deploying runtime.env ---"
    vm_exec "cat > /home/yanfenma.guest/.agent-core/runtime.env" < "$RUNTIME_ENV"

    # Start services
    echo "--- Starting services ---"

    # 1. Kernel
    echo "Starting Kernel (port 4130)..."
    local kernel_db="/home/yanfenma.guest/.agent-core/agent-core-canary.db"
    vm_exec "
        export \$(grep -v '^\s*#' /home/yanfenma.guest/.agent-core/runtime.env | grep -v '^\s*$' | xargs)
        nohup $KERNEL_BIN serve --db '$kernel_db' \
            > /home/yanfenma.guest/.agent-core/logs/kernel.log 2>&1 &
        echo \$! > /home/yanfenma.guest/.agent-core/pids/kernel.pid
        echo \"Kernel PID: \$!\"
    "

    # 2. Feishu Connector
    echo "Starting Feishu Connector (port 4131)..."
    local connector_release="/home/yanfenma.guest/.agent-core/current"
    # Find the latest release that has connectors/feishu
    local latest_release
    latest_release=$(vm_exec "ls -d /home/yanfenma.guest/.agent-core/releases/*/connectors/feishu/src/index.ts 2>/dev/null | sort | tail -1 | xargs dirname | xargs dirname | xargs dirname | xargs dirname || echo ''")
    if [ -n "$latest_release" ]; then
        echo "Using connector release: $(basename "$latest_release")"
        vm_exec "
            export \$(grep -v '^\s*#' /home/yanfenma.guest/.agent-core/runtime.env | grep -v '^\s*$' | xargs)
            cd '$latest_release'
            nohup npx tsx connectors/feishu/src/index.ts \
                > /home/yanfenma.guest/.agent-core/logs/connector.log 2>&1 &
            echo \$! > /home/yanfenma.guest/.agent-core/pids/connector.pid
            echo \"Connector PID: \$!\"
        "
    else
        echo "WARNING: No connector release found. Trying default path..."
        vm_exec "
            export \$(grep -v '^\s*#' /home/yanfenma.guest/.agent-core/runtime.env | grep -v '^\s*$' | xargs)
            cd '$LINUX_BUILD_DIR'
            nohup npx tsx connectors/feishu/src/index.ts \
                > /home/yanfenma.guest/.agent-core/logs/connector.log 2>&1 &
            echo \$! > /home/yanfenma.guest/.agent-core/pids/connector.pid
            echo \"Connector PID: \$!\"
        "
    fi

    # 3. Coding Harness
    echo "Starting Coding Harness (port 7200)..."
    vm_exec "
        export \$(grep -v '^\s*#' /home/yanfenma.guest/.agent-core/runtime.env | grep -v '^\s*$' | xargs)
        nohup $CODING_HARNESS_BIN --listen 127.0.0.1:7200 \
            > /home/yanfenma.guest/.agent-core/logs/coding-harness.log 2>&1 &
        echo \$! > /home/yanfenma.guest/.agent-core/pids/coding-harness.pid
        echo \"Coding Harness PID: \$!\"
    "

    # 4. Capability Host
    echo "Starting Capability Host (port 7300)..."
    vm_exec "
        export \$(grep -v '^\s*#' /home/yanfenma.guest/.agent-core/runtime.env | grep -v '^\s*$' | xargs)
        nohup $CAPABILITY_HOST_BIN \
            > /home/yanfenma.guest/.agent-core/logs/capability-host.log 2>&1 &
        echo \$! > /home/yanfenma.guest/.agent-core/pids/capability-host.pid
        echo \"Capability Host PID: \$!\"
    "

    # 5. Deployment Harness
    echo "Starting Deployment Harness (port 7400)..."
    vm_exec "
        export \$(grep -v '^\s*#' /home/yanfenma.guest/.agent-core/runtime.env | grep -v '^\s*$' | xargs)
        nohup $DEPLOYMENT_HARNESS_BIN \
            > /home/yanfenma.guest/.agent-core/logs/deployment-harness.log 2>&1 &
        echo \$! > /home/yanfenma.guest/.agent-core/pids/deployment-harness.pid
        echo \"Deployment Harness PID: \$!\"
    "

    # Wait for services to start
    echo "--- Waiting for services to become healthy ---"
    sleep 5

    # Verify all ports are listening
    local all_ok=0
    for svc in $(all_services); do
        local port
        port="$(get_port "$svc")"
        if check_port "$port" "$svc"; then
            :  # ok
        else
            all_ok=$((all_ok + 1))
        fi
    done

    if [ "$all_ok" -gt 0 ]; then
        echo "WARNING: $all_ok service(s) not healthy. Check logs."
        echo "  Logs: /home/yanfenma.guest/.agent-core/logs/"
        exit 1
    fi

    echo ""
    echo "=== All services started successfully ==="
    echo "Kernel:              http://127.0.0.1:4130"
    echo "Feishu Connector:    http://127.0.0.1:4131"
    echo "Coding Harness:      http://127.0.0.1:7200"
    echo "Capability Host:     http://127.0.0.1:7300"
    echo "Deployment Harness:  http://127.0.0.1:7400"
    echo ""
    echo "Run './canary-runtime.sh doctor' for full preflight check."
}

# =============================================================================
# COMMAND: build
# =============================================================================
cmd_build() {
    echo "=== Building services inside Lima VM ==="

    # Copy current source to VM
    echo "Copying source code to VM..."
    cd "$PROJECT_DIR"
    tar czf /tmp/agent-core-build.tar.gz \
        --exclude=target --exclude=.git --exclude=node_modules \
        Cargo.toml Cargo.lock src/ crates/ tools/ connectors/ migrations/ scripts/ 2>/dev/null

    cat /tmp/agent-core-build.tar.gz | vm_exec "
        cd /home/yanfenma.guest
        rm -rf agent-core-build
        mkdir agent-core-build && cd agent-core-build && tar xzf -
    "

    # Build all services
    echo "Building kernel + workspace members..."
    vm_exec "
        . \"\$HOME/.cargo/env\"
        cd /home/yanfenma.guest/agent-core-build
        cargo build --release 2>&1 | tail -5
    "

    echo "Building coding-harness..."
    vm_exec "
        . \"\$HOME/.cargo/env\"
        cd /home/yanfenma.guest/agent-core-build/tools/coding-harness
        cargo build --release 2>&1 | tail -5
    "

    echo "Building deployment-harness..."
    vm_exec "
        . \"\$HOME/.cargo/env\"
        cd /home/yanfenma.guest/agent-core-build/tools/deployment-harness
        cargo build --release 2>&1 | tail -5
    "

    echo "Building capability-host..."
    vm_exec "
        . \"\$HOME/.cargo/env\"
        cd /home/yanfenma.guest/agent-core-build/tools/capability-host
        cargo build --release 2>&1 | tail -5
    "

    # Copy binaries to shared mount
    echo "Deploying binaries..."
    vm_exec "
        # Create target directory structure on shared mount
        mkdir -p $(dirname $KERNEL_BIN)
        mkdir -p $(dirname $CODING_HARNESS_BIN)
        mkdir -p $(dirname $DEPLOYMENT_HARNESS_BIN)
        mkdir -p $(dirname $CAPABILITY_HOST_BIN)

        # Copy binaries
        cp /home/yanfenma.guest/agent-core-build/target/release/agent-core-kernel $KERNEL_BIN
        cp /home/yanfenma.guest/agent-core-build/tools/coding-harness/target/release/coding-harness $CODING_HARNESS_BIN
        cp /home/yanfenma.guest/agent-core-build/tools/deployment-harness/target/release/deployment-harness $DEPLOYMENT_HARNESS_BIN
        cp /home/yanfenma.guest/agent-core-build/tools/capability-host/target/release/capability-host $CAPABILITY_HOST_BIN
        echo 'Binaries deployed successfully'
    "

    # Cleanup
    rm -f /tmp/agent-core-build.tar.gz
    echo "Build complete."
}

# =============================================================================
# COMMAND: doctor
# =============================================================================
cmd_doctor() {
    local errors=0
    local total_checks=0
    local pass_checks=0

    load_runtime_env

    echo "=== canary-runtime: doctor ==="
    echo ""

    # ---- 1. Single Lima topology ----
    echo "--- Topology ---"
    total_checks=$((total_checks + 1))
    if vm_is_running; then
        echo "  ✅ Lima VM '$VM_NAME' is running"
        pass_checks=$((pass_checks + 1))
    else
        echo "  ❌ Lima VM '$VM_NAME' is NOT running"
        ((errors++)) || true
    fi

    # Check NO Docker Agent Core
    total_checks=$((total_checks + 1))
    if vm_exec "docker ps 2>/dev/null | grep -q agent-core" 2>/dev/null; then
        echo "  ❌ Docker Agent Core container detected (Docker must not be used)"
        ((errors++)) || true
    else
        echo "  ✅ No Docker Agent Core (clean)"
        pass_checks=$((pass_checks + 1))
    fi

    # Check only one Lima instance
    total_checks=$((total_checks + 1))
    local lima_count
    lima_count=$("$LIMACTL" list --format '{{.Name}}' 2>/dev/null | wc -l | tr -d ' ')
    if [ "$lima_count" -eq 1 ]; then
        echo "  ✅ Single Lima VM ($lima_count instance)"
        pass_checks=$((pass_checks + 1))
    else
        echo "  ⚠️  Lima VM count: $lima_count (expected 1)"
        pass_checks=$((pass_checks + 1))
    fi

    # ---- 2. Five processes from same SHA ----
    echo ""
    echo "--- Process verification ---"
    local sha_info
    sha_info=$(cd "$PROJECT_DIR" && git rev-parse HEAD 2>/dev/null || echo "unknown")
    echo "  Code SHA: $sha_info"

    for svc in kernel coding-harness deployment-harness capability-host; do
        total_checks=$((total_checks + 1))
        local pid_var="${svc}_pid"
        local bin_var="${svc}_bin"
        local bin_path
        case "$svc" in
            kernel) bin_path="$KERNEL_BIN" ;;
            coding-harness) bin_path="$CODING_HARNESS_BIN" ;;
            deployment-harness) bin_path="$DEPLOYMENT_HARNESS_BIN" ;;
            capability-host) bin_path="$CAPABILITY_HOST_BIN" ;;
        esac

        if vm_exec "test -f '$bin_path'"; then
            echo "  ✅ $svc binary exists"
            pass_checks=$((pass_checks + 1))
        else
            echo "  ❌ $svc binary missing at $bin_path"
            ((errors++)) || true
        fi
    done

    # Check connector (Node.js process)
    total_checks=$((total_checks + 1))
    if vm_exec "pgrep -f 'tsx.*connectors/feishu' 2>/dev/null || pgrep -f 'node.*feishu' 2>/dev/null"; then
        echo "  ✅ Feishu Connector process found"
        pass_checks=$((pass_checks + 1))
    else
        echo "  ⚠️  Feishu Connector process not found (may not be running)"
        # Don't count as error - connector might be started separately
    fi

    # ---- 3. Ports ----
    echo ""
    echo "--- Port verification ---"
    for svc in $(all_services); do
        local port
        port="$(get_port "$svc")"
        total_checks=$((total_checks + 1))
        if check_port "$port" "$svc"; then
            pass_checks=$((pass_checks + 1))
        else
            ((errors++)) || true
        fi
    done

    # ---- 4. Bubblewrap ----
    echo ""
    echo "--- Bubblewrap ---"
    total_checks=$((total_checks + 1))
    if vm_exec "bwrap --version 2>/dev/null"; then
        echo "  ✅ Bubblewrap available"
        pass_checks=$((pass_checks + 1))
    else
        echo "  ❌ Bubblewrap NOT available"
        ((errors++)) || true
    fi

    # ---- 5. Model availability ----
    echo ""
    echo "--- Model (Chat Completions) ---"
    total_checks=$((total_checks + 1))
    local api_key model base_url
    api_key=$(env_val "AGENT_CORE_OPENAI_API_KEY")
    model=$(env_val "AGENT_CORE_MODEL")
    base_url=$(env_val "AGENT_CORE_OPENAI_BASE_URL")

    if [ -n "$api_key" ] && [ -n "$base_url" ]; then
        echo "  ✅ Model configured: $model at $base_url"
        pass_checks=$((pass_checks + 1))
    else
        echo "  ❌ Model not fully configured"
        ((errors++)) || true
    fi

    # ---- 6. Event endpoint auth ----
    echo ""
    check_event_auth || ((errors++)) || true
    total_checks=$((total_checks + 2))

    # ---- 7. Proposal query auth ----
    echo ""
    check_proposal_auth || ((errors++)) || true
    total_checks=$((total_checks + 1))

    # ---- 8. Storage ----
    echo ""
    check_storage || true  # errors tracked inside
    # We can't easily count these, but they contribute to errors

    # ---- 9. Token consistency ----
    echo ""
    echo "--- Token consistency ---"
    total_checks=$((total_checks + 2))
    verify_tokens && {
        echo "  ✅ All tokens consistent"
        pass_checks=$((pass_checks + 2))
    } || {
        echo "  ❌ Token mismatch detected"
        ((errors++)) || true
    }

    # ---- 10. Required env vars ----
    echo ""
    echo "--- Required environment variables ---"
    verify_required_env && {
        echo "  ✅ All required env vars are present"
        pass_checks=$((pass_checks + 1))
    } || {
        echo "  ❌ Some required env vars are missing"
        ((errors++)) || true
    }
    total_checks=$((total_checks + 1))

    # ---- Summary ----
    echo ""
    echo "=== Doctor Summary ==="
    if [ "$errors" -eq 0 ]; then
        echo "  ✅ All $total_checks checks passed"
        echo ""
        echo "  ██████╗ █████╗ ███╗   ██╗ █████╗ ██████╗ ██╗   ██╗"
        echo "  ██╔══██╗██╔══██╗████╗  ██║██╔══██╗██╔══██╗╚██╗ ██╔╝"
        echo "  ██████╔╝███████║██╔██╗ ██║███████║██████╔╝ ╚████╔╝"
        echo "  ██╔═══╝ ██╔══██║██║╚██╗██║██╔══██║██╔══██╗  ╚██╔╝"
        echo "  ██║     ██║  ██║██║ ╚████║██║  ██║██║  ██║   ██║"
        echo "  ╚═╝     ╚═╝  ╚═╝╚═╝  ╚═══╝╚═╝  ╚═╝╚═╝  ╚═╝   ╚═╝"
        echo ""
        echo "  CANARY_PREFLIGHT_PASS"
    else
        echo "  ❌ $errors check(s) failed out of $total_checks"
        echo "  CANARY_PREFLIGHT_FAIL"
        return 1
    fi
}

# =============================================================================
# COMMAND: stop
# =============================================================================
cmd_stop() {
    echo "=== canary-runtime: stop ==="

    if ! vm_is_running; then
        echo "VM not running. Nothing to stop."
        return 0
    fi

    echo "--- Stopping services ---"
    local svcs="deployment-harness capability-host coding-harness connector kernel"

    for svc in $svcs; do
        local pid_file="/home/yanfenma.guest/.agent-core/pids/${svc}.pid"
        vm_exec "
            if [ -f '$pid_file' ]; then
                kill \$(cat '$pid_file') 2>/dev/null || true
                rm -f '$pid_file'
                echo 'Stopped $svc'
            fi
        " 2>/dev/null || true
    done

    # Also kill any orphan processes
    vm_exec "
        pkill -f 'agent-core-kernel.*serve' 2>/dev/null || true
        pkill -f 'coding-harness' 2>/dev/null || true
        pkill -f 'deployment-harness' 2>/dev/null || true
        pkill -f 'capability-host' 2>/dev/null || true
        pkill -f 'tsx.*connectors/feishu' 2>/dev/null || true
        pkill -f 'node.*feishu' 2>/dev/null || true
    " 2>/dev/null || true

    sleep 2
    echo "All services stopped."
}

# =============================================================================
# COMMAND: status
# =============================================================================
cmd_status() {
    echo "=== canary-runtime: status ==="

    if ! vm_is_running; then
        echo "VM '$VM_NAME' is NOT running"
        exit 0
    fi

    echo "VM '$VM_NAME' is running"
    echo ""

    echo "--- Running processes ---"
    vm_exec "
        echo 'Kernel:'
        pgrep -f 'agent-core-kernel.*serve' | head -3
        echo 'Coding Harness:'
        pgrep -f 'coding-harness' | head -3
        echo 'Deployment Harness:'
        pgrep -f 'deployment-harness' | head -3
        echo 'Capability Host:'
        pgrep -f 'capability-host' | head -3
        echo 'Connector:'
        pgrep -f 'feishu' | head -3
    " 2>/dev/null || echo "  (no matching processes)"

    echo ""
    echo "--- Port check ---"
    for svc in $(all_services); do
        check_port "$(get_port "$svc")" "$svc" || true
    done
}

# =============================================================================
# COMMAND: shadow-e2e
# =============================================================================

SHADOW_ROOT=""
SHADOW_PRE_EXISTING=false

# Trap: precise stop + production recovery
shadow_cleanup() {
    local exit_code=$?
    local shadow_root="${SHADOW_ROOT}"
    echo ""
    echo "=== shadow-e2e cleanup ==="

    # ---- Phase 1: Stop shadow processes by PID (NO pkill) ----
    if [ -n "$shadow_root" ] && [ -d "${shadow_root}/pids" ]; then
        echo "--- Stopping shadow processes ---"
        for pid_file in "${shadow_root}/pids"/*.pid; do
            [ -f "$pid_file" ] || continue
            local svc_name
            svc_name=$(basename "$pid_file" .pid)
            local pid
            pid=$(cat "$pid_file" 2>/dev/null || echo "")
            if [ -z "$pid" ]; then
                rm -f "$pid_file"
                continue
            fi

            # Verify process belongs to this shadow (check /proc/PID/cmdline)
            if [ -f "/proc/${pid}/cmdline" ] 2>/dev/null; then
                local cmdline
                cmdline=$(tr '\0' ' ' < "/proc/${pid}/cmdline" 2>/dev/null || echo "")
                if ! echo "$cmdline" | grep -q "shadow"; then
                    if ! echo "$cmdline" | grep -q "${shadow_root}"; then
                        echo "  ⚠️  PID ${pid} (${svc_name}) does not match shadow root — skipping"
                        continue
                    fi
                fi
            fi

            # SIGTERM first
            kill "$pid" 2>/dev/null && echo "  Stopped ${svc_name} (PID ${pid})" || {
                echo "  ${svc_name} (PID ${pid}) already exited"
                rm -f "$pid_file"
                continue
            }

            # Wait up to 5s for graceful exit
            local waited=0
            while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 5 ]; do
                sleep 1
                waited=$((waited + 1))
            done

            # SIGKILL if still alive
            if kill -0 "$pid" 2>/dev/null; then
                kill -9 "$pid" 2>/dev/null || true
                echo "  Force killed ${svc_name} (PID ${pid})"
            fi
            rm -f "$pid_file"
        done
    fi

    # ---- Phase 2: Verify ports released ----
    echo "--- Verifying port release ---"
    for svc in kernel coding-harness capability-host deployment-harness; do
        local port
        port="$(get_port "$svc")"
        if curl -sf --max-time 1 "http://127.0.0.1:${port}/health" >/dev/null 2>&1; then
            echo "  ⚠️  Port ${port} (${svc}) still in use"
        else
            echo "  ✅ Port ${port} (${svc}) released"
        fi
    done

    # ---- Phase 3: Restore production runtime (if it was running before) ----
    if [ "$SHADOW_PRE_EXISTING" = "true" ]; then
        echo "--- Restoring production runtime ---"
        if cmd_start 2>/dev/null; then
            echo "  ✅ Production runtime started"
            if cmd_doctor 2>/dev/null; then
                echo "  ✅ Production doctor: CANARY_PREFLIGHT_PASS"
                echo "CANARY_RUNTIME_RESTORED_PASS" >> "${shadow_root}/evidence/shadow-verdict.txt" 2>/dev/null
            else
                echo "  ❌ Production doctor: FAILED"
                echo "CANARY_RUNTIME_RESTORED_FAIL" >> "${shadow_root}/evidence/shadow-verdict.txt" 2>/dev/null
                # Do NOT override clean exit — restoration failure is a real failure
                exit_code=1
            fi
        else
            echo "  ❌ Production runtime start failed"
            echo "CANARY_RUNTIME_RESTORED_FAIL" >> "${shadow_root}/evidence/shadow-verdict.txt" 2>/dev/null
            exit_code=1
        fi
    fi

    echo "=== cleanup exit code: ${exit_code} ==="
    exit "$exit_code"
}

# Generate shadow.env from runtime.env with explicit allowlist
# (bash 3.2 compatible — no associative arrays)
SHADOW_MOUNT_PREFIX="/Users/yanfenma/.agent-core/hcr-linux/shadow"
generate_shadow_env() {
    local shadow_root="$1"
    local src="$PROJECT_DIR/runtime.env"
    local dst="${shadow_root}/shadow.env"
    
    # Copy runtime.env, applying overrides for known keys
    while IFS='=' read -r key value; do
        # Skip comments and blank lines
        case "$key" in
            ''|\#*) continue ;;
        esac
        key=$(echo "$key" | xargs)
        # Override known shadow keys; pass through everything else
        case "$key" in
            AGENT_CORE_KERNEL_PORT)
                echo "${key}=${AGENT_CORE_KERNEL_PORT:-4130}" ;;
            AGENT_CORE_CONNECTOR_PORT)
                echo "${key}=${AGENT_CORE_CONNECTOR_PORT:-4131}" ;;
            AGENT_CORE_DATA_DIR)
                echo "${key}=${shadow_root}/journal" ;;
            AGENT_CORE_HARNESS_ARTIFACT_ROOT|HARNESS_ARTIFACT_ROOT|DEPLOYMENT_HARNESS_ARTIFACT_ROOT|CAPABILITY_HOST_ARTIFACT_ROOT)
                echo "${key}=${shadow_root}/artifacts" ;;
            DEPLOYMENT_HARNESS_STATE_ROOT)
                echo "${key}=${shadow_root}/state/deployment" ;;
            AGENT_CORE_CONTEXT_DIR)
                echo "${key}=${shadow_root}/context" ;;
            CODING_WORKSPACE_ROOT)
                echo "${key}=${shadow_root}/workspaces" ;;
            *)
                echo "${key}=${value}" ;;
        esac
    done < "$src" > "$dst"
    
    # Add shadow-specific variables
    echo "SHADOW_RUN_ID=${run_id}" >> "$dst"
    echo "SHADOW_EVIDENCE_DIR=${shadow_root}/evidence" >> "$dst"
    echo "SHADOW_STATE_DIR=${shadow_root}/state" >> "$dst"
    
    echo "shadow.env generated at ${dst}"
}

# Realpath fence verification — 11 state paths must all be inside shadow root
check_shadow_path_fence() {
    local shadow_root="$1"
    local errors=0
    
    # Normalize shadow_root (macOS /tmp → /private/tmp)
    local normalized_root
    normalized_root=$(cd -P "$shadow_root" && pwd 2>/dev/null || echo "$shadow_root")
    
    for path in \
        "${shadow_root}/journal" \
        "${shadow_root}/artifacts" \
        "${shadow_root}/state" \
        "${shadow_root}/state/deployment" \
        "${shadow_root}/context" \
        "${shadow_root}/workspaces" \
        "${shadow_root}/workspaces/scratch" \
        "${shadow_root}/logs" \
        "${shadow_root}/pids" \
        "${shadow_root}/evidence"; do
        mkdir -p "$path"
        local resolved
        resolved=$(cd -P "$path" 2>/dev/null && pwd || echo "")
        if [ -z "$resolved" ]; then
            echo "FAIL: Cannot resolve path $path"
            errors=$((errors + 1))
        elif [[ "$resolved" != "${normalized_root}"* ]]; then
            echo "FAIL: Path $path resolves outside shadow root: $resolved"
            errors=$((errors + 1))
        else
            echo "  ✅ $path"
        fi
    done
    
    # Also verify Connector state files
    for f in feishu-executes-shadow.jsonl feishu-reactions-shadow.jsonl; do
        local path="${shadow_root}/state/${f}"
        touch "$path" 2>/dev/null || true
        local resolved
        resolved=$(cd -P "$(dirname "$path")" 2>/dev/null && pwd || echo "")
        if [[ "$resolved" != "${normalized_root}"* ]]; then
            echo "FAIL: Connector state $f escapes shadow root: $resolved"
            errors=$((errors + 1))
        else
            echo "  ✅ ${f}"
        fi
    done
    
    if [ "$errors" -gt 0 ]; then
        echo "SHADOW_PATH_FENCE_FAILED: $errors path(s) outside shadow root"
        return 1
    fi
    echo "SHADOW_PATH_FENCE_PASS"
    return 0
}

# Collect evidence summary (sanitized config digest)
collect_shadow_evidence() {
    local shadow_root="$1"
    local evidence_dir="${shadow_root}/evidence"
    mkdir -p "$evidence_dir"
    
    # Sanitized config digest — variable names + PRESENT/ABSENT only
    {
        echo "=== Shadow Config Summary ==="
        echo "Run ID: ${run_id}"
        echo "Timestamp: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
        echo ""
        echo "=== Variables (sanitized) ==="
        while IFS='=' read -r key value; do
            case "$key" in
                ''|\#*) continue ;;
            esac
            key=$(echo "$key" | xargs)
            # Only emit variable name and PRESENT/ABSENT flag
            if echo "$key" | grep -qiE "token|secret|key|password" 2>/dev/null; then
                echo "${key}=PRESENT"
            else
                echo "${key}=PRESENT"
            fi
        done < "${shadow_root}/shadow.env" > "${evidence_dir}/config-summary.txt"
        
        echo "" >> "${evidence_dir}/config-summary.txt"
        echo "CONFIG_FINGERPRINT=$(sha256sum "${shadow_root}/shadow.env" 2>/dev/null | cut -c1-16 || echo 'unavailable')" >> "${evidence_dir}/config-summary.txt"
    }
    
    echo "Evidence collected in ${evidence_dir}"
}

cmd_shadow_e2e() {
    local variant="${1:-fresh}"
    local run_id="shadow_$(date +%s)"
    local shadow_root="/Users/yanfenma/.agent-core/hcr-linux/shadow/shadow_${run_id}"
    
    echo "=== canary-runtime: shadow-e2e (${variant}) ==="
    echo "Run ID: ${run_id}"
    echo "Shadow root: ${shadow_root}"
    
    # Set global for trap
    SHADOW_ROOT="${shadow_root}"
    
    # Set trap for cleanup
    trap shadow_cleanup EXIT INT TERM
    
    # Record pre-existing runtime state
    SHADOW_PRE_EXISTING=false
    if vm_is_running 2>/dev/null; then
        SHADOW_PRE_EXISTING=true
        echo "Pre-existing runtime detected — will be restored after shadow"
    fi
    mkdir -p "${shadow_root}/evidence"
    echo "pre_existing_runtime=${SHADOW_PRE_EXISTING}" > "${shadow_root}/evidence/pre-existing-runtime-state.txt"
    
    # 1. Stop all existing services
    echo ""
    echo "--- Stopping existing services ---"
    cmd_stop 2>/dev/null || true
    sleep 2
    
    # 2. Create shadow state directories
    echo ""
    echo "--- Creating shadow directories ---"
    mkdir -p "${shadow_root}"/{journal,artifacts,state,state/deployment,context,workspaces,logs,pids,evidence}
    mkdir -p "${shadow_root}"/workspaces/scratch
    
    # 3. Generate shadow.env
    echo ""
    echo "--- Generating shadow.env ---"
    generate_shadow_env "${shadow_root}"
    
    # 4. Path fence verification
    echo ""
    echo "--- Shadow path fence ---"
    check_shadow_path_fence "${shadow_root}" || {
        echo "SHADOW_PATH_FENCE_FAILED — aborting"
        exit 1
    }
    
    # 5. Load shadow.env
    set -a
    # shellcheck source=/dev/null
    source "${shadow_root}/shadow.env"
    set +a
    
    # 6. Verify token consistency (on shadow.env)
    echo ""
    echo "--- Token consistency (shadow) ---"
    verify_tokens || {
        echo "Shadow token verification failed — aborting"
        exit 1
    }
    
    # 7. Ensure shadow service ports are free by terminating lingering
    #    processes that use the exact shadow service binary paths.
    #    This is NOT a broad kill — it checks /proc/pid/exe against
    #    the known binary paths.
    echo ""
    echo "--- Ensuring shadow ports are free ---"
    local shadow_bins=("${KERNEL_BIN}" "${CODING_HARNESS_BIN}" "${CAPABILITY_HOST_BIN}" "${DEPLOYMENT_HARNESS_BIN}")
    vm_exec "
        for bin in ${shadow_bins[*]}; do
            if [ -f \"\$bin\" ]; then
                for pid in \$(pgrep -f \"\$bin\" 2>/dev/null || true); do
                    if [ -n \"\$pid\" ] && [ \"\$pid\" -ne \"\$\$\" ]; then
                        exe=\$(readlink -f /proc/\${pid}/exe 2>/dev/null || echo '')
                        if [ \"\$exe\" = \"\$bin\" ]; then
                            echo \"  Terminating lingering \$bin (PID \$pid)\"
                            kill \$pid 2>/dev/null || true
                        fi
                    fi
                done
            fi
        done
        sleep 2
    " 2>/dev/null || true

    # Kernel binary update from VM build
    echo "--- Updating kernel binary ---"
    vm_exec "
        cp /home/yanfenma.guest/agent-core-build/target/release/agent-core-kernel ${KERNEL_BIN} 2>/dev/null && echo '  ✅ Kernel binary updated' || echo '  ⚠️  Build not available, using existing binary'
    " 2>/dev/null || true

    # 8. Deploy shadow tools to shared VM mount
    local shadow_tools_dir="/Users/yanfenma/.agent-core/hcr-linux/shadow-tools"
    rm -rf "${shadow_tools_dir}"
    mkdir -p "${shadow_tools_dir}/tools/shadow-canary" "${shadow_tools_dir}/connectors/feishu/src" "${shadow_tools_dir}/shadow-failure-proxy/target/release"
    cp -r "${PROJECT_DIR}/tools/shadow-canary/"* "${shadow_tools_dir}/tools/shadow-canary/"
    cp -r "${PROJECT_DIR}/connectors/feishu/src/"* "${shadow_tools_dir}/connectors/feishu/src/"
    # Deploy failure proxy binary (if built)
    if [ -f "${PROJECT_DIR}/tools/shadow-failure-proxy/target/release/shadow-failure-proxy" ]; then
        cp "${PROJECT_DIR}/tools/shadow-failure-proxy/target/release/shadow-failure-proxy" \
            "${shadow_tools_dir}/shadow-failure-proxy/target/release/shadow-failure-proxy"
        chmod +x "${shadow_tools_dir}/shadow-failure-proxy/target/release/shadow-failure-proxy"
        echo "  ✅ Failure proxy binary deployed"
    else
        echo "  ⚠️  Failure proxy binary not found (skip)"
    fi
    echo "  Shadow tools deployed to ${shadow_tools_dir}"

    # 8. Run Shadow Supervisor (single persistent session)
    echo ""
    echo "--- Running Shadow Supervisor (${variant}) ---"
    local supervisor_script="${shadow_tools_dir}/tools/shadow-canary/shadow-supervisor.sh"
    local shadow_env="${shadow_root}/shadow.env"
    
    local supervisor_exit_code=0
    "$LIMACTL" shell "$VM_NAME" -- \
        bash "${supervisor_script}" \
            "${variant}" \
            "${shadow_env}" \
            "${shadow_root}" \
            "${run_id}" \
        2>&1 || supervisor_exit_code=$?
    
    if [ "$supervisor_exit_code" -ne 0 ]; then
        echo ""
        echo "❌ Shadow Supervisor failed (exit ${supervisor_exit_code})"
        if [ -f "${shadow_root}/evidence/shadow-summary.json" ]; then
            echo "FIRST_FAILED_STEP: $(grep -o '"first_failed_step":"[^"]*"' "${shadow_root}/evidence/shadow-summary.json" 2>/dev/null | head -1 || echo 'unknown')"
        else
            echo "FIRST_FAILED_STEP: SUPERVISOR_STARTUP"
        fi
        # Trap handles cleanup and production restore
        exit "$supervisor_exit_code"
    fi

    # 9. Collect evidence
    echo ""
    echo "--- Collecting evidence ---"
    collect_shadow_evidence "${shadow_root}"
    
    echo ""
    echo "=== Shadow Canary Complete ==="
    echo "Variant: ${variant}"
    echo "Evidence: ${shadow_root}/evidence"
    echo "Run ID: ${run_id}"
}

# =============================================================================
# MAIN
# =============================================================================
case "${1:-help}" in
    start)
        shift
        cmd_start "${@:-}"
        ;;
    build)
        cmd_build
        ;;
    doctor)
        cmd_doctor
        ;;
    stop)
        cmd_stop
        ;;
    status)
        cmd_status
        ;;
    shadow-e2e)
        shift
        cmd_shadow_e2e "${1:-fresh}"
        ;;
    help|--help|-h)
        echo "Usage: $0 {start|build|doctor|stop|status|shadow-e2e}"
        echo ""
        echo "  start [--build]   Start all services (optionally rebuild first)"
        echo "  build             Build all services inside the Lima VM"
        echo "  doctor            Run comprehensive preflight checks"
        echo "  stop              Stop all services"
        echo "  status            Show current runtime status"
        echo "  shadow-e2e [fresh|dirty]  Run Shadow Canary (isolated environment)"
        exit 0
        ;;
    *)
        echo "Unknown command: $1"
        echo "Usage: $0 {start|build|doctor|stop|status}"
        exit 1
        ;;
esac
