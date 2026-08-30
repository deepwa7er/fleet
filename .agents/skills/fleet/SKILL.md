---
name: fleet
description: End-to-end workflow for Fizzy-backed Fleet changes — curated jj rounds reviewed in desktop Skiff, or manual Mac jj changes reviewed in VSCodium.
---

# Fleet — monorepo change workflow

Use when making **any** change to `fleet` (code, docs, style, config). DW-002
defines two lanes:

- **Desktop curated lane:** one card = one change = additive rounds. The human
  reviews in Skiff; approve lands to `origin/main` and emits the timeline.
- **Mac manual lane:** one card = one local jj change reviewed directly in
  VSCodium. It uses Fizzy for identity and the outcome, but not `dw`, Skiff,
  annotations, or the public timeline. Nothing lands until the human explicitly
  accepts the editor-reviewed diff.

There are no branches or GitHub pull requests in either lane. The retired
pre-2026-08-23 workflow lives only in git history and `archive/*` tags.

## 0. Orient (every run)

- Read `~/.claude/CLAUDE.md` (NO HACKS — authoritative; overrides summaries).
- Read `docs/deepwater-style-guide.md` (DW-001) if the change touches UI.
- Determine the lane before creating change state. Use the Mac manual lane for
  work in the local VSCodium checkout; use the desktop curated lane for Skiff
  review. Never copy `.jj` or `.workspaces` between machines.
- In both lanes, run `cargo run -p fizzy -- stream --board Playground` before
  creating a card (the fizzy skill is the CLI contract). In the desktop lane,
  `dw` additionally authors the desktop-local change log.

## 1. Discover → Card

Unchanged: search and read first; create or update a card per
`.agents/skills/fizzy/SKILL.md` §4–§5 (`draft → lint → create --dedupe`,
update in place for follow-ups). Ask before posting unless the user said
"create cards". The card number is the change's identity in both lanes.

## 2. Choose the jj working copy

In the desktop curated lane, parallel work cannot share a working copy and
rounds must be jj changes Skiff can see. Isolation is therefore a **jj
workspace** (shares the repo and op log), not a git worktree:

```bash
cd ~/code/fleet
jj git fetch --remote origin
mkdir -p .workspaces                      # jj does not create the parent
jj workspace add .workspaces/<slug>      # gitignored, like .worktrees was
cd .workspaces/<slug>
jj new main@origin                        # round 1 bases on fresh main
```

The Mac also has its own colocated repository. Initialize it once with `jj git
init --colocate` and configure the jj author identity. For a manual change, use
the VSCodium-open working copy only when `jj status` contains no unrelated
work. Otherwise create a named jj workspace and tell the human which folder to
open in VSCodium. `origin/main` is the only synchronization boundary.

**Undo is scoped to you — hard rule (DW-002 §3).** The op log is shared by
every workspace. Never run bare `jj undo`: it reverses the *globally* most
recent operation, which may be another agent's or the human's. If you must
revert, find your own operation in `jj op log` first and name it.

## 3. Work — desktop rounds are additive commits

Build to the card's acceptance criteria. When the work is ready to show:

```bash
# gates FIRST — see §4; only what you actually ran becomes a claim
jj describe -m "round 1: <what this round is>"
R1=$(jj log --no-graph -r @ -T change_id)
jj new                                    # move off; round 2 grows here
```

Register the change and round with `dw`. `gatesRan` is **only what you ran** — it is
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
dw start <card#> --title "<card title, humanized>" --session "$SESSION_ID"
dw round <card#> --revision "$R1" \
  --gate "cargo test" --gate "clippy" --gate "fleet gen --check" \
  --worth-knowing "…"
```

If the change already exists (a card's change is created once — "one card =
one change"), **rebind it to this session** with `dw bind <card#>
"$SESSION_ID"` so the review follows the session you are working in.

**Annotate — your final act, not optional.** You did the work and know why;
the review renders the code with your justifications at the point they
apply. Annotate the decisions a reader would question: the why of a cache,
a timeout's value, a deliberate asymmetry. Positions are (path, side, line)
in that round's diff; the bridge rejects files the round never touched.
**Never write review commentary into source comments instead** (DW-002 §5):

```bash
dw annotate <card#> --round 1 --path skiff/app/x.rb --line 12 --side new \
  --text "cached because …"
```

Then submit — this is your "PR is up" moment:

```bash
dw submit <card#>
```

**Revisions arrive as round n+1** (a child of round n — the bridge enforces
the stack), never by amending a submitted round; that is what keeps the
human's annotations and read-state honest. A request-changes note reaches
your session prefixed `[dw request-changes · fleet #<card#>]`; answer it
with the next round and re-submit.

### Mac manual lane

Make the card's requested change in its local jj working copy, run the same
gates in §4, and describe the reviewed unit with the card number in the
description. Do not call `dw` and do not push. Report the card URL, checkout
path, change id, diff summary, and gates so the human can review it in
VSCodium.

After the human explicitly accepts that diff and asks to ship it, fetch origin,
rebase the reviewed change onto `main@origin`, and stop for another review if a
conflict changes the diff. Otherwise move `main` to the reviewed change and
push. Then comment on the Fizzy card with the landed commit and gates; closing
the card remains a human action. This explicit post-review request is the Mac
lane's approval boundary.

## 4. Gates — before every submit or Mac review handoff

CI stays archived; these run before **every** desktop round you submit and every
Mac change you hand off for VSCodium review. Nothing checks main after a landing
except the next deploy:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
TUGBOAT_FLEET=$PWD/fleet.toml cargo run -q -p tugboat -- fleet gen --check
```

Plus the per-area gates when touched: `bun run build` in `<app>/web`;
`(cd skiff/web && bun run build && bun run test)` for Skiff's React client
(Skiff's Rust service is covered by the workspace gates); `cargo test && cargo
clippy --all-targets -- -D warnings` from `ide/`; `(cd loom && make build)`,
`(cd filament && swift test)`, `(cd shutter && make build)` on macOS; and
`node --test timeline/test/` for `timeline/`. The former Skiff Rails app and
Node bridge were deleted at the DW-004 cutover and have no gates.

## 5. Stop — the desktop human closes the curated loop

- In the desktop lane, **never** approve, run `jj git push`, move the `main`
  bookmark, use `dw ship` (the human's own-work verb), or close the card.
- In the desktop lane, the human reviews at the hosted Skiff desk, requests
  changes or approves;
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
- Never use `dw`, the desktop change log, or Skiff for the Mac manual lane.
  Never push a Mac change before the human explicitly accepts its
  VSCodium-reviewed diff; preserve its identity and outcome in Fizzy.
- Deployability is discovered: a top-level dir is deployable iff it has
  `deploy.toml`. `fleet.toml::docs.guidance` and `fleet.toml::backup` are
  the only central manifests — keep them in sync.
