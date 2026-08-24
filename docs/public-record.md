# THE PUBLIC RECORD

```
┌──────────────────────────────────────────────────────────────────────┐
│ DOC. NO.  DW-003          REV. A          CLASSIFICATION: INTERNAL     │
│ SUBJECT   The timeline — DW-002 step 04, designed before built         │
│ ORIGIN    Design session 2026-08-23                                    │
│ STATUS    Implemented. Export lives in crates/change.                  │
│ SCOPE     crates/change · record repository · timeline renderer       │
└──────────────────────────────────────────────────────────────────────┘
```

References to the bridge below record the original design vocabulary. DW-004
moved the export and its exclusion-by-default privacy boundary into the shared
Rust change crate before the Rails/Node cutover.

DW-002 §12 calls step 04 "mostly a rendering job once the layers above
exist" — and the rendering half is. But §11 deliberately left one question
open and pinned it to this step: **where the durable history lives**. That
is a storage-and-ownership decision with a warned-against failure mode
(letting the bridge's annotation store drift into permanent cross-system
history), so it gets settled here, on paper, before anything is built.

Read DW-002 first; this note leans on its vocabulary throughout.

---

## 1. WHAT THE TIMELINE IS

DW-002 §8: each entry is a shipped feature as a story — what it was, what
changed, when it went out, and what broke afterward. The unusual part is
that the public artifact is not a diff; it is **the annotated change**:
code alongside the reasoning for it, the exact document the review already
produced. The record costs nothing because it is a byproduct of reviewing.

Two properties follow directly:

- **Derived, never authored twice.** Nobody writes a timeline entry. The
  system emits one at the single moment all its content already exists —
  when a landing completes.
- **Default public by construction.** The entry contains only the fields
  §8 marks public. Privacy is enforced at export, field by field, not by a
  filter in the renderer.

---

## 2. WHERE THE DURABLE HISTORY LIVES

Three candidate homes existed. Two are wrong for reasons already recorded:

- **The bridge's change store.** Rejected by DW-002 §11 in advance. The
  store's own contract (change-store.js) says it holds active work; a full
  scan on every list is its honest cost model. Shipped changes parked
  there forever would turn it into the accidental historian §11 warns
  about — and it is one machine's XDG directory, which is not durability.
- **Warehouse.** Card #67 made warehouse the fleet's single warehouse, and
  depot — the journal-historian service — was archived for having no
  consumers. But warehouse answers *how does this repo work* for agents
  doing lookups (local SQLite, MCP, hourly ingest); it is not durable
  public storage and not a rendering source. §11 already suspected
  "retire one" was the wrong frame, and it was: the timeline needs the
  depot-shaped answer — *what happened when* — but as **inert data, not a
  resurrected service**.

So the design:

> **The record is a dedicated git repository of JSON documents, one per
> shipped change, written by the bridge at ship time and pushed.**

- **A separate repository** — not the fleet repo, which holds code and
  nothing else (§6 rejected a `.review/` directory on main for exactly
  this). A record entry is not code; it is a document about code.
- **Git, because pushing is publishing.** DW-002 §10: "GitHub stops being
  the review mechanism and becomes a publishing target." This makes that
  literal. The push gives durability (origin), history (the record of the
  record), and a transport the renderer can consume from anywhere — the
  same properties depot needed a service, a database, and a backup job to
  provide.
- **Append-only in practice.** Entries are written once at ship time;
  later enrichment (§5) appends fields, never rewrites the story.

Working name and layout:

```
deepwa7er/record            git@github.com:deepwa7er/record.git
  fleet/81.json             one self-contained entry per shipped change
  fleet/84.json
```

The local checkout lives beside the repos the bridge already manages
(`~/code/record`), and the bridge pushes after each write with the same
retry-or-record-honestly discipline approve's card comment uses: the land
is never blocked by the record, and a failed record write is visible on
the change, not silent.

---

## 3. THE ENTRY

Written by the bridge inside `completeLanding`, after the push, beside the
Fizzy comment. One JSON document, self-contained:

```jsonc
{
  "repo": "fleet",
  "card": 81,
  "title": "pi model picker",
  "landedAt": "2026-08-23T12:07:44Z",
  "tip": "57e14554a0bb…",                  // the commit that became main
  "rounds": [
    {
      "n": 1,
      "author": "agent",                    // agent | you — the kind, never a name
      "commit": "…", "changeId": "…",
      "gatesRan": ["cargo test", "clippy"], // still labelled claims when rendered
      "worthKnowing": ["+1 dependency (serde_yaml)"],
      "diff": "diff --git …",               // git format, frozen at ship time
      "annotations": [
        { "path": "…", "line": 12, "side": "new", "text": "cached because…" }
      ]
    }
  ],
  "afterward": []                           // reserved — see §5
}
```

**Diffs are frozen as text at ship time.** The entry must render forever
without the jj repository at hand — repositories get archived, rebased
histories move on, and the record must not care. Diff text for a reviewed
change is modest; self-containment is worth it.

**The privacy boundary, field by field** (§8's table made concrete). The
export includes: card, title, timestamps, commit/change ids, author *kind*,
the claims, the diffs, the annotations. It excludes: round notes ("what
prompted it" — the review conversation is private), request-changes notes,
session ids, filesystem paths, and anything else the change object grows
later — **exclusion is the default; a new field must be added to the
export deliberately.**

---

## 4. THE RENDERER

A static generator over the record repository, published through the same
shape as tugboat's existing `[docs]` pipeline (build → dist → served by
breakwater): read every entry, render the timeline index plus one page per
entry — the annotated change again, in DW-001's language. Skiff's review
already solved diff-with-annotations rendering; the generator reuses the
approach (and the palette rules), not the Rails app.

No server, no state, no auth. A rebuild is idempotent from the record
repo; "deploying the timeline" is regenerating static files. Where it is
served — a public route, or beside `public_site` once card #101 lands — is
an open question (§6) that does not affect the export or the entry format.

---

## 5. WHAT BROKE AFTERWARD

The honest answer today: the fleet has deploy events
(`~/.local/share/tugboat/deploys.jsonl`) and lighthouse's unit-state
alerts, and no mechanism linking either to a change. The entry reserves
`afterward: []` for appended events (a deploy that shipped it, an incident
that implicated it), and nothing more is designed now — building the link
before the timeline has readers would repeat depot's mistake.

---

## 6. OPEN QUESTIONS

- **The record repository's name and visibility.** `deepwa7er/record`,
  public from day one? Public-by-default is §8's position; a private start
  with a later flip is also cheap. Owner's call.
- **Where the rendered timeline is served.** A breakwater route on the
  VPS, or folded into `public_site` when the subtree import (card #101)
  finishes. The export does not depend on the answer.
- **Backfill.** Steps 02–03 shipped through the old PR workflow, and test
  changes were synthetic. Proposal: the record starts at the workflow
  cutover — no backfill. The PR history remains on GitHub for everything
  before it.

---

## 7. SEQUENCE

| Step | What it buys |
|---|---|
| **04a** | The export: bridge writes + pushes the entry at ship time. Small, ships first — every landing after it is recorded, whatever the renderer's state. |
| **04b** | The static renderer + publish pipeline. The visible timeline. |

Neither starts until this note is accepted.
