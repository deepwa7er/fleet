# KEEP — THE FLEET'S CENTRAL DATABASE

```
┌──────────────────────────────────────────────────────────────────────┐
│ DOC. NO.  DW-005          REV. B          CLASSIFICATION: INTERNAL     │
│ SUBJECT   keep: the fleet's own Rust database server over turso        │
│ ORIGIN    Design session 2026-08-29 (Mac lane, cards #143–#145)        │
│ STATUS    Proposed — implementation cards #144 (build), #145 (migrate) │
│ SCOPE     keep (new) · fleet-backup · fleet-common · [state] blocks    │
└──────────────────────────────────────────────────────────────────────┘
```

**REV. B, same day as REV. A.** REV. A chose libSQL's `sqld` — the Turso
family's production server — as a foreign daemon on OVH. On reflection the
owner rejected it: a beta-bannered daemon with a C core under a fleet whose
whole ethos is small, owned Rust services. REV. B replaces the daemon with
**the fleet's own server**: keep is an axum service in this monorepo that
embeds the same engine family (the pure-Rust `turso` crate) and speaks to the
fleet over its own API. Everything else — home, layout, backup discipline,
migration order — carries over unchanged.

## What keep is

One Rust service running on the OVH VPS, embedding the **turso engine**
(SQLite-compatible SQL, pure Rust, MVCC) as a library and holding **one
database per fleet service**. Fleet apps connect to it over the tailnet and
treat it as their primary store. Apps stop owning durable data on their own
machines — a service deploys to any machine without carrying data, and a dead
machine costs no data.

The name: the castle keep — the strongest tower, built to hold out through a
siege while everything valuable sits inside, and the English word for keeping.

## Why

Three forces converged on 2026-08-29:

