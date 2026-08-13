---
name: pull-card
description: Pull the most recent open Fizzy card and explain what needs to be done to close it. Use when the user says pull the latest open card, pull latest card, next card, or what's open.
---

# Pull Latest Card

Use when the user wants the next piece of work from Fizzy without specifying a card — phrases like "pull the latest open card", "pull latest card", "next card", "what's open on Playground".

This is a shortcut into the `fleet` workflow (`.agents/skills/fleet/SKILL.md`) for the common startup: find the most recent open card and summarize what closing it requires. Do not create a new card, worktree, branch, or PR unless the user asks.

## Steps

1. Orient — read `~/.claude/CLAUDE.md` (NO HACKS) and `.agents/skills/fleet/SKILL.md` §0.
2. List open work:
   ```bash
   cargo run -p fizzy -- boards
   cargo run -p fizzy -- stream --board Playground
   ```
   If `cargo run -p fizzy` fails (fizzy not in this workspace), try `fizzy boards` / `fizzy stream --board Playground` and note which invocation worked.
3. Pick the most recent open card on `Playground` (by creation time or card number). Fetch its full body (`cargo run -p fizzy -- show <id>` or `fizzy show <id>`) and note its URL `https://fizzy.intern.deepwa7er.net/1/cards/<id>`.
4. Summarize for the user:
   - Card title, number, URL, and board
   - Why / evidence (files:lines if present)
   - Acceptance criteria / what "done" means
   - Suggested files/areas to inspect (from card + quick search)
   - Exact next commands to start work (from fleet §2):
     ```bash
     cd ~/code/fleet && git fetch origin
     git worktree add ~/code/.drydock/<slug> -b fleet/<card#>-<slug> origin/main
     cd ~/code/.drydock/<slug>
     ```
   - Gates to pass: `cargo test`, `cargo clippy -- -D warnings`, `cargo build`
5. Stop and ask if the user wants to start the worktree. Never push, merge, deploy, or close the Fizzy card — human merges per fleet §5.
