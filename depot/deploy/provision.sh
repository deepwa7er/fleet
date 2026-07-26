#!/usr/bin/env bash
#
# Provision depot's infrastructure on the VPS:
#   - the `depot` service user (least privilege), in `systemd-journal` so it can
#     read breakwater's access log out of the journal
#   - the web asset dir (tugboat ships a bundle into it once there is one)
#   - the systemd unit (the state dir /var/lib/depot is created by the unit's
#     StateDirectory= on first start, owned by the service user)
#
# Run this for first-time setup and whenever the unit file changes. Routine code
# deploys go through tugboat (deploy.toml at the repo root), not this script — so
# this does not build, install the binary, or start the service (tugboat does).
#
# Host: set DEPOT_HOST (defaults to the `deepwa7er` ssh alias).
set -euo pipefail

HOST="${DEPOT_HOST:-deepwa7er}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # deploy/
REMOTE=/opt/depot/provision

echo ">> Syncing unit to $HOST ..."
ssh "$HOST" "mkdir -p '$REMOTE'"
rsync -az "$SCRIPT_DIR/depot.service" "$HOST:$REMOTE/depot.service"

echo ">> Provisioning on $HOST ..."
ssh "$HOST" 'bash -s' <<'REMOTE'
set -euo pipefail
P=/opt/depot/provision

# --- Service user (least privilege) -----------------------------------------
if ! id depot >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin depot
fi

# --- Journal read access ----------------------------------------------------
# depot's whole access-log source is `journalctl -u breakwater`. journald gates
# reads of other units' logs on membership of `systemd-journal`; without this the
# ingest loop runs but silently returns nothing, which looks exactly like "no
# traffic" rather than "not permitted".
usermod -aG systemd-journal depot

# --- Web asset dir (tugboat ships a built bundle here when one exists) -------
mkdir -p /opt/depot/web

# --- systemd unit -----------------------------------------------------------
install -m644 "$P/depot.service" /etc/systemd/system/depot.service
systemctl daemon-reload
systemctl enable depot.service >/dev/null 2>&1 || true

echo ">> Provisioned:"
echo "   user:    $(id depot)"
echo ">> Ship the binary with tugboat (deploy.toml)."
REMOTE

echo ">> Done."
