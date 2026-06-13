#!/usr/bin/env bash
#
# Build ferry as a static Linux binary and deploy it to the VPS.
#
# Re-runnable: it rebuilds, ships, restarts the service, health-checks it, and
# rolls the binary back if the new one fails to come up. It does NOT touch the
# config, the systemd unit, or the tailscale serve mapping — first-time setup of
# those is documented in the README under "Run on a Linux VPS".
#
# Requires a musl cross-toolchain on the build machine (see README). Configure
# your host either via the environment (e.g. FERRY_SSH_HOST=myvps) or by
# creating an untracked `deploy.local` next to this script with lines like:
#   FERRY_SSH_HOST=myvps
#   FERRY_URL=https://myvps.<tailnet>.ts.net:8443
set -euo pipefail

cd "$(dirname "$0")"

# Load local, untracked overrides if present (keeps real hostnames out of git).
[ -f deploy.local ] && . ./deploy.local

SSH_HOST="${FERRY_SSH_HOST:-your-vps}"
TARGET="${FERRY_TARGET:-x86_64-unknown-linux-musl}"
MUSL_LINKER="${FERRY_MUSL_LINKER:-x86_64-linux-musl-gcc}"
REMOTE_BIN="${FERRY_REMOTE_BIN:-/usr/local/bin/ferry}"
SERVICE="${FERRY_SERVICE:-ferry}"
HEALTH_PORT="${FERRY_HEALTH_PORT:-7777}"
FERRY_URL="${FERRY_URL:-https://your-vps.example.ts.net:8443}"

echo "==> Building release binary for $TARGET"
rustup target add "$TARGET" >/dev/null 2>&1 || true
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="$MUSL_LINKER" \
  cargo build --release --target "$TARGET"

local_bin="target/$TARGET/release/ferry"
[ -x "$local_bin" ] || { echo "error: build did not produce $local_bin" >&2; exit 1; }

echo "==> Uploading to $SSH_HOST"
scp -q "$local_bin" "$SSH_HOST:/tmp/ferry.new"

echo "==> Installing and restarting '$SERVICE' on $SSH_HOST"
ssh "$SSH_HOST" \
  "REMOTE_BIN=$REMOTE_BIN SERVICE=$SERVICE HEALTH_PORT=$HEALTH_PORT bash -s" <<'REMOTE'
set -euo pipefail
sudo=""; [ "$(id -u)" -eq 0 ] || sudo="sudo"

# Keep the current binary so a bad deploy can be undone.
[ -f "$REMOTE_BIN" ] && $sudo cp -a "$REMOTE_BIN" "$REMOTE_BIN.bak"
$sudo install -m 0755 /tmp/ferry.new "$REMOTE_BIN"
rm -f /tmp/ferry.new
$sudo systemctl restart "$SERVICE"

# Wait for the service to answer on loopback before declaring success.
# Failures are expected during the restart window, so they stay silent (-s, no
# -S); only the final verdict below is reported.
healthy=""
for _ in $(seq 1 10); do
  if curl -fs -o /dev/null "http://127.0.0.1:$HEALTH_PORT/commands"; then healthy=1; break; fi
  sleep 0.5
done

if [ -z "$healthy" ]; then
  echo "!! '$SERVICE' did not become healthy; rolling back" >&2
  if [ -f "$REMOTE_BIN.bak" ]; then
    $sudo mv "$REMOTE_BIN.bak" "$REMOTE_BIN"
    $sudo systemctl restart "$SERVICE"
  fi
  $sudo systemctl --no-pager --lines=20 status "$SERVICE" >&2 || true
  exit 1
fi

$sudo rm -f "$REMOTE_BIN.bak"
echo "   '$SERVICE' is active and healthy on 127.0.0.1:$HEALTH_PORT"
REMOTE

echo "==> Verifying over the tailnet"
if curl -fs -o /dev/null "$FERRY_URL/commands"; then
  echo "    Live at $FERRY_URL/"
else
  echo "    Deployed, but $FERRY_URL/commands is not reachable from here." >&2
  echo "    (Is this machine on the tailnet? The service itself is healthy.)" >&2
fi
