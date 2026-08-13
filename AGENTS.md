# Fleet — agent instructions

For any UI work, read `docs/deepwater-style-guide.md` (DW-001) first.

- The six rules in §1 are authoritative (whitespace, depth, warm paper `#f7f2e9` / charcoal, one Bavarian blue, instrumentation, motion).
- `taste` is the negative filter — what not to do.
- Where they conflict, DW-001 overrides `taste` (fleet paper is warm cream `#f7f2e9`, not taste's `#faf8f4` page-cream ban).

Specimens: `docs/deepwater-style-guide.html` and `docs/deepwater-404.html`.
Reference: [DW-001](docs/deepwater-style-guide.md) · [specimen](docs/deepwater-style-guide.html)

## Workflow — one card = one branch = one PR

For any fleet change, read `.agents/skills/fleet/SKILL.md` first. It covers: search first, create Fizzy triage card (`cargo run -p fizzy -- create --board Playground --dedupe`), `git worktree` isolation in `~/code/.drydock/<slug>` (never `main`), `cargo test` / `cargo clippy -- -D warnings` / `cargo build`, `git push -u origin fleet/<card#>-<slug>` + `gh pr create` linking the Fizzy card, then stop — human merges, `tugboat` ships `origin/main`. Drydock autonomous worker archived 2026-08-13 — see `drydock.ARCHIVED.md` + tag `archive/drydock-2026-08-13`.

## Quality — NO HACKS (authoritative)

Read `~/.claude/CLAUDE.md` — ABSOLUTE code quality, no hacks/workarounds/monkey-patches/partial fixes; if blocked, fix the root cause properly or report honestly. Back-compat not required.
