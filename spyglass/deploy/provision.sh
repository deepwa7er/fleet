#!/usr/bin/env bash
#
# Provision spyglass's infrastructure on the VPS:
#   - the `spyglass` service user (least privilege)
#   - the config (installed only if absent; binds loopback, fronted by breakwater)
#   - the systemd unit
#
# Run this once for first-time setup. Routine binary deploys go through tugboat
# (deploy.toml at the repo root), not this script — so this does not build or
# install the binary.
#
# Host: set SPYGLASS_HOST (defaults to the `deepwa7er` ssh alias).
set -euo pipefail

HOST="${SPYGLASS_HOST:-deepwa7er}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # deploy/
REMOTE=/opt/spyglass/provision

echo ">> Syncing unit/config to $HOST ..."
ssh "$HOST" "mkdir -p '$REMOTE'"
rsync -az "$SCRIPT_DIR/spyglass.toml" "$HOST:$REMOTE/spyglass.toml"
rsync -az "$SCRIPT_DIR/spyglass.service" "$HOST:$REMOTE/spyglass.service"

echo ">> Provisioning on $HOST ..."
ssh "$HOST" 'bash -s' <<'REMOTE'
set -euo pipefail
P=/opt/spyglass/provision

# --- Service user (least privilege) -----------------------------------------
if ! id spyglass >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin spyglass
fi

# --- Config -----------------------------------------------------------------
mkdir -p /etc/spyglass
[ -f /etc/spyglass/config.toml ] || install -m644 "$P/spyglass.toml" /etc/spyglass/config.toml
chown -R spyglass:spyglass /etc/spyglass

# --- systemd unit -----------------------------------------------------------
install -m644 "$P/spyglass.service" /etc/systemd/system/spyglass.service
systemctl daemon-reload
systemctl enable spyglass.service >/dev/null 2>&1 || true
echo ">> Provisioned. Ship the binary with tugboat (deploy.toml)."
REMOTE

echo ">> Done."
