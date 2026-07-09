#!/usr/bin/env bash
#
# init-harness-workspace.sh — Bootstrap the external Harness workspace
#
# Creates ~/.agent-core/harnesses/ and prints the CODING_CONFIG env var
# needed for the Coding Harness to operate on that directory.
#
# Usage:
#   source ./init-harness-workspace.sh   # exports CODING_CONFIG for the current shell
#   bash ./init-harness-workspace.sh      # prints the export command for manual use
#
# See docs/ops/harness-workspace-bootstrap.md for full documentation.
#

set -euo pipefail

HARNESS_DIR="${HOME}/.agent-core/harnesses"
CONFIG_FILE="$(cd "$(dirname "$0")/.." && pwd)/examples/coding-config-with-harnesses.json"

echo "=== Harness Workspace Bootstrap ==="
echo ""

# 1. Create the harness directory.
mkdir -p "${HARNESS_DIR}"
echo "✅ Created: ${HARNESS_DIR}"

# 2. Build CODING_CONFIG with the expanded HOME path.
ROOT_EXPANDED="${HOME}/.agent-core/harnesses"
CODING_CONFIG=$(cat <<JSON
{
  "workspaces": {
    "harness-dev": {
      "root": "${ROOT_EXPANDED}",
      "read": true,
      "write": true,
      "exec": true,
      "opencode": true,
      "network": true,
      "shell": false
    }
  }
}
JSON
)

# 3. Print setup instructions.
echo ""
echo "✅ Example config available at:"
echo "   ${CONFIG_FILE}"
echo ""
echo "=== Export CODING_CONFIG ==="
echo ""
echo "Run the following command to make the Coding Harness"
echo "recognise the 'harness-dev' workspace:"
echo ""
echo "  export CODING_CONFIG='${CODING_CONFIG}'"
echo ""
echo "Then restart the Coding Harness:"
echo ""
echo "  # If running via cargo:"
echo "  cargo run --bin coding-harness -- --listen 127.0.0.1:7200"
echo ""
echo "  # If running a pre-built binary:"
echo "  # (restart the existing coding-harness process)"
echo ""
echo "=== Verification ==="
echo ""
echo "To verify the workspace is recognised, check the startup log:"
echo ""
echo "  [coding-harness] loaded N workspace(s)"
echo "  [coding-harness] workspace 'harness-dev' → ${ROOT_EXPANDED}"
echo ""

# 4. Optionally export for the current shell when sourced.
if [[ "${BASH_SOURCE[0]}" != "${0}" ]]; then
    export CODING_CONFIG
    echo "✅ CODING_CONFIG exported for current shell"
fi
