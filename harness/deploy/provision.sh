#!/bin/sh
# One-time (per change) setup for harness on this MacBook: native release
# build, install the binary to ~/.local/bin, and register a launchd agent that
# keeps `harness serve` running while the user is logged in. Idempotent.
#
#   harness/deploy/provision.sh
#
# Undo:
#   launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/net.deepwa7er.harness.plist
#   rm ~/Library/LaunchAgents/net.deepwa7er.harness.plist ~/.local/bin/harness
set -eu

# This script lives at harness/deploy/provision.sh in the fleet monorepo;
# the workspace root is two directories up.
cd "$(dirname "$0")/../.."

echo "==> building harness (release)"
cargo build --release -p harness

echo "==> installing to ~/.local/bin/harness"
mkdir -p "$HOME/.local/bin"
install -m 0755 target/release/harness "$HOME/.local/bin/harness"

PLIST_SRC="harness/deploy/net.deepwa7er.harness.plist"
PLIST_DST="$HOME/Library/LaunchAgents/net.deepwa7er.harness.plist"

echo "==> installing $PLIST_DST"
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
sed "s|__HOME__|$HOME|g" "$PLIST_SRC" > "$PLIST_DST"

UID_NUM=$(id -u)
if launchctl print "gui/$UID_NUM/net.deepwa7er.harness" >/dev/null 2>&1; then
    echo "==> replacing running agent"
    launchctl bootout "gui/$UID_NUM" "$PLIST_DST"
fi
launchctl bootstrap "gui/$UID_NUM" "$PLIST_DST"
launchctl kickstart "gui/$UID_NUM/net.deepwa7er.harness"

echo "==> done. Log: ~/Library/Logs/harness.log"
# Report the address the server actually bound (it auto-discovers the tailnet
# IP and logs its choice; multiple tailnet nodes make `tailscale ip -4`
# unreliable as a predictor).
sleep 2
BOUND=$(grep -o 'http://[0-9.]*:[0-9]*' "$HOME/Library/Logs/harness.log" | tail -1 || true)
if [ -n "$BOUND" ]; then
    echo "    UI: $BOUND (tailnet only)"
else
    echo "    UI: not up yet — see ~/Library/Logs/harness.log"
fi
