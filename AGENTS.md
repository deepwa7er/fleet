# Fleet — agent instructions

For any UI work, read `docs/deepwater-style-guide.md` (DW-001) first.

- The six rules in §1 are authoritative (whitespace, depth, warm paper `#f7f2e9` / charcoal, one Bavarian blue, instrumentation, motion).
- `taste` is the negative filter — what not to do.
- Where they conflict, DW-001 overrides `taste` (fleet paper is warm cream `#f7f2e9`, not taste's `#faf8f4` page-cream ban).

Specimens: `docs/deepwater-style-guide.html` and `docs/deepwater-404.html`.
Reference: [DW-001](docs/deepwater-style-guide.md) · [specimen](docs/deepwater-style-guide.html)

## The source control redesign — shipped; this workflow is it

The card→branch→PR workflow was retired at the cutover (2026-08-23). Before working on `jj`, the `dw` CLI, the change/round/annotation model, the review in `skiff`, or the record/timeline, read [DW-002](docs/source-control-redesign.md) and [DW-003](docs/public-record.md) — they record alternatives that were tried and rejected, and re-deriving that reasoning costs more than reading it.

Skiff's only live implementation is the Rust `skiffd` service and React client
defined by [DW-004](docs/skiff-architecture.md). The former Rails application
and Node bridge were deleted at the 2026-08-24 cutover and must not be restored
or used as the basis for new work. Rails and bridge passages in DW-002 describe
historical delivery only; DW-004 supersedes them operationally.

## Workflow — desktop curation or Mac manual jj

For any fleet change, read `.agents/skills/fleet/SKILL.md` first. The Fedora
desktop uses the curated workflow: search, Fizzy card, isolated jj workspace,
gates, `dw` rounds and annotations, then human review and approval in Skiff.
The Mac uses a manual VSCodium lane: start from a Fizzy card, work in a local jj
change, run the same gates, and stop for review in the editor without `dw` or
Skiff. A Mac change may land only after the human explicitly accepts that diff
and asks to ship it; record the landed commit and gates on its Fizzy card.
Neither lane uses branches or GitHub pull requests, and `.jj` state is never
copied between machines. Never use bare `jj undo` because each repository's
operation log is shared by all of its workspaces.

Gates before every submitted desktop round or Mac review handoff. CI stays archived (2026-08-13), so these are the *only* enforcement — nothing checks `origin/main` after a landing:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
TUGBOAT_FLEET=$PWD/fleet.toml cargo run -q -p tugboat -- fleet gen --check
```

`fleet gen --check` verifies the generated registries (`breakwater/breakwater.toml`, `fleet-backup/state.sh`) still match each service's `deploy.toml`. Also `bun run build` in `<app>/web` for any web app touched — nothing else typechecks it; for Skiff, run `(cd skiff/web && bun run build && bun run test)`. Also `cargo test && cargo clippy --all-targets -- -D warnings` from `ide/` if you touched `ide/` — it is its own Cargo workspace (heavy gpui deps, see `ide/README.md`), so the workspace-wide gates above never compile it. Also, for the SwiftPM packages — `loom/` (the macOS window manager), `filament/` (the Swift UI reconciler it renders through), and `shutter/` (the macOS screenshot tool) — no Cargo gate reaches any of them. Gate whichever you touched; loom depends on filament by path, so a filament change must be gated by both:

```bash
(cd loom && make build)
(cd filament && swift test)
(cd shutter && make build)
```

macOS only. `filament`'s suite is swift-testing, whose `TestingMacros` compiler plugin ships **only with full Xcode**: if `xcode-select -p` reports `/Library/Developer/CommandLineTools`, `swift test` fails to build with `plugin for module 'TestingMacros' not found` — a toolchain gap, not a test failure. Either `sudo xcode-select -s /Applications/Xcode.app/Contents/Developer` once, or prefix the run:

```bash
(cd filament && DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer swift test)
```

Adjust the path to the Xcode that is actually installed. `loom` and `shutter` have no test suite; their `make build` needs no Xcode and works under CommandLineTools. Both also have a `make app` that codesigns the bundle with a named Apple Development identity — that is for running the app, not a gate.

Skills live in `.agents/skills/<name>/SKILL.md`:

- `fleet` — the workflow above, card → jj change → Skiff or editor review → human-approved landing
- `fizzy` — the Fizzy card CLI: `cargo run -p fizzy -- boards | stream | show | create` (a workspace member, not a binary on `PATH`)
- `pull-card` — shortcut: pull the most recent open card and summarize what closing it needs

Archived, both 2026-08-13: the drydock autonomous worker (`drydock.ARCHIVED.md`, tag `archive/drydock-2026-08-13`) and CI (`ci.ARCHIVED.md`, tag `archive/ci-2026-08-13`). Nothing in this workflow writes to `~/code/.drydock`.

## Imported upstreams — this repo is the only copy

`loom/` and `filament/` were standalone repos until 2026-08-14, imported here with full history (`git filter-repo --to-subdirectory-filter <name>`, then a merge with `--allow-unrelated-histories`) in PRs #32 and #39. `shutter/` followed on 2026-08-15 by the same method in PR #43. **`deepwa7er/loom`, `deepwa7er/filament`, and `deepwa7er/shutter` are now archived read-only on GitHub** and each carries a README pointing back here.

`public_site/` followed on 2026-08-24 using the same history-preserving import under Fizzy card #128. After that import merges, archive `deepwa7er/public_site` read-only and treat this monorepo as its canonical source.

Consequences for any change to any of them:

- Change them in this repo. `~/code/loom`, `~/code/filament`, and `~/code/shutter` are stale checkouts of archived remotes, not worktrees to edit — a push from any of them fails, and work done there is stranded outside the monorepo.
- Change `public_site/` in this repo too. `~/code/public_site` is the preserved standalone checkout used for the import, not a second working copy; after the import merges, new work there would be stranded outside the monorepo.
- Do not restore the URL dependency. `loom/Package.swift` references filament as `.package(path: "../filament")`; the old `git@github.com:deepwa7er/filament.git` requirement now points at a frozen snapshot. `loom/Package.resolved` is intentionally absent — loom has no remote dependencies left to pin.

## Quality — NO HACKS (authoritative)

Read `~/.claude/CLAUDE.md` — ABSOLUTE code quality, no hacks/workarounds/monkey-patches/partial fixes; if blocked, fix the root cause properly or report honestly. Back-compat not required.
