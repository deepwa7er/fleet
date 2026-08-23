---
name: fleet
description: End-to-end workflow for making changes to the fleet monorepo — Fizzy cards, jj workspaces, rounds with annotations submitted to the review in skiff. The human approves; approve lands.
---

# Fleet — monorepo change workflow

Use when making **any** change to `fleet` (code, docs, style, config). One
card = one change = one ordered stack of rounds ([DW-002](../../../docs/source-control-redesign.md)).
You produce rounds and submit; the human reviews in skiff; **approve** is
the only thing that lands to `origin/main`, and the timeline records what
shipped ([DW-003](../../../docs/public-record.md)). There are no branches to
push and no PRs to open — GitHub is a publishing target, not the review
mechanism. (The pre-2026-08-23 card→branch→PR workflow lives in git
history and `archive/*` tags; do not resurrect it.)

## 0. Orient (every run)

- Read `~/.claude/CLAUDE.md` (NO HACKS — authoritative; overrides summaries).
- Read `docs/deepwater-style-guide.md` (DW-001) if the change touches UI.
- `cargo run -p fizzy -- stream --board Playground` to see open cards before creating new ones (fizzy skill is the CLI contract).
- The bridge is the change API, loopback on the desktop. Auth, used by every call below — parse, never source, never echo:

```bash
BRIDGE=http://127.0.0.1:4120
PW=$(awk -F= '/^SKIFF_BRIDGE_PASSWORD=/{print $2}' ~/.config/skiff/secrets)
# usage: curl -s -u "skiff:$PW" $BRIDGE/...
```

## 1. Discover → Card

Unchanged: search and read first; create or update a card per
`.agents/skills/fizzy/SKILL.md` §4–§5 (`draft → lint → create --dedupe`,
update in place for follow-ups). Ask before posting unless the user said
"create cards". The card number is the change's identity.

## 2. Isolate — a jj workspace, never the main working copy

Parallel work cannot share a working copy, but rounds must be jj changes
the bridge can see — so isolation is a **jj workspace** (shares the repo
and op log), not a git worktree:

```bash
cd ~/code/fleet
jj git fetch --remote origin
mkdir -p .workspaces                      # jj does not create the parent
jj workspace add .workspaces/<slug>      # gitignored, like .worktrees was
cd .workspaces/<slug>
jj new main@origin                        # round 1 bases on fresh main
```

**Undo is scoped to you — hard rule (DW-002 §3).** The op log is shared by
every workspace. Never run bare `jj undo`: it reverses the *globally* most
recent operation, which may be another agent's or the human's. If you must
revert, find your own operation in `jj op log` first and name it.

## 3. Work — rounds are additive commits

Build to the card's acceptance criteria. When the work is ready to show:

```bash
# gates FIRST — see §4; only what you actually ran becomes a claim
jj describe -m "round 1: <what this round is>"
R1=$(jj log --no-graph -r @ -T change_id)
jj new                                    # move off; round 2 grows here
```

Register the change and the round with the bridge (`skiff/bridge/README.md`
is the endpoint contract). `gatesRan` is **only what you ran** — it is
displayed as a claim, verified by nothing, and lying in it defeats the
whole system. `worthKnowing` is the header's bullet list (new deps, config
touched, breaking behavior). **Bind the change to this session** — the
session's wire id is pi's session file basename (pi exposes it as
`PI_SESSION_FILE` to your bash; the bridge ids are `pi:<basename>`), and
binding is what makes the review return to the chat that asked for it
(DW-002 §6). A run whose file never persisted has no binding — omit it
and the review still works from the desk:

