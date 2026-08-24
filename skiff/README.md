# skiff

Skiff is the live agent desk described by [DW-004](../docs/skiff-architecture.md):
one Rust service, one React client, and one multiplexed WebSocket. It presents
Pi, Muse, and OpenCode sessions beside durable fleet changes, and it owns the
small command surface needed to send, abort, curate, review, and land work.

The Rails application and Node bridge were deleted at the M7 cutover. Harness
formats now end at native Rust adapters; the derived SQLite read model is safe
to discard; authored change state remains in the append-only `crates/change`
log shared with `dw`.

## Run locally

```sh
cd web
bun install --frozen-lockfile
bun run build
cd ../..
cargo run -p skiff
```

Open `http://127.0.0.1:8120`. The development default is loopback deliberately:
Skiff has no application-level authentication and can drive coding agents.
Production binds only the desktop's Tailscale address.

For client iteration, run `bun run dev` in `skiff/web` and `cargo run -p skiff`
at the workspace root. Vite proxies `/ws` to `127.0.0.1:8120`.

Every path has a default and a matching flag or environment variable; use
`cargo run -p skiff -- --help` for overrides. The read model defaults to
`$XDG_STATE_HOME/skiff/read-model.sqlite3` (or `~/.local/state/skiff`). It is
derived and can always be rebuilt.

## Install on the Fedora desktop

From a Fleet checkout on the desktop:

```sh
skiff/deploy/install-skiffd.sh
```

The idempotent installer builds and tests the React client, builds the release
binary, stages a versioned client bundle, atomically switches the installed
artifacts, retires the old Rails and bridge user units, and verifies
`/healthz`. It installs:

| Path | Purpose |
|---|---|
| `~/.local/bin/skiffd` | release binary |
| `~/.local/share/skiffd/current` | active versioned client bundle |
| `~/.config/skiff/skiffd.sh` | tailnet-only production wrapper |
| `~/.config/systemd/user/skiffd.service` | supervised user service |
| `~/.config/systemd/user/opencode-serve.service` | optional OpenCode source |

Useful operations:

```sh
systemctl --user status skiffd
systemctl --user restart skiffd
tail -f ~/.local/state/skiff/skiffd.log
```

The service is available at `https://skiff.intern.deepwa7er.net` through
Breakwater, or directly at `http://fedora.tailcfab97.ts.net:8120` on the
tailnet. Breakwater must preserve HTTP Upgrade because all reads use the one
WebSocket.

## Complete the Rails cutover

The desktop installer deliberately leaves the old VPS deployment alone. Keep
that rollback copy until the installer has committed **and** Breakwater has
been deployed with its `skiff` route pointing at the desktop. Then retire the
VPS copy from a machine with the `vps` SSH alias:

```sh
ssh vps 'bash -s' < skiff/deploy/retire-vps.sh
```

The retirement script is idempotent and guarded. It requires the canonical
`/healthz` endpoint to return Skiffd's exact `ok` response, refuses an unknown
unit or any unexpected file under `/opt/skiff`, removes the Rails container,
image tag, systemd unit, bridge resolver, runtime environment, secrets and
Lighthouse enrollment, and preserves the Tugboat deployment ledger as
historical evidence.

## Security and failure behavior

- The production wrapper resolves the Tailscale IPv4 address at every start
  and never binds `0.0.0.0`. Tailnet membership is the authentication boundary.
- There is no bridge password or Skiff secrets file. Optional Tugboat and Fizzy
  writes use those tools' own token files.
- Missing Pi, Muse, or OpenCode dependencies become named source errors; one
  unavailable harness never kills the service or hides the healthy sources.
- Landing drains through shutdown once it has started. After the irreversible
  push, deploy, public-record, and Fizzy outcomes are recorded independently;
  `dw finish <card>` retries only unfinished tail steps.
- The PWA service worker caches only immutable Vite assets and the offline
  page. Navigations are network-first; WebSocket and authored data are never
  cached.

## Gates

From the Fleet workspace root:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
TUGBOAT_FLEET=$PWD/fleet.toml cargo run -q -p tugboat -- fleet gen --check
(cd skiff/web && bun run build && bun run test)
```

Rust wire types generate `web/src/gen/`, and drift is a failing test. To accept
a protocol change:

```sh
SKIFF_WRITE_TYPES=1 cargo test -p skiff --test types
```

The generator normalizes its output before both writing and comparing it;
`web/src/gen/` contains no hand-written files. The hand-written import barrel
is `web/src/types.ts`.

## Code map

| Path | Responsibility |
|---|---|
| `src/ingest/` | native Pi, Muse, and OpenCode adapters; watermarks and topics |
| `src/store/` | rebuildable SQLite read model |
| `src/run/` | supervised native harness commands and live overlays |
| `src/views/` | closed set of subscribed live queries |
| `src/wire.rs` | typed WebSocket protocol and TypeScript roots |
| `src/server.rs` | WebSocket multiplexer, commands, health, and static bundle |
| `web/src/lib/socket.ts` | reconnecting client subscription store |
| `web/src/App.tsx` | responsive rail and one/two-pane workspace |
| `../crates/change` | authored log, jj, structured diff, landing, and tail |
| `../crates/dw` | local/offline human CLI over the same change crate |

All seven milestones in [DW-004 §13](../docs/skiff-architecture.md) are
implemented. Deployment remains a human operation after pull-request review;
the repository does not auto-merge or auto-deploy this cutover.
