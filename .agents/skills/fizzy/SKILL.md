---
name: fizzy
description: Create and list Fizzy (Once) kanban cards from the shell — the agent's write path to the kanban board. Use when you have found a repo gap that should become a triage card, or when the user asks to list boards/cards.
---

# Fizzy — kanban card CLI for Muse

`crates/fizzy` is a Rust client for Fizzy (`ghcr.io/basecamp/fizzy`) on `laptop` (`fizzy.intern.deepwa7er.net` via breakwater). It reuses the account-prefixed Bearer JSON contract of its read-only counterpart, `mirror::fizzy`.

## Quick contract

- Origin: `https://fizzy.intern.deepwa7er.net` (no trailing slash, no account)
- Account: `1` (Fizzy mounts at `/{account}/boards.json` — without it every request 302s to `/session/menu`)
- Token: file `~/.config/fizzy/write-token` (0600), `permission: write` (`Identity::AccessToken#allows?` is `GET|HEAD || write?`). Env override `FIZZY_TOKEN_FILE`.
- Create: `POST /{account}/boards/:board_id/cards.json` `{"card":{"title": "...", "description": "…"}}` → `201 {number, title}` (`CardsController#create`, `status: published`, `creator: Current.user`).

Read tokens (`/etc/mirror/fizzy-token` on the VPS) can only `GET`/`HEAD`.

## How Muse should use it

### 1. List boards (discover id vs name)

```bash
cargo run -p fizzy -- boards
# Playground  03gmeh5pknsd4ycijtmjxy4td
# (a trailing "published" marks a board with a public URL)
```

### 2. List triage (stream) — dedupe check

```bash
cargo run -p fizzy -- stream --board Playground
# #29  Serverless?  published
```

### 3. Read one card in full

```bash
cargo run -p fizzy -- show 32
# #32  fleet: blog/readout not backed up — /opt storage not in fleet-backup
# status: published
# board: Playground (03gmeh5pknsd4ycijtmjxy4td)
# creator: deepwater
# https://fizzy.intern.deepwa7er.net/1/cards/32
# --- body ---
# ## Why …
```

`stream` prints only number/title/status; `show <number>` fetches the markdown body (`GET /1/cards/:number.json`).

### 4. Create a triage card

Draft the body to a file first so the user can inspect it, then post:

```bash
cat > /tmp/card.md <<'MD'
## Why
`fleet/blog` and `fleet/readout` store SQLite at `/opt/*/storage` — not in `fleet-backup`'s `/var/lib/*` set.

## Evidence
- fleet/blog/deploy.toml: no [state]
- fleet-backup/state.sh: SQLITE_DBS="clothes/..."

## Options
1. Move both to /var/lib/<name>/ + fleet.toml [backup]

Provenance: session 6d077caa, commit c2663ac
MD

cargo run -p fizzy -- create --board Playground --title "fleet: blog backup gap — /opt not in fleet-backup" --body-file /tmp/card.md
# created #32: fleet: blog backup gap — /opt not in fleet-backup
# https://fizzy.intern.deepwa7er.net/1/cards/32
```

Flags:

- `--body "…"` vs `--body-file /tmp/card.md` (mutually exclusive)
- `--dry-run` — print what would be POSTed, no network
- `--dedupe` — skip if a published triage card with the same title already exists (checked before `--dry-run`)
- Env overrides: `FIZZY_BASE`, `FIZZY_ACCOUNT`, `FIZZY_TOKEN_FILE`

Tagging is not supported: Fizzy attaches tags via `taggings` after create, not in the card payload. Tag in the web UI.

Always use `~/.config/fizzy/write-token` (or `FIZZY_TOKEN_FILE`) — never `echo $TOKEN` in a prompt.

### When to create

- After you have finished a repo investigation and have 1–3 concrete gaps that are **actionable cards**, not observations. Show the draft `title`/`body` and ask before posting, unless the user said "create cards for the gaps."
- Prefer one card per gap, with a short `fleet: <area> — <what>` title and a body containing `Why / Evidence (file:line) / Options / Provenance`.

### After creating

Echo the printed `https://fizzy.intern.deepwa7er.net/1/cards/:number` URL back to the user so they can open it.
