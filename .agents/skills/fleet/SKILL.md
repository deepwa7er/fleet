---
name: fleet
description: End-to-end workflow for making changes to the fleet monorepo — Fizzy cards, git worktrees, PRs, and deploy after human merge.
---

# Fleet — monorepo change workflow

Use when making **any** change to `fleet` (code, docs, style, config). One card = one branch = one PR. Never merge or deploy — the human merges; `tugboat` ships `origin/main`. (Drydock was the autonomous worker; archived 2026-08-13 at `drydock.ARCHIVED.md` — tag `archive/drydock-2026-08-13`)

## 0. Orient (every run)

- Read `~/.claude/CLAUDE.md` (NO HACKS — authoritative; overrides summaries).
- Read `fleet/docs/deepwater-style-guide.md` (DW-001) if the change touches UI — six rules + tokens are authoritative; `taste` is the negative filter and DW-001 overrides it (e.g. fleet paper `#f7f2e9` is intentional, not `taste`'s `#faf8f4` ban).
- Run `cargo run -p fizzy -- boards` and `cargo run -p fizzy -- stream --board Playground` to see open cards before creating new ones.

## 1. Discover → Card

- Search and read first. If you find a gap, bug, or follow-up, scaffold a draft with `draft` (it writes the `Why / Evidence / Options / Provenance` template and normalises markdown), edit it, then create it:

```bash
cargo run -p fizzy -- draft --title "fleet: <area> — <what>"
# → /tmp/fleet-area-what.md
$EDITOR /tmp/fleet-area-what.md
cargo run -p fizzy -- lint --title "fleet: <area> — <what>" --body-file /tmp/fleet-area-what.md  # optional check
cargo run -p fizzy -- create --board Playground --title "fleet: <area> — <what>" --body-file /tmp/fleet-area-what.md --dedupe
# prints https://fizzy.intern.deepwa7er.net/1/cards/<n> — echo it back
# create normalises (blank lines around headings, list markers) and validates fleet: cards need ## Why + ## Evidence (use --raw to skip)
```

- Ask before posting unless the user said "create cards". Use `--dedupe` to avoid duplicates. Prefer `--body-file` from `draft` — `--body "…"` is deprecated (shell escaping collapses formatting). `~/.config/fizzy/write-token` (0600) is the write token; never `echo $TOKEN`.

## 2. Prepare — isolate

Never work in the shared checkout. Never commit to `main`.

Worktrees live in `.worktrees/<slug>` inside this repo (gitignored). Fizzy and
drydock are unrelated systems — nothing in this workflow writes to
`~/code/.drydock`, which the archived worker owned.

```bash
cd ~/code/fleet && git fetch origin
# fresh card:
git worktree add .worktrees/<slug> -b fleet/<card#>-<slug> origin/main
# resumed card (branch already on origin):
git worktree add .worktrees/<slug> fleet/<card#>-<slug>
cd .worktrees/<slug>   # or <service>/ inside it
```

> Archived: Drydock `ticket/<id>-<slug>` workflow removed 2026-08-13 — see `drydock.ARCHIVED.md` + tag `archive/drydock-2026-08-13` for the autonomous worker docs.

## 3. Work — build to acceptance criteria

- Match existing conventions; prefer correct design over preserving a flawed one (back-compat not required per `~/.claude/CLAUDE.md`).
- Run the gates. CI was archived 2026-08-13 (`ci.ARCHIVED.md`), so these are the *only* enforcement — nothing checks `origin/main` after you push:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
TUGBOAT_FLEET=$PWD/fleet.toml cargo run -q -p tugboat -- fleet gen --check
```

- `fleet gen --check` verifies the generated registries (`breakwater/breakwater.toml`, `fleet-backup/state.sh`) still match each service's `deploy.toml`. Run it whenever you touch a `deploy.toml` or `fleet.toml`; without it drift surfaces only at deploy time.
- Also `bun run build` in `<app>/web` for any web app you touched — nothing else typechecks it now.
- Keep the worktree's `fleet/Cargo.toml` workspace lockfile intact.
- Heartbeat if long-running: `cargo run -p fizzy -- boards` is not a heartbeat — drydock heartbeats were `drydock heartbeat "<step>"` (archived).

## 4. PR — push and open, then stop

```bash
git push -u origin fleet/<card#>-<slug>
gh pr create --title "fleet: <area> — <what> (#<card>)" --body "Closes https://fizzy.intern.deepwa7er.net/1/cards/<card>

Summary: ...

Evidence: ...

Breaking change: yes/no
Fizzy: https://fizzy.intern.deepwa7er.net/1/cards/<card>"
```

- One PR per card. Link the Fizzy URL in the PR body.
- Clean up the worktree after push (branch is safe on origin): `git worktree remove --force .worktrees/<slug>`

## 5. Wait — human gates

- **Never** `gh pr merge`, never `tugboat deploy` / `tugboat fleet deploy`, never push to `main`, never close the Fizzy card from the agent.
- The human reviews, merges to `main`, and deploy follows automatically (`tugboat` ships `origin/main`; `breakwater` routes, `fleet-backup` captures state declared in each `deploy.toml` + `fleet.toml::backup`). There is no CI gate — archived 2026-08-13 (`ci.ARCHIVED.md`) — so §3's gates must be green *before* you push.

## Hard rules

- `~/.claude/CLAUDE.md` is authoritative on quality: no hacks, no workarounds, no partial fixes. If blocked, report — don't ship a hack to reach "done". (Was `drydock block`; drydock archived 2026-08-13.)
- `docs/deepwater-style-guide.md` (DW-001) governs all UI; `taste` is the anti-slop filter — DW-001 wins on conflict.
- Deployability is discovered: a top-level dir is deployable iff it has `deploy.toml` (`fleet.toml:14-20`). No registry to update.
- `fleet.toml::docs.guidance` and `fleet.toml::backup` are the only central manifests — keep them in sync when adding docs or state.
