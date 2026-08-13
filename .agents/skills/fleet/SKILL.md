---
name: fleet
description: End-to-end workflow for making changes to the fleet monorepo — Fizzy cards, git worktrees, PRs, and deploy after human merge.
---

# Fleet — monorepo change workflow

Use when making **any** change to `fleet` (code, docs, style, config). This is the interactive counterpart to `drydock/docs/worker-task.md` (the autonomous Drydock worker). One card = one branch = one PR. Never merge or deploy — the human merges; `tugboat` ships `origin/main`.

## 0. Orient (every run)

- Read `~/.claude/CLAUDE.md` (NO HACKS — authoritative; overrides summaries).
- Read `fleet/docs/deepwater-style-guide.md` (DW-001) if the change touches UI — six rules + tokens are authoritative; `taste` is the negative filter and DW-001 overrides it (e.g. fleet paper `#f7f2e9` is intentional, not `taste`'s `#faf8f4` ban).
- Run `cargo run -p fizzy -- boards` and `cargo run -p fizzy -- stream --board Playground` to see open cards before creating new ones.

## 1. Discover → Card

- Search and read first. If you find a gap, bug, or follow-up, draft a card to `/tmp/card.md` with `Why / Evidence (file:line) / Options / Provenance`, then create it:

```bash
cargo run -p fizzy -- create --board Playground --title "fleet: <area> — <what>" --body-file /tmp/card.md --dedupe
# prints https://fizzy.intern.deepwa7er.net/1/cards/<n> — echo it back
```

- Ask before posting unless the user said "create cards". Use `--dedupe` to avoid duplicates. `~/.config/fizzy/write-token` (0600) is the write token; never `echo $TOKEN`.

## 2. Prepare — isolate

Never work in the shared checkout. Never commit to `main`.

```bash
cd ~/code/fleet && git fetch origin
# fresh card:
git worktree add ~/code/.drydock/<slug> -b fleet/<card#>-<slug> origin/main
# resumed card (branch already on origin):
git worktree add ~/code/.drydock/<slug> fleet/<card#>-<slug>
cd ~/code/.drydock/<slug>   # or ~/code/fleet/<service>/ inside it
```

Drydock tickets use `ticket/<id>-<slug>` instead of `fleet/<card#>-<slug>` — same isolation, same prohibition on touching the shared tree. Read the target service's `README.md` before coding.

## 3. Work — build to acceptance criteria

- Match existing conventions; prefer correct design over preserving a flawed one (back-compat not required per `~/.claude/CLAUDE.md`).
- Run the gates that `tugboat` and CI enforce: `cargo test`, `cargo clippy -- -D warnings`, and `cargo build` for any web app you touched. Keep the worktree's `fleet/Cargo.toml` workspace lockfile intact.
- Heartbeat if long-running: `cargo run -p fizzy -- boards` is not a heartbeat — for Drydock-backed work use `drydock heartbeat "<step>"`.

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
- Clean up the worktree after push (branch is safe on origin): `git worktree remove --force ~/code/.drydock/<slug>`

## 5. Wait — human gates

- **Never** `gh pr merge`, never `tugboat deploy` / `tugboat fleet deploy`, never push to `main`, never close the Fizzy card from the agent.
- The human reviews, merges to `main`, and deploy follows automatically (`tugboat` ships `origin/main`; `breakwater` routes, `fleet-backup` captures state declared in each `deploy.toml` + `fleet.toml::backup`). CI (`ci.yml`) must be green on `main`.

## Hard rules

- `~/.claude/CLAUDE.md` is authoritative on quality: no hacks, no workarounds, no partial fixes. If blocked, `drydock block` or report — don't ship a hack to reach "done".
- `docs/deepwater-style-guide.md` (DW-001) governs all UI; `taste` is the anti-slop filter — DW-001 wins on conflict.
- Deployability is discovered: a top-level dir is deployable iff it has `deploy.toml` (`fleet.toml:14-20`). No registry to update.
- `fleet.toml::docs.guidance` and `fleet.toml::backup` are the only central manifests — keep them in sync when adding docs or state.
