#!/usr/bin/env bash
# timeline/publish.sh — pull the record, rebuild the timeline, ship it.
#
# The publish pipeline in the [docs] shape (tugboat/src/docs.rs): assemble
# locally, rsync to a staged directory on the host, swap atomically —
# breakwater serves the directory, so there is no service to restart.
# Driven hourly by deploy/record-timeline.timer on the desktop; a run where
# the record has not moved since the last ship is a no-op, so the timer
# costs nothing between landings.
#
# Requires: the record checkout (default ~/code/record), node >= 22, and
# ssh access to the VPS (the `vps` alias). All overridable for testing.
set -euo pipefail

RECORD_DIR="${RECORD_DIR:-$HOME/code/record}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="${TIMELINE_OUT:-$HERE/dist}"
SHIP_HOST="${TIMELINE_HOST:-vps}"
SHIP_DEST="${TIMELINE_DEST:-/opt/record/web}"
STAMP="${TIMELINE_STAMP:-$HOME/.local/share/timeline/shipped-head}"

git -C "$RECORD_DIR" pull --ff-only --quiet

head="$(git -C "$RECORD_DIR" rev-parse HEAD)"
if [[ -f "$STAMP" && "$(cat "$STAMP")" == "$head" ]]; then
  echo "timeline: record unchanged at ${head:0:12}; nothing to ship"
  exit 0
fi

node "$HERE/build.mjs" --record "$RECORD_DIR" --out "$OUT_DIR"

# Staged swap, mirroring tugboat's docs ship: the live directory is replaced
# in one mv, and the previous tree survives as .tug-bak only until the swap
# lands. mkdir -p first so the very first ship needs no manual provisioning.
staged="$SHIP_DEST.tug-new"
rsync -az --delete --rsync-path="mkdir -p '$(dirname "$SHIP_DEST")' && rsync" "$OUT_DIR"/ "$SHIP_HOST:$staged/"
# shellcheck disable=SC2029 — SHIP_DEST expands locally by design.
ssh "$SHIP_HOST" "rm -rf '$SHIP_DEST.tug-bak' && { [ -e '$SHIP_DEST' ] && mv '$SHIP_DEST' '$SHIP_DEST.tug-bak' || true; } && mv '$staged' '$SHIP_DEST' && rm -rf '$SHIP_DEST.tug-bak'"

mkdir -p "$(dirname "$STAMP")"
printf '%s\n' "$head" > "$STAMP"
echo "timeline: shipped ${head:0:12} to $SHIP_HOST:$SHIP_DEST"
