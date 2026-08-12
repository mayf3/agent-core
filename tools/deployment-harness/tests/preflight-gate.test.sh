#!/usr/bin/env bash
# =============================================================================
# preflight-gate.test.sh — unit tests for preflight.sh's kernel-readiness gate.
#
# Covers the connector start decision against /health:
#   health=ok        → ready (connector starts)
#   health=degraded  → ready (core Kernel APIs remain serviceable; this is
#                      the regression fix: a terminal-unknown outbox row must
#                      not keep the Feishu control plane down)
#   health=corrupt   → NOT ready (fail closed)
#   health=garbage   → NOT ready (fail closed, malformed body)
#   health=http500   → NOT ready (fail closed)
#   unreachable      → NOT ready (fail closed)
#
# Usage: bash tools/deployment-harness/tests/preflight-gate.test.sh
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFLIGHT="$SCRIPT_DIR/../infra/preflight.sh"
PORT="${TEST_PORT:-43199}"
MODE_FILE="/tmp/preflight-gate-mode-$$.txt"
ATTEMPTS=3

# --- Mock /health server: mode file switches the response. ---------------
echo ok > "$MODE_FILE"
python3 - "$PORT" "$MODE_FILE" <<'PY' &
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

port = int(sys.argv[1])
mode_file = sys.argv[2]

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):  # noqa: N802
        try:
            mode = open(mode_file).read().strip()
        except OSError:
            mode = ""
        if mode == "ok":
            body = json.dumps({"ok": True, "status": "ok"}).encode()
        elif mode == "degraded":
            body = json.dumps({
                "ok": True,
                "status": "degraded",
                "outbox_unknown_count": 1,
                "outbox_dispatcher_running": True,
            }).encode()
        elif mode == "corrupt":
            body = json.dumps({"ok": False, "status": "corrupt"}).encode()
        elif mode == "garbage":
            body = b"this is not json"
        elif mode == "http500":
            body = b"internal error"
        else:
            body = b""
        code = 200 if mode != "http500" else 500
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):  # silence
        pass

HTTPServer(("127.0.0.1", port), Handler).serve_forever()
PY
SERVER_PID=$!
trap 'kill "$SERVER_PID" 2>/dev/null || true; rm -f "$MODE_FILE"' EXIT

# Wait for the mock server to accept connections.
for _ in $(seq 1 30); do
    if curl -s -o /dev/null --max-time 1 "http://127.0.0.1:$PORT/health"; then
        break
    fi
    sleep 0.2
done

# --- Source the preflight script (guard prevents main() from running). ----
source "$PREFLIGHT"

HEALTH_URL="http://127.0.0.1:$PORT/health"
failures=0

check() { # name expected(ready|notready)
    local name="$1" expected="$2"
    if [ "$expected" = "ready" ]; then
        if kernel_health_ready "$HEALTH_URL" "$ATTEMPTS"; then
            echo "PASS: $name"
        else
            echo "FAIL: $name (expected ready)"
            failures=$((failures + 1))
        fi
    else
        if kernel_health_ready "$HEALTH_URL" "$ATTEMPTS"; then
            echo "FAIL: $name (expected not ready)"
            failures=$((failures + 1))
        else
            echo "PASS: $name"
        fi
    fi
}

echo ok > "$MODE_FILE";      check "health=ok -> starts" ready
echo degraded > "$MODE_FILE"; check "health=degraded -> starts" ready
echo corrupt > "$MODE_FILE"; check "health=corrupt(ok=false) -> fails closed" notready
echo garbage > "$MODE_FILE"; check "health=malformed body -> fails closed" notready
echo http500 > "$MODE_FILE"; check "health=http500 -> fails closed" notready

# Unreachable: point at a port with no listener.
if kernel_health_ready "http://127.0.0.1:1/health" "$ATTEMPTS"; then
    echo "FAIL: unreachable -> fails closed (expected not ready)"
    failures=$((failures + 1))
else
    echo "PASS: unreachable -> fails closed"
fi

if [ "$failures" -ne 0 ]; then
    echo "preflight gate tests: $failures FAILURE(S)"
    exit 1
fi
echo "preflight gate tests: all passed"
exit 0
