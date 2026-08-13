# The posts are not backed up yet

**Status: known gap, deliberately deferred.** The blog's SQLite database lives
at `/opt/blog/storage/production.sqlite3` — the standard layout, the same one
`readout` uses — and nothing under `/opt` is in the fleet's encrypted offsite
backup set.

This file records why, so the gap is not rediscovered as a surprise after a
disk failure.

## Two things block it

**1. External members are invisible to `tugboat fleet gen`.**

`fleet-backup/state.sh` is generated from two sources: each service's `[state]`
block, and `fleet.toml`'s `[backup]` block. The generator can only read
manifests inside the monorepo, and this repo lives outside it. A `[state]`
block in `deploy.toml` would be silently ignored — worse than absent, because
it would read as enrollment. `lagoon` has the same constraint and is handled
via `fleet.toml [backup]` instead.

**2. `fleet-backup` only reaches `/var/lib/<service>/`.**

From the generated `state.sh`:

```sh
# /var/lib/<entry> SQLite databases, snapshotted via the online-backup API.
SQLITE_DBS="clothes/clothes.db depot/depot.db drydock/drydock.db lagoon/lagoon.sqlite recipes/recipes.db regatta/regatta.db"
```

Every entry is relative to `/var/lib/`. There is no mechanism today for a
database somewhere else.

## readout has the identical gap

`readout` keeps its database at `/opt/readout/storage/` and declares no state
anywhere, so its results are equally unbacked-up. That is the reason this app
did not simply move itself to `/var/lib/blog`: one app quietly using a
non-standard path would fix one symptom, leave the other, and leave the fleet
with two conventions for where a containerised Rails app keeps its data.

## Options, when it is time

| Approach | What it costs |
|---|---|
| Move both apps' storage to `/var/lib/<name>/` and add both to `fleet.toml [backup]` | One provision re-run and a data move per app; no new machinery. Restores the single convention. |
| Teach `fleet-backup` an absolute-path form (`sqlite_paths = [...]`) | A change to the generator and the backup script; lets a service keep `/opt` and still be backed up. |
| Add a `dirs`-style entry rooted at `/opt` | Backs up the image tar too, which is large and rebuildable. Not worth it as-is. |

The first is the smallest and keeps the fleet consistent; the second is the
more general fix if `/opt` is the layout you actually want for containerised
services.

## In the meantime

A manual snapshot is one command, and SQLite's online backup API is safe to run
against a live database:

```bash
ssh vps 'sqlite3 /opt/blog/storage/production.sqlite3 ".backup /tmp/blog-backup.sqlite3"'
scp vps:/tmp/blog-backup.sqlite3 ~/backups/blog-$(date +%Y%m%d).sqlite3
ssh vps 'rm -f /tmp/blog-backup.sqlite3'
```

Do not copy the file with `cp`/`scp` directly while the app is running — a
plain copy of a live SQLite database can capture a torn write.
