#!/usr/bin/env bash
#
# Build harbor-server on the VPS and (re)install it as a systemd service.
#
# Re-runnable. Run deploy/setup-vps.sh ONCE first (it creates the secondbrain
# hub repo and the 'harbor' service user). Mirrors lighthouse's deploy model:
# rsync the source up, build the release on the VPS, install binary + unit.
#
# Host: set HARBOR_SSH_HOST (defaults to the `deepwa7er` ssh alias).
set -euo pipefail
cd "$(dirname "$0")/.."   # -> server/

: "${HARBOR_SSH_HOST:=deepwa7er}"
SRC=/srv/harbor/server-src

# 1. Ship the source (skip build artifacts and local-only config).
rsync -az --delete \
  --exclude target \
  --exclude harbor.local.toml \
  ./ "$HARBOR_SSH_HOST:$SRC/"

# 2. Build + install on the VPS.
ssh "$HARBOR_SSH_HOST" SRC="$SRC" bash -s <<'EOF'
set -euo pipefail

# Ensure a Rust toolchain (auto-install like lighthouse does on first deploy).
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
fi
export PATH="$HOME/.cargo/bin:$PATH"

cargo build --release --manifest-path "$SRC/Cargo.toml"
install -m 0755 "$SRC/target/release/harbor-server" /usr/local/bin/harbor-server

# Install config on first deploy only (never clobber a live config).
mkdir -p /etc/harbor
if [ ! -f /etc/harbor/harbor.toml ]; then
  install -m 0644 "$SRC/harbor.toml" /etc/harbor/harbor.toml
  echo "installed /etc/harbor/harbor.toml (edit bind/IP if needed)"
fi

install -m 0644 "$SRC/deploy/harbor.service" /etc/systemd/system/harbor.service
systemctl daemon-reload
systemctl enable --now harbor
systemctl restart harbor
sleep 1
systemctl --no-pager --full status harbor | head -12
EOF

echo
echo "Deployed. Health check from the tailnet:"
echo "  curl http://100.98.184.58:8090/healthz"
echo "Then enroll in lighthouse:  systemctl add-wants lighthouse.target harbor.service"
