# tidepool

Small Go service for shuttling files and a clipboard slot between your devices
(Fedora desktop, MacBook, iPhone) over Tailscale. Files auto-delete after 24h.
The iPhone "app" is a PWA served from the same binary.

## Architecture

- Single Go binary, embeds templates + static assets via `//go:embed`.
- Joins your tailnet via [`tsnet`](https://tailscale.com/kb/1244/tsnet) — no
  public ports, listens only on the tailscale interface.
- SQLite (pure-Go via `modernc.org/sqlite`) for metadata; blobs on local disk.
- Server-rendered HTML + [htmx](https://htmx.org) for partial updates.
- Hand-authored CSS (`internal/server/static/app.css`) — no build step, no
  Node/Tailwind toolchain. Styled after the DG-001 design guide (U.S. Graphics
  school): paper + ink, monospace, hairline rules, the files list as a real
  data table.
- Optional `tidepool-clipd` daemon per device: watches the OS clipboard,
  posts changes, and subscribes to `GET /clip/stream` (SSE) so remote
  changes apply locally. Shells out to pbcopy/pbpaste on macOS and
  wl-copy/wl-paste on Linux Wayland.

## Layout

```
.
├── main.go                       # flags, tsnet bootstrap, lifecycle
├── internal/
│   ├── server/
│   │   ├── server.go             # router + handlers + sweeper
│   │   ├── hub.go                # clip pub/sub for SSE
│   │   ├── views.go              # view models + format helpers
│   │   ├── templates/*.html      # embedded
│   │   └── static/               # embedded (manifest, app.css, icons)
│   └── store/
│       └── store.go              # SQLite wrapper (files + clip)
├── cmd/
│   └── tidepool-clipd/
│       └── main.go               # per-device clipboard sync daemon
├── Makefile
└── README.md
```

## First-time setup

1. Generate placeholder PWA icons (needs imagemagick) or drop in your own
   `internal/server/static/icon-{192,512}.png`:
   ```sh
   make icons
   ```
2. Build and run in dev mode (plain HTTP on `localhost:8080`):
   ```sh
   make dev
   ```

## Production (Tailscale)

1. Mint a one-off auth key in the Tailscale admin and export it before first
   run:
   ```sh
   export TS_AUTHKEY=tskey-auth-...
   ./tidepool -hostname tidepool -tls
   ```
2. Enable HTTPS on your tailnet (admin → DNS → enable HTTPS) so `-tls` can
   provision a cert at `tidepool.<your-tailnet>.ts.net`.
3. Visit `https://tidepool.<your-tailnet>.ts.net` from any device on your
   tailnet (iPhone needs the Tailscale app installed and connected).

The state directory `./tsnet-state` holds the node's keys — back this up or
mount it on a persistent volume.

### Flags

| Flag             | Default          | Purpose |
|------------------|------------------|---------|
| `-dev`           | `false`          | Bypass tsnet, listen on localhost over HTTP |
| `-addr`          | `:8080`          | Dev-mode listen address |
| `-data`          | `./data`         | Blob + SQLite directory |
| `-tsnet-state`   | `./tsnet-state`  | tsnet state directory |
| `-hostname`      | `tidepool`       | Tailnet hostname |
| `-tls`           | `true`           | Use `ListenTLS` (requires HTTPS-on-tailnet) |
| `-ttl`           | `24h`            | File retention |
| `-max-mb`        | `100`            | Max upload size |

## iPhone PWA

Open the URL in Safari → Share → **Add to Home Screen**. The clipboard "Copy"
button requires a secure context — that's why `-tls` is the default. Plain HTTP
over Tailscale will *work* for upload/download but the in-page Copy button
will silently no-op on iOS.

## Clipboard sync daemon (`tidepool-clipd`)

For native Cmd+C / Ctrl+C → paste on the other device, run `tidepool-clipd`
on each machine. The web clipboard slot still works alongside it.

### Build

```sh
# Linux (x86_64)
GOOS=linux GOARCH=amd64 CGO_ENABLED=0 go build -trimpath -ldflags='-s -w' \
    -o tidepool-clipd-linux-amd64 ./cmd/tidepool-clipd

# macOS (Apple Silicon)
GOOS=darwin GOARCH=arm64 CGO_ENABLED=0 go build -trimpath -ldflags='-s -w' \
    -o tidepool-clipd-darwin-arm64 ./cmd/tidepool-clipd
```

Pure Go, no CGo. Requires the platform clipboard tools at runtime:
`pbcopy`/`pbpaste` (macOS, preinstalled) or `wl-copy`/`wl-paste` (Wayland;
`dnf install wl-clipboard`). X11 isn't supported by the daemon.

### Install on Linux (Wayland, systemd user unit)

```sh
install -Dm755 tidepool-clipd-linux-amd64 ~/.local/bin/tidepool-clipd
mkdir -p ~/.config/systemd/user
cat > ~/.config/systemd/user/tidepool-clipd.service <<'EOF'
[Unit]
Description=tidepool clipboard sync daemon
After=graphical-session.target
PartOf=graphical-session.target

[Service]
ExecStart=%h/.local/bin/tidepool-clipd -url https://tidepool.<tailnet>.ts.net
Restart=on-failure
RestartSec=5

[Install]
WantedBy=graphical-session.target
EOF
systemctl --user daemon-reload
systemctl --user enable --now tidepool-clipd
```

The user systemd manager needs `WAYLAND_DISPLAY` in its environment; on
modern Fedora/GNOME this is set automatically. Verify with
`systemctl --user show-environment | grep WAYLAND_DISPLAY`.

### Install on macOS (LaunchAgent)

```sh
mkdir -p ~/.local/bin ~/Library/LaunchAgents
install -m755 tidepool-clipd-darwin-arm64 ~/.local/bin/tidepool-clipd
xattr -d com.apple.quarantine ~/.local/bin/tidepool-clipd 2>/dev/null || true

cat > ~/Library/LaunchAgents/com.deepwa7er.tidepool-clipd.plist <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.deepwa7er.tidepool-clipd</string>
  <key>ProgramArguments</key>
  <array>
    <string>$HOME/.local/bin/tidepool-clipd</string>
    <string>-url</string><string>https://tidepool.<tailnet>.ts.net</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/tmp/tidepool-clipd.out.log</string>
  <key>StandardErrorPath</key><string>/tmp/tidepool-clipd.err.log</string>
</dict>
</plist>
EOF

launchctl bootstrap "gui/$(id -u)" \
    ~/Library/LaunchAgents/com.deepwa7er.tidepool-clipd.plist
```

### iPhone (Shortcuts)

iOS doesn't allow background clipboard access, so there's no
`tidepool-clipd` for iPhone. Use two iOS Shortcuts instead, triggered
manually (Back Tap, Share Sheet, Action Button, Siri, or Home Screen
icon).

**Push to tidepool** (iPhone clipboard → server):

1. **Get Clipboard**
2. **Get Contents of URL**
   - URL: `https://tidepool.<tailnet>.ts.net/clip`
   - Method: `POST`
   - Request Body: `Form`, key `text`, value = the `Clipboard` variable

**Pull from tidepool** (server → iPhone clipboard):

1. **Get Contents of URL**
   - URL: `https://tidepool.<tailnet>.ts.net/clip.json`
   - Method: `GET` (default)
2. **Get Dictionary Value**, key `text`, from the previous step's output
3. **Copy to Clipboard**, input = the dictionary value

The `/clip.json` endpoint exists specifically for clients that don't
speak SSE — it returns `{"text": ..., "updated_at": ..., "updated_by": ...}`.
POSTs to `/clip` from Shortcuts use the same form-encoded path the
desktop daemons use, so they broadcast over `/clip/stream` to any
connected daemons.

Most useful trigger: **Back Tap** (Settings → Accessibility → Touch →
Back Tap) bound to the Pull and Push shortcuts. iPhone ↔ Mac is
already covered by Apple's Universal Clipboard if you have that on,
so Shortcuts mainly bridge iPhone ↔ Linux.

### How sync works (and what it doesn't)

The desktop daemon polls the OS clipboard every 500ms (configurable
with `-poll`) and pushes a SHA-256-deduped POST to `/clip` on any
change. It also keeps a long-lived SSE connection to `/clip/stream`
and writes the local clipboard when a remote update arrives. A shared
`lastHash` updated on both write paths breaks the echo loop. Last
write wins on concurrent edits across devices.

Text only; no rich content (images, formatted text). Trailing newlines
are normalized on Linux (`wl-paste --no-newline` + `wl-copy -n`) for
roundtrip parity with macOS.

## Operations

### Updating the server binary

```sh
GOOS=linux GOARCH=amd64 CGO_ENABLED=0 go build -trimpath -ldflags='-s -w' \
    -o tidepool-linux-amd64 .
scp tidepool-linux-amd64 myhost:/usr/local/bin/tidepool.new
ssh myhost 'mv /usr/local/bin/tidepool.new /usr/local/bin/tidepool && \
            systemctl restart tidepool'
```

The two-step rename works around `ETXTBSY` (can't overwrite a running
ELF in place); `mv` only swaps the inode, which is safe while the old
binary is mapped.

### Daemon control

| Platform | Status | Restart | Logs |
|---|---|---|---|
| Linux | `systemctl --user status tidepool-clipd` | `systemctl --user restart tidepool-clipd` | `journalctl --user -u tidepool-clipd -f` |
| macOS | `launchctl print gui/$(id -u)/com.deepwa7er.tidepool-clipd \| head -20` | `launchctl kickstart -k gui/$(id -u)/com.deepwa7er.tidepool-clipd` | `tail -f /tmp/tidepool-clipd.{out,err}.log` |

### What auto-evicts vs what's permanent

- File blobs + metadata: deleted by the server's 1-minute sweeper once
  `expires_at` passes (default 24h after upload, `-ttl` configurable).
- Clipboard slot: a single row, overwritten in place. No history.
- `tsnet-state/`: persistent node identity; back this up or mount on a
  volume.

### Privacy

Every clipboard event flows through `<data-dir>/tidepool.db` on the
server. Plaintext at rest; tailnet-only in transit. Don't run clipd on
machines where you'd object to passwords landing in that SQLite file.

## Deploying to a VPS

Suggested layout:

```
/opt/tidepool/
  tidepool                # the built binary
  data/                   # blobs + sqlite (persistent)
  tsnet-state/            # tsnet keys (persistent)
```

systemd unit (`/etc/systemd/system/tidepool.service`):

```ini
[Unit]
Description=tidepool
After=network-online.target
Wants=network-online.target

[Service]
WorkingDirectory=/opt/tidepool
ExecStart=/opt/tidepool/tidepool -hostname tidepool -tls
Environment=TS_AUTHKEY=tskey-auth-...
Restart=on-failure
RestartSec=5
User=tidepool
Group=tidepool

[Install]
WantedBy=multi-user.target
```

Cross-compile from Fedora:
```sh
GOOS=linux GOARCH=amd64 make build
scp tidepool vps:/opt/tidepool/
```

## Known limitations / next steps

- No auth UI — relies entirely on tailnet ACLs. Use Tailscale ACL tags if you
  want to restrict which of your devices can reach the service.
- iPhone share-sheet → tidepool requires going through "Save to Files" then
  opening tidepool. A native iOS app or a Shortcut could close that gap.
- The clipboard slot is a single row; no history. Easy to extend later.
- htmx is currently loaded from unpkg. To vendor: drop the file into
  `internal/server/static/htmx.min.js` and change the `<script src>` in
  `templates/index.html`.
