#!/usr/bin/env bash
#
# Deploy Lighthouse to the VPS.
#
#   1. Build the React frontend locally (the VPS's Node is too old for Vite).
#   2. Rsync the Rust sources + built frontend to the VPS.
#   3. On the VPS: install the Rust toolchain if needed, build the release
#      binary, and install the binary, assets, config, service user, and unit.
#
# The dashboard binds to the VPS's Tailscale IP so it is reachable only from the
# tailnet. If Tailscale isn't logged in yet (`tailscale up`), the script still
# installs everything but leaves the service stopped and tells you what to run.
#
# Usage:  deploy/deploy.sh            (host defaults to the `deepwa7er` ssh alias)
#         LIGHTHOUSE_HOST=myalias deploy/deploy.sh
set -euo pipefail

HOST="${LIGHTHOUSE_HOST:-deepwa7er}"
REMOTE_BUILD="/opt/lighthouse/build"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

echo ">> Building frontend locally..."
(cd "$PROJECT_DIR/web" && bun install && bun run build)

echo ">> Syncing sources to $HOST:$REMOTE_BUILD ..."
ssh "$HOST" "mkdir -p '$REMOTE_BUILD'"
rsync -az --delete \
  "$PROJECT_DIR/Cargo.toml" \
  "$PROJECT_DIR/Cargo.lock" \
  "$PROJECT_DIR/lighthouse.toml" \
  "$HOST:$REMOTE_BUILD/"
rsync -az --delete "$PROJECT_DIR/src/" "$HOST:$REMOTE_BUILD/src/"
rsync -az --delete "$PROJECT_DIR/web/dist/" "$HOST:$REMOTE_BUILD/web-dist/"
rsync -az "$SCRIPT_DIR/lighthouse.service" "$HOST:$REMOTE_BUILD/lighthouse.service"

echo ">> Building and installing on $HOST ..."
ssh "$HOST" 'bash -s' <<'REMOTE'
set -euo pipefail
BUILD=/opt/lighthouse/build

# --- Rust toolchain ---------------------------------------------------------
if ! command -v cargo >/dev/null 2>&1 && [ ! -f "$HOME/.cargo/env" ]; then
  echo ">> Installing rustup (minimal profile)..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --profile minimal --default-toolchain stable
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"

# --- Build ------------------------------------------------------------------
echo ">> Building release binary..."
cd "$BUILD"
cargo build --release

# --- Install binary + assets ------------------------------------------------
install -Dm755 target/release/lighthouse /usr/local/bin/lighthouse
mkdir -p /opt/lighthouse/web
rm -rf /opt/lighthouse/web/*
cp -r web-dist/. /opt/lighthouse/web/

# --- Service user (least privilege: journal read only) ----------------------
if ! id lighthouse >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin lighthouse
fi

# --- Config -----------------------------------------------------------------
mkdir -p /etc/lighthouse
[ -f /etc/lighthouse/config.toml ] || install -m644 lighthouse.toml /etc/lighthouse/config.toml

# Auto-fill the bind address only while it is still the baseline placeholder —
# this covers both the first deploy and a re-deploy after `tailscale up`, and
# never clobbers a bind you have deliberately edited.
TSIP="$(tailscale ip -4 2>/dev/null | head -1 || true)"
if [ -n "$TSIP" ] && grep -q '^bind = "100.64.0.1"' /etc/lighthouse/config.toml; then
  sed -i "s|^bind = .*|bind = \"$TSIP\"|" /etc/lighthouse/config.toml
  echo ">> Bound dashboard to Tailscale IP $TSIP"
fi
chown -R lighthouse:lighthouse /etc/lighthouse /opt/lighthouse/web

# --- systemd unit -----------------------------------------------------------
install -m644 lighthouse.service /etc/systemd/system/lighthouse.service
systemctl daemon-reload

# Enable (on boot) + start only if Tailscale is up; otherwise binding to the
# 100.x IP would fail, so we leave the unit installed but inactive rather than
# enabling a service that would crash-loop after a reboot.
if [ -n "$TSIP" ]; then
  systemctl enable lighthouse.service >/dev/null 2>&1 || true
  systemctl restart lighthouse.service
  sleep 1
  systemctl --no-pager --lines=0 status lighthouse.service | head -4
  PORT="$(grep -oP '^port = \K[0-9]+' /etc/lighthouse/config.toml)"
  echo ">> Lighthouse is up at http://$TSIP:$PORT"
else
  echo "!! Tailscale is not logged in yet, so the dashboard was installed but not started."
  echo "!! Run:  sudo tailscale up"
  echo "!! Then re-run this deploy script (it will set the bind IP, enable, and start the service)."
fi
REMOTE

echo ">> Done."
