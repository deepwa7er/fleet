#!/usr/bin/env bash
#
# Persistent pi bridge (opencode-compatible API) for skiff (the phone web UI)
# — Fedora desktop template.
#
# deploy/install-desktop.sh substitutes the __SKIFF_DIR__ placeholder (the
# skiff repo's absolute path) and installs this to ~/.config/skiff/pi-bridge.sh,
# chmod 700. systemd (user unit com.deepwa7er.pi-bridge.service) runs it
# directly.
#
# Design decisions, recorded so they are not re-litigated later:
#   - The working directory matters: the bridge uses PI_BRIDGE_CWD (default
#     ~/code — the user's projects directory, the same convention as the Mac)
#     as the cwd for the pi processes it spawns / new sessions. cd'ing here
#     keeps parity with the old opencode wrapper and the bridge's own default.
#     $HOME keeps the wrapper machine-agnostic; no per-user absolute paths are
#     baked in.
#   - Secrets are parsed, never `source`d: the file is plain KEY=VALUE with
#     values taken verbatim, and a `$` in a value would be expanded by a
#     `source` as a shell parameter and die under `set -u` (a real bug on both
#     hosts with the old opencode server). The loop below matches the Rails
#     initializer's parsing exactly, so the two consumers agree on the format.
#   - The bridge binds loopback only by default (PI_BRIDGE_HOST default
#     127.0.0.1): skiff (the Rails app) is the only consumer and reaches it
#     over 127.0.0.1. Port 4120 is the opencode-compatible API's contract —
#     the Rails client's default URL is unchanged (the old "avoid 4096 /
#     RubyMine" rationale was opencode-specific and died with opencode serve).
#   - `node` is resolved from PATH: systemd user units include /usr/bin in
#     their default PATH, and the desktop's node is /usr/bin/node. (The old
#     wrapper's absolute-path comment was about the opencode go binary in
#     ~/.opencode/bin, which is not on the unit's PATH — node has no such
#     problem.) The bridge resolves `pi` itself (PI_BINARY, default "pi").
set -euo pipefail

# Export the secrets to the exec'd child (see design notes above). Values are
# taken literally — no shell expansion — so a `$` in a value is data, not a
# parameter reference.
while IFS= read -r _skiff_line; do
  case "$_skiff_line" in
    "" | "#"*) continue ;;
  esac
  _skiff_key="${_skiff_line%%=*}"
  _skiff_value="${_skiff_line#*=}"
  if [[ -n "$_skiff_key" && -n "$_skiff_value" ]]; then
    export "$_skiff_key=$_skiff_value"
  fi
done < "$HOME/.config/skiff/secrets"
unset _skiff_line _skiff_key _skiff_value

cd "$HOME/code"

# `node` resolves through PATH (see design notes above).
exec node __SKIFF_DIR__/bridge/server.js
