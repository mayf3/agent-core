#!/usr/bin/env bash
# check-no-root-reports.sh
#
# Guards against temporary/lint report files accumulating in the repo root.
# These files (AUDIT_REPORT, IMPLEMENTATION_REPORT, INVESTIGATION_REPORT)
# are temporary artifacts that belong in /tmp or docs/.
#
# Usage:
#   ./scripts/check-no-root-reports.sh
#
# Exit code:
#   0  — no forbidden files found
#   1  — at least one forbidden file present (with diagnostics)
#
# CI integration:
#   Add to CI pipeline to fail if report leaks into root.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
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
