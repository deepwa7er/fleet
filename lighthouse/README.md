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

## Docker containers

Lighthouse can monitor Docker containers alongside systemd services. Add a
`[docker]` section to the config listing the containers to show:

```toml
[docker]
proxy_url = "http://127.0.0.1:2375"

[[docker.containers]]
container = "navidrome"   # container name as known to Docker
name = "Music"            # optional label; omit to prettify the container name

[[docker.containers]]
container = "slskd"
```

Containers appear in the same list as services, with their state mapped onto the
same vocabulary (`running` → active, `restarting` → activating, a non-zero exit →
failed, …) and a health-check verdict shown as the sub-state when present. Logs
and start/stop/restart work exactly as they do for units. The container list is
the control allowlist — only listed containers can be inspected or controlled.

Lighthouse never touches the Docker socket directly. It speaks the Docker Engine
API to a **socket-proxy** (`deploy.sh` runs `wollomatic/socket-proxy`, bound to
`127.0.0.1:2375`) that is allow-listed to exactly four operations — container
list/inspect/logs (GET) and start/stop/restart (POST) — and denies everything
else (create, delete, exec, images, networks, volumes, the daemon itself). See
the privilege model below.

## API

Each service has a `source` (`systemd` or `docker`) and an `id` (a unit name or
container name); the routes carry both.

- `GET /api/services` — status of every monitored service across all sources.
- `GET /api/services/{source}/{id}/logs?lines=N` — most recent N log lines (default 200).
- `GET /api/services/{source}/{id}/logs/stream` — live tail via Server-Sent Events.
- `POST /api/services/{source}/{id}/control/{action}` — `action` is `start`,
  `stop`, or `restart`. Returns the post-action status.

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

For **Docker**, the equivalent of the polkit grant is the socket-proxy's
allowlist. The Docker socket is root-equivalent — anything that can create a
container can mount the host and become root — so Lighthouse never gets it.
Instead `deploy.sh` runs `wollomatic/socket-proxy` (secure-by-default: every
method/path is denied unless explicitly allowed) configured to permit only:

```
GET  /containers/json , /containers/<id>/json , /containers/<id>/logs
POST /containers/<id>/(start|stop|restart)
```

Container create, delete, exec, and every non-container endpoint (images,
networks, volumes, the daemon) are denied at the proxy. The proxy is published
on host loopback only and runs unprivileged (added to the host `docker` group
just to read the socket). So a compromised Lighthouse can start/stop/restart the
listed containers and read their logs — and nothing more.

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
