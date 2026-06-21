#!/usr/bin/env bash
#
# Provision the fleet backup on the VPS: the backup script, the systemd
# service + timer, and the config directory. Idempotent and safe to re-run
# (e.g. on a fresh VPS). Does NOT install secrets — fill /etc/fleet-backup/
# restic.env by hand (see restic.env.example), and `restic init` the repo once,
# before the first run.
#
# Host: set FLEET_HOST (defaults to the `deepwa7er` ssh alias).
set -euo pipefail

HOST="${FLEET_HOST:-deepwa7er}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REMOTE=/opt/fleet-backup/provision

echo ">> Syncing units + script to $HOST ..."
ssh "$HOST" "mkdir -p '$REMOTE'"
rsync -az \
  "$SCRIPT_DIR/fleet-backup" \
  "$SCRIPT_DIR/fleet-backup.service" \
  "$SCRIPT_DIR/fleet-backup.timer" \
  "$HOST:$REMOTE/"

echo ">> Provisioning on $HOST ..."
ssh "$HOST" 'bash -s' <<'REMOTE'
set -euo pipefail
P=/opt/fleet-backup/provision

# Tools (no-op if already present).
if ! command -v restic >/dev/null || ! command -v sqlite3 >/dev/null; then
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -q restic sqlite3
fi

# Config dir (the secret env file is installed by hand, not here).
sudo mkdir -p /etc/fleet-backup /var/lib/fleet-backup
sudo chmod 700 /etc/fleet-backup

# Script + units.
sudo install -m755 "$P/fleet-backup" /usr/local/bin/fleet-backup
sudo install -m644 "$P/fleet-backup.service" /etc/systemd/system/fleet-backup.service
sudo install -m644 "$P/fleet-backup.timer"   /etc/systemd/system/fleet-backup.timer
sudo systemctl daemon-reload

if [ -f /etc/fleet-backup/restic.env ]; then
  sudo systemctl enable --now fleet-backup.timer
  echo ">> Provisioned and timer enabled."
else
  echo "!! /etc/fleet-backup/restic.env is MISSING — fill it (see restic.env.example),"
  echo "!! then 'restic init' the repo and 'systemctl enable --now fleet-backup.timer'."
fi
REMOTE

echo ">> Done."
