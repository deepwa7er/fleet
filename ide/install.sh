#!/usr/bin/env bash
# Installs the fleet IDE client on this machine (macOS or Linux) and
# preflights the remote transport. Idempotent - safe to re-run; each step
# checks before it acts. Long-form guide: docs/remote.md section 10.
#
# Run it from inside a fleet checkout (./ide/install.sh) to use that
# checkout, or standalone to clone into ~/code/fleet (override: FLEET_DIR).
# The remote host defaults to the `desktop` ssh alias (override:
# IDE_REMOTE_HOST).
set -euo pipefail

say()  { printf '\033[1m[ide install]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[ide install]\033[0m %s\n' "$*" >&2; exit 1; }

REMOTE_HOST="${IDE_REMOTE_HOST:-desktop}"

# 0. Prefer the checkout this script lives in; fall back to ~/code/fleet.
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if top="$(git -C "$script_dir" rev-parse --show-toplevel 2>/dev/null)"; then
    FLEET_DIR="$top"
else
    FLEET_DIR="${FLEET_DIR:-$HOME/code/fleet}"
fi

# 1. Xcode Command Line Tools (macOS). The CLT installer is a GUI dialog we
#    cannot wait on, so finish it and re-run. If the build later fails around
#    metal/xcrun, full Xcode is the fallback - see docs/remote.md section 10.
if [ "$(uname -s)" = Darwin ] && ! xcode-select -p >/dev/null 2>&1; then
    say "Xcode Command Line Tools are missing - starting their installer."
    xcode-select --install || true
    fail "Finish the Command Line Tools install, then re-run this script."
fi

# 2. Rust. rustup's env file covers shells that haven't picked up the PATH yet.
if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
    # shellcheck source=/dev/null
    . "$HOME/.cargo/env"
fi
if ! command -v cargo >/dev/null 2>&1; then
    say "Installing Rust via rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    # shellcheck source=/dev/null
    . "$HOME/.cargo/env"
fi

# 3. The fleet checkout. (rev-parse, not a `.git` directory test - in a git
#    worktree `.git` is a file.)
if ! git -C "$FLEET_DIR" rev-parse --git-dir >/dev/null 2>&1; then
    say "Cloning the fleet into $FLEET_DIR..."
    mkdir -p "$(dirname "$FLEET_DIR")"
    if command -v gh >/dev/null 2>&1; then
        gh repo clone deepwa7er/fleet "$FLEET_DIR"
    else
        git clone git@github.com:deepwa7er/fleet.git "$FLEET_DIR"
    fi
else
    say "Updating fleet checkout at $FLEET_DIR..."
    git -C "$FLEET_DIR" pull --ff-only \
        || say "warning: pull failed (diverged or dirty checkout) - building what's here"
fi

# 4. Build + install the client. Release profile; the first build compiles
#    the whole gpui stack - expect 10-20 minutes, then it's incremental.
say "Building and installing ide (the first build is the long part)..."
# --locked: build the exact dependency revisions Cargo.lock pins (the tested
# ones) - cargo install re-resolves without it.
cargo install --locked --path "$FLEET_DIR/ide" --bin ide

# 5. Preflight the remote transport. 255 is ssh itself failing (host down,
#    auth); anything else means the host answered but the server is missing.
say "Preflight: ide-server on '$REMOTE_HOST'..."
if out="$(ssh -o BatchMode=yes -o ConnectTimeout=10 "$REMOTE_HOST" \
        '~/.cargo/bin/ide-server --version' 2>&1)"; then
    say "Server answered: $out"
    say "Done. Open the fleet remotely with:  ide $REMOTE_HOST:code/fleet"
else
    status=$?
    say "Client installed, but the preflight failed (exit $status):"
    printf '%s\n' "$out" >&2
    if [ "$status" -eq 255 ]; then
        say "ssh could not reach '$REMOTE_HOST' - it is usually powered off; check 'tailscale status'."
    else
        say "The host answered but ide-server is missing there. On '$REMOTE_HOST' run:"
        say "  cd ~/code/fleet && git pull && cargo install --locked --path ide --bin ide-server"
    fi
    say "Local mode works regardless:  ide <path>"
fi
