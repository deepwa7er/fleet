# SKIFF ARCHITECTURE

```
┌──────────────────────────────────────────────────────────────────────┐
│ DOC. NO.  DW-004          REV. A          CLASSIFICATION: INTERNAL     │
│ SUBJECT   Skiff rebuilt ground-up: one Rust service, one React client  │
│ ORIGIN    Design session 2026-08-23                                    │
│ STATUS    Accepted. Implemented through M7.                            │
│ SCOPE     skiff · crates/change · crates/dw · breakwater               │
└──────────────────────────────────────────────────────────────────────┘
```

One process that owns truth, one client that owns attention, and a boundary
between them that never needs re-litigating.

This is not a port. The Rails app and the Node bridge are both deleted at
cutover, and several of their central ideas are deliberately not carried
across. Where an idea *is* carried across, §12 says so and why.

---

## 1. WHAT SKIFF ACTUALLY IS

Skiff is a **live read model over state that other processes own**, plus a
small set of commands.

That sentence is the whole design. Everything that follows is a consequence of
taking it seriously, and every accident in the current system comes from never
having written it down.

There are exactly two kinds of state, and conflating them is why the bridge has
two parallel code paths for the same data — one where "the poll endpoints
compose file state with process state per request", another where "the registry
holds the composition continuously":

**Observed state.** Transcripts, run status, jj diffs, cards. Some other
program is the author. Skiff can always re-derive it, and must never be the
truth. If skiff's copy disagrees with the source, skiff is wrong.

**Authored state.** Annotations, round notes, the change state machine. Skiff
*is* the author. Small, append-only, fsync'd, irreplaceable. DW-002 gave
annotations to skiff because they are "the one thing with no other home"; that
remains the only thing skiff owns.

The split is the top-level module boundary, not a comment. Observed state gets
a rebuildable cache and no durability requirements at all. Authored state gets
an append-only log and no cache-invalidation questions at all. Neither
discipline leaks into the other.

---

## 2. SHAPE

```
                    breakwater (TLS)
                          │  http
                          ▼
  ┌───────────────────────────────────────────────────────────┐
  │  skiffd — one binary, one host (the Fedora desktop)        │
  │                                                            │
  │  sources    pi dir │ muse dir │ opencode │ jj │ change log │
  │      │      ─────────── native formats die here ────────── │
  │      ▼                                                     │
  │  ingest     normalize · watermark · invalidate topics       │
  │      │                                                     │
  │      ▼                                                     │
  │  store      SQLite (derived, rebuildable)                   │
  │      │                                                     │
  │      ▼                                                     │
  │  views      desk │ sessions │ session(id) │ change(r,c)     │
  │      │      ──── closed set, hand-written ───────────────── │
  │      ▼                                                     │
  │  socket     one WebSocket per client · snapshot + deltas    │
  │      │                                                     │
  │  static     the React bundle                                │
  └───────────────────────────────────────────────────────────┘
            │                                    ▲
            │ links                              │ links
            ▼                                    │
      crates/change ◄──────────────────────── crates/dw
      (model · log · jj · land · record ·      (the human's CLI —
       deploy trigger · fizzy comment)          no HTTP, no service,
                                                works offline)
```

**One binary, one host.** skiffd runs on the desktop, reads the desktop's
files, and is unreachable when the desktop is off. That is accepted: the
desktop is on whenever there is coding to look at. Single-host removes the
entire distributed-systems surface — no push protocol, no clock skew, no
partition semantics — and buys `inotify` instead of remote tailing.

**No loopback hop, no bridge password.** The Node bridge existed as a separate
process because Rails could not spawn and supervise harness children. Rust can.
`SKIFF_BRIDGE_PASSWORD` and `~/.config/skiff/secrets` are deleted outright: the
only auth boundary that remains is the one that was always doing the work —
tailnet membership, breakwater, and the Host allowlist.

---

## 3. THE BOUNDARY

> **Rust owns truth, derivation, and consistency.
> React owns intent and presentation.**

