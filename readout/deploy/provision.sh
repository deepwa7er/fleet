#!/usr/bin/env bash
#
# One-time (or on-change) provisioning for readout on the VPS.
#
# Run this once before the first `tugboat deploy`, and again whenever this file
# changes. Routine deploys do NOT run it — they ship a new image tar and restart
# the unit (see ../deploy.toml).
#
#   scp deploy/provision.sh deepwa7er:/tmp/ && ssh deepwa7er 'bash /tmp/provision.sh'
#
# Unlike the Rust fleet services, readout runs as a Docker container: the VPS has
# Ruby 3.2.3 and this app needs 3.4.8, so a native install would mean maintaining
# a second Ruby on the host. The systemd unit below is still the thing tugboat
# restarts and health-checks, so the deploy transaction and rollback behave
# exactly like every other service.

set -euo pipefail

NAME=readout
PORT=8101
APP_DIR=/opt/readout
STORAGE_DIR="${APP_DIR}/storage"
TOKENS_DIR="${APP_DIR}/runner-tokens"
IMAGE_TAR="${APP_DIR}/readout-image.tar"
IMAGE_TAG=readout:deploy

echo "==> creating ${APP_DIR}"
mkdir -p "${STORAGE_DIR}"

# The container runs as uid/gid 1000 (the `rails` user in the image), so the
# storage directory it writes SQLite into must be owned by that uid on the host.
chown -R 1000:1000 "${STORAGE_DIR}"
chmod 0750 "${APP_DIR}"

# Rails needs the master key to read encrypted credentials. It is deliberately
# NOT baked into the image: the image tar is shipped over the network and lands
# in a world-readable-ish path, whereas this file is 0600 root-only.
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

# Runners live on other machines, so the dashboard needs their addresses. Docker
# containers on this host cannot resolve MagicDNS names — the daemon hands them
# public resolvers, which know nothing about the tailnet — so the NAMES are
# resolved here, on the host, and the resulting IPs are passed in.
#
# Resolved at unit start rather than baked into this file, for the reason
# breakwater documents: a node's tailnet IP is not a constant. An address change
# is repaired by restarting the service, not by editing config.
#
# There are two of them now. Load can be generated from the Mac or from the
# wired Fedora desktop, and which one produced a set of numbers changes how they
# read: the desktop shares a LAN with the Campfire host, while the Mac is on
# Wi-Fi, where a ~400KB room page can saturate the link before the application
# does.
#
# A machine that does not resolve is left out of the list entirely, and the
# dashboard simply does not offer it. That degradation is deliberate and is the
# ordinary state of both boxes: the desktop is usually powered off and the Mac
# is often asleep. A dashboard that still reads results is far better than one
# that fails to start because a laptop is closed.
echo "==> installing the runner address resolver"
cat > /usr/local/bin/readout-resolve-runners <<'RESOLVER'
#!/bin/sh
# Writes RUNNERS for readout.service: the generator machines that resolve right
# now, as compact JSON.
#
# Compact on purpose — no spaces. The unit passes this through as
# `-e RUNNERS=${RUNNERS}`, and while systemd does not word-split the ${VAR} form,
# a value with no whitespace in it cannot be split by anything downstream either.
set -eu

port="${RUNNER_PORT:-7881}"

# key:display name:MagicDNS name. The key must match what the runner on that box
# is started with (`bin/runner --machine <key>`), which is also what bin/run.sh
# writes into every run-config.txt. The dashboard compares the two and refuses to
# launch on a machine that answers under another machine's name.
MACHINES="mac:MacBook:deepwater-1.tailcfab97.ts.net
desktop:Fedora desktop:fedora.tailcfab97.ts.net"

entries=$(
  echo "$MACHINES" | while IFS= read -r machine; do
    [ -n "$machine" ] || continue
    key="${machine%%:*}"
    rest="${machine#*:}"
    name="${rest%%:*}"
    host="${rest#*:}"

    ip="$(getent ahostsv4 "$host" 2>/dev/null | awk '{print $1; exit}')"
    if [ -z "$ip" ]; then
      echo "readout: could not resolve ${host}; ${name} will not be offered" >&2
      continue
    fi

    printf '{"key":"%s","name":"%s","url":"http://%s:%s","token_file":"/rails/config/runner-tokens/%s"}\n' \
      "$key" "$name" "$ip" "$port" "$key"
  done | paste -sd, -
)

