---
name: pull-card
description: Pull the most recent open Fizzy card and explain what needs to be done to close it. Use when the user says pull the latest open card, pull latest card, next card, or what's open.
---

# Pull Latest Card

Use when the user wants the next piece of work from Fizzy without specifying a card — phrases like "pull the latest open card", "pull latest card", "next card", "what's open on Playground".

This is a shortcut into the `fleet` workflow (`.agents/skills/fleet/SKILL.md`) for the common startup: find the most recent open card and summarize what closing it requires. Do not create a new card, worktree, branch, or PR unless the user asks.

## Steps

1. Orient — read `~/.claude/CLAUDE.md` (NO HACKS) and `.agents/skills/fleet/SKILL.md` §0.
2. List open work — from the repo root (`cargo -p` needs the workspace manifest):
   ```bash
   cd ~/code/fleet
   cargo run -p fizzy -- stream --board Playground
   ```
   `fizzy` is a workspace member (`crates/fizzy`); there is no installed `fizzy` binary on `PATH`, so always invoke it through `cargo run -p fizzy --`. If that command fails, **stop and report the error to the user** — do not retry it, and do not fall back to another invocation. See `.agents/skills/fizzy/SKILL.md` for the CLI contract.
3. Pick the most recent open card on `Playground` (highest card number). Fetch its full body and URL:
   ```bash
   cargo run -p fizzy -- show <number>
   ```
4. Summarize for the user:
   - Card title, number, URL, and board
   - Why / evidence (files:lines if present)
   - Acceptance criteria / what "done" means
   - Suggested files/areas to inspect (from card + quick search)
   - Exact next commands to start work (from fleet §2):
     ```bash
     cd ~/code/fleet && git fetch origin
     git worktree add .worktrees/<slug> -b fleet/<card#>-<slug> origin/main
     cd .worktrees/<slug>
     ```
   - Gates to pass: `cargo test`, `cargo clippy -- -D warnings`, `cargo build`
5. Align on design — ask clarifying questions before starting work:
   - State your interpretation in 2-4 sentences: what "done" means for this card, the approach you'd take, key files/data-model/API boundaries you'd touch, and any risks or tradeoffs you see. Keep it brief but specific to this card — not generic.
   - Then ask 2-4 focused clarifying questions to confirm alignment. Tailor them to the card; do not reuse a canned list. Good question types:
     - Scope / non-goals — is X in or out? Any related cleanup to include or defer?
     - API / data-model shape — expected interface, storage, or contract changes?
     - Behavior & edge cases — error handling, empty/missing state, backwards-compat or breaking-change tolerance?
     - Alternatives & constraints — is there a preferred direction among the card's Options, or a prior decision to respect?
     - UX / product intent (if UI) — per DW-001, confirm the intended feel before coding.
   - Prefer `muse.request_user_input` for these questions when the choices are discrete (2-3 options per question), otherwise ask directly in chat. Always wait for the user's answers and explicitly confirm the aligned direction (e.g. "Proceeding with X per your answer") before moving to implementation.
   - If the card is trivial/unambiguous, still confirm your reading with one question (e.g. "My read is X — correct?") rather than skipping alignment. If the user says "just do it" / "use your judgment", proceed with the interpretation you stated.
6. Stop and ask if the user wants to start the worktree (after alignment). Never push, merge, deploy, or close the Fizzy card — human merges per fleet §5. Do not create the worktree, branch, or start coding until the user has answered the alignment questions or explicitly told you to proceed.
