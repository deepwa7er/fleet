#!/usr/bin/env bash
#
# Pack the harbor extension into a signed CRX3 and generate its update manifest,
# for self-hosted distribution: harbor-server serves both at /extension, and
# tailnet devices force-install + auto-update from them (see README).
#
# Needs a Chromium-family browser to pack and the private signing key — which
# DEFINES the extension ID, so keep it stable and backed up. Outputs to build/.
#
#   key    : $HARBOR_EXT_KEY    (default ~/.config/harbor/extension.pem)
#   host   : $HARBOR_EXT_HOST   (default the tailnet harbor URL)
#   packer : $HARBOR_EXT_PACKER (default Brave on macOS)
set -euo pipefail
cd "$(dirname "$0")/.."   # -> harbor repo root

KEY="${HARBOR_EXT_KEY:-$HOME/.config/harbor/extension.pem}"
HOST="${HARBOR_EXT_HOST:-http://deepwa7er.tailcfab97.ts.net:8090}"
PACKER="${HARBOR_EXT_PACKER:-/Applications/Brave Browser.app/Contents/MacOS/Brave Browser}"

[ -f "$KEY" ] || { echo "signing key not found: $KEY (generate once: openssl genrsa -out \"$KEY\" 2048)" >&2; exit 1; }
[ -x "$PACKER" ] || { echo "packer not found: $PACKER (set HARBOR_EXT_PACKER)" >&2; exit 1; }

mkdir -p build

# Extension ID = SHA256(DER SPKI public key)[:16], each nibble mapped 0-f -> a-p.
ID=$(openssl rsa -in "$KEY" -pubout -outform DER 2>/dev/null \
  | openssl dgst -sha256 -binary | head -c 16 \
  | python3 -c "import sys;print(''.join(chr(97+n) for b in sys.stdin.buffer.read() for n in (b>>4,b&15)))")

VERSION=$(python3 -c "import json;print(json.load(open('extension/manifest.json'))['version'])")

# Chromium writes <dir>.crx next to the dir, but doesn't reliably exit after a
# --pack-extension run, so poll for the output then stop the process.
rm -f extension.crx
"$PACKER" --pack-extension="$PWD/extension" --pack-extension-key="$KEY" --no-message-box >/dev/null 2>&1 &
pid=$!
for _ in $(seq 1 40); do [ -s extension.crx ] && break; sleep 0.5; done
kill "$pid" 2>/dev/null || true
wait "$pid" 2>/dev/null || true
[ -s extension.crx ] || { echo "pack failed: no extension.crx produced" >&2; exit 1; }
mv -f extension.crx build/harbor.crx

cat > build/updates.xml <<XML
<?xml version='1.0' encoding='UTF-8'?>
<gupdate xmlns='http://www.google.com/update2/response' protocol='2.0'>
  <app appid='$ID'>
    <updatecheck codebase='$HOST/extension/harbor.crx' version='$VERSION' />
  </app>
</gupdate>
XML

echo "packed build/harbor.crx  (id=$ID  version=$VERSION)"