```bash
SESSION_ID="pi:$(basename "$PI_SESSION_FILE" .jsonl)"
curl -s -u "skiff:$PW" -X POST $BRIDGE/change -H 'content-type: application/json' \
  -d "{\"repo\":\"fleet\",\"card\":<card#>,\"title\":\"<card title, humanized>\",\"session\":\"$SESSION_ID\"}"
curl -s -u "skiff:$PW" -X POST $BRIDGE/change/fleet/<card#>/round -H 'content-type: application/json' \
  -d "{\"author\":\"agent\",\"changeId\":\"$R1\",\"gatesRan\":[\"cargo test\",\"clippy\",\"fleet gen --check\"],\"worthKnowing\":[\"…\"]}"
```

If the change already exists (a card's change is created once — "one card =
one change"), the create answers 409; **rebind it to this session** so the
review follows the session you are working in: `curl -s -u "skiff:$PW" -X
POST $BRIDGE/change/fleet/<card#>/session -H 'content-type:
application/json' -d "{\"session\":\"$SESSION_ID\"}"`.

**Annotate — your final act, not optional.** You did the work and know why;
the review renders the code with your justifications at the point they
apply. Annotate the decisions a reader would question: the why of a cache,
a timeout's value, a deliberate asymmetry. Positions are (path, side, line)
in that round's diff; the bridge rejects files the round never touched.
**Never write review commentary into source comments instead** (DW-002 §5):

```bash
curl -s -u "skiff:$PW" -X POST $BRIDGE/change/fleet/<card#>/annotation -H 'content-type: application/json' \
  -d '{"round":1,"path":"skiff/app/x.rb","line":12,"side":"new","text":"cached because …"}'
```

Then submit — this is your "PR is up" moment:

```bash
curl -s -u "skiff:$PW" -X POST $BRIDGE/change/fleet/<card#>/submit
```

**Revisions arrive as round n+1** (a child of round n — the bridge enforces
the stack), never by amending a submitted round; that is what keeps the
human's annotations and read-state honest. A request-changes note reaches
your session prefixed `[dw request-changes · fleet #<card#>]`; answer it
with the next round and re-submit.

## 4. Gates — before every submit

CI stays archived; these run before **every** round you submit, not once
per change, and nothing checks main after a landing except the next deploy:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
TUGBOAT_FLEET=$PWD/fleet.toml cargo run -q -p tugboat -- fleet gen --check
```

Plus the per-area gates when touched, unchanged from before: `bun run
build` in `<app>/web`; `cargo test && cargo clippy --all-targets -- -D
warnings` from `ide/`; `(cd loom && make build)`, `(cd filament && swift
test)`, `(cd shutter && make build)` on macOS; `node --test
bridge/test/*.test.js` from `skiff/` for the bridge; `bin/ci` from `skiff/`
for Rails; `node --test timeline/test/` for `timeline/`.

## 5. Stop — the human closes the loop

- **Never** approve, never `jj git push`, never move the `main` bookmark,
  never `dw ship` (it is the human's own-work verb), never close the card.
- The human reviews at the desk (skiff), requests changes or approves;
  approve rebases onto `origin/main` and pushes — the same artifact a
  merged PR used to be. Landing does not deploy; deploys stay human-requested.
- If approve comes back with conflicts, the conflicted round commits carry
  them (`lastLanding.conflicts`). Resolve **in those commits** — `jj edit
  <changeId>`, fix, `jj new` — the change ids are stable, and this is the
  one case where submitted rounds legitimately change under their
  annotations (the landing rebase already rewrote them).
- After the change ships: `jj workspace forget <name>` from the main
  checkout, then remove `.workspaces/<slug>`.

## Hard rules

- `~/.claude/CLAUDE.md` is authoritative on quality: no hacks, no
  workarounds, no partial fixes. If blocked, report — don't submit a hack
  to reach "done".
- `docs/deepwater-style-guide.md` (DW-001) governs all UI.
- `gatesRan` is a claim about what you actually ran. The human's reading of
  the code is the only verification in the system — do not poison it.
- Deployability is discovered: a top-level dir is deployable iff it has
  `deploy.toml`. `fleet.toml::docs.guidance` and `fleet.toml::backup` are
  the only central manifests — keep them in sync.
