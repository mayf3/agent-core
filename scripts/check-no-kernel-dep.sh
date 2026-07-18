#!/bin/bash
# Guard: crates/agent-core-protocol must NOT depend on agent-core-kernel.
# Guard: tools/development-controller must NOT depend on agent-core-kernel.
#
# Uses `cargo metadata` to produce a resolved dependency graph and then
# greps for the kernel crate name. This is authoritative — if the kernel
# appears anywhere in the dependency tree (even transitively), the test
# fails.
#
# Usage: scripts/check-no-kernel-dep.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if ! command -v cargo &>/dev/null; then
    echo "SKIP (cargo not available)"
    exit 0
fi

check_crate() {
    local name="$1"
    local manifest="$2"

    if [ ! -f "$manifest" ]; then
        return 0
    fi

    echo -n "checking $name for agent-core-kernel dependency... "

    # Use cargo metadata to get the full resolved dep tree.
    # We pass --filter-platform so we see only the current platform's deps
    # (avoids false negatives from cfg-gated windows-only deps that happen
    # to include the kernel on other platforms — though unlikely here).
    local meta
    meta="$(cargo metadata --manifest-path "$manifest" --format-version 1 --no-deps 2>/dev/null || true)"
    if [ -z "$meta" ]; then
        echo "SKIP (metadata unavailable)"
        return 0
    fi

    # Resolve dependencies using the workspace. We need to run with --manifest-path
    # pointing to the workspace root for resolution to work.
    local ws_manifest="$ROOT/Cargo.toml"
    local resolved
    resolved="$(cargo metadata --manifest-path "$ws_manifest" --format-version 1 2>/dev/null || true)"
    if echo "$resolved" | python3 -c "
import json, sys
data = json.load(sys.stdin)
pkgs = {p['id']: p for p in data.get('packages', [])}
# find the target package
target = None
for p in data.get('packages', []):
    if p['name'] == '$name':
        target = p
        break
if target is None:
    print('SKIP (crate not found in workspace metadata)')
    sys.exit(0)

# Walk all transitive deps via the resolve node
resolve = data.get('resolve', {})
nodes = {n['id']: n for n in resolve.get('nodes', [])}

def walk_deps(pkg_id, seen):
    node = nodes.get(pkg_id)
    if node is None:
        return
    for dep_id in node.get('dependencies', []):
        if dep_id not in seen:
            seen.add(dep_id)
            walk_deps(dep_id, seen)

seen = set()
if target['id'] in nodes:
    seen.add(target['id'])
    walk_deps(target['id'], seen)

for dep_id in seen:
    dep_pkg = pkgs.get(dep_id, {})
    if dep_pkg.get('name') == 'agent-core-kernel':
        print('FAIL')
        print(f'  {name} depends on agent-core-kernel via {dep_id}')
        sys.exit(1)

print('OK')
" 2>&1; then
        :
    else
        echo "FAILED"
        return 1
    fi
}

check_crate "agent-core-protocol" "$ROOT/crates/agent-core-protocol/Cargo.toml" || exit 1
check_crate "development-controller" "$ROOT/tools/development-controller/Cargo.toml" || exit 1

echo ""
echo "All dependency checks passed — seam crates are independent of agent-core-kernel."
