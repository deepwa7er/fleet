#!/usr/bin/env bash
#
# Builds and installs skiffd (the DW-004 rebuild) as a systemd user unit on the
# Fedora desktop, alongside the Rails stack it will eventually replace.
#
#   ~/.local/bin/skiffd                        the binary
#   ~/.local/share/skiffd/web/                 the client bundle
#   ~/.config/skiff/skiffd.sh                  the wrapper
#   ~/.config/systemd/user/skiffd.service      the unit
#
# Everything is under $HOME — no sudo anywhere. Safe to re-run: it rebuilds,
# replaces the installed artifacts, and `daemon-reload` plus `enable --now` are
# idempotent.
#
# The binary is installed rather than run from the checkout on purpose: the
# rebuild is being developed in a jj workspace, and a unit pointing into a
# working copy would break the moment that copy is rebased or removed.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="${HOME}/.local/bin"
SHARE_DIR="${HOME}/.local/share/skiffd"
STATE_DIR="${HOME}/.local/state/skiff"
WRAPPER_DIR="${HOME}/.config/skiff"
UNIT_DIR="${HOME}/.config/systemd/user"

command -v bun >/dev/null || { echo "bun is required to build the client" >&2; exit 1; }
command -v tailscale >/dev/null || { echo "tailscale is required (the bind address)" >&2; exit 1; }

echo "==> building the client"
(cd "${REPO}/web" && bun install --frozen-lockfile && bun run build)

echo "==> building skiffd"
(cd "${REPO}/.." && cargo build --release -p skiff)

echo "==> installing"
mkdir -p "${BIN_DIR}" "${SHARE_DIR}" "${STATE_DIR}" "${WRAPPER_DIR}" "${UNIT_DIR}"

# Install to a temporary name and rename: replacing a running binary in place
# fails with ETXTBSY, and a rename is atomic.
install -m 755 "${REPO}/../target/release/skiff" "${BIN_DIR}/.skiffd.new"
mv -f "${BIN_DIR}/.skiffd.new" "${BIN_DIR}/skiffd"

rm -rf "${SHARE_DIR}/web"
mkdir -p "${SHARE_DIR}/web"
cp -R "${REPO}/web/dist/." "${SHARE_DIR}/web/"

install -m 700 "${REPO}/deploy/skiffd.sh" "${WRAPPER_DIR}/skiffd.sh"
install -m 644 "${REPO}/deploy/skiffd.service" "${UNIT_DIR}/skiffd.service"

echo "==> starting"
systemctl --user daemon-reload
systemctl --user enable --now skiffd.service
systemctl --user restart skiffd.service

# Give it a moment, then say plainly whether it came up.
sleep 2
if systemctl --user is-active --quiet skiffd.service; then
  # The MagicDNS name, trimmed of its trailing dot — that is the name to use
  # from another machine, and the one breakwater's route points at.
  name="$(tailscale status --json | python3 -c \
    'import json,sys; print(json.load(sys.stdin)["Self"]["DNSName"].rstrip("."))')"
  echo "skiffd is running:"
  echo "  http://${name}:8121"
  echo "  http://$(tailscale ip -4):8121"
else
  echo "skiffd failed to start; last log lines:" >&2
  tail -20 "${STATE_DIR}/skiffd.log" >&2 || true
  exit 1
fi
