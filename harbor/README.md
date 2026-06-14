# harbor

A personal mission-control **Chrome new-tab page** — a live view into the
[secondbrain](https://github.com/deepwa7er/secondbrain) portfolio and the
`deepwa7er` VPS.

Open a new tab and you see your **Fleet** (every project with its current
status) and your **Areas** (the VPS).

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

secondbrain lives on **GitHub** (private). On the VPS, harbor keeps its own
checkout by pulling from GitHub, reads it, and serves it on the tailnet; any
computer's extension queries it.

1. **One-time setup** (creates the unprivileged `harbor` user and a read-only
   SSH **deploy key**):
   ```sh
   server/deploy/setup-vps.sh
   ```
   The script prints a public key — add it to the secondbrain repo on GitHub
   (**Settings → Deploy keys → Add**, leave *Allow write access* unchecked). This
   is what lets the VPS read the private repo.
2. **Provision the unit + config** (installs the systemd unit bound to the
   Tailscale IP on `:8090` — tailnet-only, no public exposure — and the starter
   config). One-time / on unit changes:
   ```sh
   server/deploy/provision.sh
   ```
3. **Ship the binary** with [tugboat](https://github.com/deepwa7er/tugboat)
   (static musl cross-compile → atomic swap → restart → health-check → rollback;
   on first start it clones secondbrain via the deploy key):
   ```sh
   tugboat            # reads ./deploy.toml
   ```
4. Point `extension/config.js` at `http://deepwa7er.tailcfab97.ts.net:8090` (or
   the IP `http://100.98.184.58:8090`) and reload the extension. Plain HTTP is
   fine — the tailnet encrypts transport.
5. Enroll in lighthouse so harbor monitors itself (tugboat also does this on
   every deploy): `systemctl add-wants lighthouse.target harbor.service`.

## Installing the extension (self-hosted, auto-updating)

harbor-server distributes its own extension over the tailnet: it serves a
signed `harbor.crx` and an update manifest at `/extension`. Devices install it
from there and **auto-update** over the tailnet — no Chrome Web Store. The
*install* mechanism differs by OS (see below); auto-update is the same
everywhere (the crx's manifest `update_url` points back at `/extension`).

- **Signing key:** `~/.config/harbor/extension.pem` (outside the repo — it
  *defines* the extension ID `mphgmoeghlcdljjpglbhhfpgmicoenlm`, so back it up;
  losing it changes the ID). `extension/pack.sh` packs + signs the crx and
  writes `build/updates.xml`; `tugboat` runs it on every deploy and ships both
  to `/srv/harbor/dist`.
- **Release an update:** bump `version` in `extension/manifest.json`, then
  `tugboat` (from this repo). Devices pick it up on their next update poll.

### Linux — force-install via managed policy

Drop `extension/helium-policy.json` into the browser's managed-policy dir
(root-owned), then fully restart the browser:
```sh
sudo install -Dm644 extension/helium-policy.json \
  /etc/chromium/policies/managed/harbor.json   # see path note below
```
Confirm at `chrome://policy` (`helium://policy`) that `ExtensionInstallForcelist`
is active, then `chrome://extensions` — harbor installs itself (force-installed,
"installed by your organization", can't be removed). Verified on Helium /
Fedora.

> **Managed-policy dir** is per-browser and *not* always the brand name. Helium
> (verified on `helium-bin` / Fedora) keeps upstream Chromium's compiled-in path
> `/etc/chromium/policies/managed/` — *not* `/etc/helium` or
> `/etc/net.imput.helium`. Chrome uses `/etc/opt/chrome/policies/managed/`,
> Brave `/etc/brave/policies/managed/`. To find a fork's real path, grep its ELF:
> `grep -aoE '/etc/[A-Za-z0-9_.+-]+/policies' <binary> | sort -u`. Plain **HTTP**
> over the tailnet is accepted (verified Helium 148) — no HTTPS needed.

### macOS — sideload the crx

macOS (like Windows) **refuses to force-install a non-Web-Store extension unless
the machine is enterprise-managed** (MDM / cloud-managed). On an unmanaged Mac a
config-profile policy loads but the entry is `[BLOCKED]`. So don't use a policy
there — **sideload the signed crx** instead (Helium, being ungoogled-based,
permits local crx installs that stock Chrome blocks):

1. Download it: `curl -o ~/Downloads/harbor.crx http://deepwa7er.tailcfab97.ts.net:8090/extension/harbor.crx`
2. `helium://extensions` → Developer mode **on** → drag `harbor.crx` onto the
   page → **Add**.
3. Turn Developer mode **off** — it stays enabled (it's a real install, not an
   unpacked one), and auto-updates from the same `update_url`.

## Status

**v0.1.0 — deployed.** Running on deepwa7er (systemd `harbor.service`, bound to
the tailnet IP on :8090), serving live project/area state from a GitHub checkout
of secondbrain.

### Next

- Live VPS service health by consuming lighthouse's `/api/services`.
