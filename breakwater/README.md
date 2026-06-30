# breakwater

A small reverse proxy that is the front door to the `deepwa7er` tailnet. It
terminates TLS on the Tailscale interface and routes HTTPS requests to local
services **by hostname**, so services are reached as
`https://<name>.intern.deepwa7er.net` instead of by raw `host:port`.

Like the rest of the fleet, it binds only the Tailscale IP — the tailnet is the
security boundary, so there is no public exposure and no per-service auth.

## How it routes

One TLS listener, host-based routing from a config table:

```toml
[[routes]]
host = "lighthouse.intern.deepwa7er.net"
upstream = "127.0.0.1:8080"
```

Requests are streamed (no buffering), hop-by-hop headers are stripped, the
`X-Forwarded-For`/`-Proto`/`-Host` trio is set, the original `Host` is preserved,
and WebSocket/`Upgrade` connections are tunnelled. A plain-HTTP listener
308-redirects everything to HTTPS, and a loopback `/healthz` backs the deploy
health check.

Adding a service (or changing a route) is a new/edited `[[routes]]` block in
`breakwater.toml` followed by `tugboat deploy` — the config ships with the binary
and the deploy restarts breakwater, which re-reads it. No DNS change (thanks to
the wildcard record below) and no per-app changes.

## Certificates

Two mutually-exclusive modes; set exactly one:

- **`[acme]`** (production) — breakwater obtains and renews a wildcard
  certificate for `*.intern.deepwa7er.net` via ACME DNS-01, solved through the
  Cloudflare API. It runs the whole lifecycle in-process: issue at startup
  (reusing the on-disk cache if still fresh), renew ~30 days before expiry, and
  hot-swap the new certificate with zero downtime.
- **`[tls]`** (e.g. local testing) — serve a `cert`/`key` PEM pair from disk.

```toml
[acme]
domains = ["*.intern.deepwa7er.net"]
contact = "mailto:you@example.com"
cloudflare_zone = "deepwa7er.net"
cloudflare_token_file = "/etc/breakwater/cloudflare-token"
cache_dir = "/var/lib/breakwater/acme"
# directory = "https://acme-staging-v02.api.letsencrypt.org/directory"  # test first
```

### One-time Cloudflare setup

1. **DNS:** a wildcard record `*.intern.deepwa7er.net` → the Tailscale IP
   (`100.98.184.58`), **DNS-only** (grey cloud — Cloudflare must not proxy a
   private IP). Set once; new services need no further DNS changes.
2. **Token:** a scoped API token with `Zone:DNS:Edit` + `Zone:Read` on the
   `deepwa7er.net` zone, used only to create/delete the `_acme-challenge` TXT
   record. Install it on the VPS at `/etc/breakwater/cloudflare-token` (mode
   600, owned by `breakwater`) — never in git.

DNS-01 propagation is confirmed against the zone's authoritative nameservers
before the challenge is marked ready, so a slow record can't fail the order.

## Build

Cross-compiles to a static musl binary (ring crypto provider throughout, so no
C/cmake toolchain needed):

```sh
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc \
  cargo build --release --target x86_64-unknown-linux-musl
```

`cargo test` runs config/proxy unit tests and an end-to-end proxy test.

## Deploy

Infrastructure is provisioned once; the binary **and `breakwater.toml`** ship via
[tugboat](https://github.com/deepwa7er/tugboat) (so routing changes are just a
config edit + redeploy). **First deploy order matters** because the first start
performs an ACME issuance:

1. **Provision** the host (service user, `/etc/breakwater` directory, systemd
   unit — the config file itself ships in step 4 via tugboat):
   ```sh
   ./deploy/provision.sh
   ```
2. **Install the Cloudflare token** on the VPS (as root):
   ```sh
   install -m600 -o breakwater -g breakwater /dev/stdin \
     /etc/breakwater/cloudflare-token <<< 'YOUR_CLOUDFLARE_TOKEN'
   ```
3. **Confirm the wildcard DNS record** exists (see above).
4. **Test against Let's Encrypt staging first** by uncommenting the staging
   `directory` in `breakwater.toml` (the repo file — tugboat ships it), then
   deploy:
   ```sh
   tugboat
   ```
   Check `journalctl -u breakwater` for `certificate issued and cached`. Staging
   certs are untrusted — that is expected; this only proves the flow.
5. **Switch to production:** comment the staging `directory` back out in
   `breakwater.toml`, clear the staging cache so a trusted cert is issued, and
   redeploy:
   ```sh
   ssh deepwa7er 'rm -f /var/lib/breakwater/acme/*'   # drop staging account+cert
   tugboat
   ```

The unit binds `:443`/`:80` unprivileged via `CAP_NET_BIND_SERVICE`, orders
after `tailscaled`, and caches certs under `/var/lib/breakwater` (managed by
systemd `StateDirectory`). tugboat enrolls it in `lighthouse.target`, so it
shows up in the lighthouse dashboard.

## Config reference

| key | meaning |
|---|---|
| `https_port` | TLS listener port, bound on the resolved Tailscale IP (e.g. `443`) |
| `http_redirect_port` | optional plain-HTTP listener port (same IP) that redirects to HTTPS |
| `health_addr` | optional loopback health endpoint for tugboat |
| `[tls] cert` / `key` | static-cert mode: PEM files on disk |
| `[acme]` | automatic-cert mode (see above) |
| `[[routes]] host` / `upstream` | route a public hostname to a local `host:port` |
