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
         lighthouse       drydock     tidepool     warehouse  …apps
        (systemd obs)  (job queue)  (file sync) (dev ware)

   tugboat ──── builds, ships, swaps, health-checks, rolls back ────▶ VPS
```

## The platform

| Service | What it does |
|---|---|
| **breakwater** | Reverse proxy and the single entry point. Terminates TLS, routes by hostname, tunnels WebSockets. Runs the full ACME lifecycle in-process — issues and renews a wildcard certificate over DNS-01 and hot-swaps it with zero downtime. |
| **tugboat** | Manifest-driven deployer. Builds from a clean checkout of the default branch, ships the artifact, swaps it atomically, restarts, health-checks, and **rolls back automatically** if the new build fails to come up. |
| **lighthouse** | Observability over `systemd`/`journalctl` — service status, live log streaming, and one-click redeploy that relays to tugboat. |
| **warehouse** | Local data warehouse for all of `~/code` + shell + git (SQLite+Parquet, hourly ingest on Fedora, R2 backup, MCP query). Replaces depot (archived 2026-08-15). |
| **fleet-backup** | Encrypted offsite backup of each service's state, assembled from the same manifests. |

## Applications

| Service | What it does |
|---|---|
| **drydock** | Ticket queue for autonomous agent work. Enforced state machine, compare-and-swap claiming so concurrent workers can't collide, and a blocking `needs-input` state for human answers. |
| **tidepool** | Cross-device file and clipboard sync (Go). Joins the tailnet as its own node via `tsnet`, propagates clipboard changes over SSE, and serves a PWA for iOS. |
| **harbor** | Chrome new-tab dashboard over the project portfolio, backed by a Rust API. Self-hosts its own signed auto-update channel. |
| **atlas** | Code map and call-flow tracer for the fleet's Rust, derived from rust-analyzer's SCIP index. |
| **spyglass** | Federated search that fans one query across the other services. |
| **harness** | A minimal coding-agent harness — durable sessions, self-compacting context, terminal REPL and web UI. |
| **ferry** | Turns the browser address bar into a command line for tailnet services. |
| **warehouse** | Dev data warehouse — crawls `~/code`, git, and shell history into SQLite+Parquet, heuristic integrations, MCP for agents (`get_repo_context`/`search_build_knowledge`). Hourly `systemd --user` on Fedora, not VPS routed. |
| **clothes**, **recipes**, **regatta**, **driftword** | Smaller applications riding the same platform. |

## Native apps

These run on the desktop rather than on the tailnet, so they have no
`deploy.toml` and tugboat never ships them. They also sit outside the Cargo
workspace, so the fleet-wide gates do not build them — each carries its own
(`AGENTS.md` records the command).

| App | What it does |
|---|---|
| **ide** | The fleet's own IDE — a native GPUI app, IntelliJ New UI layout, DW-001 palette. Its own Cargo workspace. |
| **loom** | macOS window manager: one window at a time filling the screen, `⌘1`–`⌘9` to switch. Swift/SwiftPM, built with `make app`. |
| **shutter** | macOS screenshot tool: answers the system's own ⌘⇧3/4/5, captures the screen *before* the overlay draws, annotates, copies, saves. Swift/SwiftPM, built with `make app`. |

**filament** is the Swift half of the shared layer — a React-style reconciler
(elements, hooks, fragments, keyed reordering) plus an AppKit host that drives
real `NSView`s. It is a library, not an app: loom renders its command-panel chip
rows through it, and depends on it by path (`../filament`), so one checkout
builds both and a reconciler change lands in the same commit as its use.

## How it fits together

**Deployability is discovered, not declared.** A directory is a deployable
service *iff* it contains a `deploy.toml`. That one file makes it visible to
`tugboat deploy`, to lighthouse's dashboard, and to the backup set — there is
no central registry to update and no roster to keep in sync.

**One workspace, one lockfile.** Every Rust service is a member of a single
Cargo workspace sharing `fleet-common` and `fleet-api`. The whole fleet
cross-compiles locally to statically linked musl binaries, so nothing is ever
built on the server.

**The gates are local.** tugboat ships `origin/main`, so the invariant behind
every deploy is that main stays deployable: workspace tests, `clippy -D
warnings`, and a check that generated registries match their declarations
(`tugboat fleet gen --check`). These run before every PR — see
`.agents/skills/fleet/SKILL.md` §3. CI was archived 2026-08-13 and there is no
automated gate on `origin/main`; see `ci.ARCHIVED.md`.

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
breakwater/  tugboat/  lighthouse/             the platform
drydock/  tidepool/  harbor/  atlas/  warehouse/ …  applications
ide/  loom/  shutter/                          native apps (own build)
crates/fleet-common/                           shared HTTP + storage
crates/fleet-api/                              shared API types
web/                                           shared React components
filament/                                      shared Swift UI reconciler
fleet.toml                                     git working set + docs config
```

Each service directory has its own README covering its design in more detail.

## Stack

Rust (Tokio, Axum, rusqlite, Clap, tracing) · Go · Swift (AppKit) · React +
TypeScript (Vite, Tailwind) · SQLite · systemd · Tailscale

## License

MIT — see [LICENSE](LICENSE).

---

Each top-level directory was its own repository until 2026-07-01 — loom,
filament, and shutter followed in August 2026. History was imported with
`git filter-repo --to-subdirectory-filter` and the old repos are archived.
