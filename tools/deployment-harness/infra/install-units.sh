#!/bin/bash
# =============================================================================
# install-units.sh — Install / refresh the fixed-runtime systemd --user units.
# =============================================================================
#
# Deployment Harness deployment artifact. This is the ONLY installer for the
# supervised service units; it is idempotent and safe to re-run. It is
# deliberately thin: it copies the versioned units + shared scripts from this
# repo into the VM home, reloads the user manager, and enables the units in
# Kernel-first order. It contains no orchestration platform, no health
# business logic. This round supervises ONLY kernel + feishu-connector (see
# the UNITS list below); the three Harness services are not part of this
# round's supervision contract and must not be enabled by this installer.
#
# Runtime supervision (single instance + crash auto-recovery) is systemd's
# job. Ordering/readiness is expressed in the units + preflight.sh.
#
# Run INSIDE the Lima VM (yanfenma user), with this script's directory copied
# alongside the units:
#   ./install-units.sh            # install + enable --now
#   ./install-units.sh --no-start # install/refresh only, do not enable
#
# Source layout (this repo):  tools/deployment-harness/infra/
# Target layout (VM home):
#   ~/.config/systemd/user/agent-core-*.service   (units)
#   ~/.agent-core/infra/{run-svc.sh,preflight.sh,health.sh}
# =============================================================================
set -euo pipefail

# This round supervises ONLY the Kernel and the Feishu Connector — the two
# services whose lifecycle coupling caused the 2026-07-31 outage (Kernel died
# unattended while the Connector kept receiving Feishu messages). The three
# Harness units (coding-harness, capability-host, deployment-harness) are
# deliberately NOT installed by this installer: their runtime env files are
# incomplete for deployment-harness, and they are not part of this round's
# supervision contract. They remain as uninstalled artifacts only.
UNITS=(
    agent-core-kernel.service
    agent-core-feishu-connector.service
)
SCRIPTS=(run-svc.sh preflight.sh health.sh)

# Resolve the directory this script lives in (repo infra dir when run from
# source, or a staged copy inside the VM).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

USER_UNITS_DIR="${HOME}/.config/systemd/user"
INFRA_DIR="${HOME}/.agent-core/infra"

START=1
[ "${1:-}" = "--no-start" ] && START=0

echo "=== install-units: deploying fixed-runtime units ==="
mkdir -p "$USER_UNITS_DIR" "$INFRA_DIR"

# Units
for u in "${UNITS[@]}"; do
    src="${SCRIPT_DIR}/${u}"
    [ -f "$src" ] || { echo "FATAL: unit not found: $src" >&2; exit 1; }
    install -m 0644 "$src" "${USER_UNITS_DIR}/${u}"
    echo "  installed ${u}"
done

# Shared scripts
for s in "${SCRIPTS[@]}"; do
    src="${SCRIPT_DIR}/${s}"
    [ -f "$src" ] || { echo "FATAL: script not found: $src" >&2; exit 1; }
    install -m 0755 "$src" "${INFRA_DIR}/${s}"
    echo "  installed infra/${s}"
done

echo "--- daemon-reload (user manager) ---"
systemctl --user daemon-reload

if [ "$START" -eq 1 ]; then
    # Enable the supervised units. Kernel first so the Connector's readiness
    # gate sees a healthy Kernel before it starts.
    echo "--- enabling + starting (Kernel first) ---"
    for u in "${UNITS[@]}"; do
        systemctl --user enable "$u" >/dev/null 2>&1 || true
        systemctl --user start "$u"
    done
    echo "=== install-units: done. 'systemctl --user status agent-core-kernel agent-core-feishu-connector' ==="
else
    echo "=== install-units: done (--no-start). Enable with: systemctl --user enable --now agent-core-kernel agent-core-feishu-connector ==="
fi
