#!/usr/bin/env bash
#
# Install the generic git-autocommit machinery on a host: the script + the
# systemd template units. Idempotent. Enabling a watch for a specific repo is a
# separate, per-repo step (see README "Enable a watch").
#
# Host: set AUTOCOMMIT_HOST (defaults to the `deepwa7er` ssh alias).
set -euo pipefail

HOST="${AUTOCOMMIT_HOST:-deepwa7er}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REMOTE=/opt/git-autocommit/provision

echo ">> Syncing to $HOST ..."
ssh "$HOST" "mkdir -p '$REMOTE'"
rsync -az \
  "$SCRIPT_DIR/git-autocommit" \
  "$SCRIPT_DIR/git-autocommit@.service" \
  "$SCRIPT_DIR/git-autocommit@.path" \
  "$HOST:$REMOTE/"

ssh "$HOST" 'bash -s' <<'REMOTE'
set -euo pipefail
P=/opt/git-autocommit/provision
command -v git >/dev/null || sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -q git
sudo install -m755 "$P/git-autocommit" /usr/local/bin/git-autocommit
sudo install -m644 "$P/git-autocommit@.service" /etc/systemd/system/git-autocommit@.service
sudo install -m644 "$P/git-autocommit@.path"    /etc/systemd/system/git-autocommit@.path
sudo systemctl daemon-reload
echo ">> git-autocommit installed."
REMOTE

echo ">> Done."
