# Drydock

A ticket queue for autonomous fleet work. A single Rust binary serves a web
view (for you) and a worker CLI (for Claude) over one SQLite store, so a human
answer posted from the browser is visible to the worker on its next run.

Scope today: **modifications to existing services**. New-service tickets come later.

## How it fits together

```
Claude (scheduled task)  ──CLI──┐
                                 ├──► drydock serve ──► SQLite
You (browser / phone)  ──HTTP───┘        (single writer)
```

`drydock serve` is the only writer to SQLite. Every CLI subcommand is a thin
HTTP client over the local server, which keeps writes serialized and lets the
web view reflect worker activity live (polled).

## Ticket lifecycle

```
            ┌──────────── you answer ───────────────┐
            │                                         │
create→ open ──claim──▶ in-progress ──needs-input──▶ needs-input
            ▲                │  │  │
            │ you unblock    │  │  └── resolve(pr) ─▶ in-review ──you: done──▶ done
            │                │  └───── block ───────▶ blocked ──you: unblock──┘
            └─ stale reclaim ┘
```

Transitions are enforced in `core::state`; illegal ones return 409. `claim` is a
compare-and-swap (`open → in-progress`), so two runs can't grab the same ticket.
A crashed run that leaves a ticket stuck `in-progress` past `DRYDOCK_STALE_HOURS`
is automatically returned to `open` on the next `drydock next`.

## Build & install

```sh
# web bundle (output: web/dist, served by the binary)
cd web && bun install && bun run build && cd ..

# binary (installs `drydock` into ~/.cargo/bin)
cargo install --path .
```

## Run the server (the always-on daemon)

```sh
drydock serve
```

Configuration (environment):

| Var                  | Default                            | Meaning                                   |
| -------------------- | ---------------------------------- | ----------------------------------------- |
| `DRYDOCK_DB`         | `$XDG_DATA_HOME/drydock/drydock.db`| SQLite database path                      |
| `DRYDOCK_ADDR`       | `127.0.0.1:7878`                   | listen address (also used by the CLI)     |
| `DRYDOCK_WEB_DIR`    | `web/dist`                         | built web bundle to serve                 |
| `DRYDOCK_STALE_HOURS`| `3`                                | reclaim in-progress tickets older than    |

The web view is at `http://127.0.0.1:7878/`. To reach it from your phone later,
front it with breakwater at e.g. `drydock.internal.deepwa7er.com`.

## Worker CLI (what the scheduled task calls)

```sh
drydock next --json                       # next actionable ticket (reclaims stale first)
drydock claim <id> --branch <branch>      # open -> in-progress; exit 2 if already claimed
drydock show <id> --json                  # full ticket + thread (for resume)
drydock needs-input <id> "question"       # -> needs-input, parks for you
drydock block <id> "reason"               # -> blocked (use instead of hacking around a wall)
drydock resolve <id> --pr <url>           # -> in-review, links the PR
```

The worker never merges or deploys; `in-review` is its terminal state. See
[`docs/worker-task.md`](docs/worker-task.md) for the full prompt to paste into a
Claude Desktop local scheduled task.
