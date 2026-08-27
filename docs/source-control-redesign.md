# SOURCE CONTROL REDESIGN

```
┌──────────────────────────────────────────────────────────────────────┐
│ DOC. NO.  DW-002          REV. B          CLASSIFICATION: INTERNAL     │
│ SUBJECT   Replacing git's ceremony without leaving its ecosystem       │
│ ORIGIN    Design session 2026-08-22                                    │
│ STATUS    Shipped. Cutover 2026-08-23 — this is the live workflow.     │
│ SCOPE     fleet · fizzy · skiff                                        │
└──────────────────────────────────────────────────────────────────────┘
```

The workflow and domain model remain live. Sections that place their
implementation in Rails or the Node bridge describe the original delivery;
[DW-004](skiff-architecture.md) superseded that runtime with `crates/change`,
`dw`, skiffd, and React at the 2026-08-24 cutover.

Revision B makes both development machines first-class with two deliberately
different Fizzy-backed workflows: curated agent changes on the desktop and
manual, editor-reviewed jj changes on the Mac. Skiff remains a single-host
desktop service; the Mac does not create a second review system.

In the desktop's curated lane: capture you never touch, curation the agents do
for you, and a public record that falls out of reviewing their work. The Mac's
manual lane keeps the card and local capture, then uses editor review and a
Fizzy outcome comment instead of Skiff curation.

Read this before building any of it. Several sections record alternatives that
were tried and rejected; the reasoning matters more than the conclusions, and
re-deriving it costs more than reading it.

---

## 1. THE PROBLEM

Git makes the commit serve three purposes at once, and asks you to satisfy all
three every time you type `git commit`:

- **A safety net**, so experimentation is free and an hour of lost work is recoverable.
- **A readable record** of what changed and why.
- **A public narrative** of the work.

These want opposite things. A safety net should be exhaustive, automatic and
private. A record should be sparse and deliberate. A narrative should be
derived, never authored twice. Collapsing them into one artifact is why source
control feels like a tax — every save asks you to decide whether this moment is
worth recording and how to describe it.

Almost all of the ceremony disappears once the three are separated. Nothing
else about git is the problem.

---

## 2. THREE LAYERS

| Layer | Character | What it is |
|---|---|---|
| **Capture** | automatic, private | The working copy is always already a commit, and every operation is reversible. Nothing to build. |
| **Curation** | agent-authored | A feature organized into rounds with justifications attached. The agent does this. |
| **Publication** | derived, default public | A timeline of shipped features rendered from the curated layer. No second authoring pass. |

**The engine stays git** — specifically `jj` in colocated mode, where `.git`
remains real and authoritative, so `cargo`, `gh`, tugboat's git handling and
every tool that shells out keep working untouched.

Writing a storage or merge engine is years of subtle work whose failure mode is
losing code, and there is no ergonomic upside hiding in it. All of the
ergonomics live in the layers above it.

---

## 3. CAPTURE IS A PROPERTY, NOT A SYSTEM

Losing work means the things *git* does: edits destroyed by a checkout or a
reset, changes wiped by a clean, work stranded on a branch you forgot, a rebase
that silently drops commits, a stash never recovered, a force-push that
overwrote history, an agent doing something destructive in a worktree nobody was
watching. That fear is why people push constantly, and why "commit early, commit
often" is advice at all.

**jj answers all of it structurally, and there is nothing to build.**

- The working copy *is* a commit, continuously. "Uncommitted work" stops being a
  category that can exist, so no checkout or reset has anything to destroy.
- Every operation — every amend, rebase, abandon, and every action an agent
  takes — lands in the operation log, and `jj undo` reverses any of them,
  including the ones git treats as unrecoverable.
- Nothing the operation log references is garbage collected, so work cannot
  become unreferenced and vanish. There is no stash to lose, because there is no
  reason to stash.
- A change keeps a stable identity across every amend and rebase — the handle
  that lets one identifier thread a card, a change, a review and a deploy. Git
  cannot do this, because every amend gives you a new hash.

So this layer is a day of adoption rather than a project. It is reversible
throughout, since `.git` stays authoritative the whole time.

### Retention: keep everything

Retention reduces to a single question — **how long deleted work stays
recoverable** — and everything else about it is a rounding error on disk.
Operations are tiny; the real cost is the trees old operations keep alive, and
since content is deduplicated, a day spent editing source in a Rust monorepo
amounts to very little. Reclaiming space also takes two steps rather than one:
collection frees almost nothing until operations are expired first, because
everything remains reachable through the log.

