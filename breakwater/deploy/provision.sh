#!/usr/bin/env bash
#
# Provision breakwater's infrastructure on the VPS:
#   - the `breakwater` service user (least privilege)
#   - the /etc/breakwater directory (the config file itself ships via tugboat;
#     breakwater resolves the tailnet IP at startup, so there is nothing to patch)
#   - the systemd unit
#
# Run this for first-time setup and whenever the unit or infra layout changes.
# Routine binary deploys go through tugboat (deploy.toml at the repo root), not
# this script — so it does not build, install the binary, or restart the service.
#
# The Cloudflare API token is NOT shipped by this script (it is a secret, kept
# out of git). Install it out of band before the first start — this script tells
# you how if it is missing.
#
# Host: set BREAKWATER_HOST (defaults to the `deepwa7er` ssh alias).
set -euo pipefail

HOST="${BREAKWATER_HOST:-deepwa7er}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # deploy/
REMOTE=/opt/breakwater/provision

echo ">> Syncing unit to $HOST ..."
ssh "$HOST" "mkdir -p '$REMOTE'"
rsync -az "$SCRIPT_DIR/breakwater.service" "$HOST:$REMOTE/breakwater.service"

echo ">> Provisioning on $HOST ..."
ssh "$HOST" 'bash -s' <<'REMOTE'
set -euo pipefail
P=/opt/breakwater/provision

# --- Service user (least privilege) -----------------------------------------
if ! id breakwater >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin breakwater
fi

# --- Config directory -------------------------------------------------------
# breakwater.toml itself is shipped by tugboat (deploy.toml), not here. This
# only ensures the directory exists with tight perms — it also holds the
# Cloudflare token — and breakwater resolves the tailnet IP at startup, so
# there is nothing to patch into the config.
mkdir -p /etc/breakwater
chown breakwater:breakwater /etc/breakwater
chmod 750 /etc/breakwater

# --- Cloudflare token (secret; installed out of band) -----------------------
if [ -f /etc/breakwater/cloudflare-token ]; then
  chown breakwater:breakwater /etc/breakwater/cloudflare-token
  chmod 600 /etc/breakwater/cloudflare-token
  echo ">> Cloudflare token present."
else
  echo "!! /etc/breakwater/cloudflare-token is MISSING — ACME will fail without it."
  echo "!! Install it (as root on $HOSTNAME) before starting the service, e.g.:"
  echo "!!   install -m600 -o breakwater -g breakwater /dev/stdin \\"
  echo "!!     /etc/breakwater/cloudflare-token <<< 'YOUR_CLOUDFLARE_TOKEN'"
fi

# --- systemd unit -----------------------------------------------------------
install -m644 "$P/breakwater.service" /etc/systemd/system/breakwater.service
systemctl daemon-reload
systemctl enable breakwater.service >/dev/null 2>&1 || true
echo ">> Provisioned. Ship the binary with tugboat (deploy.toml at the repo root)."
REMOTE

echo ">> Done."
