#!/usr/bin/env bash
#
# One-time (or on-change) provisioning for skiff on the VPS.
#
# Run this once before the first `tugboat deploy`, and again whenever this file
# changes. Routine deploys do NOT run it — they ship a new image tar and restart
# the unit (see ../deploy.toml).
#
#   scp deploy/provision.sh deepwa7er:/tmp/ && ssh deepwa7er 'bash /tmp/provision.sh'
#
# Like blog/readout, skiff runs as a Docker container: the VPS has Ruby 3.2.3
# and this app needs Ruby 4.0.6, so a native install would mean maintaining a
# second Ruby on the host. The systemd unit below is still the thing tugboat
# restarts and health-checks, so the deploy transaction and rollback behave
# exactly like every other service.
#
# The pi bridge (opencode-compatible API) is skiff's only data source. It
# speaks HTTP to Rails over loopback on 127.0.0.1:4120. When skiff runs as a
# container, the bridge stays on the host (not inside the container) — the
# container reaches it via host.docker.internal (added via --add-host). The
# bridge must be running and reachable or the UI renders the "opencode server
# unreachable" state; the app boots cleanly either way (health is /up, which
# does not require the bridge).
#
# Secrets: the bridge password (OPENCODE_SERVER_PASSWORD) lives in
# ~/.config/skiff/secrets on the desktop and at /opt/skiff/bridge-secrets on
# the VPS. The provision script does not require it — the unit starts without
# it and the UI simply shows the unreachable state until it is placed. When
# provisioning, copy the secrets file from the desktop:
#
#   scp ~/.config/skiff/secrets deepwa7er:/opt/skiff/bridge-secrets
#
# Then restart the bridge and skiff:
#
#   ssh deepwa7er 'systemctl restart skiff'

set -euo pipefail

NAME=skiff
PORT=8120
APP_DIR=/opt/skiff
IMAGE_TAR="${APP_DIR}/skiff-image.tar"
IMAGE_TAG=skiff:deploy

echo "==> creating ${APP_DIR}"
mkdir -p "${APP_DIR}"

# The container runs as uid/gid 1000 (the `rails` user in the image), so any
# paths it reads must be owned/readable by that uid on the host when mounted.
chmod 0750 "${APP_DIR}"

# Rails needs the master key to read encrypted credentials. It is deliberately
# NOT baked into the image: the image tar is shipped over the network and lands
# in a world-readable-ish path, whereas this file is 0600.
if [ ! -f "${APP_DIR}/master.key" ]; then
  echo "!! ${APP_DIR}/master.key is missing."
  echo "   Copy it from the repo (config/master.key), which is gitignored:"
  echo "     scp config/master.key deepwa7er:${APP_DIR}/master.key"
  echo "   then re-run this script."
  exit 1
fi

# Owned by uid 1000 so the container's `rails` user can read it, and still 0600.
# uid 1000 is unassigned on this host, so this grants no host account access —
# root-owned 0600 would simply be unreadable inside the container, which fails
# the deploy with a bare "Permission denied @ rb_sysopen".
chown 1000:1000 "${APP_DIR}/master.key"
chmod 0600 "${APP_DIR}/master.key"

# Bridge secrets: optional at provision time, required at runtime for data.
# Placed at /opt/skiff/bridge-secrets (mode 0600) when available; the unit
# reads it into the container's environment via an env file generated at start.
# This file is the VPS analogue of ~/.config/skiff/secrets on the desktop.
if [ -f "/opt/skiff/bridge-secrets" ]; then
  chown 1000:1000 /opt/skiff/bridge-secrets || chown root:root /opt/skiff/bridge-secrets
  chmod 0600 /opt/skiff/bridge-secrets
  echo "==> bridge secrets present at /opt/skiff/bridge-secrets"
else
  echo "==> no bridge secrets at /opt/skiff/bridge-secrets (UI will show unreachable until placed)"
fi

