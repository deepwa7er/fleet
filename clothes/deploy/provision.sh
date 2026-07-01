#!/usr/bin/env bash
#
# Provision clothes' infrastructure on the VPS:
#   - the `clothes` service user (least privilege)
#   - the web asset dir (tugboat ships the bundle into it)
#   - the systemd unit (the state dir /var/lib/clothes is created by the unit's
#     StateDirectory= on first start, owned by the service user)
#
# Run this for first-time setup and whenever the unit file changes. Routine
# code/asset deploys go through tugboat (deploy.toml at the repo root), not this
# script — so this does not build, install the binary/web assets, or start the
# service (tugboat does that).
#
# Host: set CLOTHES_HOST (defaults to the `deepwa7er` ssh alias).
set -euo pipefail

HOST="${CLOTHES_HOST:-deepwa7er}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # deploy/
REMOTE=/opt/clothes/provision

echo ">> Syncing unit to $HOST ..."
ssh "$HOST" "mkdir -p '$REMOTE'"
rsync -az "$SCRIPT_DIR/clothes.service" "$HOST:$REMOTE/clothes.service"

echo ">> Provisioning on $HOST ..."
ssh "$HOST" 'bash -s' <<'REMOTE'
set -euo pipefail
P=/opt/clothes/provision

# --- Service user (least privilege) -----------------------------------------
if ! id clothes >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin clothes
fi

# --- Web asset dir (tugboat ships the built bundle here) --------------------
mkdir -p /opt/clothes/web

# --- systemd unit -----------------------------------------------------------
install -m644 "$P/clothes.service" /etc/systemd/system/clothes.service
systemctl daemon-reload
systemctl enable clothes.service >/dev/null 2>&1 || true
echo ">> Provisioned. Ship code/assets with tugboat (deploy.toml)."
REMOTE

echo ">> Done."
