#!/usr/bin/env bash
#
# One-time (or on-change) provisioning for public_site on the VPS.
#
# Run this once before the first `tugboat deploy`, and again whenever this file
# changes. Routine deploys do NOT run it — they ship a new image tar and restart
# the unit (see ../deploy.toml).
#
#   scp deploy/provision.sh vps:/tmp/ && ssh vps 'bash /tmp/provision.sh'
#
# Like blog/readout, public_site runs as a Docker container: the VPS has
# Ruby 3.2.3 and this app needs 3.4.8, so a native install would mean maintaining
# a second Ruby on the host. The systemd unit below is still the thing tugboat
# restarts and health-checks, so the deploy transaction and rollback behave
# exactly like every other service.
#
# This script does NOT touch nginx. The front door is a separate concern with
# its own file and its own review: deploy/nginx-public_site.conf

set -euo pipefail

NAME=public_site
PORT=8104
APP_DIR=/opt/public_site
STATE_DIR="${APP_DIR}/storage"
IMAGE_TAR="${APP_DIR}/public_site-image.tar"
IMAGE_TAG=public_site:deploy

PUBLIC_HOST=deepwa7er.com

echo "==> creating ${APP_DIR} and ${STATE_DIR}"
mkdir -p "${APP_DIR}" "${STATE_DIR}"

# Layout matches blog/readout: the deploy artifact and the state sit together
# under /opt/public_site.
#
#   /opt/public_site/public_site-image.tar   replaced wholesale on every deploy
#   /opt/public_site/storage/                the forum + cats data (if any)
#
# CAVEAT, recorded so it is not rediscovered the hard way: fleet-backup only
# snapshots /var/lib/<service>/, so nothing here is in the encrypted offsite
# backup set. blog/readout have the same gap. All three are to be solved
# together rather than by moving this one app somewhere non-standard.
chmod 0750 "${APP_DIR}"

# The container runs as uid/gid 1000 (the `rails` user in the image), so the
# directory it writes SQLite into must be owned by that uid on the host.
chown -R 1000:1000 "${STATE_DIR}"
chmod 0750 "${STATE_DIR}"

# Rails needs the master key to read encrypted credentials. It is deliberately
# NOT baked into the image: the image tar is shipped over the network and lands
# in a world-readable-ish path, whereas this file is 0600.
if [ ! -f "${APP_DIR}/master.key" ]; then
  echo "!! ${APP_DIR}/master.key is missing."
  echo "   Copy it from the repo (config/master.key), which is gitignored:"
  echo "     scp config/master.key vps:${APP_DIR}/master.key"
  echo "   then re-run this script."
  exit 1
fi

# Owned by uid 1000 so the container's `rails` user can read it, and still 0600.
# uid 1000 is unassigned on this host, so this grants no host account access —
# root-owned 0600 would simply be unreadable inside the container, which fails
# the deploy with a bare "Permission denied @ rb_sysopen".
chown 1000:1000 "${APP_DIR}/master.key"
chmod 0600 "${APP_DIR}/master.key"

echo "==> writing systemd unit"
cat > /etc/systemd/system/${NAME}.service <<UNIT
[Unit]
Description=Homepage at https://${PUBLIC_HOST} — DW-001 warm-paper
Documentation=https://github.com/deepwa7er
After=network-online.target docker.service
Requires=docker.service

[Service]
Type=exec
Restart=on-failure
RestartSec=3

# tugboat ships a new image tar and restarts this unit; loading on every start
# is what makes the restart pick up the new build. Loading an already-present
# image is a fast no-op, so this costs nothing on an ordinary restart.
ExecStartPre=-/usr/bin/docker rm -f ${NAME}
ExecStartPre=/usr/bin/docker load -i ${IMAGE_TAR}

# Binds loopback only. The public edge (nginx) reaches it here — nothing about
# this app listens on a public or tailnet address directly.
#
# This app is public-only (no host-separated admin like blog), so no admin-host
# env vars are needed here. If a tailnet-only admin is ever added, add
# BLOG_ADMIN_HOST-style vars together with a literal Host pin in the nginx vhost.
#
# Memory is the binding constraint on this box (2GB total, shared with the rest
# of the fleet), so Puma runs a single worker and the container is capped. Left
# unbounded, a Rails app will happily use more than this VPS has.
ExecStart=/usr/bin/docker run --rm --name ${NAME} \\
  -p 127.0.0.1:${PORT}:80 \\
  -e RAILS_ENV=production \\
  -e WEB_CONCURRENCY=1 \\
  -e RAILS_MAX_THREADS=3 \\
  -v ${STATE_DIR}:/rails/storage \\
  -v ${APP_DIR}/master.key:/rails/config/master.key:ro \\
  --memory=350m \\
  --memory-swap=350m \\
  ${IMAGE_TAG}

ExecStop=/usr/bin/docker stop ${NAME}

[Install]
WantedBy=multi-user.target
UNIT

echo "==> reloading systemd"
systemctl daemon-reload
systemctl enable ${NAME}.service

echo
echo "Provisioned. Next:"
echo "  1. tugboat deploy                 (from the public_site directory — ships the image and starts it)"
echo "  2. install deploy/nginx-public_site.conf and issue the certificate (see that file's header)"
echo "  NOTE: the database is NOT backed up yet — see blog/deploy/fleet-backup.md analogue"
echo
echo "The service will not start until an image tar exists at ${IMAGE_TAR}."