# Helper that materialises the bridge env file for the container. Runs as an
# ExecStartPre so a password change is picked up on every (re)start without
# re-provisioning. Reads /opt/skiff/bridge-secrets (VPS) or falls back to
# /home/deepwater/.config/skiff/secrets for parity with the desktop path.
echo "==> installing bridge env resolver"
cat > /usr/local/bin/skiff-resolve-bridge <<'RESOLVER'
#!/bin/sh
set -eu
out="/run/skiff-bridge.env"
rm -f "$out"
# Bridge URL: the container reaches the host's bridge via host.docker.internal.
echo "OPENCODE_SERVER_URL=http://host.docker.internal:4120" > "$out"
# Password: first try the VPS path, then the desktop-path locations.
for f in /opt/skiff/bridge-secrets "${HOME:-}/.config/skiff/secrets" /home/deepwater/.config/skiff/secrets; do
  if [ -f "$f" ]; then
    while IFS= read -r line; do
      case "$line" in
        "" | "#"* ) continue ;;
      esac
      key="${line%%=*}"
      val="${line#*=}"
      if [ "$key" = "OPENCODE_SERVER_PASSWORD" ] && [ -n "$val" ]; then
        echo "OPENCODE_SERVER_PASSWORD=$val" >> "$out"
        break
      fi
    done < "$f"
    break
  fi
done
chmod 0600 "$out" 2>/dev/null || true
# Always succeed — the app boots without a bridge (it renders the unreachable
# state) so a missing password file must not block the unit from starting.
exit 0
RESOLVER
chmod 0755 /usr/local/bin/skiff-resolve-bridge

echo "==> writing systemd unit"
cat > /etc/systemd/system/${NAME}.service <<UNIT
[Unit]
Description=Skiff — phone web UI for the pi bridge (tailnet-only)
Documentation=https://github.com/deepwa7er/fleet
After=network-online.target docker.service
Requires=docker.service

[Service]
Type=exec
Restart=on-failure
RestartSec=3

# tugboat ships a new image tar and restarts this unit; loading on every start
# is what makes the restart pick up the new build. Loading an already-present
# image is a fast no-op, so this costs nothing on an ordinary restart.
# Podman on the build host prefixes short tags with `localhost/`; the tar
# therefore loads as `localhost/skiff:deploy` on the VPS. Normalize it so the
# unit's `docker run skiff:deploy` finds the image regardless of which runtime
# built it.
ExecStartPre=-/usr/bin/docker rm -f ${NAME}
ExecStartPre=/usr/bin/docker load -i ${IMAGE_TAR}
ExecStartPre=-/usr/bin/docker tag localhost/${NAME}:deploy ${IMAGE_TAG}
ExecStartPre=/usr/local/bin/skiff-resolve-bridge
EnvironmentFile=-/run/skiff-bridge.env

# Binds loopback only: breakwater is the sole tailnet-facing entry point, the
# same model as every other fleet service. The SSE stream (sessions#stream) must
# pass through unbuffered — breakwater's proxy is configured to preserve
# X-Accel-Buffering: no-cache and to not buffer upstream SSE frames.
#
# The container reaches the host's pi bridge via host.docker.internal (added
# below). Rails reads OPENCODE_SERVER_URL and OPENCODE_SERVER_PASSWORD from the
# environment (see app/lib/opencode_client.rb); the initializer also reads
# ~/.config/skiff/secrets when running outside Docker, but inside the container
# the env vars are authoritative.
#
# Memory is the binding constraint on this box (2GB total, shared with the rest
# of the fleet), so Puma runs a single worker and the container is capped. Left
# unbounded, a Rails app will happily use more than this VPS has. Threads are
# 5 (skiff's default) to accommodate one SSE stream per viewer (each occupies a
# Puma thread).
#
# The master key is mounted to config/master.key rather than passed as an env
# var: Rails reads that path natively, and there is no RAILS_MASTER_KEY_FILE
# variable. Keeping it out of the image also keeps it out of the shipped tar.
ExecStart=/usr/bin/docker run --rm --name ${NAME} \\
  -p 127.0.0.1:${PORT}:3000 \\
  --add-host=host.docker.internal:host-gateway \\
  -e RAILS_ENV=production \\
  -e WEB_CONCURRENCY=1 \\
  -e RAILS_MAX_THREADS=5 \\
  -e OPENCODE_SERVER_URL=\${OPENCODE_SERVER_URL} \\
  -e OPENCODE_SERVER_PASSWORD=\${OPENCODE_SERVER_PASSWORD} \\
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
echo "  1. tugboat deploy      (from the fleet checkout — ships the image and starts it)"
echo "  2. breakwater route is generated via 'cargo run -p tugboat -- fleet gen' (commit breakwater/breakwater.toml)"
echo "  3. deploy breakwater   (ships the new route)"
echo
echo "The service will not start until an image tar exists at ${IMAGE_TAR}."
echo "If the pi bridge is not yet on this host, the UI will boot but show"
echo "“opencode server unreachable” until the bridge is installed and"
echo "reachable at 127.0.0.1:4120 on the host."
