#!/usr/bin/env bash
#
# Provision keep's infrastructure on OVH (the fleet's first OVH service):
#   - the `keep` service user (least privilege)
#   - /etc/keep/tokens, the per-database Bearer [REDACTED] (`name token` per line)
#   - the systemd unit (the state dir /var/lib/keep is created by the unit's
#     StateDirectory= on first start, owned by the service user)
#   - restic + sqlite3 via dnf (snapshot shipping and drill verification)
#
# Run this for first-time setup and whenever the unit changes. Routine code
# deploys go through tugboat (deploy.toml at the crate root), not this
# script — so this does not build, install the binary, or start the service.
#
# The tokens file is NOT in this repo and never should be. Write one locally
# (`name token` per line, e.g. `recipes <64 hex chars>` — mint with
# `openssl rand -hex 32` per database) and pass it:
#
#   KEEP_TOKENS_FILE=~/.config/keep/tokens ./deploy/provision.sh
#
# Without KEEP_TOKENS_FILE the script leaves an existing tokens file on the
# box alone, so re-provisioning after a unit change needs no secret in hand.
# New databases are provisioned by adding a line and re-running with the file.
#
# R2 credentials live OUTSIDE this script: write /etc/keep/restic.env on the
# box (see deploy/restic.env.example) and `systemctl restart keep`. Without
# it keep still serves and snapshots locally, warning that the off-box half
# is missing. `restic init` the repository once before the first deploy.
#
# Host: set KEEP_HOST (defaults to the `ovh` ssh alias). Unlike the old VPS
# (root over SSH), OVH lands as `fedora` with passwordless sudo, so every
# privileged remote step goes through `sudo`.
set -euo pipefail

HOST="${KEEP_HOST:-ovh}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # deploy/
REMOTE=/opt/keep/provision

echo ">> Syncing unit to $HOST ..."
ssh "$HOST" "sudo mkdir -p '$REMOTE' && sudo chown fedora:fedora '$REMOTE'"
rsync -az "$SCRIPT_DIR/keep.service" "$HOST:$REMOTE/keep.service"

if [[ -n "${KEEP_TOKENS_FILE:-}" ]]; then
  if [[ ! -s "$KEEP_TOKENS_FILE" ]]; then
    echo "!! KEEP_TOKENS_FILE=$KEEP_TOKENS_FILE is missing or empty" >&2
    exit 1
  fi
  echo ">> Installing the tokens file ..."
  # Over stdin, not as an argument and not through rsync. An argument would be
  # visible in `ps` for as long as the command ran; rsync would land the file
  # with default permissions first and tighten them after. The umask means the
  # secrets are never readable by anyone but root, not even momentarily.
  ssh "$HOST" "umask 077 && cat > '$REMOTE/tokens.incoming'" < "$KEEP_TOKENS_FILE"
fi

echo ">> Provisioning on $HOST ..."
ssh "$HOST" 'bash -s' <<'REMOTE'
set -euo pipefail
P=/opt/keep/provision

# --- Packages (snapshot shipping + drill verification) ----------------------
if ! command -v restic >/dev/null 2>&1; then
  sudo dnf install -y restic sqlite3
fi

# --- Service user (least privilege) -----------------------------------------
if ! id keep >/dev/null 2>&1; then
  sudo useradd --system --no-create-home --shell /usr/sbin/nologin keep
fi

# --- Secrets ----------------------------------------------------------------
sudo mkdir -p /etc/keep
if [[ -s "$P/tokens.incoming" ]]; then
  sudo install -o root -g keep -m640 "$P/tokens.incoming" /etc/keep/tokens
  rm -f "$P/tokens.incoming"
fi

# --- Unit -------------------------------------------------------------------
sudo install -m644 "$P/keep.service" /etc/systemd/system/keep.service
sudo systemctl daemon-reload
sudo systemctl enable keep >/dev/null

echo ">> Provisioned:"
echo "   user:   $(id keep)"
echo "   unit:   $(systemctl is-enabled keep)"
if [[ -f /etc/keep/tokens ]]; then
  echo "   tokens: $(wc -l < /etc/keep/tokens) database(s)"
else
  echo "   tokens: MISSING — re-run with KEEP_TOKENS_FILE before deploying"
fi
if [[ -f /etc/keep/restic.env ]]; then
  echo "   restic: configured"
else
  echo "   restic: MISSING — write /etc/keep/restic.env (see deploy/restic.env.example)"
fi
echo ">> Ship the binary with tugboat (deploy.toml), then drill per keep/README.md."
REMOTE

echo ">> Done."
