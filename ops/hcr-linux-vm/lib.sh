#!/bin/bash
# shellcheck shell=bash
# Common helpers for the HCR Linux VM ops scripts.
#
# This module resolves the Lima control binary and enforces that it runs
# NATIVELY on Apple Silicon.  On this host the PATH resolves the x86_64
# limactl from /usr/local/bin ahead of the native arm64 one under
# /opt/homebrew/bin, which forces Rosetta translation and makes
# `limactl list` fail.  Every ops script must source this file and call
# `hcr_resolve_limactl` before invoking limactl.
#
# Resolution priority:
#   1. $LIMACTL_BIN  (explicit override)
#   2. /opt/homebrew/bin/limactl
#   3. command -v limactl
#
# After resolution the architecture is verified.  On Apple Silicon, if only
# an x86_64 limactl is available the script fails closed with
# LIMA_NATIVE_ARM64_REQUIRED instead of continuing under Rosetta.
#
# Constraints honored:
#   - set -euo pipefail safe (no unset references in callers)
#   - no eval
#   - does not modify any global shell configuration
#   - never prints secrets
#   - single focused module, well under 500 lines

# Print an architecture tag for a Mach-O executable: "arm64", "x86_64",
# "universal", or "" when it cannot be determined.
_hcr_macho_arch() {
    local bin="$1"
    local info
    info="$(file "$bin" 2>/dev/null || true)"
    case "$info" in
        *"universal binary"*"arm64"*) echo "universal" ;;
        *"universal binary"*)
            # Universal but no native arm64 slice — treat as non-native.
            echo "universal-no-arm64" ;;
        *"arm64"*) echo "arm64" ;;
        *"x86_64"*) echo "x86_64" ;;
        *) echo "" ;;
    esac
}

_hcr_is_apple_silicon() {
    [ "$(uname -s)" = "Darwin" ] && [ "$(uname -m)" = "arm64" ]
}

# Resolve and validate the native limactl executable.
#
# Side effects:
#   - exports LIMACTL_BIN with the chosen absolute path (idempotent).
#
# On success returns 0 and prints the resolved path on the last line.
# On failure prints a diagnostic and exits the calling script with a
# non-zero status (the script sources this file with the expectation that
# resolution failure stops execution).
hcr_resolve_limactl() {
    local candidate=""

    # Honour an explicit override if it points at a usable binary.
    if [ -n "${LIMACTL_BIN:-}" ]; then
        if [ ! -x "$LIMACTL_BIN" ]; then
            echo "ERROR: LIMACTL_BIN='$LIMACTL_BIN' is not executable." >&2
            return 1
        fi
        candidate="$LIMACTL_BIN"
    elif [ -x /opt/homebrew/bin/limactl ]; then
        candidate="/opt/homebrew/bin/limactl"
    elif command -v limactl >/dev/null 2>&1; then
        candidate="$(command -v limactl)"
    fi

    if [ -z "$candidate" ]; then
        echo "ERROR: limactl not found. Install native ARM64 lima with:" >&2
        echo "       arch -arm64 /opt/homebrew/bin/brew install lima" >&2
        return 1
    fi

    # ALWAYS verify architecture, even for an explicit LIMACTL_BIN override.
    local arch
    arch="$(_hcr_macho_arch "$candidate")"

    if _hcr_is_apple_silicon; then
        case "$arch" in
            arm64|universal)
                : # native — proceed
                ;;
            *)
                echo "ERROR: resolved limactl '$candidate' is '$arch' (not native arm64)." >&2
                echo "       On Apple Silicon this runs under Rosetta and cannot control the VM." >&2
                echo "       Install native lima: arch -arm64 /opt/homebrew/bin/brew install lima" >&2
                echo "       or set LIMACTL_BIN to a native arm64 limactl." >&2
                echo "LIMA_NATIVE_ARM64_REQUIRED" >&2
                return 1
                ;;
        esac
    fi

    export LIMACTL_BIN="$candidate"
    echo "$LIMACTL_BIN"
    return 0
}