The test for which side a thing belongs on: *would two clients looking at the
same data have to agree about it?* If yes, Rust. If it is a function of what
this person is looking at right now, React.

| | Rust | React |
|---|---|---|
| harness formats, normalization | ✓ | never sees them |
| overlay resolution, compaction, abort | ✓ | — |
| desk ordering ("what needs you") | ✓ | — |
| markdown, syntax highlighting | → typed blocks + tokens | renders each block as a component |
| diffs | → files/hunks/lines + anchored annotations | expand/collapse, unified vs split |
| change state machine, landing | ✓ | — |
| routing, panes, scroll, focus, collapse | — | ✓ |
| filters, search-as-you-type, drafts | — | ✓ |
| optimistic send | echoes the client id back | owns the pending bubble |

Two consequences worth stating because they are easy to violate later:

- **Rust never emits markup.** Not HTML, not class names, not markdown to be
  rendered downstream. It emits typed data. The current system server-renders
  HTML *and* invents CSS class names (`tok-keyword`, `tool-line--bad`) in Ruby;
  those are presentation decisions and they move to React.
- **React never parses.** Not markdown, not diffs, not harness formats. If the
  client is parsing something, the boundary has been crossed in the other
  direction.

---

## 4. INGEST

One adapter per source. An adapter's only job is to turn its native format into
domain records and report a watermark. Adapters never touch HTTP, SQL, or
views.

```rust
trait Source {
    /// Human name, for the degraded-source readout.
    fn name(&self) -> &str;
    /// Watch for change; each tick means "read from the cursor".
    async fn watch(&self, tx: Sender<SourceTick>) -> Result<()>;
    /// Read forward from the stored cursor; return records + new cursor.
    async fn read_from(&self, cursor: Cursor) -> Result<Batch>;
}
```

**Watermarks.** For JSONL sources the cursor is
`(inode, byte_offset, lines_read)`. On every read: if the inode changed, or the
file is shorter than the offset, the file was rotated or rewritten — discard
the cursor and re-read from zero. Otherwise read forward. A partial trailing
line is not consumed; it is re-read next tick. This is the same discipline the
bridge already applies to harness JSONL, and it is the reason the offset can be
trusted at all.

**The file is append-only; the conversation is not.** A pi session file is a
*tree* — entries linked by `id`/`parentId` — and the conversation is the chain
from the newest entry back to the root. Entries on abandoned branches stay in
the file forever and must never surface. So the two concerns are separated
cleanly:

- **Ingest** appends entries. It is forward-only, and the byte watermark is
  exactly right for it.
- **The transcript** is a *query* over those entries — the leaf path — not a
  stored list. In SQLite that is a recursive CTE from the newest entry; sessions
  are small enough that recomputing it is not worth caching.

The consequence for §7: a new entry whose parent is not the previous leaf is a
**rebranch**, and messages that were in the transcript are now gone. That is not
expressible as an append, so it emits `Reset` and the next frame is a snapshot —
the same path compaction takes. Rebranching is rare and a snapshot is cheap;
inventing a truncate delta to save bytes on the rare case would buy nothing and
add a second way for the transcript to be wrong.

**Watching.** `notify` (inotify) with a ~50 ms debounce for file sources.
opencode is a server, not a directory, so its adapter follows opencode's own
event stream and falls back to a timer.

**A missing source degrades, it never kills.** A harness whose binary is absent
becomes a named error attached to that source, surfaced on the desk. It is
never a dead service and never a silently short session list. This is the one
operational rule carried across from the bridge unchanged, because it was
right.

**Restart is not destructive.** Because observed state re-derives from files,
skiffd restarting mid-run loses only the in-flight overlay; the transcript
converges on the next ingest. Today a bridge restart is destructive. This
property is free and should not be traded away.

---

## 5. STORE

SQLite (`rusqlite`, bundled), holding **only derived data**.

