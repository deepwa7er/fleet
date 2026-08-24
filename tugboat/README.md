# tugboat

A small, manifest-driven deployer for personal services on the `deepwa7er` VPS.

One tool, one convention: each service repo carries a `deploy.toml`. tugboat
builds the artifact, ships it, swaps it in atomically, restarts the unit,
health-checks it, and **rolls back if the new build fails to come up** — then
optionally enrolls the unit in `lighthouse.target` so lighthouse discovers it.

It replaces the per-service `deploy.sh` scripts that had each reinvented (and
drifted on) the same build → ship → restart → health-check → rollback dance.

## Build contract

The `[build].cmd` in each `deploy.toml` is the complete, authoritative build
recipe. Tugboat expands its placeholders and executes it unchanged; it does not
select a compiler, package manager, or container runtime on the service's
behalf. This keeps tool-specific behavior in the manifest where it is visible
and testable.

Containerized fleet services use Docker and Docker Buildx in their build
recipes, and their image archives are loaded by Docker on the VPS. A machine
that builds those services must therefore provide a working `docker` CLI with
Buildx. Native-binary and static-site services do not require Docker unless
their own build recipe says otherwise.

## Install

```sh
cargo install --path .
```

## Use

From a service repo that has a `deploy.toml`:

```sh
tugboat deploy                 # build, ship, install, restart, health-check, enroll
tugboat deploy --dry-run       # print the plan, change nothing
tugboat deploy --host other    # override the SSH host
tugboat deploy --manifest path/to/deploy.toml
tugboat deploy --working-tree  # deploy the current checkout as-is (see below)
tugboat deploy --working-tree --skip-build   # …and reuse its existing build
```

### What gets deployed

By default `tugboat deploy` ships **origin's default branch**: it fetches
`origin`, checks the default branch (`main`/…) out into a fresh **detached
worktree**, and builds *that*. The deploy is therefore reproducible from committed
history and **independent of whatever branch the working tree happens to be on** —
so a stray `git checkout`, or the drydock worker leaving a repo parked on a feature
branch, can never change what ships. The worktree is removed when the deploy ends.

For local iteration or to smoke-test a branch on the VPS before merging, pass
`--working-tree`: it builds the current checkout exactly as it is (current branch,
uncommitted changes included). `--skip-build` (reuse the existing artifacts) only
applies in this mode — a default-branch deploy always builds its clean checkout.

