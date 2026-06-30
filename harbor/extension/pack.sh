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
HOST="${HARBOR_EXT_HOST:-https://harbor.intern.deepwa7er.net}"
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

# Human-facing install page, served at $HOST/extension/ alongside the crx. A
# Chromium browser won't install a .crx straight from a link (by design), so the
# page walks through the drag-onto-chrome://extensions flow. Once installed the
# extension auto-updates from updates.xml, so this is a one-time step.
cat > build/index.html <<HTML
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>harbor extension</title>
<style>
  :root {
    --bg:#0d0f12; --surface:#14181d; --ink:#c9d1d9; --ink-muted:#7d8694;
    --rule:#222a31; --accent:#4dabf7;
    --font-mono:"Berkeley Mono","JetBrains Mono","IBM Plex Mono",ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;
  }
  * { box-sizing:border-box; }
  body { margin:0; background:var(--bg); color:var(--ink); font-family:var(--font-mono);
         font-size:14px; line-height:1.6; padding:3rem 1.25rem; }
  main { max-width:42rem; margin:0 auto; }
  h1 { font-size:1.4rem; margin:0; letter-spacing:.5px; }
  .sub { color:var(--ink-muted); margin:.25rem 0 2rem; }
  .dl { display:inline-block; background:var(--accent); color:#04121f; font-weight:700;
        text-decoration:none; padding:.7rem 1.4rem; border:1px solid var(--accent);
        text-transform:uppercase; letter-spacing:.5px; }
  .dl:hover { background:#1864ab; border-color:#1864ab; color:#fff; }
  .ver { color:var(--ink-muted); font-size:12px; margin:.6rem 0 2.5rem; }
  section { border:1px solid var(--rule); background:var(--surface); padding:1.25rem 1.5rem; }
  h2 { font-size:11px; text-transform:uppercase; letter-spacing:1.5px; color:var(--ink-muted);
       margin:0 0 .75rem; padding-bottom:.5rem; border-bottom:1px solid var(--rule); }
  ol { margin:0; padding-left:1.2rem; }
  li { margin:.35rem 0; }
  code { background:var(--bg); border:1px solid var(--rule); padding:0 .3em; color:var(--accent); }
  .note { color:var(--ink-muted); font-size:12px; margin:1rem 0 0; }
  a { color:var(--accent); }
</style>
</head>
<body>
<main>
  <h1>harbor</h1>
  <p class="sub">fleet new-tab extension — activity pulse + lagoon capture</p>
  <a class="dl" href="/download/harbor.crx" download>Download harbor.crx</a>
  <p class="ver">version $VERSION · auto-updates from this server once installed</p>
  <section>
    <h2>Install (one time)</h2>
    <ol>
      <li>Download <code>harbor.crx</code> above.</li>
      <li>Open your browser's extensions page — <code>chrome://extensions</code>, <code>brave://extensions</code>, <code>helium://extensions</code>.</li>
      <li>Turn on <strong>Developer mode</strong> (top-right toggle).</li>
      <li>Drag <code>harbor.crx</code> from your downloads onto the page and confirm <em>Add extension</em>.</li>
    </ol>
    <p class="note">After this it updates itself from <code>$HOST/extension/updates.xml</code> — no need to repeat. If you still have an older copy from the previous domain, remove it first.</p>
  </section>
</main>
</body>
</html>
HTML

echo "packed build/harbor.crx  (id=$ID  version=$VERSION)"
echo "wrote  build/index.html + build/updates.xml"
