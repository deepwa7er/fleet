#!/usr/bin/env bash
#
# Provision tide's infrastructure on the VPS:
#   - the `tide` service user (least privilege)
#   - the config (installed only if absent; binds loopback, fronted by breakwater)
#   - the systemd unit (StateDirectory=tide creates /var/lib/tide on start)
#
# Run this once for first-time setup. Routine binary deploys go through tugboat
# (deploy.toml at the repo root), not this script — so this does not build or
# install the binary.
#
# Host: set TIDE_HOST (defaults to the `deepwa7er` ssh alias).
set -euo pipefail

HOST="${TIDE_HOST:-deepwa7er}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # deploy/
REMOTE=/opt/tide/provision

echo ">> Syncing unit/config to $HOST ..."
ssh "$HOST" "mkdir -p '$REMOTE'"
rsync -az "$SCRIPT_DIR/tide.toml" "$HOST:$REMOTE/tide.toml"
rsync -az "$SCRIPT_DIR/tide.service" "$HOST:$REMOTE/tide.service"

echo ">> Provisioning on $HOST ..."
ssh "$HOST" 'bash -s' <<'REMOTE'
set -euo pipefail
P=/opt/tide/provision

# --- Service user (least privilege) -----------------------------------------
if ! id tide >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin tide
fi

# --- Config -----------------------------------------------------------------
mkdir -p /etc/tide
[ -f /etc/tide/config.toml ] || install -m644 "$P/tide.toml" /etc/tide/config.toml
chown -R tide:tide /etc/tide

# --- systemd unit -----------------------------------------------------------
install -m644 "$P/tide.service" /etc/systemd/system/tide.service
systemctl daemon-reload
systemctl enable tide.service >/dev/null 2>&1 || true
echo ">> Provisioned. Ship the binary with tugboat (deploy.toml)."
REMOTE

echo ">> Done."
