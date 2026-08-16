#!/usr/bin/env bash
# Provision shutter-relay on the VPS:
#   - the `shutter-relay` service user (least privilege)
#   - the systemd unit
#
# Run for first-time setup and whenever the unit changes.
# Routine binary deploys go through tugboat (deploy.toml at the repo root).

set -euo pipefail
HOST="${HOST:-deepwa7er}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REMOTE=/opt/shutter-relay/provision

echo ">> Syncing unit to $HOST ..."
ssh "$HOST" "mkdir -p '$REMOTE'"
rsync -az "$SCRIPT_DIR/shutter-relay.service" "$HOST:$REMOTE/shutter-relay.service"

echo ">> Provisioning on $HOST ..."
ssh "$HOST" 'bash -s' <<'REMOTE'
set -euo pipefail
P=/opt/shutter-relay/provision

if ! id shutter-relay >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin shutter-relay
fi

install -m644 "$P/shutter-relay.service" /etc/systemd/system/shutter-relay.service
systemctl daemon-reload
systemctl enable shutter-relay.service >/dev/null 2>&1 || true
echo ">> Provisioned. Ship the binary with: cargo run -p tugboat -- deploy (from shutter-relay/)"
REMOTE

echo ">> Done."
