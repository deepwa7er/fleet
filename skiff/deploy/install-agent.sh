#!/usr/bin/env bash
#
# Installs (idempotently) the launchd agent that runs skiff in production.
#
#   ~/.config/skiff/skiff-server.sh                         the server wrapper
#   ~/Library/LaunchAgents/com.deepwa7er.skiff.plist        the launchd agent
#
# The wrapper SOURCE is deploy/skiff-server.mac.sh (the macOS variant — the
# Linux/desktop wrapper is deploy/skiff-server.sh). Keep the two straight:
# this installer must never pick up the Linux wrapper, which omits the mise
# resolution the macOS launchd PATH needs.
#
# The wrapper is installed to ~/.config/skiff/ — same home as the pi bridge
# wrapper and the shared secrets file — and the plist references that
# INSTALLED wrapper path, not the repo's deploy/ copy, so the agent keeps
# working however the repo moves around.
#
# Safe to re-run: bootout of a not-loaded service is ignored, and bootstrap
# (re)loads the freshly written plist. Requires the skiff repo's deploy/
# directory (this file's sibling) to exist.
set -euo pipefail

SKIFF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WRAPPER_INSTALL_DIR="${HOME}/.config/skiff"
WRAPPER_DEST="${WRAPPER_INSTALL_DIR}/skiff-server.sh"
PLIST_DEST="${HOME}/Library/LaunchAgents/com.deepwa7er.skiff.plist"
LABEL="com.deepwa7er.skiff"

mkdir -p "${WRAPPER_INSTALL_DIR}"

# The wrapper: `cd __SKIFF_DIR__` (the repo) is baked in at install time.
sed "s|__SKIFF_DIR__|${SKIFF_DIR}|g" "${SKIFF_DIR}/deploy/skiff-server.mac.sh" \
  > "${WRAPPER_DEST}"
chmod 700 "${WRAPPER_DEST}"

# The plist: the wrapper is referenced at its installed path, and every
# remaining __SKIFF_DIR__ (the log paths) becomes the repo path. Substitution
# order matters — the longer wrapper path first, so the generic replacement
# below never touches it.
sed \
  -e "s|__SKIFF_DIR__/deploy/skiff-server.mac.sh|${WRAPPER_DEST}|" \
  -e "s|__SKIFF_DIR__|${SKIFF_DIR}|g" \
  "${SKIFF_DIR}/deploy/com.deepwa7er.skiff.plist" > "${PLIST_DEST}"

# Fail loudly on a malformed plist instead of bootstrapping a broken agent.
plutil -lint "${PLIST_DEST}"

launchctl bootout "gui/$(id -u)/${LABEL}" 2>/dev/null || true
launchctl bootstrap "gui/$(id -u)" "${PLIST_DEST}"

echo "Installed. skiff runs at http://$(tailscale ip -4):8120"
