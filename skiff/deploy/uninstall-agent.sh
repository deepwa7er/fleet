#!/usr/bin/env bash
#
# Removes the skiff launchd agent installed by deploy/install-agent.sh:
# unloads the agent and deletes the two installed files. Safe to re-run (the
# files are rm -f'd and a not-loaded service bootout is ignored).
set -euo pipefail

LABEL="com.deepwa7er.skiff"
PLIST_DEST="${HOME}/Library/LaunchAgents/com.deepwa7er.skiff.plist"
WRAPPER_DEST="${HOME}/.config/skiff/skiff-server.sh"

launchctl bootout "gui/$(id -u)/${LABEL}" 2>/dev/null || true
rm -f "${PLIST_DEST}" "${WRAPPER_DEST}"

echo "Uninstalled."
