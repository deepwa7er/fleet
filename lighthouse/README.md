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
target = "lighthouse.target"  # services are discovered as members of this target

# Optional override layer — leave empty for pure discovery.
# [[services]]
# unit = "nginx.service"      # pin a unit that isn't a target member
# name = "Web (nginx)"        # custom label; omit to use the default
```

## Service discovery

Lighthouse discovers which services to show from a passive systemd **target**
(`lighthouse.target`). A service appears on the dashboard by becoming a member of
that target. Two ways to enroll:

```sh
# Non-invasive (doesn't touch the unit file):
systemctl add-wants lighthouse.target myapp.service

# Or self-enrolling — add to the unit's [Install] section, then reenable:
#   WantedBy=multi-user.target lighthouse.target
```

To un-enroll: `rm /etc/systemd/system/lighthouse.target.wants/myapp.service`.

Discovery runs at request time, so an enrolled service appears (and an un-enrolled
one disappears) on the next poll — no restart or redeploy needed **to view it**.
Display labels default to the prettified unit name (`sonar-discovery.service` →
"Sonar Discovery"); the optional `[[services]]` block overrides a label or pins a
unit that isn't a target member.

The monitored set is the allowlist: every API request's unit is checked against
it before any `systemctl`/`journalctl` runs, and all commands use explicit
argument vectors (never a shell), so unit names cannot inject anything.

## API

- `GET /api/services` — status of every configured service.
- `GET /api/services/{unit}/logs?lines=N` — most recent N log lines (default 200).
- `GET /api/services/{unit}/logs/stream` — live tail via Server-Sent Events.
- `POST /api/services/{unit}/control/{action}` — `action` is `start`, `stop`, or
  `restart`. Returns the unit's post-action status.

## Alerting

The dashboard is passive — you have to look at it. To be told when something
breaks, add an `[alerts]` section:

```toml
[alerts]
notify_url = "https://ntfy.sh/your-secret-topic"
interval_secs = 30
```

A background watcher then polls each monitored service and POSTs a notification
when one **enters the failed state** or **starts crash-looping** (its restart
count jumps), and once more when it **recovers** to a stable running state. It
alerts on transitions only and seeds silently on startup, so a Lighthouse
restart won't replay alerts and a crash loop notifies once, not every poll.

Notifications are sent by shelling out to `curl` (no HTTP-client dependency),
formatted for [ntfy](https://ntfy.sh): the body is the message and a `Title`
header names the service. Point `notify_url` at an ntfy topic your phone is
subscribed to (or any endpoint that accepts a POST). Omit the section to
disable alerting entirely.

## Service control & privilege model

Start/stop/restart go through `systemctl`, which talks to systemd over D-Bus; for
a non-root caller the action is authorized by **polkit**. `deploy.sh` installs
`/etc/polkit-1/rules.d/50-lighthouse.rules` granting the `lighthouse` user exactly
the `start`/`stop`/`restart` verbs on exactly the configured units — nothing else.

This keeps the service unprivileged: no root, no sudo, no setuid, so it remains
compatible with the unit's `NoNewPrivileges=true` hardening. The polkit rule is a
second enforcement layer behind the discovery allowlist (the API rejects unknown
units with 404 before ever calling `systemctl`).

The polkit grant is regenerated on every deploy from the target's members (plus
any pinned units). Because discovery is live but the polkit grant is static, the
split is: enrolling a service makes it **viewable** immediately, but
**controllable** only after the next `deploy/deploy.sh` (which is also when the
grant could be written — Lighthouse runs unprivileged and can't edit polkit
rules itself).

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
