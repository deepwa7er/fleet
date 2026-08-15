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

`fleet gen --check` verifies the generated registries (`breakwater/breakwater.toml`, `fleet-backup/state.sh`) still match each service's `deploy.toml`. Also `bun run build` in `<app>/web` for any web app touched — nothing else typechecks it. Also `cargo test && cargo clippy --all-targets -- -D warnings` from `ide/` if you touched `ide/` — it is its own Cargo workspace (heavy gpui deps, see `ide/README.md`), so the workspace-wide gates above never compile it. Also, for the SwiftPM packages — `loom/` (the macOS window manager) and `filament/` (the Swift UI reconciler it renders through) — no Cargo gate reaches either, and loom depends on filament by path, so a filament change must be gated by both:

```bash
(cd loom && make build)
(cd filament && swift test)
```

macOS only. `filament`'s suite is swift-testing, whose `TestingMacros` compiler plugin ships **only with full Xcode**: if `xcode-select -p` reports `/Library/Developer/CommandLineTools`, `swift test` fails to build with `plugin for module 'TestingMacros' not found` — a toolchain gap, not a test failure. Either `sudo xcode-select -s /Applications/Xcode.app/Contents/Developer` once, or prefix the run:

```bash
(cd filament && DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer swift test)
```

Adjust the path to the Xcode that is actually installed. `loom`'s `make build` needs no Xcode and works under CommandLineTools.

Skills live in `.agents/skills/<name>/SKILL.md`:

- `fleet` — the workflow above, card → branch → PR → human merges
- `fizzy` — the Fizzy card CLI: `cargo run -p fizzy -- boards | stream | show | create` (a workspace member, not a binary on `PATH`)
- `pull-card` — shortcut: pull the most recent open card and summarize what closing it needs

Archived, both 2026-08-13: the drydock autonomous worker (`drydock.ARCHIVED.md`, tag `archive/drydock-2026-08-13`) and CI (`ci.ARCHIVED.md`, tag `archive/ci-2026-08-13`). Nothing in this workflow writes to `~/code/.drydock`.

## Imported upstreams — this repo is the only copy

`loom/` and `filament/` were standalone repos until 2026-08-14, imported here with full history (`git filter-repo --to-subdirectory-filter <name>`, then a merge with `--allow-unrelated-histories`) in PRs #32 and #39. **`deepwa7er/loom` and `deepwa7er/filament` are now archived read-only on GitHub** and each carries a README pointing back here.

Consequences for any change to either:

- Change them in this repo. `~/code/loom` and `~/code/filament` are stale checkouts of archived remotes, not worktrees to edit — a push from either fails, and work done there is stranded outside the monorepo.
- Do not restore the URL dependency. `loom/Package.swift` references filament as `.package(path: "../filament")`; the old `git@github.com:deepwa7er/filament.git` requirement now points at a frozen snapshot. `loom/Package.resolved` is intentionally absent — loom has no remote dependencies left to pin.

## Quality — NO HACKS (authoritative)

Read `~/.claude/CLAUDE.md` — ABSOLUTE code quality, no hacks/workarounds/monkey-patches/partial fixes; if blocked, fix the root cause properly or report honestly. Back-compat not required.
