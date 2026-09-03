#!/usr/bin/env bash
#
# Provision recipes' infrastructure on the VPS:
#   - the `recipes` service user (least privilege)
#   - /etc/recipes/keep-token, the Bearer [REDACTED] recipes' keep database
#   - the web asset dir (tugboat ships the bundle into it)
#   - the systemd unit
#
# Run this for first-time setup and whenever the unit or token changes.
# Routine code/asset deploys go through tugboat (deploy.toml at the repo
# root), not this script — so this does not build or install the
# binary/web assets. It does restart a RUNNING recipes when the unit or
# token changed (and waits for it to answer /healthz); it never starts a
# stopped one behind your back, and a no-op re-run never bounces it.
#
# The keep token is NOT in this repo and never should be. The recipes line
# of the keep tokens file (minted for keep's own provision) is extracted and
# shipped; pass the same file:
#
#   RECIPES_KEEP_TOKEN_FILE=~/.config/keep/tokens ./deploy/provision.sh
#
# Without it the script leaves an existing token alone, so re-provisioning
# after a unit change needs no secret in hand.
#
# Host: set RECIPES_HOST (defaults to the `deepwa7er` ssh alias).
set -euo pipefail

HOST="${RECIPES_HOST:-deepwa7er}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # deploy/
REMOTE=/opt/recipes/provision

echo ">> Syncing unit to $HOST ..."
ssh "$HOST" "mkdir -p '$REMOTE'"
rsync -az "$SCRIPT_DIR/recipes.service" "$HOST:$REMOTE/recipes.service"

if [[ -n "${RECIPES_KEEP_TOKEN_FILE:-}" ]]; then
  if [[ ! -s "$RECIPES_KEEP_TOKEN_FILE" ]]; then
    echo "!! RECIPES_KEEP_TOKEN_FILE=$RECIPES_KEEP_TOKEN_FILE is missing or empty" >&2
    exit 1
  fi
  TOKEN=$(awk '$1 == "recipes" { print $2 }' "$RECIPES_KEEP_TOKEN_FILE")
  if [[ -z "$TOKEN" ]]; then
    echo "!! no recipes line in $RECIPES_KEEP_TOKEN_FILE" >&2
    exit 1
  fi
  echo ">> Installing the keep token ..."
  # Over stdin, not as an argument and not through rsync: an argument would
  # be visible in `ps`, and rsync would land the file readable first. The
  # umask means the secret is never readable by anyone but root, not even
  # momentarily.
  ssh "$HOST" "umask 077 && cat > '$REMOTE/keep-token.incoming'" <<<"$TOKEN"
fi

echo ">> Provisioning on $HOST ..."
ssh "$HOST" 'bash -s' <<'REMOTE'
set -euo pipefail
P=/opt/recipes/provision
HEALTH_URL="http://127.0.0.1:8097/healthz"
RESTART=0

# --- Service user (least privilege) -----------------------------------------
if ! id recipes >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin recipes
fi

# --- Web asset dir (tugboat ships the built bundle here) --------------------
mkdir -p /opt/recipes/web

# --- keep token ---------------------------------------------------------------
mkdir -p /etc/recipes
if [[ -s "$P/keep-token.incoming" ]]; then
  if ! cmp -s "$P/keep-token.incoming" /etc/recipes/keep-token 2>/dev/null; then
    install -o root -g recipes -m640 "$P/keep-token.incoming" /etc/recipes/keep-token
    RESTART=1
  fi
  rm -f "$P/keep-token.incoming"
fi

# --- systemd unit -----------------------------------------------------------
if ! cmp -s "$P/recipes.service" /etc/systemd/system/recipes.service 2>/dev/null; then
  install -m644 "$P/recipes.service" /etc/systemd/system/recipes.service
  systemctl daemon-reload
  RESTART=1
fi
systemctl enable recipes.service >/dev/null 2>&1 || true

# --- Restart, only when something changed and only when running --------------
if [[ "$RESTART" == 1 ]]; then
  if systemctl -q is-active recipes; then
    echo ">> Unit/token changed; restarting recipes ..."
    systemctl restart recipes
    HEALTHY=0
    for ((i = 0; i < 15; i++)); do
      if curl -sf -m2 "$HEALTH_URL" >/dev/null 2>&1; then
        HEALTHY=1
        break
      fi
      sleep 1
    done
    if [[ "$HEALTHY" == 1 ]]; then
      echo "   recipes: restarted and healthy"
    else
      echo "!! recipes restarted but is not answering /healthz — investigate before deploying" >&2
      exit 1
    fi
  else
    echo "   recipes: not running — unit/token changes apply on next start"
  fi
fi

echo ">> Provisioned:"
echo "   user:   $(id recipes)"
echo "   unit:   $(systemctl is-enabled recipes || true)"
echo "   active: $(systemctl is-active recipes)"
if [[ -f /etc/recipes/keep-token ]]; then
  echo "   keep token: present"
else
  echo "   keep token: MISSING — re-run with RECIPES_KEEP_TOKEN_FILE before deploying"
fi
echo ">> Ship code/assets with tugboat (deploy.toml)."
REMOTE

echo ">> Done."
