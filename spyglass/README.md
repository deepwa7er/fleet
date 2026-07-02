# spyglass

Federated search across the **deepwa7er** fleet — one box that fans a query out
to the fleet's search services and shows the results grouped by source.

Today it federates:

- **code** — the [`source`](https://github.com/deepwa7er/source) service
  (ripgrep over every fleet repo's working tree). Each hit links to the file and
  line on GitHub.
- **notes** — the [`lagoon`](https://github.com/deepwa7er/lagoon) service (FTS +
  semantic search over thoughts).

Each source is queried in parallel and adapted into a common result shape. A slow
or unreachable source is **isolated** — its group shows "unavailable" while the
others still return — so code results show when notes are down, and vice-versa.

## Design

- **Stateless.** spyglass holds no index of its own; every result is fetched live
  from an upstream. Adding a source is a config entry, not a re-index.
- **Plain HTTP, no auth.** Upstreams are reached over the tailnet / loopback; the
  tailnet is the security boundary, like the rest of the fleet. `bind` is
  loopback and [breakwater](https://github.com/deepwa7er/breakwater) is the front
  door at `https://spyglass.intern.deepwa7er.net`.
- **One-file UI.** A single embedded HTML/CSS/JS page (DG-001, dark by default,
  honors the fleet [`tide`](https://github.com/deepwa7er/tide) theme), baked into
  the binary — the binary is the whole deploy.

## Layout

```
src/
  main.rs     CLI (serve), config load, runtime, bind
  config.rs   bind/port/github_org + the [[sources]] list
  search.rs   parallel fan-out + per-source adapters + the common Hit shape
  web.rs      router: the embedded UI + GET /api/search
assets/       index.html, app.css, app.js (the embedded UI)
deploy/       systemd unit, runtime config, provision.sh
deploy.toml   tugboat deploy manifest
```

## Adding a source

Add a `[[sources]]` block to the config with a `name`, a `kind` (`code` or
`notes` — picks the response adapter), and the upstream `url`. New result shapes
need a new adapter in `search.rs` (`fetch_*`) and a `SourceKind` variant.

## Develop

```
cargo run -- serve --config deploy/spyglass.toml   # adjust source URLs for local
```

Deploy (VPS): one-time `deploy/provision.sh`, then `tugboat deploy`.