Tables: `session`, `message`, `part`, `run`, `source_cursor`, `source_health`,
plus a projection of the change log (`change`, `round`, `annotation`) so that
every view has one query surface.

Because it is derived by definition, two otherwise-awkward problems disappear:

- **Migrations.** `PRAGMA user_version`; on mismatch, drop every derived table
  and re-ingest. No migration scripts, ever. The authored JSONL log is never
  dropped and never migrated destructively.
- **Corruption.** "If in doubt, rebuild" is always a legal answer.

Authored state stays where DW-002 put it — one append-only JSONL event log per
change, fsync'd before acknowledgement — and that log is **just another source
for the ingest to tail**. This is the move that makes the whole layering hold
together: any process may append to the log (per-change lock file, atomic
appends), skiffd notices via `notify` and updates its projection, and the
"single writer" constraint that would otherwise force everything back through
one process simply never arises.

---

## 6. VIEWS AND LIVE QUERIES

**Every read on screen is a subscription.** There is no request/response read
path. Cold load and live update are the same mechanism, which deletes an entire
class of bug: fetch-then-subscribe races, stale-after-navigate, "does the list
refresh after I approve?", and the desk poller.

The view set is **closed and hand-written**. Four views:

| view | data | invalidated by |
|---|---|---|
| `desk` | changes in review, then working sessions, then idle | `SessionList`, `ChangeList`, any `Run` |
| `sessions` | the full session list | `SessionList`, any `Run` |
| `session(id)` | transcript, overlay, working, orchestrator, bound change ref | `Session(id)`, `Run(id)`, `Change(bound)` |
| `change(repo, card)` | record, rounds, annotations, structured diffs, bound session ref | `Change(repo,card)`, `Session(bound)` |

Invalidation is by **topic**, not by a dependency graph. Ingest emits topics;
each subscription declares the topics it cares about; an invalidated
subscription recomputes its data, diffs against what it last sent, and emits
deltas. This is deliberately not a reactive query engine — those are a swamp,
and four hand-written views with explicit topic lists stay legible forever.

Recompute-and-diff is the default because it is obviously correct. The one view
that cannot afford it is `session(id)` during a live run, so its streaming path
is special-cased in §7. Nothing else is.

---

## 7. THE WIRE

One WebSocket per client, multiplexed. Multi-pane means several concurrent
subscriptions is the base case, not an optimization.

Client → server:

```jsonc
{ "t": "subscribe",   "sub": 7, "view": { "kind": "session", "id": "pi:abc" } }
{ "t": "unsubscribe", "sub": 7 }
{ "t": "command",     "req": 12, "cmd": { "kind": "send", "session": "pi:abc",
                                          "text": "…", "clientId": "c-91" } }
```

Server → client:

```jsonc
{ "t": "snapshot", "sub": 7, "seq": 1, "data": { … } }
{ "t": "delta",    "sub": 7, "seq": 2, "event": { … } }
{ "t": "ack",      "req": 12, "result": { … } }
{ "t": "err",      "req": 12, "error": "…" }
```

**Reconnect: re-subscribe everything, take fresh snapshots. There is no replay
buffer.** The snapshot-on-every-connect *is* the convergence guarantee, and it
is why there is no position protocol to get wrong. A resubscription always uses
a **new `sub` id**, so late deltas addressed to the dead id are discarded
without any sequence reasoning at all. `seq` is retained as a cheap
self-describing invariant for debugging, not as a resume token.

Session deltas are **semantic**, not DOM-shaped:

```rust
enum SessionDelta {
    MessageAppended { message: Message },
    OverlayOpened   { run: RunId, message: Message },
    OverlayBlocks   { run: RunId, part: usize, from: usize, blocks: Vec<Block> },
    OverlayResolved { run: RunId, message: Message },
    OverlayDropped  { run: RunId },
    WorkingChanged  { working: bool },
    OrchestratorChanged { readout: Orchestrator },
    Reset,                 // compaction or rebranch: next frame is a snapshot
}
```

