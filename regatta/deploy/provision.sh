#!/usr/bin/env bash
#
# Provision regatta's infrastructure on the VPS:
#   - the `regatta` service user (least privilege)
#   - the web asset dir (tugboat ships the bundle into it)
#   - the systemd unit (the state dir /var/lib/regatta is created by the unit's
#     StateDirectory= on first start, owned by the service user)
#
# Run this for first-time setup and whenever the unit file changes. Routine
# code/asset deploys go through tugboat (deploy.toml at the service root), not
# this script — so this does not build, install the binary/web assets, or start
# the service (tugboat does that).
#
# Host: set REGATTA_HOST (defaults to the `deepwa7er` ssh alias).
set -euo pipefail

HOST="${REGATTA_HOST:-deepwa7er}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # deploy/
REMOTE=/opt/regatta/provision

echo ">> Syncing unit to $HOST ..."
ssh "$HOST" "mkdir -p '$REMOTE'"
rsync -az "$SCRIPT_DIR/regatta.service" "$HOST:$REMOTE/regatta.service"

echo ">> Provisioning on $HOST ..."
ssh "$HOST" 'bash -s' <<'REMOTE'
set -euo pipefail
P=/opt/regatta/provision

# --- Service user (least privilege) -----------------------------------------
if ! id regatta >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin regatta
fi

# --- Web asset dir (tugboat ships the built bundle here) --------------------
mkdir -p /opt/regatta/web

# --- systemd unit -----------------------------------------------------------
install -m644 "$P/regatta.service" /etc/systemd/system/regatta.service
systemctl daemon-reload
systemctl enable regatta.service >/dev/null 2>&1 || true
echo ">> Provisioned. Ship code/assets with tugboat (deploy.toml)."
REMOTE

echo ">> Done."
