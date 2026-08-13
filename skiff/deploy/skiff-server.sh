#!/usr/bin/env bash
#
# skiff's production server wrapper — Fedora desktop template.
#
# deploy/install-desktop.sh substitutes the __SKIFF_DIR__ placeholder (the skiff
# repo's absolute path) and installs this to ~/.config/skiff/skiff-server.sh,
# chmod 700. systemd (user unit skiff.service) runs it directly.
#
# Design decisions, recorded so they are not re-litigated later:
#   - Assets are precompiled on every start because config.assets.compile is
#     off in production (propshaft only serves precompiled assets). An
#     already-up-to-date precompile is cheap and idempotent.
#   - The bind address is the tailnet IP resolved fresh at each start: a node's
#     tailnet IP is not a constant, so it is resolved at runtime rather than
#     hard-coded. If tailscale is down, `tailscale ip -4` fails, this exits
#     non-zero, and systemd retries (Restart=on-failure).
#   - Secrets are deliberately NOT read here: the Rails initializer
#     config/initializers/skiff_env.rb loads ~/.config/skiff/secrets into ENV
#     (the pi bridge's basic-auth password), so the secrets never leave that one file.
#   - No mise logic, unlike the macOS launchd wrapper: on the desktop the repo's
#     ruby is the system Ruby 4.0.6, resolved through PATH, so `bin/rails` just
#     works. (The Mac wrapper needed `mise env` because launchd's login-shell
#     PATH lacks the mise ruby shims — a Mac-specific fix that does not apply
#     here; the desktop has no mise.)
set -euo pipefail

cd __SKIFF_DIR__
export RAILS_ENV=production

# Assets must exist before Rails serves them (config.assets.compile is off).
bin/rails assets:precompile

# Bind the tailnet IP, resolved fresh each start (a node's tailnet IP is not
# a constant). If tailscale is down this exits non-zero and systemd retries.
exec bin/rails server -b "$(tailscale ip -4)" -p 8120
