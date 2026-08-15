# Fleet — agent instructions

For any UI work, read `docs/deepwater-style-guide.md` (DW-001) first.

- The six rules in §1 are authoritative (whitespace, depth, warm paper `#f7f2e9` / charcoal, one Bavarian blue, instrumentation, motion).
- `taste` is the negative filter — what not to do.
- Where they conflict, DW-001 overrides `taste` (fleet paper is warm cream `#f7f2e9`, not taste's `#faf8f4` page-cream ban).

Specimens: `docs/deepwater-style-guide.html` and `docs/deepwater-404.html`.
Reference: [DW-001](docs/deepwater-style-guide.md) · [specimen](docs/deepwater-style-guide.html)

## Workflow — one card = one branch = one PR

For any fleet change, read `.agents/skills/fleet/SKILL.md` first. It covers: search first, create a Fizzy triage card (`cargo run -p fizzy -- create --board Playground --dedupe`), `git worktree` isolation in `.worktrees/<slug>` inside this repo (never `main`), the gates below, `git push -u origin fleet/<card#>-<slug>` + `gh pr create` linking the Fizzy card, then stop — human merges, `tugboat` ships `origin/main`.

Gates before every PR. CI was archived 2026-08-13, so these are the *only* enforcement — nothing checks `origin/main` after you push:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
TUGBOAT_FLEET=$PWD/fleet.toml cargo run -q -p tugboat -- fleet gen --check
```

`fleet gen --check` verifies the generated registries (`breakwater/breakwater.toml`, `fleet-backup/state.sh`) still match each service's `deploy.toml`. Also `bun run build` in `<app>/web` for any web app touched — nothing else typechecks it. Also `cargo test && cargo clippy --all-targets -- -D warnings` from `ide/` if you touched `ide/` — it is its own Cargo workspace (heavy gpui deps, see `ide/README.md`), so the workspace-wide gates above never compile it. Also `make build` from `loom/` if you touched `loom/` — it is a SwiftPM package, not a Cargo crate (see `loom/README.md`), so no Cargo gate reaches it either; it needs macOS and network on the first build (its Filament dependency is fetched from git).

Skills live in `.agents/skills/<name>/SKILL.md`:

- `fleet` — the workflow above, card → branch → PR → human merges
- `fizzy` — the Fizzy card CLI: `cargo run -p fizzy -- boards | stream | show | create` (a workspace member, not a binary on `PATH`)
- `pull-card` — shortcut: pull the most recent open card and summarize what closing it needs

Archived, both 2026-08-13: the drydock autonomous worker (`drydock.ARCHIVED.md`, tag `archive/drydock-2026-08-13`) and CI (`ci.ARCHIVED.md`, tag `archive/ci-2026-08-13`). Nothing in this workflow writes to `~/code/.drydock`.

## Quality — NO HACKS (authoritative)

Read `~/.claude/CLAUDE.md` — ABSOLUTE code quality, no hacks/workarounds/monkey-patches/partial fixes; if blocked, fix the root cause properly or report honestly. Back-compat not required.
