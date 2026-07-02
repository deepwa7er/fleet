#!/usr/bin/env bash
#
# Provision harbor's infrastructure on the VPS: the systemd unit and the starter
# config. Run setup-vps.sh FIRST (it creates the `harbor` service user, the
# /srv/harbor dir, and the read-only secondbrain deploy key).
#
# This is the occasional/one-time path. Routine code deploys ship a new binary
# via tugboat (deploy.toml at the repo root), not this script — so provisioning
# does not restart the service.
#
# Re-runnable: installs the unit, and the config only if absent.
#
# Host: set HARBOR_SSH_HOST (defaults to the `deepwa7er` ssh alias).
set -euo pipefail
: "${HARBOR_SSH_HOST:=deepwa7er}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # server/deploy
SERVER_DIR="$(dirname "$SCRIPT_DIR")"                        # server

rsync -az "$SERVER_DIR/harbor.toml" "$HARBOR_SSH_HOST:/tmp/harbor.toml.new"
rsync -az "$SCRIPT_DIR/harbor.service" "$HARBOR_SSH_HOST:/tmp/harbor.service.new"

ssh "$HARBOR_SSH_HOST" 'bash -s' <<'EOF'
set -euo pipefail

# Served at /extension by harbor-server; tugboat ships harbor.crx + updates.xml here.
install -d -o harbor -g harbor -m 0755 /srv/harbor/dist

mkdir -p /etc/harbor
if [ ! -f /etc/harbor/harbor.toml ]; then
  install -m 0644 /tmp/harbor.toml.new /etc/harbor/harbor.toml
  echo "installed /etc/harbor/harbor.toml"
else
  echo "kept existing /etc/harbor/harbor.toml"
fi
rm -f /tmp/harbor.toml.new

install -m 0644 /tmp/harbor.service.new /etc/systemd/system/harbor.service
rm -f /tmp/harbor.service.new
systemctl daemon-reload
systemctl enable harbor >/dev/null 2>&1 || true
echo "provisioned harbor unit + config (ship the binary with tugboat)"
EOF
