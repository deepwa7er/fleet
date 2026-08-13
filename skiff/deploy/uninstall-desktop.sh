#!/usr/bin/env bash
#
# Removes the systemd user units installed by deploy/install-desktop.sh:
# disables and stops both units, then deletes the installed wrappers and unit
# files. Safe to re-run: rm -f on missing files is fine, and `disable --now`
# on a unit that is not enabled/active is a no-op success.
set -euo pipefail

UNIT_INSTALL_DIR="${HOME}/.config/systemd/user"
WRAPPER_INSTALL_DIR="${HOME}/.config/skiff"

systemctl --user disable --now skiff.service com.deepwa7er.pi-bridge.service

rm -f \
  "${UNIT_INSTALL_DIR}/skiff.service" \
  "${UNIT_INSTALL_DIR}/com.deepwa7er.pi-bridge.service" \
  "${WRAPPER_INSTALL_DIR}/skiff-server.sh" \
  "${WRAPPER_INSTALL_DIR}/pi-bridge.sh"

systemctl --user daemon-reload

echo "Uninstalled."
