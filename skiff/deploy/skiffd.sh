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
#   - No secrets. skiffd has none: `dw` links the change crate directly and
#     agents use `dw`, so the bridge password that the Rails stack needs does
#     not exist here (DW-004 §2).
set -euo pipefail

export SKIFF_WEB_DIST="${HOME}/.local/share/skiffd/web"
export SKIFF_STORE="${HOME}/.local/state/skiff/read-model.sqlite3"
export RUST_LOG="${RUST_LOG:-skiff=info}"

exec "${HOME}/.local/bin/skiffd" --addr "$(tailscale ip -4):8121"
