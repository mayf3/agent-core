#!/bin/bash
# Guard: the Kernel seam must not contain product names or acceptance facts.
#
# The External Orchestration Seam V0 module (src/orchestration.rs) is a
# generic governance binding. It must NOT hardcode product names like
# failure-viewer, event.observe.v0, or any acceptance-kit identifier.
#
# This script greps the seam source for disallowed tokens. It is a
# fast, coarse, text-level check — not a semantic analysis — so it errs
# on the side of flagging false positives (which are then reviewed).
#
# Usage: scripts/check-orchestration-no-product-facts.sh

set -euo pipefail

FILE="src/orchestration.rs"
if [ ! -f "$FILE" ]; then
    echo "OK (no orchestration module yet)"
    exit 0
fi

# Tokens that are forbidden in the seam module.
# Add product-specific tokens here as they are discovered.
FORBIDDEN=(
    "failure-viewer"
    "event.observe.v0"
    "context.prepare.v0"
    "hook-consumer"
    "acceptance_kit"
    "AcceptanceKit"
    "DevelopmentRequest"
    "TargetKind"
    "Repair"
    "Candidate"
    "GateKind"
    "GateAttempt"
    "Settlement"
    "HcrClaim"
)

found=0
for token in "${FORBIDDEN[@]}"; do
    # Only check non-comment lines (no //!, ///, /*! ... */).
    # This avoids flagging doc comments that say "this round does NOT include X".
    if grep -q "$token" <(grep -v '^\s*//[/!]' "$FILE" 2>/dev/null) 2>/dev/null; then
        echo "FAIL: '$token' found in $FILE"
        found=$((found + 1))
    fi
done

# Also check the protocol crate
for crate in crates/agent-core-protocol tools/development-controller; do
    if [ ! -d "$crate" ]; then
        continue
    fi
    for token in "${FORBIDDEN[@]}"; do
        # Only check non-comment lines.
        for f in "$crate/src/"*.rs; do
            [ -f "$f" ] || continue
            if grep -q "$token" <(grep -v '^\s*//[/!]' "$f" 2>/dev/null) 2>/dev/null; then
                echo "FAIL: '$token' found in $crate"
                found=$((found + 1))
                break
            fi
        done
    done
done

if [ "$found" -gt 0 ]; then
    echo ""
    echo "ERROR: $found forbidden product fact(s) found in the orchestration seam."
    echo "The seam must be product-agnostic. Move product-specific logic into the"
    echo "Development Controller or defer to a later milestone."
    exit 1
fi

echo "OK: no forbidden product facts in orchestration seam"
