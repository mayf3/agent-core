#!/usr/bin/env bash
# check-no-root-reports.sh
#
# Guards against temporary/lint report files accumulating in the repo root.
# These files (AUDIT_REPORT, IMPLEMENTATION_REPORT, INVESTIGATION_REPORT)
# are temporary artifacts that belong in /tmp or docs/.
#
# Usage:
#   ./scripts/check-no-root-reports.sh [repository-root]
#
# Exit code:
#   0  — no forbidden files found
#   1  — at least one forbidden file present (with diagnostics)
#
# The optional root is used by the Rust integration test so `cargo test`
# continuously enforces this repository policy.

set -euo pipefail

if [ "$#" -gt 1 ]; then
    echo "Usage: $0 [repository-root]" >&2
    exit 2
fi

if [ "$#" -eq 1 ]; then
    if [ ! -d "$1" ]; then
        echo "ERROR: repository root is not a directory: $1" >&2
        exit 2
    fi
    REPO_ROOT="$(cd "$1" && pwd)"
else
    REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
fi
BAD=0

for pattern in '*_AUDIT_REPORT.md' '*_IMPLEMENTATION_REPORT.md' '*_INVESTIGATION_REPORT.md'; do
    while IFS= read -r -d '' f; do
        echo "ERROR: Forbidden report file in repo root: ${f##*/}"
        echo "       Temporary reports belong in /tmp; permanent docs in docs/"
        BAD=1
    done < <(find "$REPO_ROOT" -maxdepth 1 -type f -name "$pattern" -print0 2>/dev/null)
done

if [ "$BAD" -eq 1 ]; then
    echo "FAIL: remove the listed files from repo root before committing."
fi
exit "$BAD"
