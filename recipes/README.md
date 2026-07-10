# recipes

Recipe book for the deepwa7er fleet: create, view, and edit recipes from one
web page at **https://recipes.intern.deepwa7er.net**.

A single Rust binary serves a JSON API and the built React SPA over one SQLite
store — the same shape as the other small fleet services (see `clothes`).

## Model

One table: `recipes`. Ingredients and steps are TEXT with one entry per line —
the natural way to type a recipe — and the UI renders them as a list and
numbered steps. Tags are a lowercased, comma-joined TEXT column exposed as a
string array on the wire; servings and prep/cook minutes are optional
integers; `source_url` records where a recipe came from.

## Web view

- **Index** (`#/`) — a searchable table of every recipe (title, ingredient,
  and tag search; tag filter buttons), ordered like a cookbook index.
- **Recipe** (`#/recipe/<id>`) — ingredients beside numbered steps, with
  servings/times/source in the header. Hash routes keep recipes linkable
  without a server-side route table.
- Create and edit share one modal form; delete confirms first.

## Development

```sh
cargo run -p recipes          # API + SPA on 127.0.0.1:8097 (recipes.db in CWD)
cd web && bun install && bun run dev   # Vite dev server proxying /api to 8097
cargo test -p recipes
```

Configuration (env): `RECIPES_DB`, `RECIPES_ADDR`, `RECIPES_WEB_DIR`.

## Deploy

- First time / unit change: `deploy/provision.sh` (service user, unit, dirs).
- Routine: `tugboat deploy` from this directory (builds web + musl binary,
  ships both, restarts, health-checks `/healthz`).
- Breakwater's route is generated from `deploy.toml` by `tugboat fleet gen`.
