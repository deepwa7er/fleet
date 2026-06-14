#!/usr/bin/env bash
#
# One-time setup so harbor-server can run on the VPS and pull the (private)
# secondbrain repo from GitHub.
#
# Creates:
#   - the unprivileged `harbor` service user
#   - /srv/harbor      (writable by harbor; the server clones secondbrain here)
#   - /etc/harbor      with a read-only SSH deploy key + GitHub's pinned host key
#
# It prints the deploy PUBLIC key at the end. Add it to the secondbrain repo on
# GitHub: Settings -> Deploy keys -> Add deploy key, and leave "Allow write
# access" UNCHECKED (harbor only ever reads). Then run deploy.sh.
#
# Re-runnable: it no-ops anything already in place.
#
# Host: set HARBOR_SSH_HOST (defaults to the `deepwa7er` ssh alias).
set -euo pipefail
: "${HARBOR_SSH_HOST:=deepwa7er}"

ssh "$HARBOR_SSH_HOST" bash -s <<'EOF'
set -euo pipefail

# Service account harbor-server runs as.
if ! id harbor >/dev/null 2>&1; then
  useradd --system --shell /usr/sbin/nologin --no-create-home harbor
  echo "created service user 'harbor'"
fi

install -d -o harbor -g harbor -m 0755 /srv/harbor
install -d -m 0755 /etc/harbor

# Read-only deploy key for pulling the private secondbrain repo.
KEY=/etc/harbor/deploy_key
if [ ! -f "$KEY" ]; then
  ssh-keygen -t ed25519 -N '' -C 'harbor-secondbrain-deploy-key' -f "$KEY" >/dev/null
  echo "generated deploy key $KEY"
fi
chown harbor:harbor "$KEY" "$KEY.pub"
chmod 600 "$KEY"
chmod 644 "$KEY.pub"

# Pin GitHub's host key so the unprivileged harbor user's git fetch never prompts.
if [ ! -s /etc/harbor/known_hosts ]; then
  ssh-keyscan -t ed25519,rsa github.com > /etc/harbor/known_hosts 2>/dev/null
  echo "wrote /etc/harbor/known_hosts"
fi
chown harbor:harbor /etc/harbor/known_hosts

echo
echo "=== Add THIS read-only deploy key to GitHub ==="
echo "secondbrain repo -> Settings -> Deploy keys -> Add deploy key"
echo "(leave 'Allow write access' UNCHECKED):"
echo
cat "$KEY.pub"
EOF

cat <<'EOF'

Next:
  1. Add the deploy key printed above to the GitHub secondbrain repo
     (Settings -> Deploy keys; read-only).
  2. Run: server/deploy/deploy.sh
EOF
