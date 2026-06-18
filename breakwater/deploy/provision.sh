#!/usr/bin/env bash
#
# Provision breakwater's infrastructure on the VPS:
#   - the `breakwater` service user (least privilege)
#   - /etc/breakwater config (installed only if absent; the tailnet-facing
#     listeners are auto-bound to the current Tailscale IP)
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
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
REMOTE=/opt/breakwater/provision

echo ">> Syncing unit + config to $HOST ..."
ssh "$HOST" "mkdir -p '$REMOTE'"
rsync -az "$PROJECT_DIR/breakwater.toml" "$HOST:$REMOTE/breakwater.toml"
rsync -az "$SCRIPT_DIR/breakwater.service" "$HOST:$REMOTE/breakwater.service"

echo ">> Provisioning on $HOST ..."
ssh "$HOST" 'bash -s' <<'REMOTE'
set -euo pipefail
P=/opt/breakwater/provision

# --- Service user (least privilege) -----------------------------------------
if ! id breakwater >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin breakwater
fi

# --- Config (installed only if absent) --------------------------------------
mkdir -p /etc/breakwater
[ -f /etc/breakwater/breakwater.toml ] || install -m640 "$P/breakwater.toml" /etc/breakwater/breakwater.toml

# Bind the tailnet-facing listeners to the current Tailscale IP (ports kept).
TSIP="$(tailscale ip -4 2>/dev/null | head -1 || true)"
if [ -n "$TSIP" ]; then
  sed -i -E "s|^(https_addr = \")[0-9.]+(:443\")|\1${TSIP}\2|" /etc/breakwater/breakwater.toml
  sed -i -E "s|^(http_redirect_addr = \")[0-9.]+(:80\")|\1${TSIP}\2|" /etc/breakwater/breakwater.toml
  echo ">> Bound tailnet listeners to Tailscale IP $TSIP"
fi
chown -R breakwater:breakwater /etc/breakwater
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
