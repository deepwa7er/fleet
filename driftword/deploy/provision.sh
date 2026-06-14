#!/usr/bin/env bash
#
# Provision driftword's infrastructure on the VPS:
#   - the `driftword` service user (least privilege)
#   - the config (installed only if absent; bind address auto-filled to the
#     Tailscale IP while still the baseline placeholder)
#   - the systemd unit
#
# Run this once for first-time setup. Routine binary deploys go through tugboat
# (deploy.toml at the repo root), not this script — so this does not build or
# install the binary.
#
# Host: set DRIFTWORD_HOST (defaults to the `deepwa7er` ssh alias).
set -euo pipefail

HOST="${DRIFTWORD_HOST:-deepwa7er}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # deploy/
REMOTE=/opt/driftword/provision

echo ">> Syncing unit/config to $HOST ..."
ssh "$HOST" "mkdir -p '$REMOTE'"
rsync -az "$SCRIPT_DIR/driftword.toml" "$HOST:$REMOTE/driftword.toml"
rsync -az "$SCRIPT_DIR/driftword.service" "$HOST:$REMOTE/driftword.service"

echo ">> Provisioning on $HOST ..."
ssh "$HOST" 'bash -s' <<'REMOTE'
set -euo pipefail
P=/opt/driftword/provision

# --- Service user (least privilege) -----------------------------------------
if ! id driftword >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin driftword
fi

# --- Config -----------------------------------------------------------------
mkdir -p /etc/driftword
[ -f /etc/driftword/config.toml ] || install -m644 "$P/driftword.toml" /etc/driftword/config.toml

# Auto-fill the bind address only while it is still the baseline placeholder.
TSIP="$(tailscale ip -4 2>/dev/null | head -1 || true)"
if [ -n "$TSIP" ] && grep -q '^bind = "100.64.0.1"' /etc/driftword/config.toml; then
  sed -i "s|^bind = .*|bind = \"$TSIP\"|" /etc/driftword/config.toml
  echo ">> Bound driftword to Tailscale IP $TSIP"
fi
chown -R driftword:driftword /etc/driftword

# --- systemd unit -----------------------------------------------------------
install -m644 "$P/driftword.service" /etc/systemd/system/driftword.service
systemctl daemon-reload
systemctl enable driftword.service >/dev/null 2>&1 || true
echo ">> Provisioned. Ship the binary with tugboat (deploy.toml)."
REMOTE

echo ">> Done."
