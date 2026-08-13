#!/usr/bin/env bash
#
# skiff's production server wrapper — macOS launchd template (kept distinct
# from deploy/skiff-server.sh, which is the Linux/desktop wrapper: this one
# resolves the Ruby toolchain via mise because a launchd-spawned fish -l PATH
# would otherwise pick the system Ruby).
#
# deploy/install-agent.sh substitutes the __SKIFF_DIR__ placeholder (the skiff
# repo's absolute path) and installs this to ~/.config/skiff/skiff-server.sh,
# chmod 700. launchd runs it through a login fish shell, which gets the full
# user PATH (tailscale, mise) rather than launchd's bare default; the repo's
# ruby is still resolved explicitly via mise — see the design notes below.
#
# Design decisions, recorded so they are not re-litigated later:
#   - Assets are precompiled on every start because config.assets.compile is
#     off in production (propshaft only serves precompiled assets). An
#     already-up-to-date precompile is cheap and idempotent.
#   - The bind address is the tailnet IP resolved fresh at each start: a node's
#     tailnet IP is not a constant, so it is resolved at runtime rather than
#     hard-coded. If tailscale is down, `tailscale ip -4` fails, this exits
#     non-zero, and launchd retries (KeepAlive).
#   - Secrets are deliberately NOT read here: the Rails initializer
#     config/initializers/skiff_env.rb loads ~/.config/skiff/secrets into ENV
#     (the pi bridge's basic-auth password), so the secrets never leave that one file.
#   - The Ruby toolchain is resolved via mise rather than the login shell's
#     PATH: the PATH a launchd-spawned `fish -l` gets is the user's universal
#     PATH, not the interactive one, and there the mise ruby bin sits after
#     /usr/bin — so `env ruby` would pick the system Ruby (2.6 on macOS) and
#     `bin/rails` would die. `mise env` puts the repo's ruby (from
#     .ruby-version) first while keeping the rest of the PATH (tailscale etc.).
set -euo pipefail

cd __SKIFF_DIR__
export RAILS_ENV=production

# Captured first so a failing `mise env` aborts the script (set -e) instead of
# silently eval'ing nothing and then failing later with a system-Ruby backtrace.
# --shell=bash: without it mise would emit fish syntax when this bash wrapper
# is launched from the login fish shell.
mise_env="$(mise env --shell=bash)"
eval "${mise_env}"

# Assets must exist before Rails serves them (config.assets.compile is off).
bin/rails assets:precompile

# Bind the tailnet IP, resolved fresh each start (a node's tailnet IP is not
# a constant). If tailscale is down this exits non-zero and launchd retries.
exec bin/rails server -b "$(tailscale ip -4)" -p 8120
