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

secondbrain is hosted on the VPS as the **canonical hub**: your computers push
edits straight to it over the tailnet (push-to-deploy), harbor reads the working
tree and serves it, and any computer's extension queries it. No GitHub, no
deploy key.

1. **One-time hub + service-account setup** (creates the push-to-deploy
   secondbrain repo and the unprivileged `harbor` user):
   ```sh
   server/deploy/setup-vps.sh
   ```
   Then point each computer's secondbrain at the VPS and push (the script prints
   the exact commands):
   ```sh
   git -C ~/secondbrain remote add vps deepwa7er:/srv/harbor/secondbrain
   git -C ~/secondbrain push -u vps main
   ```
2. **Deploy the server** (builds the release on the VPS, installs the systemd
   unit bound to the Tailscale IP on `:8090` — tailnet-only, no public exposure):
   ```sh
   server/deploy/deploy.sh
   ```
3. Point `extension/config.js` at `https://deepwa7er.tailcfab97.ts.net:8090` (or
   `http://100.98.184.58:8090`) and reload the extension.
4. Enroll in lighthouse so harbor monitors itself:
   `systemctl add-wants lighthouse.target harbor.service`.

> **⚠️ Backup:** the VPS hub has no off-site backup yet — a deliberate "later"
> item. Until then the VPS is the only copy of secondbrain (plus whatever git
> clones live on your computers).

## Status

**v0.1.0 — working prototype.** Live project/area state end-to-end. Not yet
deployed to the VPS. Quick-launch chips are static; the `b …` box is inert
pending ferry wiring.

### Next

- Run the VPS hub + deploy (`server/deploy/`), migrate secondbrain off GitHub.
- Add an off-site **backup** for the VPS hub (deliberately deferred).
- Live VPS service health by consuming lighthouse's `/api/services`.
- Wire the `b …` box and chips to ferry.
