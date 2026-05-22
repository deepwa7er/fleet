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
- Tailwind via the standalone CLI (no Node toolchain).

## Layout

```
.
├── main.go                       # flags, tsnet bootstrap, lifecycle
├── internal/
│   ├── server/
│   │   ├── server.go             # router + handlers + sweeper
│   │   ├── views.go              # view models + format helpers
│   │   ├── templates/*.html      # embedded
│   │   └── static/               # embedded (manifest, generated app.css)
│   └── store/
│       └── store.go              # SQLite wrapper (files + clip)
├── web/input.css                 # Tailwind entry (not embedded)
├── tailwind.config.js
├── Makefile
└── README.md
```

## First-time setup

1. Install the standalone Tailwind CLI (no npm):
   ```sh
   make tailwind-install
   ```
2. Generate placeholder PWA icons (needs imagemagick) or drop in your own
   `internal/server/static/icon-{192,512}.png`:
   ```sh
   make icons
   ```
3. Build and run in dev mode (plain HTTP on `localhost:8080`):
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
