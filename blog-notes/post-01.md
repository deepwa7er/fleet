# Post 1: The Fleet — a clipboard app that became a self-driving platform

## Summary
The story of the `fleet` monorepo (295 commits, May 21 – Aug 8, 2026): a single-person cloud that starts as a clipboard-sync app and ends as an autonomous, monorepo-based platform with a design language, a CI gate, and a public license. Told in ten acts so the author can add personal commentary on each.

## Notes

### Act I — The Seed (May 21)
- First commit `eab6e7e` "Initial commit: tidepool" — a clipboard sync tool.
- Same day: bidirectional clipboard sync over SSE (`883d48e`), iOS app skeleton, Share Extension wired into Xcode (`876e2d6` → `3b6f70e`), no-UI auto-post for a true 2-tap flow (`f4ff182`).
- `2296a02` adds a native App Intent for Siri/Spotlight/Shortcuts.
- Theme from hour zero: machines handing things to each other.

### Act II — The dashboard arrives (June 13)
- Lighthouse VPS service dashboard: start/stop/restart, live log tails default-on, deep-links to services, alerts on crash-loop (`d66a4ef`, `e090fb7`, `8d3d993`).
- Docker containers monitored alongside systemd — then quickly reverted (`8903033` → `bfd669a`).
- harbor: new-tab extension streaming live GitHub activity + VPS health + clickable notes over the secondbrain (`82404b4`, `f3dc6ff`).
- DG-001 (U.S. Graphics) design guide; Tailwind dropped (`ca02ff1`, `0e45c65`).

### Act III — Infrastructure detour (June 14–17)
- tugboat: manifest-driven deployer replacing bespoke deploy.sh (`56c7a53`, `1085ece`).
- breakwater: tailnet reverse proxy; ACME DNS-01 auto-certs with in-process renewal land the same day (`f5c3e88`).
- Fleet-wide migration: bind loopback, breakwater is the tailnet front door (`970f798`).

### Act IV — A nervous system (June 20–24)
- tugboat serve: HTTP deploy daemon + Deploy button in the dashboard (`fc2db79`, `57558b0`).
- Append-only deploy ledger, rollback-aware, per-service freshness + undeployed-commit tracking (`235fcfc`, `9a98315`).
- lagoon note capture: `b lg <text>` (`35f0e8d`).
- restic backups to R2 (`534c608`), git-autocommit systemd units (`258f874`).
- Fleet docs site auto-regenerates on commit and counts total LOC (`ad1ae23`, `b132d50`).
- tide: fleet-wide dark/light theme (`674f174`).

### Act V — The fleet fixes itself (June 22–24)
- drydock: ticket queue where an autonomous worker takes tickets and opens PRs (`7b299a7`, `405a7d0`).
- Commit log shows robot PRs: "Merge pull request #1 from deepwa7er/ticket/2-..." through ticket/10 (`400aa43`, `645253e`).
- Worker liveness panel to watch whether the Desktop worker is stuck (`ea4b5e6`).

### Act VI — Harden, move, rename (June 27–30)
- Atomic crash-safe migrations, fsync-durable settings and certs (`cb201c1`, `3e59bd8`, `f60aaa9`).
- Domain move internal.deepwa7er.com → intern.deepwa7er.net via reusable rename-fleet-domain.sh (`9190ac3`, `38dfa45`).
- source: browse + edit fleet code in a browser (`38dfa45`); spyglass federated search (`f2bf962`); clothes wardrobe organizer (`7bc502d`).

### Act VII — The monorepo (July 1)
- One day, fifteen imports: "import <name> (full history, moved under <name>/)" — breakwater, clothes, driftword, drydock, ferry, harbor, lighthouse, spyglass, tide, tugboat, tidepool and more (`dde9e9b` → `123fe2c`).
- Cargo workspace: one lockfile, one target dir, root release profile (`b5a4899`).
- fleet-common + @fleet/ui shared scaffolding (`0edc466`); contract crates so ledger/search shapes compile on both sides (`f3ec64a`).
- CI: "the gate behind every deploy" (`de2b9d8`), toolchain pinned to 1.96.0 (`9f621d8`).
- Monorepo-aware deploys via {workspace} (`f99fe06`).

### Act VIII — Life tools (July 2–12)
- regatta: "a course is the countdown — 10 of one thing, 9 of another" (`23e5f89` → `206968a`).
- recipes, sonar, trawler.
- DG-002 TRITIUM design-token sweep across the fleet (`faf44b7`, `831b2c7`).

### Act IX — The meta-turn: harness (July 26)
- harness lands — the agent harness that runs autonomous agents (`13c3f2d`).
- APNs push so a finished turn reaches the phone (`e460665`); end-to-end tests for the agent loop (`db7fffc`).
- `1123bdc`: "stop reporting a failure for a message that succeeded" — Safari JSON.parse("") threw a scary red note next to a working conversation; fixed as a class by reading the body once. Favorite commit body.
- Discovery: "K3's context window is 1M, not the assumed 128k" (`55bd9ab`) → compact context instead of walking into the window (`df0d63b`).

### Act X — Growing up, going public (July 30 → Aug 8)
- README: "describe the platform, not the migration" + MIT license (`b7c76d9`, `65e4edc`).
- mirror: public read-only view of a published board (`d00a023`); mailpit; blog routes.
- DW-001 composite style guide lands via merge PRs (`38d5e41` → `2fe7c00`).

## Key moments
- The one-day monorepo import (July 1) — 15 repos folding into one tree.
- The drydock PRs — the commit log stopped being purely human.
- `1123bdc` — a single bad JSON.parse masquerading as a dead message.
- The 128k → 1M context-window correction.
- "Bind loopback; breakwater is the tailnet front door" — the moment it became an architecture.
- README pivot from migration notes to a platform manifesto.

## Todo (for the author)
- Go through each act, add personal commentary and context the commits can't convey.
- Decide on a through-line hook: "a clipboard app that became an operating system."
