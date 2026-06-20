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
tugboat deploy                 # build, ship, install, restart, health-check, enroll
tugboat deploy --dry-run       # print the plan, change nothing
tugboat deploy --skip-build    # reuse the last build
tugboat deploy --host other    # override the SSH host
tugboat deploy --manifest path/to/deploy.toml
```

## The fleet

`tugboat fleet …` operates on the whole suite at once, driven by a `fleet.toml`
that lists the member repos (shipped in this repo). The manifest is found via
`--manifest`, else `TUGBOAT_FLEET`, else the nearest `fleet.toml` searching
upward from the current directory.

```sh
tugboat fleet list      # show members and whether each is tugboat-deployed
tugboat fleet clone     # clone any members not yet checked out (new machine)
tugboat fleet pull      # fast-forward-only pull every clean member checkout
tugboat fleet status    # git summary (branch / clean / ahead-behind) per member
tugboat fleet deploy    # deploy each `deploy = true` member, in listed order
tugboat fleet deploy --only lighthouse,buoy   # a subset
tugboat fleet deploy --dry-run                # print every member's plan
tugboat fleet deploy --continue-on-error      # don't stop at the first failure
```

`fleet deploy` reuses the single-service engine per member, so each member gets
its own atomic install + health-check + rollback. It stops at the first failure
unless `--continue-on-error`. Members without a `deploy.toml` set `deploy =
false`; they still participate in `clone`/`pull`/`status`.

A `fleet.toml` member records only `path` (relative to `root`, a leading `~/`
expands to `$HOME`) and its `repo` remote — deploy details stay in each repo's
own `deploy.toml`, so there is one source of truth per service.

> **Caveat — consistent layout.** A member's `path` is relative to `root` and
> must resolve the same on every machine. `sonar` currently lives at
> `~/code/apple/sonar` on the laptop but `~/code/sonar` on fedora; until that's
> standardized, `fleet clone`/`status` will treat it as missing on the laptop.

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

[[artifacts]]             # one or more files/dirs to install
src  = "target/x86_64-unknown-linux-musl/release/ferry"  # {workdir} expanded
dest = "/usr/local/bin/ferry"                            # absolute remote path
kind = "file"             # "file" (default, scp) or "dir" (rsync --delete)
mode = "0755"             # optional, default 0755 (ignored for dirs)

# A directory artifact — e.g. built web assets (lighthouse):
# [[artifacts]]
# kind = "dir"
# src  = "web/dist"
# dest = "/opt/lighthouse/web"

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
filesystem) — files via scp, dirs via `rsync --delete` (perms preserved, but
not the local uid/gid). On the host, in one transaction: move the live file/dir
aside to `<dest>.tug-bak`, rename the new one into place (atomic — safe even
though a running ELF can't be written in place), restart the unit, then
health-check. If the check never passes, every artifact is restored from its
backup and the unit restarted on the old version; tugboat exits non-zero.

## Scope and limits

Built for services that **build locally** and ship a binary (± an asset tree).
The whole fleet cross-compiles to a static musl binary on the dev machine
(`x86_64-unknown-linux-musl`), so nothing is built on the VPS. Adopters: ferry,
tidepool (Go), harbor, lighthouse.

Deliberately **not** handled:

- **Build-on-VPS** — `build.cmd` runs locally only. Cross-compiling instead
  (the ferry model) removed the need; no service requires it.
- **Unit / config / polkit installation** — tugboat swaps binaries/assets and
  restarts; it does not install systemd units, `/etc` config, or polkit grants.
  Those are each service's `provision.sh` / one-time setup, run on infra changes.
- **Ruby/Python/Docker services** are out of scope.

### The lighthouse enrollment caveat

`enroll = true` runs `systemctl add-wants lighthouse.target {name}.service`, so
lighthouse — which discovers target members at request time — sees the service
immediately. But lighthouse's polkit **control** grant (start/stop/restart) is
regenerated only when *lighthouse itself* is deployed. So a brand-new service
becomes **visible** in lighthouse right away but not **controllable** until the
next lighthouse deploy.
