# Fleet — Muse memory

For any UI work, read `docs/deepwater-style-guide.md` (DW-001) before coding.

- The six rules in §1 are authoritative (whitespace, depth, warm paper `#f7f2e9` / charcoal, one Bavarian blue, instrumentation, motion).
- `taste` is the negative filter — what not to do.
- Where they conflict, DW-001 overrides `taste` (fleet paper is warm cream `#f7f2e9`, not taste's `#faf8f4` page-cream ban).

Specimens: `docs/deepwater-style-guide.html` and `docs/deepwater-404.html`.
Cross-agent copy: `AGENTS.md` and `.claude/CLAUDE.md` carry the same pointer.

## Workflow — one card = one branch = one PR

For any fleet change, read `.agents/skills/fleet/SKILL.md` first. It covers: search first, create Fizzy triage card (`cargo run -p fizzy -- create --board Playground --dedupe`), `git worktree` isolation in `~/code/.drydock/<slug>` (never `main`), `cargo test` / `cargo clippy -- -D warnings` / `cargo build`, `git push -u origin fleet/<card#>-<slug>` + `gh pr create` linking the Fizzy card, then stop — human merges, `tugboat` ships `origin/main`. See also `drydock/docs/worker-task.md` for the autonomous worker variant.

## Quality — NO HACKS (authoritative)

Read `~/.claude/CLAUDE.md` — ABSOLUTE code quality, no hacks/workarounds/monkey-patches/partial fixes; if blocked, fix the root cause properly or report honestly (per `drydock/docs/worker-task.md` STANDARDS block). Back-compat not required.
