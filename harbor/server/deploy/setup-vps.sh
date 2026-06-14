#!/usr/bin/env bash
#
# One-time setup: make the VPS the canonical "hub" for secondbrain and create the
# unprivileged service account harbor runs as.
#
# It creates a push-to-deploy git repo at /srv/harbor/secondbrain whose WORKING
# TREE is updated whenever you `git push` to it (receive.denyCurrentBranch =
# updateInstead). harbor-server reads that tree. No GitHub, no deploy key.
#
# After running this, point your computers at the VPS and push (printed at the
# end). Re-runnable: it no-ops anything already in place.
#
# Host: set HARBOR_SSH_HOST (defaults to the `deepwa7er` ssh alias).
set -euo pipefail

: "${HARBOR_SSH_HOST:=deepwa7er}"
REPO=/srv/harbor/secondbrain

ssh "$HARBOR_SSH_HOST" REPO="$REPO" bash -s <<'EOF'
set -euo pipefail

# Service account (no login, no home) that harbor-server runs as.
if ! id harbor >/dev/null 2>&1; then
  useradd --system --shell /usr/sbin/nologin --no-create-home harbor
  echo "created service user 'harbor'"
fi

mkdir -p /srv/harbor

# Canonical secondbrain repo, push-to-deploy.
if [ ! -d "$REPO/.git" ]; then
  git init -q "$REPO"
  git -C "$REPO" symbolic-ref HEAD refs/heads/main        # default branch = main
  git -C "$REPO" config receive.denyCurrentBranch updateInstead
  echo "created push-to-deploy repo at $REPO"
else
  echo "$REPO already initialized"
fi

# World-readable working tree so the unprivileged harbor user can read it.
chmod -R a+rX /srv/harbor
EOF

cat <<EOF

VPS hub ready. Now point this machine's secondbrain at it and push:

  git -C ~/secondbrain remote rename origin github 2>/dev/null || true
  git -C ~/secondbrain remote add vps "$HARBOR_SSH_HOST:$REPO"
  git -C ~/secondbrain push -u vps main

(Repeat the 'remote add vps' + 'push' on any other computer. The old GitHub
remote is kept as 'github' but no longer the default — delete it when ready.)
EOF
