#!/usr/bin/env bash
#
# Provision mirror's infrastructure on the VPS:
#   - the `mirror` service user (least privilege)
#   - /etc/mirror/fizzy-token, the read-only Fizzy access token
#   - the systemd unit (the state dir /var/lib/mirror is created by the unit's
#     StateDirectory= on first start, owned by the service user)
#   - the PUBLIC nginx vhost for https://board.deepwa7er.com, and its
#     certificate, in the order nginx insists on
#
# Run this for first-time setup and whenever the unit or the vhost changes.
# Routine code deploys go through tugboat (deploy.toml at the crate root), not
# this script — so this does not build, install the binary, or start the
# service (tugboat does).
#
# The token is NOT in this repo and never should be. Mint one in Fizzy at
# /my/access_tokens with **read** permission, save it to a file, and pass it:
#
#   MIRROR_TOKEN_FILE=~/.config/mirror/fizzy-token ./deploy/provision.sh
#
# Without MIRROR_TOKEN_FILE the script leaves an existing token on the box
# alone, so re-provisioning after a unit change needs no secret in hand.
#
# Host: set MIRROR_HOST (defaults to the `deepwa7er` ssh alias).
set -euo pipefail

HOST="${MIRROR_HOST:-deepwa7er}"
DOMAIN="${MIRROR_DOMAIN:-board.deepwa7er.com}"
ACME_EMAIL="${ACME_EMAIL:-hello@joemafrici.com}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # deploy/
REMOTE=/opt/mirror/provision

echo ">> Syncing unit and vhost to $HOST ..."
ssh "$HOST" "mkdir -p '$REMOTE'"
rsync -az "$SCRIPT_DIR/mirror.service" "$HOST:$REMOTE/mirror.service"
rsync -az "$SCRIPT_DIR/nginx-mirror.conf" "$HOST:$REMOTE/nginx-mirror.conf"

if [[ -n "${MIRROR_TOKEN_FILE:-}" ]]; then
  if [[ ! -s "$MIRROR_TOKEN_FILE" ]]; then
    echo "!! MIRROR_TOKEN_FILE=$MIRROR_TOKEN_FILE is missing or empty" >&2
    exit 1
  fi
  echo ">> Installing the Fizzy token ..."
  # Over stdin, not as an argument and not through rsync. An argument would be
  # visible in `ps` on the remote host for as long as the command ran; rsync
  # would land the file with default permissions first and tighten them after
  # (and macOS ships openrsync, which has no --chmod at all). The umask means
  # the secret is never readable by anyone but root, not even momentarily.
  ssh "$HOST" "umask 077 && cat > '$REMOTE/fizzy-token.incoming'" < "$MIRROR_TOKEN_FILE"
fi

echo ">> Provisioning on $HOST ..."
ssh "$HOST" DOMAIN="$DOMAIN" ACME_EMAIL="$ACME_EMAIL" 'bash -s' <<'REMOTE'
set -euo pipefail
P=/opt/mirror/provision

# --- Service user (least privilege) -----------------------------------------
if ! id mirror >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin mirror
fi

# --- The Fizzy token --------------------------------------------------------
# Owned by the service user and unreadable by anyone else. This is the only
# credential mirror holds, and it is read-only: Fizzy's
# Identity::AccessToken#allows? refuses anything but GET and HEAD for a token
# with `read` permission.
install -d -m755 /etc/mirror
if [[ -f "$P/fizzy-token.incoming" ]]; then
  install -o mirror -g mirror -m600 "$P/fizzy-token.incoming" /etc/mirror/fizzy-token
  shred -u "$P/fizzy-token.incoming"
  echo "   token: installed"
elif [[ -f /etc/mirror/fizzy-token ]]; then
  echo "   token: left as-is"
else
  echo "!! No token at /etc/mirror/fizzy-token and none supplied."
  echo "   mirror will start, fail its first sync, and serve an empty page."
  echo "   Re-run with MIRROR_TOKEN_FILE=… once you have one."
fi

# --- systemd unit -----------------------------------------------------------
install -m644 "$P/mirror.service" /etc/systemd/system/mirror.service
systemctl daemon-reload
systemctl enable mirror.service >/dev/null 2>&1 || true

# --- Public nginx vhost + certificate ---------------------------------------
# nginx will not load a `listen … ssl` block whose certificate file is absent,
# so on a first run the real vhost cannot be installed until the cert exists,
# and the cert cannot be obtained until something answers HTTP-01 on this
# hostname. Hence the temporary block.
if [[ ! -d "/etc/letsencrypt/live/$DOMAIN" ]]; then
  echo ">> No certificate for $DOMAIN yet; obtaining one ..."
  cat > "/etc/nginx/sites-available/$DOMAIN" <<TEMP
server {
    listen 147.182.250.13:80;
    server_name $DOMAIN;
    location / { return 200 "provisioning\n"; }
}
TEMP
  ln -sf "/etc/nginx/sites-available/$DOMAIN" "/etc/nginx/sites-enabled/$DOMAIN"
  nginx -t && systemctl reload nginx
  # certonly, so the vhost file below is not rewritten (and its comments lost).
  certbot certonly --nginx -d "$DOMAIN" --non-interactive --agree-tos -m "$ACME_EMAIL"
fi

install -m644 "$P/nginx-mirror.conf" "/etc/nginx/sites-available/$DOMAIN"
ln -sf "/etc/nginx/sites-available/$DOMAIN" "/etc/nginx/sites-enabled/$DOMAIN"
nginx -t && systemctl reload nginx

echo ">> Provisioned:"
echo "   user:  $(id mirror)"
echo "   vhost: https://$DOMAIN"
echo ">> Ship the binary with tugboat (deploy.toml)."
REMOTE

echo ">> Done."
