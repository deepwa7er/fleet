# keep

The fleet's central database service (Fizzy #144, DW-005): the turso engine
embedded in a small axum server on OVH, holding one database per fleet
service. Fleet apps connect over the tailnet and treat keep as their primary
store; apps stop owning durable data on their own machines.

v1 serves **Rust clients via `fleet-common::keep` only**. The Rails services
retire in place; their future Rust rewrites will be keep-native from birth.

## API

```text
GET  /healthz                        "ok"
POST /v1/{db}/query   {sql, params?, batch?}  -> Outcome
POST /v1/{db}/tx      {statements:[...]}      -> {results:[Outcome]}
```

Every `/v1` route requires `Authorization: Bearer <token>` for that
database (401 without, 404 for an unknown database). The listener binds
OVH's tailnet address, so the token is the second layer, not the first.

`Outcome` is `{columns: [{name, decl_type}], rows, rowid, changes}`: rows are
arrays of tagged values (`{"type":"integer","value":42}` — null, integer,
real, text, blob), `rowid` is `last_insert_rowid`, `changes` the rows the
statement wrote. A `query` entry is one prepared statement (params may be
empty — a param-less SELECT still comes here). A `batch` entry is a
multi-statement script with no params (DDL, migrations); its outcome carries
no change count. `tx` runs its entries under `BEGIN IMMEDIATE`: all commit,
or all roll back with the error. Transactions are capped at 1000 statements.

Status codes follow the fleet `{"error": …}` shape: constraint/misuse → 400
(the client's fault), store busy → 503 (surface, don't spin), everything
else → 500. Services hard-fail on keep errors — a dead store takes the
fleet's writes with it, visibly, by design (DW-005).

## Running it

Configuration is environment (`deploy/keep.service` in production):

| Variable | Default | Meaning |
|---|---|---|
| `KEEP_ADDR` | `127.0.0.1:8106` | listen address (unit: `100.73.64.99:8106`) |
| `KEEP_DATA_DIR` | XDG `keep/` | live `<name>.db` files (`/var/lib/keep`) |
| `KEEP_TOKENS_FILE` | — (required) | `name token` per line |
| `KEEP_SNAPSHOT_DIR` | `<data>/snapshots` | `VACUUM INTO` staging |
| `KEEP_SNAPSHOT_INTERVAL_SECS` | `60` | snapshot cadence |
| `RESTIC_REPOSITORY` et al | — | off-box half (unit: `/etc/keep/restic.env`) |

```bash
printf 'recipes <token>\n' > /tmp/keep-tokens
KEEP_TOKENS_FILE=/tmp/keep-tokens cargo run -p keep
```

Provisioning OVH is `deploy/provision.sh` (first OVH service — user setup,
unit, tokens install, restic); routine deploys are `tugboat deploy` from
`keep/`. Provisioning a new app database is one line in the tokens file plus
a restart; its file is created lazily and its app's migrations arrive over
the client.

## Backup

Every minute each database is snapshotted with `VACUUM INTO` (a consistent
plain-SQLite single file — no WAL sidecar) and the snapshot dir is handed to
restic → R2. At fleet data size a full-file snapshot every minute *is*
continuous backup; the recovery point is sixty seconds. A nightly pass tags
one snapshot set `keep-nightly` and applies retention: minute tier forgotten
past 48h, nightly tier kept 7 daily / 4 weekly / 6 monthly.

The nightly tier doubles as the engine-independent fallback: snapshot files
open in stock SQLite (asserted by `store::tests::snapshots_open_in_stock_sqlite`,
which reads a snapshot through rusqlite with no keep code involved), so a
restore never depends on turso itself. There is deliberately no SQL-text
`.dump` path — the files already satisfy the property the dump was for.

## Restore drill (the hard gate)

No app migrates until a restore from **both** retention tiers has been
performed into a scratch keep and verified. Procedure, on OVH:

```bash
# 1. Canary: write a known row through the live keep.
# 2. Wait for a minute tick; confirm the snapshot exists in R2:
set -a; . /etc/keep/restic.env; set +a
restic snapshots --tag keep-minute   # and: --tag keep-nightly
# 3. Restore each tier to its own scratch dir (NOT the live snapshot dir):
restic restore latest --tag keep-minute --target /tmp/keep-drill/minute
restic restore latest --tag keep-nightly --target /tmp/keep-drill/nightly
# 4. Open each restored file with stock sqlite3 and read the canary back:
sqlite3 /tmp/keep-drill/minute/var/lib/keep/snapshots/recipes.db 'SELECT ...'
# 5. Boot a scratch keep against the restored files on a different port and
#    read the canary through the API with the real client (copy the restored
#    `<db>.db` files flat first — keep opens `<data_dir>/<name>.db`):
mkdir -p /tmp/keep-drill/data
cp /tmp/keep-drill/minute/var/lib/keep/snapshots/*.db /tmp/keep-drill/data/
KEEP_ADDR=127.0.0.1:8199 KEEP_DATA_DIR=/tmp/keep-drill/data \
  KEEP_TOKENS_FILE=/etc/keep/tokens /usr/local/bin/keep serve
# 6. rm -rf the scratch dirs. Both tiers verified, or the migration waits.
```

Record the drill (date, tiers, result) as a comment on card #144. A backup
that has never been restored is a rumor, not a backup.