1. **The fleet is now multi-machine** (old VPS → OVH migration, the Fedora
   laptop's Docker services, the desktop). A path convention inside one
   `/var/lib` cannot unify machines; only a service can.
2. **The backup story has known holes.** Card #32 records that blog and
   readout keep their SQLite at `/opt/*/storage`, outside fleet-backup's
   reach — and notes and public_site have since grown the identical gap.
   Recovery point is nightly at best, silent failure has no alarm, and no
   restore has ever been drilled.
3. **The owner wants one thing to back up and one thing to restore.** The
   whole point of this design is that "the fleet's data" has exactly one
   answer.

And the force that produced REV. B: **ownership**. A database server built by
this repo, reviewed by this repo's gates, and deployed by tugboat is a fleet
member; a third-party daemon is a dependency with a roadmap we don't control.

## The decision

| Question | Answer |
|---|---|
| Engine | **turso** — the pure-Rust SQLite-compatible engine, embedded as a library |
| Server | **Ours**: an axum service in this monorepo, built and deployed by tugboat like every other service |
| Home | The OVH VPS (permanent), native binary under systemd — no Docker |
| Reachability | Tailnet only; listener bound to `100.73.64.99` |
| Auth | One bearer token per database; tokens live in per-app `/etc` env files |
| Data layout | **One database per app** — the 1:1 continuation of today's one-file-per-app, and every app is already a single writer |
| API | SQL + bound params + transactions over HTTP/JSON (sketch below) |
| Backup | `VACUUM INTO` snapshots every minute → restic → R2, plus a nightly long-retention tier (7d/4w/6mo); snapshots are plain SQLite, no `.dump` needed |
| Migration | One service at a time; recipes pilots |
| Store-down posture | Apps hard-fail (v1); a dead store takes the fleet's writes with it, visibly |

SQLite dialect is the load-bearing choice: every fleet service already speaks
it, so fleet-common's open/migrate plumbing grows a keep client backend
instead of every app rewriting its data layer.

## API surface (designed in card #144 — `keep/README.md` is the contract)

- `POST /v1/{db}/query` — `{sql, params}` → rows; single statement,
  autocommit. Errors use fleet-common's `{"error": …}` shape.
- `POST /v1/{db}/tx` — a batch of statements executed atomically.
- `Authorization: Bearer <token>` per database; no database is reachable
  without its token; the listener itself is tailnet-only.
- `fleet-common` owns the client, so apps never hand-roll HTTP or JSON.

At this fleet's write volume, a thin HTTP/JSON seam costs nothing and buys
total control of the protocol. The precedent is rqlite, which has run this
exact shape (server around an embedded SQLite) for a decade.

## Architecture

- **keep is a deployable like any other**: a top-level `keep/` directory with
  a `deploy.toml`, discovered by tugboat, built through the standard pipeline,
  run as a systemd unit. Provisioning is dnf-based (OVH is Fedora 44).
- **One database per service.** Ownership stays exactly as clear as today's
  file-per-app model. Breakwater routing and lighthouse need no changes; keep
  is reached by apps, not by users.
- **fleet-common** gains the keep client so the open/migrate dance keeps its
  single home. Apps keep running their own schema migrations at startup, now
  against their database in keep.
- **Build, verified not flagged**: turso's `io_uring` is an opt-in
  non-default feature the `turso` facade crate does not forward — it is not
  in the build and never was the risk (checked against turso 0.8.0-pre.7's
  manifest during card #144). keep builds with `default-features = false`
  (system allocator, no FTS — it needs neither, and shedding them drops the
  C the defaults pull in); the slimmed tree check-compiles for
  `x86_64-unknown-linux-musl` cleanly. No glibc fallback, no Docker.
- **`[state]` shrinks honestly.** As each app migrates, its `db =` declaration
  leaves `deploy.toml` and `fleet gen --check` keeps the backup set honest
  during the transition — a service is either on file backup or in keep, and
  the generated registries show which, per app, at every gate.
- **fleet-backup survives, smaller.** It keeps covering what keep cannot
  reach: ferry config, breakwater's ACME cache, tugboat ledgers, lagoon
  (deferred), and the Docker boxes' file state (Fizzy, Jellyfin). Its DB half
  retires app by app as migrations land.

## Backup design

The scale insight that shapes everything: **the fleet's data is kilobytes to a
few megabytes**, so "continuous backup" is simply *snapshot the whole file
every minute* — keep runs `VACUUM INTO` per database (a consistent
plain-SQLite single file, no WAL sidecar) and hands the snapshot dir to
restic → R2. Deduplication makes each snapshot tiny; the recovery point is
sixty seconds; restore is "copy the file back." No WAL-streaming machinery,
no CDC plumbing, no restore-replay tooling. (`VACUUM INTO` over a
checkpoint-and-copy: the turso facade exposes no checkpoint call, and a raw
copy of a live database can catch a torn write — the verified path wins.)

Two retention tiers, because backups are the reason keep exists:

1. **Primary — minute snapshots to R2**, forgotten past 48h: the recovery
   point, not an archive.
2. **Fallback — nightly snapshots**, retained 7d/4w/6mo, restorable into any
   SQLite-compatible engine without keep-specific code (verified against
   stock SQLite before this shipped — there is deliberately no SQL-text
   `.dump` path; the files already satisfy the property the dump was for).

**The restore drill is a hard gate** (card #144): before the first app
migrates, a restore from *both* paths must be performed into a scratch keep
and verified against the live data. A backup that has never been restored is
a rumor, not a backup — and with a beta engine under the data, this gate is
the load-bearing safety mechanism, not a formality.

## Migration order

recipes (pilot — small, real data, low blast radius, and the beta engine's
trust-building run) → mirror (its DB is a rebuildable Fizzy cache; safe
second). One jj change per app, full gates each time. The Rails services
(notes, blog, readout, public_site) retire in place — no Ruby client is
built for dying services, so their `/opt` gap (card #32) stays open until
they retire; their future Rust rewrites are keep-native from birth. Lagoon
(external repo, iOS) is deferred and keeps its file for now; Fizzy and
Jellyfin (Docker/Rails) are out of scope permanently for this design. Card
#145 carries the sequence; card #32 stays open until the Rails retirements
land it.

## Rejected alternatives — and why, so they stay rejected

- **libSQL `sqld` (REV. A's choice)** — the Turso family's production server:
  mature engine, real namespaces and auth, first-party S3 replication.
  Rejected because it is a beta-bannered foreign daemon with a C core and a
  company roadmap drifting toward its rewrite; the ownership cost outweighed
  the convenience. Its self-hosting shape remains the fallback if the turso
  engine proves unusable — that would be a REV. C, decided out loud.
- **Turso Database standalone** — no self-hosted server product; embedded
  only. It didn't disappear in REV. B: it became the engine *inside* keep.
- **Turso Cloud** — outsources the backup to a vendor; contradicts the
  self-owned, R2-based premise.
- **Postgres** — the boring-correct database server, declined by the owner.
  Real advantages (concurrent writers, cross-schema grants, decades of ops
  tooling) were weighed; dialect continuity and ops surface lost.
- **Collector/harvest pattern** (apps keep local primaries; a service ships
  copies into one store) — honest runner-up. Keeps write independence, but
  cross-app reads become mirrors-of-mirrors and the live data still dies with
  its machine. Kept in mind as a *complement* for the Docker boxes.
- **rqlite** — the decade-old proof of this REV. B pattern (server around an
  embedded SQLite). Go, HTTP/JSON, MIT. The nearest exit if turso
  disappoints; otherwise superseded by keep doing the same thing in Rust,
  in-repo.
- **MariaDB / CockroachDB / TiDB** — mature but Postgres-class ops weight, or
  distributed-consensus overkill; dialect rewrites either way.
- **CouchDB** — replication-native and battle-tested, but a document model
  that rewrites every app.
- **Dolt** — versioned data (git-for-data) is the coolest backup-native idea
  on the board, but MySQL dialect and the largest per-app rewrite.
- **Embedded engines without a server (redb, sled, fjall)** — using them
  means hand-building durability, query, and backup ourselves. REV. B is
  deliberately *not* that: the engine is turso's job; only the seam is ours.

## Accepted risks — stated plainly

- **A beta engine sits under the fleet's data.** The owner accepted this
  explicitly (2026-08-29) in exchange for full ownership. Mitigations: pilot
  on recipes before anything precious moves; minute snapshots plus nightly
  dumps; the restore drill as a hard gate; the engine version pinned and
  upgraded deliberately; willingness to file and fix upstream. The exit is
  real: the databases are SQLite-format files that walk into any
  SQLite-compatible engine.
- **The API seam is ours forever.** keep's HTTP contract and the fleet-common
  client are this repo's code, maintained like everything else here.
- **Dependency weight.** turso pulls ~300 crates including C sources; the
  pre-release engine upgrades deliberately (version pinned exactly), and
  `default-features = false` holds the C surface to what the slimmed tree
  needs. The musl cross-compile is verified, not assumed.
- **Single writer per database.** True of every app today (one connection
  behind a mutex); it is a ceiling, not a defect, at this fleet's scale.
- **A dead store stops the fleet's writes.** Hard-fail is v1's honest answer;
  a dead-man's switch on keep is part of standing it up, not an afterthought
  (card #144).

## Deferred

Cross-app reads (an app querying another app's database through keep),
read-through fallbacks while the store is down, lagoon enrollment, and any
GraphQL-style query surface over the data. None of these change the
foundation; all of them are easier after keep exists.

## Direction (2026-09-03) — keep as the fleet's data warehouse

Owner direction, recorded so agents stop re-deriving it: keep is the fleet's
data warehouse and the long-term primary store for as much service data as
possible. Services whose storage shape does not fit keep today are redesigned
to integrate — ferry's TOML command file is the first such case — rather than
left on local files plus fleet-backup. The pilot order above is the starting
sequence, not the ceiling; v1's Rust-only client support constrains sequencing,
not the destination. Caching and performance issues from a migration are solved
as they arise, against measurements — not speculated about in advance, and
never via workarounds. fleet-backup keeps covering whatever keep cannot yet
reach and shrinks honestly as migrations land (see Architecture above).

## Provenance

Designed in conversation, 2026-08-29. REV. A chose sqld; REV. B, same day,
replaced it with the fleet's own server over the turso engine. Cards: #143
(this doc), #144 (build keep + drill), #145 (service migrations). Companion
reading: card #32 (the `/opt` backup gap), `fleet-backup/README.md` (the
system keep shrinks).

Corrected 2026-09-03 during #144's build: io_uring was never in the build
(opt-in non-default — a design-room worry, struck after reading the
manifest); scope narrowed to Rust services, Rails retire (see card #145).