Three things this buys over the current `{index, entry}` ops:

1. **Stable identity.** The overlay carries a real `RunId` from the moment it
   opens. The current system names it `"<pending>"` and swaps in the real id at
   settlement — which is precisely why the reasoning disclosure needed a
   positional key to survive settling (card #110, `reasoning_state_controller`).
   With a stable id that whole workaround is structurally unnecessary: React
   keys on `(message_id, part_index)` and nothing remounts.
2. **Bytes proportional to new text.** `OverlayBlocks` replaces the tail from
   `from`, which while streaming is almost always the last block. The current
   `replace` resends the entire entry every 100 ms flush — O(reply²) over a long
   answer.
3. **A typed leaf.** The payload is typed all the way down, which is where "end
   to end types" actually cashes out; `entry: Value` is where today's
   untypedness lives.

**Commands** travel up the same socket with a `req` id. `send` carries a
client-generated `clientId`, which the server echoes on the resulting message —
so the optimistic bubble React drew is replaced by identity, never by guesswork.

---

## 8. CONTENT

Message content crosses the boundary as **typed blocks with highlight tokens** —
never HTML, never raw markdown.

```rust
enum Block {
    Paragraph { inlines: Vec<Inline> },
    Heading   { level: u8, inlines: Vec<Inline> },
    Code      { lang: Option<String>, tokens: Vec<Token> },
    List      { ordered: bool, items: Vec<Vec<Block>> },
    Quote     { blocks: Vec<Block> },
    Table     { head: Vec<Vec<Inline>>, rows: Vec<Vec<Vec<Inline>>> },
    Rule,
}
enum Inline { Text(String), Code(String), Emph(Vec<Inline>),
              Strong(Vec<Inline>), Link { href: String, inlines: Vec<Inline> } }
struct Token { class: TokenClass, text: String }
enum TokenClass { Keyword, Str, Comment, Number, Ident, Deleted, Inserted, Plain }
```

`pulldown-cmark` (already a workspace dependency) parses; `syntect` with
`default-features = false, features = ["default-fancy"]` highlights — the
`fancy-regex` backend keeps oniguruma's C dependency out of the static build.
`TokenClass` is a small closed taxonomy mapped from syntect scopes, exactly as
`SyntaxTokenFormatter` maps Rouge's today; React turns it into class names.

Why blocks rather than the two obvious alternatives:

- **vs. server HTML:** `dangerouslySetInnerHTML` forfeits interactive code
  blocks, and re-rendering all history whenever the renderer changes.
- **vs. raw markdown:** re-parsing on every 100 ms streaming flush, plus the
  bundle weight of a markdown and highlighting stack on the client.

Blocks give parse-once, tail-only invalidation while streaming, real components
for code blocks, and a typed wire — all four at once.

**Parts** are normalized at ingest, not at render:

```rust
enum Part {
    Text      { blocks: Vec<Block> },
    Reasoning { blocks: Vec<Block> },
    Tool      { name: String, status: ToolStatus, title: Option<String> },
    File      { filename: String },
}
```

Synthetic text parts (auto-attached context) and control parts
(`step-start`/`step-finish`) are dropped **at ingest** and never reach the wire.
Today they are carried the whole way and discarded in the template, which means
every layer has to know about a concept only one layer acts on.

**Diffs** are structured by `crates/change`, not parsed by the client:
files → hunks → lines, each line carrying `kind` and its old/new numbers, with
annotations resolved onto `(path, side, line)` anchors. The parsing that
`GitDiff` does in Ruby moves into the crate, where the annotations it anchors
already live.

---

## 9. THE CHANGE CRATE

`crates/change` owns the DW-002/DW-003 domain: the model, the append-only log,
jj integration, the structured diff, the state machine, landing, the record
export, the deploy trigger, and the Fizzy comment.

**Why it is a crate and not part of skiffd.** Today `dw status` cannot answer
without a Node service running and a password file present, in order to read a
JSONL log sitting on the same disk. That is the tell: the change model is the
fleet's source-control domain, and skiff is one of three clients of it, not its
owner. As a linked crate, `dw` works offline with no service and no secret.

**Why landing's tail lives here too, rather than in skiffd.** `approve` is one
transaction with a hard ordering — fetch → rebase → conflict-check → push (the
irreversible half), then resolve tip, trigger deploy and poll, export to the
record, comment on the card. Steps after the push share one discipline already:
best-effort, outcome recorded visibly on the change, never allowed to un-ship
it. They happen *because a land happened*, not because a browser was open.
Splitting them out would mean inventing an event bus so another process could
observe landings and reproduce ordering that is currently three sequential
awaits.

Specifically:

- **Record export.** DW-003's privacy boundary is field-by-field
  exclusion-by-default in `build_public_change`. It must sit beside the struct it
  filters, so that a newly added field's *omission* is visible in the same
  review that adds it. This is the strongest of the three.
- **Deploy trigger.** A `DeployTrigger` port with the tugboat HTTP client as one
  implementation. It is already token-gated to off — `createTugboatClient`
  returns `null` without `TUGBOAT_SERVE_TOKEN` — so "feature off" is `None`,
  which is the existing design, and the crate stays testable with no daemon.
- **Fizzy comment.** Links `crates/fizzy` directly. The Node module's own header
  says it re-speaks "the same [contract] `crates/fizzy` speaks from Rust"; this
  is a straight deletion of a reimplementation.

What stays in skiffd is only the **read** side of these: the desk's "approval
will deploy the whole fleet" preview (`/services`, 60 s TTL) and the in-flight
deploy readout. The rule:

> **Writes and their consequences live in the change crate.
> The readouts of them live in skiffd.**

### 9.1 Who lands

`land()` is a plain async function on the crate, but **skiffd is its only
caller**, and that is a documented policy rather than a mechanism. It is also
just a description of reality: approve is a desk verb (DW-002 — approve is "the
only lander"), and `dw ship` creates a change rather than landing one. `dw` gets
`dw finish`, not `dw land`.

Making skiffd the lander keeps the supervisor that drains in-flight landings on
shutdown. But that guarantee is weaker than it reads — it covers *graceful*
shutdown only, so a power cut, an OOM, or a restart racing a land still abandons
the tail, and the tail is the long part because the deploy poll runs to a
deadline. So, independently of who calls it:

**Each tail step records its own outcome in the append-only log.** An
interrupted landing surfaces on the desk as an unfinished landing naming
exactly which steps did not complete, and `dw finish <card>` — or a desk button —
re-runs only those.

**Detection is automatic; action is explicit.** Nothing re-triggers a fleet
deploy or re-posts a Fizzy comment without being asked. Automatic resumption
was considered and rejected: it needs per-step at-most-once markers to be
exactly right, and the failure mode is a duplicate fleet deploy.

---

## 10. THE CLIENT

Vite · React 19 · TypeScript · Tailwind 4 · `@fleet/ui`, matching `lighthouse`
and `recipes`. Assets are built to `skiff/web/dist` and served by axum with an
SPA fallback — the house pattern, not an embedded bundle.

**Types are generated, not hand-written.** `ts-rs` derives on every wire type;
a `cargo test` regenerates into `skiff/web/src/gen/` and fails on drift. This is
the point at which "type safety end to end" stops being an aspiration: a
protocol change that the client has not absorbed becomes a failing gate rather
than a runtime surprise. `lighthouse` hand-writes its `types.ts`, which is fine
for its surface and would not be fine for transcripts, parts, blocks, diffs, and
annotations.

**One subscription store, no data-fetching library.** TanStack Query and friends
solve request/response caching, which this design does not have. The whole
client data layer is a socket multiplexer plus a hook:

```ts
const { status, data } = useView({ kind: "session", id })
```

Subscribe on mount, unsubscribe on unmount, resubscribe on reconnect. That is
roughly 150 lines and it is not a framework.

**Multi-pane workspace.** A persistent desk rail on the left, and a main area
holding one or two panes; opening a change from a session opens it *beside*,
not over it — which is DW-002 §6's "the change comes back to the session that
produced it", finally with room to be literal. Panes are React state and the
URL encodes them (`/s/pi:abc/c/fleet/123`). Each pane holds its own
subscription.

**Keyboard-first**, because the primary client is a desktop browser: `⌘K`
palette, `j`/`k` in lists, `⌘1`/`⌘2` to focus a pane, `⌘↵` to send.

**Phone is a responsive layout of the same app**, not a second design target:
the rail becomes a sheet and the pane area shows one pane. DW-001 §6's polling
rationale was phone battery discipline; the primary client is a desktop
browser, so that trade is retired and the connection stays open.

---

## 11. GATES AND TESTS

No CI (archived 2026-08-13), so local gates are the only enforcement:

```bash
cargo test --workspace          # picks up skiff and crates/change
cargo clippy --workspace --all-targets -- -D warnings
(cd skiff/web && bun run build) # nothing else typechecks the client
```

Three testing rules specific to this design:

- **Ingest is tested against recorded fixtures.** Real pi/muse/opencode session
  files checked into `skiff/tests/fixtures/`. This is how the bridge's suite
  ports across as something meaningful rather than as a re-implementation of the
  same mocks.
- **Type generation is a test.** Regenerate, compare, fail on drift.
- **The store is rebuildable, so tests may always start from empty.** No
  fixtures of derived state, ever — derive them.

---

## 12. WHAT IS CARRIED ACROSS, AND WHAT IS NOT

Carried across, because it was right:

- **Server-side reconciliation.** The per-harness judgment about when an
  appended entry resolves the overlay, compaction resets, aborted-run removal.
  This is the subtlest logic in skiff and it would exist in three copies if the
  client did it.
- **Snapshot on every connect.** The reason there is no position protocol.
- **A missing harness degrades to a named error**, never a dead service.
- **The append-only change log**, fsync'd, "authored output, not cache".
- **Tokenized highlighting** rather than opaque HTML — the instinct in
  `SyntaxTokenFormatter` was already correct, it was just on the wrong side of
  the boundary.

Not carried across:

- **DOM-shaped index ops.** §7.
- **`"<pending>"` as the overlay's identity.** §7.
- **The loopback bridge and its password.** §2.
- **Polling anything.** The desk poller and DW-001 §6's battery rationale. §10.
- **Server-rendered markup and server-invented class names.** §3.
- **`dw` as an HTTP client of a service.** §9.
- **Node's re-implementation of the Fizzy contract.** §9.

---

## 13. MILESTONES

Ordered so that the risky, valuable part came first and every milestone was
independently usable. During M1–M6 skiffd ran on **port 8121** alongside the
live Rails+bridge stack on 8120; M7 moved skiffd to 8120 and deleted both
replaced processes.

| | |
|---|---|
| **M1** | Skeleton: workspace member, axum, WebSocket, SQLite store, `Source` trait, pi ingest (sessions only), `sessions` view, React shell. |
| **M2** | The session view: transcript ingest, blocks, run/overlay lifecycle, send and abort. Usable for pi. |
| **M3** | muse and opencode adapters; source health; capabilities; rename, model picker, orchestrator toggle. |
| **M4** | `crates/change`: model, log, jj, structured diff. `dw` cuts over to linking it. Agents move to `dw round` / `dw annotate`. |
| **M5** | Review: change view, annotations, approve and request-changes; landing, record export, deploy trigger, card comment; `dw finish`. |
| **M6** | Desk, multi-pane workspace, command palette, keyboard navigation. |
| **M7** | Cutover: breakwater flips to skiffd, port moves to 8120, Rails and bridge deleted, PWA manifest and service worker. |

The cutover is a flip, not a migration — there is no state to move, because
everything skiffd caches is derived and everything it authors is already in the
change log that both stacks read.
