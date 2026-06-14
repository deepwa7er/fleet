# harbor

A personal mission-control **Chrome new-tab page** — a live view into the
[secondbrain](https://github.com/deepwa7er/secondbrain) portfolio and the
`deepwa7er` VPS.

Open a new tab and you see your **Fleet** (every project with its current
status), your **Areas** (the VPS), and a quick-launch strip.

## Components

```
extension/   MV3 Chrome extension — the new-tab UI (this is what you load)
server/      harbor-server — Rust/Axum service that reads a checkout of the
             secondbrain repo, parses the project/area frontmatter, and serves
             it as JSON. Runs on the VPS, reachable over the tailnet.
```

The extension fetches `GET /api/state` from the server and renders it. Project
state is the **frontmatter** in the secondbrain files (see that repo's
`SCHEMA.md`) — harbor reads structured fields, it does not scrape prose.

## Run it locally (full pipeline)

1. **Start the server** against your secondbrain checkout:
   ```sh
   cd server
   cargo run -- harbor.local.toml      # reads ~/secondbrain, binds 127.0.0.1:8090
   ```
2. **Load the extension:** `chrome://extensions` → Developer mode → Load
   unpacked → select `extension/`.
3. Open a new tab. The footer should read `live · secondbrain @<commit>`.

`extension/config.js` points the UI at the server (`http://127.0.0.1:8090`
locally). Whatever URL you set there must also be in `manifest.json`
`host_permissions`.

## Deploy to the VPS (tailnet)

> Not yet automated — this is the planned path, mirroring ferry/lighthouse.

- Build a static Linux binary, ship it, run it as a systemd unit bound to the
  Tailscale IP on `:8090` (tailnet-only, no public exposure / no auth).
- `server/harbor.toml` is the VPS baseline: it sets `git_remote` so harbor keeps
  its own checkout of the **private** secondbrain repo in sync. The VPS therefore
  needs read access — add a **read-only deploy key** for `deepwa7er/secondbrain`.
- Point `extension/config.js` at `https://deepwa7er.tailcfab97.ts.net:8090`.
- Enroll the unit in `lighthouse.target` so harbor monitors itself.

## Status

**v0.1.0 — working prototype.** Live project/area state end-to-end. Not yet
deployed to the VPS. Quick-launch chips are static; the `b …` box is inert
pending ferry wiring.

### Next

- Deploy to deepwa7er.
- Live VPS service health by consuming lighthouse's `/api/services`.
- Wire the `b …` box and chips to ferry.
- Optional GitHub enrichment (commit counts / last-commit dates) per `repo`.
