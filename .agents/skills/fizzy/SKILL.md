---
name: fizzy
description: Create and list Fizzy (Once) kanban cards from the shell — the agent's write path to the kanban board. Use when you have found a repo gap that should become a triage card, or when the user asks to list boards/cards.
---

# Fizzy — kanban card CLI for Muse

`crates/fizzy` is a Rust client for Fizzy (`ghcr.io/basecamp/fizzy`) on `laptop` (`fizzy.intern.deepwa7er.net` via breakwater). It reuses the account-prefixed Bearer [REDACTED] contract of its read-only counterpart, `mirror::fizzy`.

## Quick contract

- Origin: `https://fizzy.intern.deepwa7er.net` (no trailing slash, no account)
- Account: `1` (Fizzy mounts at `/{account}/boards.json` — without it every request 302s to `/session/menu`)
- Token: file `~/.config/fizzy/write-token` (0600), `permission: write` (`Identity::AccessToken#allows?` is `GET|HEAD || write?`). Env override `FIZZY_TOKEN_FILE`.
- Create: `POST /{account}/boards/:board_id/cards.json` `{"card":{"title": "...", "description": "…"}}` → `201 {number, title}` (`CardsController#create`, `status: published`, `creator: Current.user`).
- Comment: `POST /{account}/cards/:number/comments.json` `{"comment":{"body": "<html>"}}` → `201 {id, url, body:{plain_text}}` (`Cards::CommentsController#create`). `403` means the card is a draft — `Card::Commentable#commentable?` is `published?`. Closed cards still accept comments.

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

### 4. Create a triage card — use `draft` for readable cards

Card bodies are markdown rendered by Fizzy's Trix/ActionText. The CLI
normalises them (blank lines around headings, list markers, `---` rules)
and validates `fleet:` cards for the `Why / Evidence` scaffold. To avoid
walls-of-text and missing sections:

```bash
# 1. Scaffold a draft with the canonical sections
cargo run -p fizzy -- draft --title "fleet: blog — backup gap — /opt not in fleet-backup"
# draft: "fleet: blog — backup gap — /opt not in fleet-backup" → /tmp/fleet-blog-backup-gap.md

# 2. Edit the file (it contains Why / Evidence / Options / Provenance)
$EDITOR /tmp/fleet-blog-backup-gap.md
cat /tmp/fleet-blog-backup-gap.md
## Why
`fleet/blog` and `fleet/readout` store SQLite at `/opt/*/storage` — not in `fleet-backup`'s `/var/lib/*` set.

## Evidence
- `fleet/blog/deploy.toml:12` — no [state]
- `fleet-backup/state.sh:4` — SQLITE_DBS="clothes/..."

## Options
1. Move both to /var/lib/<name>/ + fleet.toml [backup]

---
Provenance: session 6d077caa, commit c2663ac

# 3. Check it (optional) then post
cargo run -p fizzy -- lint --title "fleet: blog — backup gap — /opt not in fleet-backup" --body-file /tmp/fleet-blog-backup-gap.md
cargo run -p fizzy -- create --board Playground --title "fleet: blog — backup gap — /opt not in fleet-backup" --body-file /tmp/fleet-blog-backup-gap.md --dedupe
# created #32: fleet: blog — backup gap — /opt not in fleet-backup
# https://fizzy.intern.deepwa7er.net/1/cards/32
```

Flags:

- `draft --title <TITLE> [--output /tmp/card.md] [--force]` — writes the `Why / Evidence / Options / Provenance` template (default `/tmp/<slug>.md`). This is the preferred entry point.
- `lint --title <TITLE> (--body "…" | --body-file <PATH>)` — validates and prints the normalised body without posting. `fleet:` titles require `## Why` + `## Evidence`; all titles print soft warnings for missing `Provenance` / title dash.
- `create --board <BOARD> --title <TITLE> (--body-file <PATH> | --body "…") [--dry-run] [--dedupe] [--raw]` — posts the card. Bodies are normalised before POST (blank lines around headings, `*text` → `* text`, `---` rules). `fleet:` cards missing `Why`/`Evidence` are rejected unless `--raw`. `--body` is deprecated — it still works but warns; prefer `draft` + `--body-file` to preserve newlines.
- `--dry-run` — print what would be POSTed (normalised body) and exit 0.
- `--dedupe` — skip if a published triage card with the same title already exists (checked before `--dry-run`).
- Env overrides: `FIZZY_BASE`, `FIZZY_ACCOUNT`, `FIZZY_TOKEN_FILE`

Formatting guarantees (in `crates/fizzy/src/format.rs`): `\r\n`→`\n`, trailing ws stripped, at most one blank line, blank line before/after every `##` heading and `---` rule, blank line after heading before content, list markers normalised (`-item` → `- item`, `1.item` → `1. item`), fenced blocks preserved, ends with single `\n`.

### 5. Comment on a card — record an outcome

```bash
cargo run -p fizzy -- comment 81 --body-file /tmp/shipped.md
# commented on #81
# https://fizzy.intern.deepwa7er.net/1/cards/81/comments/0193...
```

`comment <number> (--body-file <PATH> | --body "…") [--dry-run]`. The body is
markdown, normalised and rendered to HTML like a card description — but the
`Why / Evidence` scaffold is a *card* convention and is deliberately not
enforced on comments.

Tagging is not supported: Fizzy attaches tags via `taggings` after create, not in the card payload. Tag in the web UI.

**Closing a card is not supported, by design.** Standing changes (closed /
not-now / back to triage) go through `Columns::Cards::Drops::*`, whose actions
render only `create.turbo_stream.erb` and have no JSON representation. Bearer
auth is honored only when `request.format.json?`
(`Authentication#bearer_token_authenticatable_request?`), so a JSON request
authenticates, runs the side effect, and then fails to render — while a
non-JSON request is not authenticated at all. Every `.json.jbuilder` in Fizzy
is a read view, and each endpoint meant for the API declares `format.json`
explicitly. Close cards in the web UI; use a comment to record what happened.

Always use `~/.config/fizzy/write-token` (or `FIZZY_TOKEN_FILE`) — never `echo $TOKEN` in a prompt.

### When to create

- After you have finished a repo investigation and have 1–3 concrete gaps that are **actionable cards**, not observations. Show the draft `title`/`body` and ask before posting, unless the user said "create cards for the gaps."
- Prefer one card per gap, with a short `fleet: <area> — <what>` title and a body containing `Why / Evidence (file:line) / Options / Provenance`. Use `draft` so you don't hand-roll the scaffold.

### After creating

Echo the printed `https://fizzy.intern.deepwa7er.net/1/cards/:number` URL back to the user so they can open it.