`tugboat serve` (the dashboard's Deploy button) and `tugboat fleet deploy` always
use the default-branch path; `--working-tree` is a `tugboat deploy` CLI opt-in.

## The fleet

There are two distinct ideas here — keep them separate:

- **Deployability is discovered, not declared.** A repo is a deployable service
  **iff it contains a `deploy.toml`**. `tugboat serve`, `fleet deploy`, and
  lighthouse all find deployable services by scanning the fleet `root` for
  `*/deploy.toml`. So **a new service is deployable and fleet-visible the moment
  it has a `deploy.toml`** — no roster entry, no commit, no daemon restart.
- **`fleet.toml` is the git working set.** It lists the member repos tugboat
  manages together for whole-fleet *git* operations (`clone`/`pull`/`status`).
  Listing a repo here is only about having it checked out across machines; it is
  **not** what makes something deployable.

> **Adding a new service:** give it a `deploy.toml` (and a `provision.sh` for
> first-time infra) — that alone makes it deployable, discoverable by the daemon,
> and visible in lighthouse. Add it to `fleet.toml` *only* if you also want it
> cloned/pulled on your other machines (optional, and decoupled from deploying).

The `fleet.toml` manifest is found via `--manifest`, else `TUGBOAT_FLEET`, else
the nearest `fleet.toml` searching upward from the current directory.

```sh
tugboat fleet list      # show members and whether each is deployable (has deploy.toml)
tugboat fleet clone     # clone any members not yet checked out (new machine)
tugboat fleet pull      # fast-forward-only pull every clean member checkout
tugboat fleet status    # git summary (branch / clean / ahead-behind) per member
tugboat fleet deploy    # deploy every discovered deployable service
tugboat fleet deploy --only lighthouse,buoy   # a subset (by service name)
tugboat fleet deploy --dry-run                # print every service's plan
tugboat fleet deploy --continue-on-error      # don't stop at the first failure
tugboat fleet docs           # build + ship the docs site (see "Fleet documentation")
tugboat fleet hooks install  # auto-refresh the docs on every commit
```

`fleet deploy` reuses the single-service engine per service, so each gets its own
atomic install + health-check + rollback. It stops at the first failure unless
`--continue-on-error`.

A `fleet.toml` member records only `path` (relative to `root`, a leading `~/`
expands to `$HOME`) and its `repo` remote — deploy details stay in each repo's
own `deploy.toml`, so there is one source of truth per service. A member's `path`
must resolve the same on every machine, so fleet members are checked out at a
consistent location (`~/code/<name>`) everywhere — not tucked under per-machine
grouping folders.

## Serving deploys (trigger from anywhere on the tailnet)

`tugboat serve` runs the deploy pipeline behind an HTTP API so a deploy can be
triggered from another device — e.g. the lighthouse dashboard on the VPS, opened
from any machine on the tailnet. The build still happens **here**, on the dev
machine: a request runs the exact same engine as `tugboat deploy`, building
**origin's default branch** in a clean worktree (see [What gets
deployed](#what-gets-deployed)), and the transcript is streamed back live. The
daemon also fetches each deployable's `origin` periodically, so the dashboard's
"undeployed commits" reflects freshly-merged work without a manual pull.

```sh
TUGBOAT_SERVE_TOKEN=$(openssl rand -hex 32) \
  tugboat serve --bind <this-machine's-tailscale-ip> --port 7878
```

- `GET /health` — the running build (sha, build time, pid, start time).
  **Unauthenticated**; used by `tugboat self-deploy` to confirm a restart.
- `GET /services` — the deployable fleet members.
- `POST /deploy/{name}` — start a deploy; returns `{ "job_id": … }`. Returns
  `409` if that service is already deploying.
- `GET /jobs/{id}/stream` — replay the transcript so far, then stream the rest
  live over Server-Sent Events, closing when the deploy finishes.
- `GET /docs` — the docs auto-refresh state (last build outcome, whether
  building). `POST /docs/refresh` — request a docs rebuild (the commit hooks call
  this; returns at once). See [Fleet documentation](#fleet-documentation).

Every request except `GET /health` must carry `Authorization: Bearer
$TUGBOAT_SERVE_TOKEN`. The token is **required** — the daemon refuses to start
without one, because the deploy endpoints run builds and are more than a
read-only surface. `/health` is exempt because it exposes no secrets and a
self-deploy must reach it while restarting the daemon. The default `--bind` is
loopback; pass the tailnet IP to expose it to the fleet.

### Running it as a launchd agent

So it's always up while you work, install it as a login agent from the template
in `deploy/tugboat-serve.plist`:

```sh
cargo install --path .                       # build tugboat (with `serve`) into ~/.cargo/bin
rustup target add x86_64-unknown-linux-musl  # once, for the fleet's cross-compiled builds

token=$(openssl rand -hex 32)
ip=$(tailscale ip -4)
sed -e "s|__HOME__|$HOME|g" -e "s|__TAILSCALE_IP__|$ip|g" -e "s|__TOKEN__|$token|g" \
  deploy/tugboat-serve.plist > ~/Library/LaunchAgents/com.deepwa7er.tugboat-serve.plist
chmod 600 ~/Library/LaunchAgents/com.deepwa7er.tugboat-serve.plist   # holds the token

launchctl load ~/Library/LaunchAgents/com.deepwa7er.tugboat-serve.plist
echo "$token"   # set this as lighthouse's [deploy].token on the VPS
```

The agent launches through a login `fish` shell so it inherits the same PATH and
toolchain (cargo, the musl cross-linker, bun, ssh/scp/rsync, git) that an
interactive deploy uses — rather than launchd's minimal default environment.

To finish the loop, point lighthouse at the daemon (on the VPS, in
`/etc/lighthouse/config.toml`):

```toml
[deploy]
tugboat_url = "http://<this-machine's-tailscale-ip>:7878"
token = "<the token printed above>"
```

Then redeploy lighthouse. A **Deploy** button appears for every monitored
service that maps to a deployable fleet member.

> **Requires the dev machine to be on.** The build runs here, so the box must be
> awake with the agent running when you click Deploy. If it's asleep or the
> agent is down, the dashboard reports the daemon unreachable and changes
> nothing — there is no half-deploy.

### Updating tugboat itself

tugboat deploys remote VPS services over ssh + systemd, but the `serve` daemon
is a **local launchd agent** — a different target on every axis (localhost, not
ssh; launchd, not systemd; `~/.cargo/bin`, not `/usr/local/bin`). So tugboat
updates itself with a dedicated command rather than the generic engine:

```sh
tugboat self-deploy            # rebuild, swap the binary, restart the agent, verify
tugboat self-deploy --dry-run  # print the plan, change nothing
tugboat self-deploy --skip-build           # reuse the last release build
tugboat self-deploy --health-url URL       # override the daemon URL to poll
```

Run it from the tugboat checkout. It builds a release binary, **atomically swaps**
`~/.cargo/bin/tugboat` (backing up the old one), `launchctl kickstart -k`s the
agent, then polls the daemon's `GET /health` until it reports a **newer start
time** — proof the agent came back up on the new binary. If it doesn't return
within the timeout (e.g. the new build crashes on boot), the previous binary is
restored and restarted, so a bad build can't leave the daemon down.

It is **CLI-only by design**: the deploying process must be separate from the
daemon it restarts. A daemon can't cleanly restart itself mid-request, so this is
deliberately *not* exposed through `serve` / the lighthouse Deploy button.

`GET /health` (unauthenticated — it carries no secrets) reports the running
build's git sha, build time, pid, and start time. `tugboat version [--json]`
prints the same build identity for the local binary.

### The deploy ledger

Every deploy appends one line to a per-service ledger **on the host**,
`/var/lib/tugboat/{name}.jsonl` — written inside the deploy transaction, so it's
accurate whether the deploy came from the CLI or the daemon, and it doesn't
depend on any second service being up. lighthouse reads these files (it runs on
the same host) to show which sha each service is running and its deploy history.

This file is the **contract** between tugboat (the only writer) and any reader.
Each line is one JSON object; append-only; newest last:

```json
{"v":2,"id":"1718900000-1a2b3c4d","sha":"<full sha>","short":"<8 chars>","dirty":false,"branch":"main","result":"deployed","at":1718900000}
```

- `v` — schema version (currently `2`); bump it on any breaking change so
  readers can adapt.
- `id` — `{at}-{short}` (or `{at}-nogit` outside a checkout). Names this deploy's
  **transcript file** (see below) so a reader can pair history with its log.
- `result` — `deployed` if the new build came up healthy, or `rolled_back` if it
  failed its health check and tugboat restored the previous version. The
  **currently-running** version is therefore the last entry with `result =
  "deployed"` — a trailing `rolled_back` means the prior good version is live.
- `dirty` — whether the build tree had uncommitted changes at deploy time (so
  `sha` doesn't fully describe what shipped). Always `false` for a default-branch
  deploy (it builds a clean checkout); only a `--working-tree` deploy can record
  `true`.
- `at` — Unix epoch seconds (stamped at deploy *start*; shared with `id`).

The write is a single short line to an `O_APPEND` file and is best-effort
(`… || true`): a ledger hiccup never fails an otherwise-healthy deploy. Entries
are tiny (~150 bytes); the file is not yet rotated.

### Deploy transcripts

Alongside the ledger, every deploy that reaches the install step writes its full
transcript (build → ship → install, including a rollback's diagnostics) to
`/var/lib/tugboat/{name}/{id}.log` on the host — for both outcomes, since a
rolled-back deploy's log is the most useful to keep. The `{id}` matches the
ledger entry, so a reader (lighthouse) lists history from the ledger and opens
the matching transcript on demand. Captured uniformly for CLI and daemon deploys
via a teeing sink; shipped over ssh on the deploy's stdin (so contents are never
shell-quoted). Best-effort like the ledger, and pruned to the most recent 50 per
service. Deploys that fail *before* the remote install (e.g. a local build error)
have no ledger entry and no transcript — that output is a dev-box concern and is
visible live.

## Fleet documentation

`tugboat fleet docs` generates the fleet's documentation site (the `pilot`
frontend repo) — a service reference plus each Rust repo's rustdoc — and ships
it. It is a whole-fleet op because it joins facts from every member: each repo's
`deploy.toml`, breakwater's routing table, and `cargo metadata`, plus `cargo doc
--no-deps` per Rust repo.

```sh
tugboat fleet docs                 # build the frontend + harvest + rustdoc, then ship
tugboat fleet docs --out ./site    # assemble locally into ./site, don't ship
tugboat fleet docs --skip-rustdoc  # emit only fleet.json (+ frontend), skip cargo doc
tugboat fleet docs --skip-build    # reuse the frontend's existing dist
tugboat fleet docs --only ferry    # limit the (slow) rustdoc pass to some repos
tugboat fleet docs --dry-run       # print the plan
```

Configured by a `[docs]` table in `fleet.toml`:

```toml
[docs]
repo  = "pilot"                                  # member holding the frontend
build = "cd web && bun install && bun run build"
dist  = "web/dist"                               # built frontend, relative to the repo
host  = "deepwa7er"                              # ship target
dest  = "/opt/pilot/web"                         # served by breakwater (a serve_dir route)
url   = "https://docs.intern.deepwa7er.net"    # polled after a ship
```

The site is process-less static files (breakwater serves the directory), so the
ship is a directory rsync + atomic swap, not a unit deploy. The harvested model
is `fleet.json`; each repo's rustdoc mounts at `/doc/<repo>/` (one bundle per
repo, so rustdoc's per-invocation search index keeps working). A service's
description comes from its crate's Cargo `description`, overridden by an optional
`description` in its `deploy.toml` — authoritative, and the right source for a
workspace (no single crate speaks for the service) or a non-Rust service.

### Auto-refresh on commit

The `serve` daemon keeps the docs current. A debounced, single-flight **docs
keeper** rebuilds + reships whenever the fleet's combined git-HEAD fingerprint
moves. It's woken two ways:

- a **commit hook** → `POST /docs/refresh`, for immediacy, and
- a **periodic catch-up** in the daemon (every 5 min) — the backstop that covers
  a missed hook (the daemon was down, a repo lacks the hook, or a commit was
  pulled from another machine).

The fingerprint (cached at `~/.cache/tugboat/docs-fingerprint`) is recorded only
after a successful ship, so a failed build is retried, not masked; `GET /docs`
reports the last outcome. Builds are coalesced (a 20s debounce) and run one at a
time.

Install the hooks into every fleet member:

```sh
tugboat fleet hooks install     # --url / --token override the daemon URL / token
tugboat fleet hooks uninstall   # unset core.hooksPath in each member
```

`install` writes the shared hook scripts to `~/.config/tugboat/hooks` (plus the
daemon URL and token they read) and points each member at them via
`core.hooksPath`. The URL defaults to `http://$(tailscale ip -4):7878`, the token
to `$TUGBOAT_SERVE_TOKEN`. The hooks (`post-commit`, `post-merge`,
`post-rewrite`) are **fire-and-forget** — a commit never blocks or fails on them.
Re-run after `fleet clone` or adding a member; the catch-up covers any gap.

> Same dev-machine-awake constraint as the Deploy button: the build runs here, so
> auto-refresh happens when the box is up with the agent running. The catch-up
> means the docs self-heal on wake.

### The docs site on a new dev box

```sh
tugboat fleet clone          # check out the members
cargo install --path .       # install tugboat
# install + load the serve agent — see "Running it as a launchd agent" above
tugboat fleet hooks install  # wire the commit hooks to the daemon
```

The daemon's catch-up runs the first build on startup, so the site comes current
on its own once the agent is up.

## Agent deploys (dev-machine binaries)

`tugboat agent deploy` installs a **per-user binary onto the dev machines
themselves** (built locally, or rsync'd over SSH with an atomic swap) — rather
than a root systemd service on the VPS. Two shapes:

- a **daemon**, restarted after install via a launchd login agent (macOS) or a
  `systemd --user` unit (Linux) — e.g. tidepool's `tidepool-clipd`;
- a **CLI tool**, just a binary on `PATH` with nothing to restart — e.g. the
  `drydock` worker CLI in `~/.cargo/bin`. Set neither `launchd` nor
  `systemd_user`.

The binary is built per target — cross-compiled when `goos`/`goarch` are given,
or a native build when they aren't. No health-check / rollback / ledger — these
are trivially replaceable user binaries, not the VPS's load-bearing services.

```sh
tugboat agent deploy                 # build + install on every target
tugboat agent deploy --only desktop  # one target
tugboat agent deploy --dry-run       # print the plan
```

Driven by an `agent.toml` in the daemon's repo:

```toml
name  = "tidepool-clipd"
build = "GOOS={goos} GOARCH={goarch} CGO_ENABLED=0 go build -o {out} ./cmd/tidepool-clipd"

[[targets]]
name = "mac"
local = true                          # build + install on this machine
goos = "darwin"
goarch = "arm64"
dest = "~/.local/bin/tidepool-clipd"
launchd = "com.deepwa7er.tidepool-clipd"

[[targets]]
name = "desktop"
ssh = "deepwater@fedora"              # reached over Tailscale SSH
goos = "linux"
goarch = "amd64"
dest = "~/.local/bin/tidepool-clipd"
systemd_user = "tidepool-clipd"
```

Each target is `local = true` (built + installed here) or `ssh = "user@host"`
(rsync'd over SSH); a daemon restarts via `launchd = "<label>"` or
`systemd_user = "<unit>"`. `{out}` is the binary path tugboat provides;
`{goos}`/`{goarch}` choose the build platform for a cross-compiled build.

A **CLI tool** omits the platform and restart fields entirely — a native build,
installed on `PATH`, nothing to restart:

```toml
name  = "drydock"                       # one binary: `drydock serve` + worker CLI
build = "cargo build --release && cp target/release/drydock {out}"

[[targets]]
name = "mac"
local = true
dest = "~/.cargo/bin/drydock"
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

[[artifacts]]             # one or more files/dirs to install
src  = "target/x86_64-unknown-linux-musl/release/ferry"  # {workdir} expanded
dest = "/usr/local/bin/ferry"                            # absolute remote path
kind = "file"             # "file" (default) or "dir" (rsync --delete); both ship via rsync
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
filesystem) over rsync — a plain copy for files, `--delete` for dirs (perms
preserved, but not the local uid/gid). On the host, in one transaction: move the live file/dir
aside to `<dest>.tug-bak`, rename the new one into place (atomic — safe even
though a running ELF can't be written in place), restart the unit, then
health-check. If the check never passes, every artifact is restored from its
backup and the unit restarted on the old version; tugboat exits non-zero.

## Deploy events (local)

Every deploy attempt appends one JSON line to
`${XDG_DATA_HOME:-~/.local/share}/tugboat/deploys.jsonl` on the machine that ran
it — the timing breakdown, and the failures that never reach the host at all.

```json
{"v":1,"at":1785087648,"name":"tide","host":"deepwa7er","source":"default_branch",
 "sha":"a99c146b…","short":"a99c146b","branch":"main","dirty":false,
 "result":"deployed","build_ms":14245,"ship_ms":1786,"install_ms":2364,"total_ms":20173}
```

This is **not** the host ledger, and the split is deliberate:

| | host ledger (`tugboat-ledger`) | this file |
|---|---|---|
| answers | what is this service running *now* | how did the deploy go |
| written | inside the remote transaction, on the host | locally, after the deploy |
| durability | must never be lost — a dashboard would lie | best-effort; a lost line costs a chart row |
| read by | lighthouse | warehouse (local ingest), eventually |

The ledger's entry is composed *before* the deploy runs (both outcomes are baked
into the remote script, so the host can pick one inside the same transaction as
the install) — which is precisely why nothing measured *during* a deploy can go
there. Hence a second, separate record.

- `result` is `deployed` or `failed`; `stage` names where a failure happened
  (`build`, `artifacts`, `ship`, `install`).
- **`rolled_back` is deliberately absent.** When the remote transaction fails,
  tugboat sees only a non-zero exit — it can't distinguish a health-check
  rollback from a failed `sudo` or a dropped ssh connection. The host ledger
  *does* know, so join the two on `at`: both sides stamp it from the same value,
  as does the transcript id.
- `build_ms` is absent (not `0`) when the build was skipped.
- Writing is best-effort — a deploy's outcome never changes because an analytics
  write failed; problems are warned about and dropped.

```sh
# slowest deploys
jq -r '[.total_ms,.name,.result]|@tsv' ~/.local/share/tugboat/deploys.jsonl | sort -rn | head
```

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
