#!/usr/bin/env bash
#
# skiffd's production wrapper — Fedora desktop.
#
# deploy/install-skiffd.sh installs this to ~/.config/skiff/skiffd.sh (chmod
# 700) and the systemd user unit runs it directly.
#
# Design decisions, recorded so they are not re-litigated:
#
#   - The bind address is the tailnet IP resolved fresh at each start. A node's
#     tailnet IP is not a constant, so it cannot be baked in; and binding
#     0.0.0.0 would put an UNAUTHENTICATED app that can drive a coding agent
#     onto the desktop's LAN. The tailnet is the boundary, deliberately and
#     only. If tailscale is down, `tailscale ip -4` fails, this exits non-zero,
#     and systemd retries.
#
#   - The binary and the client bundle are installed to fixed paths rather than
#     read out of a checkout, so the unit does not break when the repo moves,
#     is rebased, or is being worked in. `install-skiffd.sh` is what copies a
#     freshly built pair into place.
#
#   - No bridge secret. The Rails bridge password disappears. Landing's
#     optional tugboat write uses tugboat's existing 0600 token file; Fizzy
#     reads its existing token file directly. Nothing is copied into Skiff.
set -euo pipefail

export SKIFF_WEB_DIST="${HOME}/.local/share/skiffd/web"
export SKIFF_STORE="${HOME}/.local/state/skiff/read-model.sqlite3"
export RUST_LOG="${RUST_LOG:-skiff=info}"

# systemd's PATH deliberately need not know Cargo. Resolve jj once at boot so
# a missing binary is reported by the change view rather than by an interactive
# shell assumption.
if [ -x "${HOME}/.cargo/bin/jj" ]; then
  export JJ_BINARY="${HOME}/.cargo/bin/jj"
elif [ -x "${HOME}/.local/bin/jj" ]; then
  export JJ_BINARY="${HOME}/.local/bin/jj"
fi

# Token-gated off when tugboat has not provisioned its hooks. The URL and
# token are the daemon's own configuration, so the two clients cannot drift.
TUGBOAT_CONFIG="${XDG_CONFIG_HOME:-${HOME}/.config}/tugboat"
if [ -r "${TUGBOAT_CONFIG}/serve-url" ] && [ -r "${TUGBOAT_CONFIG}/serve-token" ]; then
  export TUGBOAT_SERVE_URL="$(<"${TUGBOAT_CONFIG}/serve-url")"
  export TUGBOAT_SERVE_TOKEN="$(<"${TUGBOAT_CONFIG}/serve-token")"
fi

exec "${HOME}/.local/bin/skiffd" --addr "$(tailscale ip -4):8121"
