# tugboat

A small, manifest-driven deployer for personal services on the `deepwa7er` VPS.

One tool, one convention: each service repo carries a `deploy.toml`. tugboat
builds the artifact, ships it, swaps it in atomically, restarts the unit,
health-checks it, and **rolls back if the new build fails to come up** — then
optionally enrolls the unit in `lighthouse.target` so lighthouse discovers it.

It replaces the per-service `deploy.sh` scripts that had each reinvented (and
drifted on) the same build → ship → restart → health-check → rollback dance.

## Install

```sh
cargo install --path .
```

## Use

From a service repo that has a `deploy.toml`:

```sh
tugboat                 # build, ship, install, restart, health-check, enroll
tugboat --dry-run       # print the plan, change nothing
tugboat --skip-build    # reuse the last build
tugboat --host other    # override the SSH host
tugboat --manifest path/to/deploy.toml
```

## The manifest

`deploy.toml` (committed) describes the deploy. An optional, untracked
`deploy.local.toml` overlays machine- or tailnet-specific values that shouldn't
live in git (it overrides any field it sets).

```toml
name = "ferry"            # service + systemd unit base name ({name}.service)
host = "deepwa7er"        # ssh alias; override with --host or TUGBOAT_HOST

[build]
# Shell command, run locally in the manifest's directory.
# {workdir} expands to a fresh temp dir for build output.
cmd = "cargo build --release --target x86_64-unknown-linux-musl"

[[artifacts]]             # one or more files to install
src  = "target/x86_64-unknown-linux-musl/release/ferry"  # {workdir} expanded
dest = "/usr/local/bin/ferry"                            # absolute remote path
mode = "0755"             # optional, default 0755

[health]                  # optional; omit to use `systemctl is-active {name}`
url = "http://127.0.0.1:7777/commands"   # curled on the host loopback
retries = 10              # optional
interval_ms = 500         # optional

[verify]                  # optional; from THIS machine after deploy. Informational.
url = "https://deepwa7er.tailcfab97.ts.net:8443/commands"

[lighthouse]
enroll = true             # systemctl add-wants lighthouse.target {name}.service
```

### How install + rollback work

Each artifact is shipped to `<dest>.tug-new` next to its destination (same
filesystem). On the host, in one transaction: back up the live file to
`<dest>.tug-bak`, `mv` the new file over it (an atomic rename — safe even though
a running ELF can't be written in place), restart the unit, then health-check.
If the check never passes, every artifact is restored from its backup and the
unit restarted on the old binary; tugboat exits non-zero.

## Scope and limits (v0.1)

Built for the compiled single-/multi-binary services (ferry, tidepool, …).
Deliberately **not** yet handled:

- **Build-on-VPS** (harbor, lighthouse build Rust on the box) — `build.cmd`
  runs locally only.
- **Asset trees** (lighthouse ships a `web/dist`) — artifacts are individual
  files, not rsync'd directories.
- **Unit / config installation** — tugboat swaps binaries and restarts; it does
  not install systemd units or `/etc` config. Those stay in each service's
  one-time setup.
- **Ruby/Python/Docker services** are out of scope.

### The lighthouse enrollment caveat

`enroll = true` runs `systemctl add-wants lighthouse.target {name}.service`, so
lighthouse — which discovers target members at request time — sees the service
immediately. But lighthouse's polkit **control** grant (start/stop/restart) is
regenerated only when *lighthouse itself* is deployed. So a brand-new service
becomes **visible** in lighthouse right away but not **controllable** until the
next lighthouse deploy.
