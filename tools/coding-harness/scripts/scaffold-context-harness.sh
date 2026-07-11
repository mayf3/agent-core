#!/usr/bin/env bash
#
# scaffold-context-harness.sh — Create a scaffold context.prepare.v0 Harness
#
# Generates a standard external Harness project from template files:
#   <root>/<harness_id>/
#
# The generated Harness implements the context.prepare.v0 hook with
# a fixed smoke word. It includes a server, tests, manifest, and README.
#
# Usage:
#   tools/coding-harness/scripts/scaffold-context-harness.sh <harness_id>
#   tools/coding-harness/scripts/scaffold-context-harness.sh --root /path <harness_id>
#
# Arguments:
#   --root PATH   Root directory for harness workspaces (default: ~/.agent-core/harnesses)
#   <harness_id>  Identifier for the new harness (lowercase, digits, hyphens only)
#
# Exit codes:
#   0  — success
#   1  — invalid harness_id / argument error
#   2  — target conflict (exists, non‑empty, symlink in path)
#   3  — root not writable, unreachable, or unusable
#   4  — scaffold failed (template missing, write error)
#

set -euo pipefail

# ── Defaults ──────────────────────────────────────────────────────────────

ROOT="${HOME}/.agent-core/harnesses"
HARNESS_ID=""

# ── Paths ──────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
TEMPLATE_DIR="${SCRIPT_DIR}/../templates/context-prepare-harness"

# ── Helpers ────────────────────────────────────────────────────────────────

usage() {
    cat >&2 <<'USAGE_EOF'
Usage: scaffold-context-harness.sh [--root PATH] <harness_id>

Create a scaffold context.prepare.v0 Harness project.

  --root PATH   Root directory (default: ~/.agent-core/harnesses)
  <harness_id>  Identifier for the new harness
USAGE_EOF
    exit 0
}

die() {
    local code="$1"
    shift
    echo "$*" >&2
    exit "$code"
}

# Check that no component of $1 (between $1 and $2) is a symlink.
# This prevents path-traversal attacks via symlinks inside root.
check_no_symlink_in_path() {
    local current="$1"
    local anchor="$2"
    while [[ "$current" != "$anchor" && "$current" != "/" && "$current" != "." ]]; do
        if [[ -L "$current" ]]; then
            echo "error: path component is a symlink: $current" >&2
            return 1
        fi
        current="$(dirname "$current")"
    done
    return 0
}

# ── Parse arguments ───────────────────────────────────────────────────────

while [[ $# -gt 0 ]]; do
    case "$1" in
        --root)
            [[ -n "${2:-}" ]] || die 3 "error: --root requires a path argument"
            ROOT="$2"
            shift 2
            ;;
        --root=*)
            ROOT="${1#*=}"
            shift
            ;;
        --help|-h)
            usage
            ;;
        --*)
            die 1 "error: unknown option: $1"
            ;;
        *)
            [[ -z "$HARNESS_ID" ]] || die 1 "error: unexpected argument: $1"
            HARNESS_ID="$1"
            shift
            ;;
    esac
done

# ── Validate harness_id ───────────────────────────────────────────────────

[[ -n "$HARNESS_ID" ]] || die 1 "error: harness_id is required"

if ! [[ "$HARNESS_ID" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?$ ]]; then
    die 1 "error: invalid harness_id '${HARNESS_ID}' — use only lowercase letters, digits, and hyphens
harness_id must not start or end with a hyphen"
fi

# ── Resolve root to absolute path ─────────────────────────────────────────

if [[ "$ROOT" != /* ]]; then
    ROOT="$(pwd -P)/${ROOT}"
fi
ROOT="${ROOT%/}"

# ── Resolve target path ───────────────────────────────────────────────────

TARGET="${ROOT}/${HARNESS_ID}"

# ── Safety: root existence and writability ────────────────────────────────

if [[ -e "$ROOT" ]]; then
    if [[ ! -d "$ROOT" ]]; then
        die 3 "error: root exists and is not a directory: $ROOT"
    fi
    if [[ ! -w "$ROOT" ]]; then
        die 3 "error: root directory is not writable: $ROOT"
    fi
else
    mkdir -p "$ROOT" 2>/dev/null || die 3 "error: cannot create root directory: $ROOT"
    [[ -w "$ROOT" ]] || die 3 "error: root directory is not writable after creation: $ROOT"
fi

# ── Safety: symlink chain check (path traversal defense) ──────────────────

if [[ -d "$ROOT" ]]; then
    check_no_symlink_in_path "$TARGET" "$ROOT" || exit 2
fi

# ── Safety: target existence checks ──────────────────────────────────────

if [[ -e "$TARGET" || -L "$TARGET" ]]; then
    if [[ -L "$TARGET" ]]; then
        die 2 "error: target is a symlink: $TARGET"
    fi
    if [[ -d "$TARGET" ]]; then
        if ls -A "$TARGET" 2>/dev/null | grep -q .; then
            die 2 "error: target directory already exists and is not empty: $TARGET"
        fi
    else
        die 2 "error: target exists and is not a directory: $TARGET"
    fi
fi

# ── Validate template directory ───────────────────────────────────────────

[[ -d "$TEMPLATE_DIR" ]] || die 4 "error: template directory not found: $TEMPLATE_DIR"

# ── Create directory structure ────────────────────────────────────────────

mkdir -p "${TARGET}/test"

# ── Copy static templates (no placeholders) ───────────────────────────────

cp "${TEMPLATE_DIR}/package.json.template"       "${TARGET}/package.json"
cp "${TEMPLATE_DIR}/server.mjs.template"         "${TARGET}/server.mjs"
cp "${TEMPLATE_DIR}/server.test.mjs.template"    "${TARGET}/test/server.test.mjs"

# ── Copy and replace __HARNESS_ID__ placeholders ─────────────────────────

sed -e "s|__HARNESS_ID__|${HARNESS_ID}|g" \
    "${TEMPLATE_DIR}/harness.manifest.json.template" > "${TARGET}/harness.manifest.json"

sed -e "s|__HARNESS_ID__|${HARNESS_ID}|g" \
    "${TEMPLATE_DIR}/README.md.template" > "${TARGET}/README.md"

# ── Success output ────────────────────────────────────────────────────────

echo "✅ scaffold created: ${TARGET}"
echo ""
echo "  cd ${TARGET}"
echo "  npm test"
echo "  npm start"
echo ""
