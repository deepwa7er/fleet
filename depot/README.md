# depot

The fleet's **data warehouse** — live at https://depot.intern.deepwa7er.net.

Every other fleet service holds a snapshot of *now*. When a drydock ticket
closes, the fact that it was open three days is gone; lighthouse's health store
is the only history anywhere, and it prunes to a retention window. depot is the
historian for the facts that would otherwise be overwritten, pruned, or never
recorded at all.

## What it is (and isn't)

"Data warehouse" could mean two things here, and only one was worth building:

- **A query surface** joining the fleet's SQLite DBs — low value. ~14 tiny
  services, a few thousand rows each, and the join keys between wardrobe items
  and recipes don't exist.
- **A historian** keeping what gets overwritten — the real thing. The value is
  not breadth across services, it's **time**: being able to take derivatives.

So the design question was *which facts do you want the derivative of?* Three
were both valuable and unrecorded anywhere: **usage**, **deploys**, and
long-horizon health.

## Sources

Built emitters-first, deliberately: storage last, because the fleet didn't emit
the facts worth warehousing. Both emitters stand on their own.

| Source | How | What it answers |
|---|---|---|
| breakwater access log | **pulled** from journald every 60s | which services actually get used |
| tugboat deploy events | **pushed** to `/api/events/deploy` | how long deploys take, what fails, where |

The split is not arbitrary. depot runs on the VPS beside breakwater's journal,
so access-log ingest is a local read with no delivery to fail — and the journal
is the buffer if depot is down. tugboat runs on the dev box, which sleeps and is
often off the tailnet, so it cannot be polled and pushes instead (keeping its own
local JSONL as the durable record).

## Why it runs on the VPS

The dev box sleeps. A warehouse with holes in it is not a warehouse. That rules
out the `source`/`atlas` pattern even though depot has no need of the working
trees.

## Why SQLite, not a columnar engine

The original sketch called for DuckDB over Parquet. Measurement killed it:

- **~823k access rows a year (~160 MB).** Columnar earns its keep at a scale
  this is nowhere near; SQLite answers every query here from an index.
- **The VPS has ~1.0 GB RAM available.** A memory-hungry analytical engine is a
  bad tenant on that box.

Using `fleet-common`'s `open_migrated` also keeps the migration invariants and
the static-musl cross-build that clothes, recipes and drydock already prove.

## Ingestion is idempotent — on purpose

Both paths can deliver the same record twice: journald can be re-read from an
older cursor, and tugboat retries a push. Every table therefore carries a natural
key with a `UNIQUE` constraint and inserts use `INSERT OR IGNORE`.

That single property is what makes recovery trivial: **re-ingesting is always
safe**, so nothing ever has to reason about what was already stored. To backfill
everything the journal still holds, delete the cursor and run one pass:

```sh
rm /var/lib/depot/breakwater.cursor
depot ingest      # prints seen / stored / skipped / dropped upstream
```

Position is journald's job, not ours — `journalctl --cursor-file` resumes after
the last entry it handed over, so depot never reasons about timestamps.

## Gaps stay visible

breakwater drops access records rather than delay a request under load, and says
so with `{"kind":"access_dropped","count":N}`. depot counts those and warns:
those requests are permanently absent and no amount of re-reading recovers them.
A gap in the data reads as a gap, never as quiet.

## API

```
GET  /healthz
GET  /api/summary                          row counts + the window held
GET  /api/usage?days=7&include_probe=false requests per host, busiest first
GET  /api/deploys?limit=50                 recent deploys, newest first
POST /api/events/deploy                    ingest one tugboat event (idempotent)
```

`include_probe` defaults to false. lighthouse's reachability probe fetches every
routed host on an interval, so counting it as usage would make every service look
equally popular; it identifies itself as `lighthouse-probe/1`.

```sh
curl -s 'https://depot.intern.deepwa7er.net/api/usage?days=7' | jq -r \
  '.[] | "\(.requests)\t\(.host)"'
```

There is **no web view yet** — the surface is the JSON API, so `/` 404s.

## Layout

```
src/schema.rs   append-only fact tables; natural keys, INSERT OR IGNORE
src/store.rs    typed facts in, aggregates out
src/ingest.rs   journald -> access records, position via --cursor-file
src/server.rs   the HTTP surface
src/main.rs     `serve` (server + ingest loop), `ingest` (one pass)
```

## Deploy

Binary ships via [tugboat](../tugboat) (`deploy.toml`). Infrastructure is
`deploy/provision.sh` — run it for first-time setup and whenever the unit
changes.

The one non-obvious bit is **journal access**: depot's entire access-log source
is `journalctl -u breakwater`, and journald gates reads of another unit's logs on
membership of `systemd-journal`. `provision.sh` adds the service user to that
group *and* the unit sets `SupplementaryGroups=`, because systemd builds the
process's groups from the unit rather than from `/etc/group` alone. Either one
alone is silently useless — the ingest loop runs and returns nothing, which looks
exactly like "no traffic" rather than "not permitted".

## Not done yet

- **Retention and rollups.** Raw rows are kept indefinitely today. The plan is
  raw ~90d, then hourly/daily aggregates kept forever — at which point
  lighthouse's pruner becomes a hand-off rather than a delete, and journald's
  `SystemMaxUse` can be capped (it currently holds 3.3 GB) because depot becomes
  the long-term store.
- **Nightly snapshots of service DBs**, discovered via each `deploy.toml`'s
  `[state]` block and read with fleet-backup's online `.backup` path.
- **A web view.**
