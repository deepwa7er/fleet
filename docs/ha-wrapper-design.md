# HA wrapper — v1 design

**Status: design accepted, not implemented.** This document records the design
decisions for the fleet's high-availability wrapper, settled in an interview on
2026-08-18. No code exists yet; the build order at the bottom names the PRs
that will. Anchored to Fizzy card
[#75 — Dynamic load management](https://fizzy.intern.deepwa7er.net/1/cards/75).

## What this is

A generic wrapper that gives **automatic failover and replication-grade
durability** to any unmodified service whose entire state lives in SQLite.
When a wrapped service's machine dies, a warm standby on another machine is
promoted and breakwater reroutes to it — no human involved, downtime bounded
by the lease TTL plus promotion time.

What v1 deliberately does **not** do:

- **No write scaling.** One primary per service at all times. Standbys serve
  no traffic; they exist to take over. Read-replica routing and active-active
  are explicitly out of scope (they were steps 4–5 of the original sketch and
  were cut).
- **No protection for the VPS itself.** The VPS is the ingress (breakwater)
  and the lease authority. If it dies, everything is down — a pre-existing
  property of the architecture that this design does not worsen and does not
  fix.
- **No state outside SQLite.** Filesystem blobs (e.g. Active Storage upload
  directories) are not replicated. See *Eligibility* below — this is enforced,
  not assumed. Campfire and Fizzy therefore cannot be wrapped until a v2
  blob-directory story exists.

## Why failover, not load balancing

The fleet's four machines are the VPS (always on), the laptop (usually on),
the desktop (often powered off), and the Mac (sleeps). The practical pool for
any service is one to three highly variable backends. The scarce resource is
not throughput, it is *a machine being up at all* — so the machinery worth
building is health-aware failover with replicated state, not request
spreading. A wrapper below the app can deliver exactly that for unmodified
apps; true multi-writer scaling always requires the app's cooperation
(conflict semantics live in the app, not the filesystem), so it is not
attempted generically.

## Decision record

Each decision below was made deliberately; the alternative considered is noted
where it clarifies the choice.

1. **Coordination: a TTL'd lease service on the VPS.** A new standalone fleet
   service issues per-service leases; the lease holder is the primary.
   Rationale: the VPS is already the single point of failure for ingress, so
   anchoring coordination there adds no new failure mode, and a lease server
   is radically simpler to build and reason about than consensus. (Raft — 
   hand-rolled or via crate — was considered and rejected: it would keep
   primaryship alive through VPS loss, but ingress dies with the VPS anyway.)
2. **Replication: asynchronous WAL shipping (Litestream-style).** A per-machine
   agent tails each wrapped service's SQLite `-wal` file and streams frames to
   standby agents. The app is unmodified and unaware. Accepted RPO: the last
   few seconds of writes may be lost on failover. (FUSE-level interception,
   LiteFS-style, was considered and deferred — it buys synchronous capture and
   filesystem-level fencing at a much higher build cost.)
3. **Topology: one agent per machine,** managing every wrapped service on that
   box — one lease-service connection, one transport, one systemd unit, one
   place for machine-level facts. (Sidecar-per-service was considered:
   stronger isolation, N× the moving parts.)
4. **Transport: plain HTTP between agents** (axum/hyper, matching the fleet's
   stack). A standby streams frames from the primary's agent over a long-lived
   response body; snapshots and acks are further endpoints. Debuggable with
   `curl -N`; the tailnet supplies encryption and identity. (gRPC/tonic was
   considered and rejected: the payload is opaque WAL bytes, the fleet has no
   protobuf toolchain, and gRPC solves none of the actual replication
   problems — positions, catch-up, resume.)
5. **Failover: fully automatic.** Lease expires → the standby with the
   freshest replication position is promoted → breakwater reroutes. No human
   in the loop.
6. **Route updates: breakwater subscribes to a watch stream** from the lease
   service and hot-swaps affected routes in memory (`arc-swap`, the same
   pattern as its ACME certificate rotation). On a lost subscription it keeps
   serving the last-known primary — stale routing beats no routing.
   (Poll-on-interval adds its period to every failover; rewrite-config-and-
   restart drops every in-flight request and WebSocket fleet-wide.)
7. **Fencing: stop the app + starve the ingress.** A primary that cannot renew
   its lease has its service stopped by the local agent via systemd, *and*
   breakwater only ever routes to the current lease holder. Either alone has a
   hole (agent wedged; direct tailnet traffic bypassing breakwater); together
   they close it. WAL tailing cannot block the app's local writes, so fencing
   must live at the process and routing layers.
8. **Divergence: quarantine, then rejoin.** A deposed primary that comes back
   holding unreplicated writes (the async tail) snapshots its database aside
   — timestamped, kept locally, surfaced in lighthouse — then rewinds to the
   new primary's lineage and rejoins as a standby automatically. The lost tail
   is never silently destroyed. (Auto-discard was rejected: "accepted as
   possible loss" and "destroyed without a trace" are different promises.
   Halt-for-human was rejected: it turns every failover into homework.)
9. **v1 scope: SQLite only, enforced.** See *Eligibility*.

## Components

Names are proposals (see *Open questions*); placeholders used here:
**harbormaster** (lease service) and **wake** (per-machine agent).

### harbormaster — lease service (new fleet service, VPS)

- Owns a lease per wrapped service: `{service, holder, generation, expires_at}`.
- Primaries renew on an interval well inside the TTL. Expiry makes the lease
  grantable.
- Standby agents continuously report their replication position; on expiry,
  harbormaster grants the lease to the reporting standby with the freshest
  position and increments the **generation** (a monotonic promotion counter —
  frames are tagged with it, which is how a deposed primary's leftover writes
  are recognized as divergent).
- Serves the watch stream breakwater subscribes to.
- Ordinary fleet service: own `deploy.toml`, own SQLite for lease state,
  `/healthz`, enrolled in lighthouse.

### wake — per-machine agent

One systemd unit per machine; its config lists the wrapped services on that
box. Per service it plays one of two roles:

- **Primary role:** holds and renews the lease; controls SQLite checkpointing
  (the Litestream technique — the agent holds a read lock so the app cannot
  checkpoint the WAL out from under the tail, and performs checkpoints
  itself); serves the frame stream and snapshot endpoints over HTTP; on lease
  loss, stops the service via systemd (fencing).
- **Standby role:** keeps a local replica by streaming frames from the primary
  agent (bootstrap = full snapshot fetch, then the stream from that position);
  reports its position to harbormaster; on grant, replays to the tip, starts
  the service via systemd, and begins renewing.
- **Rejoin after deposition:** on seeing a higher generation than its own, the
  agent quarantines the local database (timestamped copy), re-bootstraps from
  the new primary's snapshot, and enters standby role. v1 "rewind" is a full
  re-clone — correct and simple; incremental rewind is an optimization for
  later.

### breakwater changes

- Route targets grow a **pool** form: a set of `machine:port` upstreams plus a
  policy. v1 policies: `stateless` (health-checked selection across all
  upstreams) and `single-writer` (route only to the current lease holder).
- A watch-stream client against harbormaster; primary changes swap the
  affected route via `arc-swap`. Subscription loss ⇒ keep last-known routing.
- Connect-failure handling for pools: try the next healthy upstream
  (stateless) or 502 (single-writer — never guess at a primary).

### deploy.toml / tugboat changes

- A `[topology]` section: `class = "stateless" | "single-writer"` and
  `hosts = [...]` (today's singular `host` remains the degenerate case).
- `fleet deploy` ships a multi-host service to every listed host.
- `fleet gen` emits pool routes (MagicDNS upstreams) instead of the hardcoded
  `127.0.0.1:<port>`, and emits wake's per-machine service lists.

## Eligibility — enforced, not assumed

A service may be wrapped as `single-writer` **only if its entire mutable state
is the declared SQLite database**. Replicating the database while the app also
writes local files does not merely lose the files on failover — it produces a
database that references files that do not exist on the new primary.
Inconsistency is worse than absence, so the rule is mechanical:

> wake refuses to wrap a service whose `deploy.toml` declares state files
> outside the database (`state.files = true`), and the fleet doctrine for new
> services is: keep all state in SQLite or stay out of the wrapper.

Consequence: the fleet's own Rust services are immediately eligible; Once apps
(Campfire, Fizzy) are not until blob-directory replication exists (v2).

## Failure walkthroughs

- **Primary machine dies.** Renewals stop; lease expires (TTL); harbormaster
  grants to the freshest standby at generation *g+1*; breakwater's watch
  stream delivers the change and the route swaps; the new primary's wake
  starts the service. Downtime ≈ TTL + replay-to-tip + service start. Writes
  in the unshipped tail are lost (accepted RPO).
- **Deposed primary returns.** Its wake sees generation *g+1* > its own *g*,
  quarantines the local DB, re-bootstraps from the new primary, rejoins as
  standby. It never starts the service, because it holds no lease.
- **Network partition (primary alive but cut off).** Primary's wake fails to
  renew and stops the service (fence 1); breakwater routes only to the new
  holder (fence 2). No split brain: the old primary can neither receive
  ingress traffic nor keep running.
- **harbormaster down.** No promotions and no route changes, but existing
  primaries keep serving (breakwater keeps last-known routes; wake treats
  "can't reach harbormaster" as distinct from "lease denied" and does not
  self-fence while the authority itself is unreachable — otherwise a lease-
  service deploy would stop every wrapped service fleet-wide). Availability
  degrades to today's status quo until harbormaster returns.
- **All standbys offline (desktop off, Mac asleep).** The primary serves
  alone; replication buffers or falls behind harmlessly (async). Failover is
  simply unavailable until a standby returns and catches up — the design
  degrades to exactly today's fleet.

## Open questions — proposed defaults (veto in review)

- **Names.** Lease service: `harbormaster` (assigns berths). Agent: `wake`
  (the WAL stream is literally the trail the primary leaves behind).
- **Lease TTL / renewal.** 10s TTL, renew every 3s — failover detection in
  ≤10s without flapping on transient tailnet blips. Tune with data later.
- **Snapshot bootstrap.** `VACUUM INTO` on the primary side for a consistent
  snapshot file, streamed over the agent HTTP channel, then frames from the
  snapshot's WAL position.
- **Checkpoint policy.** Agent-controlled checkpoints after acked ship of the
  checkpointed range, plus a size threshold, mirroring Litestream's approach.
- **Pilot service.** Smallest in-repo SQLite-backed service, or a purpose-built
  toy service (a trivial counter app) first — a toy makes kill-the-primary
  testing consequence-free. Decide when implementation starts.
- **Quarantine retention.** Keep the last N=5 quarantined snapshots per
  service, surfaced in lighthouse; manual cleanup.

## Build order

Each step lands as its own card + PR per the fleet workflow and is useful on
its own. **None of these are started.**

1. **breakwater: upstream pools + health checks** — the `stateless` policy end
   to end, proven on a stateless service. No harbormaster yet.
2. **wake: WAL shipping + warm standby** — replication without failover;
   verify replica fidelity at leisure; cross-machine durability for free.
3. **harbormaster + automatic failover** — leases, promotion, watch stream,
   fencing, quarantine/rejoin. The wrapper is real at the end of this step.

Deliberately after v1, if ever: read-replica routing with read-your-writes
(step 4), blob-directory replication enabling Once apps (v2), active-active
for cooperating services (step 5).
