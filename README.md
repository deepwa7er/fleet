# fleet

A self-hosted platform for running personal services. Everything here — the
reverse proxy, the deployer, the observability dashboard, the applications
they carry — is built from scratch and runs on one VPS behind a private
[Tailscale](https://tailscale.com) network.

The interesting part is not any single service. It is that adding a new one
takes a `deploy.toml` and nothing else: routing, TLS, deployment, health
checks, monitoring, and backups all attach automatically.

```
                    ┌─────────────────────────────────────────┐
   your devices ────┤  breakwater  (TLS, host-based routing)   │
   (tailnet only)   └────────────────────┬────────────────────┘
                                         │
              ┌──────────────┬───────────┼───────────┬──────────────┐
              │              │           │           │              │
         lighthouse       drydock     tidepool     depot        …apps
        (systemd obs)  (job queue)  (file sync) (warehouse)

   tugboat ──── builds, ships, swaps, health-checks, rolls back ────▶ VPS
```

## The platform

| Service | What it does |
|---|---|
| **breakwater** | Reverse proxy and the single entry point. Terminates TLS, routes by hostname, tunnels WebSockets. Runs the full ACME lifecycle in-process — issues and renews a wildcard certificate over DNS-01 and hot-swaps it with zero downtime. |
| **tugboat** | Manifest-driven deployer. Builds from a clean checkout of the default branch, ships the artifact, swaps it atomically, restarts, health-checks, and **rolls back automatically** if the new build fails to come up. |
| **lighthouse** | Observability over `systemd`/`journalctl` — service status, live log streaming, and one-click redeploy that relays to tugboat. |
| **depot** | Data warehouse. Ingests the proxy's access log and tugboat's deploy events so facts the fleet would otherwise overwrite survive. |
| **fleet-backup** | Encrypted offsite backup of each service's state, assembled from the same manifests. |

## Applications

| Service | What it does |
|---|---|
| **drydock** | Ticket queue for autonomous agent work. Enforced state machine, compare-and-swap claiming so concurrent workers can't collide, and a blocking `needs-input` state for human answers. |
| **tidepool** | Cross-device file and clipboard sync (Go). Joins the tailnet as its own node via `tsnet`, propagates clipboard changes over SSE, and serves a PWA for iOS. |
| **harbor** | Chrome new-tab dashboard over the project portfolio, backed by a Rust API. Self-hosts its own signed auto-update channel. |
| **atlas** | Code map and call-flow tracer for the fleet's Rust, derived from rust-analyzer's SCIP index. |
| **source** | Browse and search every repo in the fleet from one page. |
| **spyglass** | Federated search that fans one query across the other services. |
| **harness** | A minimal coding-agent harness — durable sessions, self-compacting context, terminal REPL and web UI. |
| **ferry** | Turns the browser address bar into a command line for tailnet services. |
| **tide** | Fleet-wide settings. Today, the theme every UI honors. |
| **clothes**, **recipes**, **regatta**, **driftword** | Smaller applications riding the same platform. |

## How it fits together

**Deployability is discovered, not declared.** A directory is a deployable
service *iff* it contains a `deploy.toml`. That one file makes it visible to
`tugboat deploy`, to lighthouse's dashboard, and to the backup set — there is
no central registry to update and no roster to keep in sync.

**One workspace, one lockfile.** Every Rust service is a member of a single
Cargo workspace sharing `fleet-common` and `fleet-api`. The whole fleet
cross-compiles locally to statically linked musl binaries, so nothing is ever
built on the server.

**CI is the deploy gate.** tugboat ships `origin/main`, so the invariant behind
every deploy is that main stays deployable: workspace tests, `clippy -D
warnings`, a build of every web app, and a check that generated registries match
their declarations.

## Threat model

The tailnet is the security boundary. Services bind **loopback** and are
reachable only through breakwater on the Tailscale interface — nothing listens
on a public address, and there is no per-service authentication. This is a
deliberate trade: device authentication happens once, at the network layer,
rather than being reimplemented badly in a dozen small services.

The consequence is that anything reaching the tailnet reaches everything on it.
`harness` in particular executes code, so it is the highest-value target here
and is best read as a lab service rather than a pattern to copy.

## Layout

```
breakwater/  tugboat/  lighthouse/  depot/     the platform
drydock/  tidepool/  harbor/  atlas/  …        applications
crates/fleet-common/                           shared HTTP + storage
crates/fleet-api/                              shared API types
web/                                           shared React components
fleet.toml                                     git working set + docs config
```

Each service directory has its own README covering its design in more detail.

## Stack

Rust (Tokio, Axum, rusqlite, Clap, tracing) · Go · React + TypeScript (Vite,
Tailwind) · SQLite · systemd · Tailscale · GitHub Actions

## License

MIT — see [LICENSE](LICENSE).

---

Each top-level directory was its own repository until 2026-07-01; history was
imported with `git filter-repo --to-subdirectory-filter` and the old repos are
archived.