**Nothing expires until it visibly costs something.** Run it, watch `.jj/`, and
design a strategy when there is a number worth reacting to. The value of an
operation decays far faster than its storage does. The log is local and
per-repository, never pushed, so this decision affects one disk and nothing else.

### Undo has to be scoped to you

Workspaces share one repository and therefore one operation log, so with several
agents working different cards at once that log is a single busy structure with
everyone's entries interleaved. A bare `jj undo` reverses whatever happened last
*globally* — which may well be an agent's operation rather than yours. Reach for
undo after a mistake, have an agent commit a round half a second earlier, and you
silently revert its work instead of your own.

**Undo in this system is scoped to your own workspace and never maps onto a bare
`jj undo`.** The constraint comes from running agents concurrently rather than
from jj itself, and it is the kind of thing that otherwise gets discovered the
hard way.

### Out of scope

Machine-level durability — disk failure, a box that doesn't come back — is
deliberately **not** part of this. That is a different concern with a different
solution and belongs to the fleet's high-availability work. An earlier draft
conflated the two and specified a filesystem-watching capture daemon with
continuous replication to the VPS; that was answering a question this design does
not ask.

---

## 4. WHAT A CHANGE IS

One card, one change, one logical unit of work — an implementation of a feature
you asked for. The card number is the only identifier that ever reaches you.

A change is an **ordered sequence of rounds**, and a round is exactly one commit.
Round 1 is the agent's implementation; you read it, ask for revisions, and the
agent adds round 2. That is the flow pull requests already give you, and it
survives intact.

```
Change
  ├ card       #81                    the only id you see
  ├ state      working → in review → approved → shipped
  └ rounds[]   ordered, additive
       ├ n             1, 2, 3…
       ├ author        agent | you
       ├ commit        one jj change
       ├ annotations[] positioned in that commit's diff
       └ note          what prompted it — private
```

**Rounds are additive, never amended**, and that one property removes most of
the complexity a review system usually carries. Nothing beneath an annotation
ever moves, so no annotation can go stale and no anchor needs re-fitting. Two
views fall out for free: *this round's diff*, which is what changed since you
last looked, and the *cumulative diff*, the feature as it now stands.

> **Rejected:** an earlier draft gave a change two dimensions, separating the
> agent's logical steps from the rounds of revision. That was borrowed from
> stacked-diff workflows and is not how this works. Collapsing them removed
> machinery without losing anything — the decomposition of a large change is
> carried by its annotations, grouped by concern, which is more informative than
> commit boundaries anyway.

### Curation is the agent's job

The agent did the work and knows why it did it. Its final act is not writing a
commit message — it is authoring the justifications that make the round
readable. You never curate anything; curation exists in the system, it just
isn't yours. Your own code skips it entirely (§7).

### Parallel agents and stale bases

Several agents work different cards at once, so main moves while a change sits
in review. The change **does not rebase while you are reading it** — work under
review must not mutate underneath you, or the diff you approved is not the diff
that lands. It rebases once, at approval.

If that rebase conflicts, nothing special happens: it returns as the next round,
because "here is a reason to revise" is already the only mechanism the system
has. jj records conflicts inside commits rather than refusing to proceed, so the
rebase always completes and hands the agent something to resolve. The
predictable collisions in a monorepo — `Cargo.lock`, the workspace manifests,
the generated registries — are mechanical, and an agent resolves them by
re-running the build.

On approval the round commits land as they are, unsquashed, exactly like merging
a pull request today.

---

## 5. THE ANNOTATED CHANGE

This is the artifact that justifies building rather than adopting. You read
**the code itself, annotated with the agent's justifications at the point they
apply** — not a description sitting above an undifferentiated diff. Pull-request
comments are a bolt-on: conversational, after the fact, positioned by whoever
happened to click a line. What is wanted is authored explanation, written as part
of finishing, sitting where the reasoning is relevant.

**These annotations must never become code comments.** An agent that wants to
justify itself will happily write its reasoning into the source, and the
codebase ends up carrying its own review commentary forever. Annotations live in
the review layer, bound to positions in the change. On approval they go to the
timeline, not into the file.

```
skiff/app/models/harness.rb          round 2

  +  def available_models
  +    cache.fetch(:models, expires_in: 5.minutes) do
  +      adapter.list_models
  +    end
  +  end

  ▏ cached because pi shells out to `pi --mode rpc`
  ▏ for this, and the phone list re-polls on every
  ▏ render. 5 min because model lists change on
  ▏ harness restart, not during a session.
```

