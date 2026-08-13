# fizzy — Fleet client for Fizzy (Once) kanban

Rust client for Fizzy (`ghcr.io/basecamp/fizzy`) on `laptop` (`https://fizzy.intern.deepwa7er.net` via breakwater). The agent's write path to the kanban board — create triage cards for repo gaps.

Library: `crates/fizzy::Client` (account-prefixed Bearer JSON, same contract as `mirror::fizzy`). Binary: `fizzy` (boards / stream / show / create). It is a workspace member, not an installed binary — always invoke it with `cargo run -p fizzy --` from the repo root.

```
cargo run -p fizzy -- boards
cargo run -p fizzy -- stream --board Playground
cargo run -p fizzy -- show 32
cargo run -p fizzy -- create --board Playground --title "fleet: …" --body-file /tmp/card.md --dedupe
```

Config: `FIZZY_BASE` (default `https://fizzy.intern.deepwa7er.net`), `FIZZY_ACCOUNT` (`1`), `FIZZY_TOKEN_FILE` (`~/.config/fizzy/write-token`, 0600, permission `write`). Read token at `vps:/etc/mirror/fizzy-token` is `read`-only (GET/HEAD only).

See `.agents/skills/fizzy/SKILL.md` for agent usage.
