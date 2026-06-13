# Lighthouse

A small web dashboard for the status and logs of systemd services on the
`deepwa7er` VPS. Rust (Axum) backend, React (Vite/TS/Tailwind) frontend.

No authentication: the dashboard binds to the VPS's **Tailscale IP**, so it is
reachable only from your tailnet and nothing is exposed on the public interface.

## Layout

```
src/             Rust backend
  config.rs        TOML config + the unit allowlist
  systemd.rs       systemctl / journalctl wrappers (status, recent + live logs)
  api.rs           HTTP handlers
  main.rs          wiring, static file serving, listener
web/             React frontend (built to web/dist, served by the backend)
deploy/
  lighthouse.service   systemd unit
  deploy.sh            build + ship + install
lighthouse.toml  baseline config (embedded in the binary; written on first run)
```

## How access control works

The server binds the listener to one address: the value of `bind` in the config.
Set to the VPS's `100.x` Tailscale IP, the OS only accepts connections arriving
on the Tailscale interface — the public IP has nothing listening on the port.
There is no application-level auth because there is no public surface to protect.

## Configuration (`/etc/lighthouse/config.toml`)

```toml
bind = "100.x.x.x"          # the VPS's Tailscale IP
port = 8080
static_dir = "/opt/lighthouse/web"

[[services]]
unit = "notes.service"
name = "Notes"
# … one block per monitored service
```

Only units listed here can be queried — the unit in every API request is checked
against this allowlist before any `systemctl`/`journalctl` command runs, and all
commands are invoked with explicit argument vectors (never a shell), so unit
names cannot inject anything. Edit this file and `systemctl restart lighthouse`
to add or remove services; no rebuild needed.

## API

- `GET /api/services` — status of every configured service.
- `GET /api/services/{unit}/logs?lines=N` — most recent N log lines (default 200).
- `GET /api/services/{unit}/logs/stream` — live tail via Server-Sent Events.
- `POST /api/services/{unit}/control/{action}` — `action` is `start`, `stop`, or
  `restart`. Returns the unit's post-action status.

## Service control & privilege model

Start/stop/restart go through `systemctl`, which talks to systemd over D-Bus; for
a non-root caller the action is authorized by **polkit**. `deploy.sh` installs
`/etc/polkit-1/rules.d/50-lighthouse.rules` granting the `lighthouse` user exactly
the `start`/`stop`/`restart` verbs on exactly the configured units — nothing else.

This keeps the service unprivileged: no root, no sudo, no setuid, so it remains
compatible with the unit's `NoNewPrivileges=true` hardening. The polkit rule is a
second enforcement layer behind the config allowlist (the API rejects unknown
units with 404 before ever calling `systemctl`).

The polkit rule is regenerated from the config on every deploy. So: adding a
service to **view** only needs a config edit + `systemctl restart lighthouse`;
adding one you can also **control** means re-running `deploy/deploy.sh` so the
polkit grant picks it up.

## Deploy

One-time: log the VPS into your tailnet (interactive — prints a URL to open in
your laptop browser):

```sh
ssh deepwa7er 'sudo tailscale up'
```

Then, from this directory:

```sh
deploy/deploy.sh
```

This builds the frontend locally, ships everything to the VPS, installs the Rust
toolchain if needed, builds the release binary, installs the binary/assets/config,
creates the unprivileged `lighthouse` service user (in `systemd-journal` for log
read access), and starts the service. It auto-detects the Tailscale IP and writes
it into `bind`. Open `http://<tailscale-ip>:8080` from any device on your tailnet.

## Local development

```sh
cargo run -- --config dev-config.toml     # backend on 127.0.0.1:8080
cd web && bun run dev                      # frontend on :5173, proxies /api
```

`dev-config.toml` binds to localhost and points `static_dir` at `web/dist`. The
`systemctl`/`journalctl` calls only work on the Linux VPS, so the live data is
exercised there; locally the dev server is used for UI work against the proxy.