# Always written, even when empty. An unset RUNNERS makes the app fall back to
# its developer default — the runner on this same box, over loopback — which is
# not what is meant here: it would be a machine that does not exist. An empty
# list says plainly that nothing is reachable.
echo "RUNNERS=[${entries}]"
RESOLVER
chmod 0755 /usr/local/bin/readout-resolve-runners

# The secret generator machines present when publishing a finished run.
#
# Results are parsed where the load was generated and pushed here, so this is the
# one route into the app that writes without a person driving it. Generated here
# rather than copied in, because this host is the authority on it — each
# generator gets a copy of what this produces.
#
# Before the runner tokens below, deliberately: a generator cannot be set up
# without this, and this script must not be blocked from producing it by a
# machine that has not been set up yet.
if [ ! -f "${APP_DIR}/ingest-token" ]; then
  echo "==> generating an ingest token"
  head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n' > "${APP_DIR}/ingest-token"
  echo "" >> "${APP_DIR}/ingest-token"
  echo "   Copy it to each generator machine, into ~/.config/readout/ingest-token"
fi
chown 1000:1000 "${APP_DIR}/ingest-token"
chmod 0600 "${APP_DIR}/ingest-token"

# One token per machine: each runner generates its own on first start, so they
# are different secrets and the dashboard needs every one of them.
#
# A warning rather than a failure, and the difference matters: a machine's token
# legitimately arrives after this runs, because the runner has to start once to
# create it. The dashboard simply does not offer a machine it has no token for,
# which is the same way it treats one that is switched off.
echo "==> checking runner tokens"
mkdir -p "${TOKENS_DIR}"
for machine in mac desktop; do
  if [ ! -f "${TOKENS_DIR}/${machine}" ]; then
    echo "!! no token for '${machine}' — it will not be offered as a generator."
    echo "   Copy it from that box, which generates its own on first start."
    echo "   From the Mac, for itself:"
    echo "     cat ~/code/campfire-stress/.runner-token | ssh deepwa7er 'cat > ${TOKENS_DIR}/${machine}'"
    echo "   or for another machine:"
    echo "     ssh <alias> cat code/campfire-stress/.runner-token | ssh deepwa7er 'cat > ${TOKENS_DIR}/${machine}'"
  fi
done
# Readable by the container's uid 1000, like the master key.
chown -R 1000:1000 "${TOKENS_DIR}"
chmod 0700 "${TOKENS_DIR}"
find "${TOKENS_DIR}" -type f -exec chmod 0600 {} +

echo "==> writing systemd unit"
cat > /etc/systemd/system/${NAME}.service <<UNIT
[Unit]
Description=Readout — campfire-stress results dashboard
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
ExecStartPre=/bin/sh -c '/usr/local/bin/readout-resolve-runners > /run/readout-runners.env'
EnvironmentFile=-/run/readout-runners.env

# Binds loopback only: breakwater is the sole tailnet-facing entry point, the
# same model as every other fleet service.
#
# Memory is the binding constraint on this box (2GB total, shared with the rest
# of the fleet), so Puma runs a single worker and the container is capped. Left
# unbounded, a Rails app will happily use more than this VPS has.
#
# The master key is mounted to config/master.key rather than passed as an env
# var: Rails reads that path natively, and there is no RAILS_MASTER_KEY_FILE
# variable. Keeping it out of the image also keeps it out of the shipped tar.
ExecStart=/usr/bin/docker run --rm --name ${NAME} \\
  -p 127.0.0.1:${PORT}:80 \\
  -e RAILS_ENV=production \\
  -e WEB_CONCURRENCY=1 \\
  -e RAILS_MAX_THREADS=3 \\
  -e RUNNERS=\${RUNNERS} \\
  -e INGEST_TOKEN_FILE=/rails/config/ingest-token \\
  -v ${STORAGE_DIR}:/rails/storage \\
  -v ${APP_DIR}/master.key:/rails/config/master.key:ro \\
  -v ${TOKENS_DIR}:/rails/config/runner-tokens:ro \\
  -v ${APP_DIR}/ingest-token:/rails/config/ingest-token:ro \\
  --memory=400m \\
  --memory-swap=400m \\
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
echo "  1. tugboat deploy      (from the readout repo — ships the image and starts it)"
echo "  2. add the breakwater route and deploy breakwater"
echo
echo "The service will not start until an image tar exists at ${IMAGE_TAR}."