### Three verbs, not two

- **Approve.**
- **Request changes** — type a message, walk away; it returns as a new round.
- **Edit it yourself** — `dw edit 81` puts you in the change with your own
  editor. It lands as a round authored by you.

The third verb matters ergonomically: writing a paragraph to obtain a one-line
fix is worse than making the fix, and forbidding it just pushes you out to the
terminal, which breaks the record. It is also the single place a filesystem path
is allowed to surface, because editing requires one. There are no editor
integrations — see §9.

### The header above the code

Gates stay manual — you and the agents run them by hand. The system does not
run, schedule or verify builds. What it does is **carry the agent's report as a
claim, labelled as one**:

```
#81  pi model picker              round 2
     lets you switch harness models from the phone

     agent ran    cargo test · clippy · fleet gen --check
     worth knowing
     · +1 dependency (serde_yaml)
     · touched breakwater.toml
```

The distinction between `agent ran` and a verdict is deliberate and must survive
into the UI. Nothing here checked anything — an agent that skipped a gate and
said otherwise would pass straight through, and the only thing standing between
that and the codebase is you reading the code.

---

## 6. THE REVIEW IN SKIFF

Skiff has **no database**. It is a stateless Rails app proxying a bridge over
loopback, with every piece of state living in the harnesses' own stores. It also
carries an explicit rule worth obeying rather than breaking: *reconciliation
lives in the bridge, not the view — the view renders ops, it never diffs.*

So the review extends the existing bridge rather than standing up a second one,
and Rails stays a renderer.

```
browser ──> Rails (skiff, stateless) ──> loopback
                                          │
                                          └── bridge :4120
                                                ├── harnesses   pi · muse · opencode
                                                ├── jj repos    rounds are commits
                                                ├── fizzy       cards
                                                └── annotations the one thing with no other home
```

The repository is most of the store — rounds are commits, so they are already
versioned — and the card binding lives in Fizzy. Annotations are the only part
with nowhere natural to live, and the bridge owns them, which keeps the
repository holding code and nothing else instead of landing a `.review/`
directory on main forever.

### One page, ordered by what needs you

Sessions and changes are different objects: a session is an open-ended
conversation that may produce nothing, while a change is a unit of work with a
lifecycle and an ending. But object type is the wrong way to organize the page.

```
needs you    changes in review        ← the only count that matters
working      sessions currently running
idle         quiet sessions, landed changes
```

Changes sit on top because they are the only things blocking on a human. `root`
moves off `sessions#index` to a controller that renders both registers and
reuses the existing partial unchanged.

### The loop closes in one view

Skiff already streams sessions over SSE and already has a composer. So **request
changes navigates nowhere**: you type the note, the agent begins working, and you
watch it work in the view you were just reviewing in. When it finishes, round
*n+1* appears in place.

The review and the session transcript are the same page at different moments of
its life — and the embedding is bidirectional. The change carries the session
that produced it (the agent binds it at create), the session payload carries the
change back, and the session page renders the same review region the change page
does: the header's claims, the latest round's annotated diff, and the verbs,
which return to the session instead of navigating away. The change page keeps
the full view — rounds navigation, the cumulative diff, and the bound session
embedded below. One shared partial renders both, so the two views of a change
can never disagree.

> Diff highlighting goes through `TranscriptHelper`'s existing Rouge-to-palette
> mapping. Skiff's own notes say its tokens map to the app's palette and "never a
> second color set" — and a diff view is exactly where a second one gets
> introduced carelessly.

### What approve does

Fetch `origin/main`, rebase the change's rounds onto it, push. That is the entire
mechanism, and it targets `origin/main` directly — precisely where a pull-request
merge lands today. So **approve produces the same artifact as merging a PR**,
nothing downstream changes, and no tugboat work is required. The pull request
simply stops existing. That equivalence is also what makes the whole system safe
to adopt gradually, and safe to abandon.

Landing does not deploy, and nothing here changes that. `tugboat serve` exposes
`POST /deploy/{name}`; a merge increments the dashboard's undeployed-commit count
and a human requests the deploy.

Three ways it fails, none of which needs a new concept:

- **The rebase conflicts.** Not an error state — it returns as the next round for
  the agent to resolve. Approve can fail *into* "needs another round".
- **The push is rejected.** A parallel agent landed between the fetch and the
  push. Retry the loop a few times; if it still loses, make it a round.
