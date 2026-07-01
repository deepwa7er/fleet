# clothes

A wardrobe organizer for building and maintaining a classic wardrobe. Catalogue
clothes by category, drop in product links while shopping, and track each piece
from wishlist → ordered → owned. Each category carries a target count so you can
see how close each part of the wardrobe is to complete.

Part of the fleet: a single Rust binary (axum + SQLite) serving a JSON API and a
React/Vite SPA, styled with the shared DG-001 design system and themed live from
`tide`. Binds loopback; `breakwater` fronts it at
`https://clothes.intern.deepwa7er.net`.

## Model

- **Category** — a section of the wardrobe (e.g. "Footwear") with an optional
  target count of pieces to own and a line of guidance. A fresh database is
  seeded with the standard classic men's wardrobe; rename/retarget/delete freely.
- **Item** — a product link with a title, store, price, image, size, notes, and a
  status: `wishlist`, `ordered`, `owned`, or `skipped`. Pasting a link and
  hitting **Fetch** pulls the title/store/image/price from the page's Open Graph
  tags (best-effort; every field is editable by hand).

## Develop

Two processes. Backend:

```sh
cargo run -- serve          # serves the API on 127.0.0.1:8099
```

Frontend (proxies `/api` to the running backend):

```sh
cd web && bun install && bun run dev
```

Open the Vite dev URL. In production the same binary serves the built `web/dist`
bundle, so the API is same-origin.

### Configuration (env vars)

| Var               | Default                          | Purpose                     |
| ----------------- | -------------------------------- | --------------------------- |
| `CLOTHES_ADDR`    | `127.0.0.1:8099`                 | listen address              |
| `CLOTHES_DB`      | `$XDG_DATA_HOME/clothes/clothes.db` | SQLite database path     |
| `CLOTHES_WEB_DIR` | `web/dist`                       | built SPA directory to serve |

## Test

```sh
cargo test
```

## Deploy

Routine deploys go through tugboat (`deploy.toml`): build the web bundle and a
static musl binary locally, ship both to the VPS. First-time / unit changes:
`deploy/provision.sh` (service user, web dir, systemd unit). Add the host route
in `breakwater.toml` and it's reachable at `https://clothes.intern.deepwa7er.net`.
