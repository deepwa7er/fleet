#!/usr/bin/env bash
#
# Installs (idempotently) the systemd user units that run skiff in production
# on the Fedora desktop.
#
#   ~/.config/skiff/skiff-server.sh                        the server wrapper
#   ~/.config/skiff/skiff-bridge.sh                        the bridge wrapper
#   ~/.config/systemd/user/skiff.service                   the skiff user unit
#   ~/.config/systemd/user/skiff-bridge.service            the bridge unit
#   ~/.config/systemd/user/opencode-serve.service          opencode harness
#
# Mirrors deploy/install-agent.sh (the macOS launchd path) for systemd *user*
# units: everything lives under $HOME, so there is no sudo anywhere.
#
# The wrappers are installed to ~/.config/skiff/ — same home as the shared
# secrets file — and the units reference those INSTALLED wrapper paths, not the
# repo's deploy/ copies, so they keep working however the repo moves around.
#
# Safe to re-run: the files are rewritten, then `daemon-reload` and
# `enable --now` are idempotent (enable --now just re-enables/re-starts an
# already-active unit). Requires the skiff repo's deploy/ directory (this
# file's sibling) to exist.
set -euo pipefail

SKIFF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WRAPPER_INSTALL_DIR="${HOME}/.config/skiff"
UNIT_INSTALL_DIR="${HOME}/.config/systemd/user"

mkdir -p "${WRAPPER_INSTALL_DIR}" "${UNIT_INSTALL_DIR}"

# The wrappers. Both get __SKIFF_DIR__ baked in at install time: skiff-server.sh
# cds into the repo; skiff-bridge.sh execs the bridge (the repo's
# bridge/server.js).
sed "s|__SKIFF_DIR__|${SKIFF_DIR}|g" "${SKIFF_DIR}/deploy/skiff-server.sh" \
  > "${WRAPPER_INSTALL_DIR}/skiff-server.sh"
chmod 700 "${WRAPPER_INSTALL_DIR}/skiff-server.sh"

sed "s|__SKIFF_DIR__|${SKIFF_DIR}|g" "${SKIFF_DIR}/deploy/skiff-bridge.sh" \
  > "${WRAPPER_INSTALL_DIR}/skiff-bridge.sh"
chmod 700 "${WRAPPER_INSTALL_DIR}/skiff-bridge.sh"

# skiff.service: ExecStart is substituted to the INSTALLED wrapper path first,
# then the generic __SKIFF_DIR__ becomes the repo path (the log paths).
# Substitution order matters — the longer wrapper path first, so the generic
# replacement below never touches it (same trick as the Mac installer).
sed \
  -e "s|__SKIFF_DIR__/deploy/skiff-server.sh|${HOME}/.config/skiff/skiff-server.sh|" \
  -e "s|__SKIFF_DIR__|${SKIFF_DIR}|g" \
  "${SKIFF_DIR}/deploy/skiff.service" > "${UNIT_INSTALL_DIR}/skiff.service"

# skiff-bridge.service and opencode-serve.service have no __SKIFF_DIR__
# placeholders — their ExecStarts use the systemd user specifier %h for the
# home directory.
cp "${SKIFF_DIR}/deploy/skiff-bridge.service" \
  "${UNIT_INSTALL_DIR}/skiff-bridge.service"
cp "${SKIFF_DIR}/deploy/opencode-serve.service" \
  "${UNIT_INSTALL_DIR}/opencode-serve.service"

# The bridge's pre-multi-harness identity: retire the old unit name and
# wrapper if this host still carries them, so two bridges never race for
# port 4120.
if systemctl --user list-unit-files com.deepwa7er.pi-bridge.service >/dev/null 2>&1; then
  systemctl --user disable --now com.deepwa7er.pi-bridge.service 2>/dev/null || true
fi
rm -f "${UNIT_INSTALL_DIR}/com.deepwa7er.pi-bridge.service" \
      "${WRAPPER_INSTALL_DIR}/pi-bridge.sh"

systemctl --user daemon-reload
systemctl --user enable --now skiff.service skiff-bridge.service opencode-serve.service

# User services only start at boot when linger is enabled for the user; without
# it they start at the next login. `loginctl enable-linger` needs polkit
# permissions this user may not have, so a failure is tolerated — the services
# still run whenever the desktop user is logged in.
loginctl enable-linger "$USER" 2>/dev/null || true

echo "Installed. skiff runs at http://$(tailscale ip -4):8120"