- **The Fizzy comment fails.** Hence the ordering rule — **land the code first,
  then write to the card.** The land is the valuable, irreversible half; the card
  annotation is recoverable metadata. Reversed, you risk a card claiming a
  landing that never happened.

Approve is a request rather than an instant: `in review → landing → shipped`, or
back to `in review` carrying a conflict round. It is unavailable while a round is
in flight.

### Approve comments; it does not close

An earlier draft had approve close the card. **Fizzy's API cannot do that**, and
the limit is deliberate rather than an oversight. Standing changes — closed,
not-now, back to triage — run through `Columns::Cards::Drops::*`, whose actions
render only `create.turbo_stream.erb` and have no JSON representation; Bearer
auth is honored only when `request.format.json?`. So a JSON request
authenticates, runs the side effect, then fails to render, while a non-JSON
request is never authenticated at all. Every `.json.jbuilder` in Fizzy is a read
view, and each endpoint intended for the API declares `format.json` explicitly.

So approve **posts a comment** recording what landed, and closing stays a human
act in the web UI. That is arguably the right split anyway: landing code and
declaring a feature done are different judgments, and you may well land a change
while the card stays open because the feature is not finished.

> Shipped: `Client::comment_on_card` and `fizzy comment <number>` (card #90, PR
> #49). A `403` means the card is a draft — `Card::Commentable#commentable?` is
> `published?` — while closed cards still accept comments, which is what lets
> approve write to a card whatever its standing.

---

## 7. WHAT YOU TOUCH

You hold one noun. Not card, worktree, branch, commit, PR and deploy — just the
thing you are trying to get done, which is a Fizzy card. The other five still
exist; you never see one again.

```
$ dw

  waiting on you                    review in skiff
  ▸ #81  pi model picker      round 2
    #84  shutter crash fix     round 1

  running
    #86  breakwater route dedupe

  yours
    3 files changed in fleet, all recoverable
```

- `dw` — what's happening, and confirmation that your own work is safe.
- `dw ship` — for your own work.

Capture has no command, because it isn't a thing that happens (§3). Reviewing has
no command either — it happens in skiff (§6), so the terminal only ever tells you
the state of things.

### Your own code

Your changes skip the review flow entirely — self-review is theatre. But if they
skipped everything, the public record would silently contain only agent work,
which is the opposite of a portfolio. So `dw ship` takes a sentence from you.

That sentence is **the only ceremony left in the system**, and it lands at the one
moment when you already have something to say.

---

## 8. THE PUBLIC RECORD

Privacy falls out along the layers instead of needing a system of its own:

| | |
|---|---|
| **Capture** | always private — the working copy and operation log, including everything half-broken |
| **Code & annotations** | default public — deliberate, authored output, safe to publish precisely because it was written to be read |
| **Review conversation** | private — your notes back to the agents stay yours |

Each entry is a shipped feature as a story: what it was, what changed, when it
went out, and what broke afterward. The blog, `public_site` and the notes are
untouched — this sits beside them as the record.

The unusual part is that the public artifact isn't a diff. It's the annotated
change: code alongside the reasoning for it. That shows how the work was actually
made, it is more interesting than a contribution graph, and it costs nothing
because it is a byproduct of reviewing.

---

## 9. WHERE YOU WORK

**The Mac workstation and Fedora desktop are both development machines, with
different review lanes.** Work can begin on either without the other being
awake.

- **Desktop: curated agent workflow.** The desktop owns Fizzy-backed changes,
  additive rounds, annotations, and the continuously hosted Skiff at
  `https://skiff.intern.deepwa7er.net`. Approval in Skiff remains the only
  lander for that lane.
- **Mac: card-backed VSCodium workflow.** The Mac uses a Fizzy card and its
  colocated `jj` working copy directly. Changes are reviewed in VSCodium, not
  registered with `dw` and not sent to Skiff. An agent may prepare and gate the
  change, but it stops before landing until the human explicitly says the
  editor-reviewed change should ship.
- **Completed work synchronizes through Git.** `origin/main` is the boundary
  between machines. Fetch it before starting a change. Never copy `.jj`,
  `.workspaces`, or the desktop's active change log between machines.

The Mac lane is intentionally manual. Start in the VSCodium-open checkout when
its working-copy change contains no unrelated work; otherwise create a named jj
workspace and open that folder in VSCodium. After implementation, run the gates,
describe the jj change, and leave it visible for review. Only after explicit
human acceptance: fetch `origin`, rebase the reviewed change onto the current
`main@origin`, stop if that introduces a conflict, then move `main` to the
reviewed change and push it. Record the landed commit and gates as a Fizzy card
comment. Because this lane creates no change-log record, it creates no
annotations or public timeline entry. Work that needs those artifacts starts on
the desktop instead.

### Editing

`dw edit <card>` **prints a path and does nothing else.** There are no editor
integrations, deliberately.

For desktop-curated work, VSCodium and JetBrains (RubyMine) run in remote mode
against the desktop, so the editor is attached to the same filesystem as Skiff.
For Mac-manual work, the local VSCodium window is the review surface; `dw edit`
does not participate.

`$VISUAL`/`$EDITOR` are still honored for the terminal case, but that is a
convention rather than a coupling, and it is not the primary path.

> **VSCodium gotcha:** Microsoft's `remote-ssh` extension is not licensed for
> VSCodium and is absent from OpenVSX. The working substitute is the
> `open-remote-ssh` fork. JetBrains Gateway needs no such workaround.

---

## 10. WHAT CHANGES IN THE FLEET

| | |
|---|---|
| **Fizzy** | Becomes the spine. Self-hosted on the VPS, so binding the loop to it adds no external dependency. |
| **Worktrees** | Keep the isolation, delete the visibility. Parallel agents genuinely cannot share a tree, but you should never again see a `.worktrees/` path, a `fleet/81-slug` branch name, or a `git worktree remove --force`. |
| **GitHub** | Stops being the review mechanism and becomes a publishing target. |
| **Skiff** | Becomes the review surface, and therefore the primary surface of the whole system (§6). |
| **Verification** | Explicitly out of scope. Gates are run by hand; the system reports what the agent claims it ran and never pretends to have checked it. |
| **Tugboat** | Also out of scope. Its deploy transaction is good and should not be rebuilt; its *scope* is the problem — `docs.rs` is 23% of a deployer and isn't deployment. A later project. |

---

## 11. OPEN QUESTIONS

- **Nothing independently checks anything — decided, not overlooked.** The
  agent's report of which gates it ran is a claim, and your reading of the code
  is what stands behind it. The first real build of any landed change happens
  when you next deploy that service.

- **Two parallel changes can each be correct and jointly wrong.** Agent A renames
  something Agent B calls; both rebase cleanly and main breaks. Textual conflicts
  surface at approval (§6); semantic ones do not.

  Worth recording, because it is counter-intuitive: **pre-land verification
  cannot catch this.** Both changes verify against `main@X`; A lands making
  `main@Y`; B rebases onto Y and lands making `main@Z`, which nothing ever built.
  Only something running *after* the rebase can see it — either gating between
  rebase and push, or building main after each land. An elaborate per-change
  verification queue would pass both and main would still break.

  The accepted backstop is that tugboat builds from a clean checkout and a build
  that does not compile never reaches the host. Its limits: tugboat builds but
  does not test; deploys are requested per service rather than automatic, so the
  gap between landing and the first build is unbounded; and lighthouse alerts on
  systemd unit states, not on builds. It is not silent only because you triggered
  the deploy and are watching it.

- **Operation log growth.** Settled for now: nothing expires, and a cleanup
  strategy gets designed once `.jj/` is large enough to be worth reacting to
  (§3). The trigger is a number on disk, not a date.

- **Where the durable history lives — and it does not block this.**
  *(Answered at step 04, as predicted: [DW-003](public-record.md) — a
  dedicated record repository of ship-time exports; inert data, not a
  resurrected depot.)* Warehouse
  answers *how does this repo work* for agents doing lookups; a depot would
  answer *what happened when*. Different shapes, different consumers, overlapping
  mainly in that both ingest git history — so "retire one" may be the wrong
  frame. The question only becomes real at step 04. **The one thing to avoid
  meanwhile:** letting the bridge's annotation store drift into permanent
  cross-system history, which would answer the question by accident.

---

## 12. SEQUENCE

Each step is useful on its own and none requires the next to exist.

| Step | What it buys |
|---|---|
| **01** | Adopt colocated `jj` independently on the Mac and desktop. Use one Fizzy card per change, desktop curation for recorded changes, and the Mac's direct jj lane for VSCodium review (§3, §9). |
| **02** | The change object — rounds, annotations, and the card binding (§4). Nothing to look at yet, but it is what the review renders. |
| **03** | The review in skiff (§6) — the bridge extension, the annotated diff, the three verbs, and approve. The daily experience, and the reason for the whole project. Its Fizzy prerequisite is done. |
| **04** | The timeline. Mostly a rendering job once the layers above exist. |
