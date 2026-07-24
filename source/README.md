# source

Browse and **search** the deepwa7er fleet's source code from one tailnet page —
live at https://source.intern.deepwa7er.net.

The fleet's canonical working trees live only on the dev box (the VPS holds built
binaries, not source). So `source` runs **on the dev box** as a launchd agent —
the same home as `tugboat serve` — reads those working trees directly, and is
fronted by [breakwater](https://github.com/deepwa7er/breakwater) over the tailnet.

```
source.intern.deepwa7er.net
        │  (breakwater on the VPS proxies over the tailnet)
        ▼
  dev box :7879  ──  source serve
        ├─ /api/repos, /tree, /file, /search   (JSON)
        └─ /                                    (the built React app)
              reads ~/code/<repo> working trees
```

## What it serves

Everything is **tracked source only**: file lists come from `git ls-files` and
search runs through `ripgrep`, so `.gitignore`d secrets and build output are
excluded by construction. Per file the viewer enforces: in-tree paths only (no
traversal), tracked-only, a binary-file guard, and a 2 MB size cap.

- **Browse** — pick a repo, walk its file tree, read a file with syntax
  highlighting (Shiki).
- **Search** — full-text across the whole fleet (or one repo), literal by
  default or as a regex; click a hit to open the file at that line. Results are
  capped at 500 matches (flagged when truncated).

## API

| Route | Returns |
|---|---|
| `GET /api/repos` | the fleet's checked-out repos |
| `GET /api/tree?repo=NAME` | a repo's tracked file paths |
| `GET /api/file?repo=NAME&path=REL` | one file's contents + metadata |
| `PUT /api/file` `{repo,path,content,message?}` | edit a tracked file: write + commit + push |
| `GET /api/search?q=TERM[&repo=NAME][&regex=1]` | grouped match results |
| `GET /api/healthz` | liveness |

## Editing

`PUT /api/file` writes the tracked file, makes a **path-scoped** commit
(`git commit -- <path>`, so other in-progress changes in the repo aren't swept
in), and pushes. Same gate as reads: tracked, in-tree files only. Identical
content is a no-op (`committed: false`). The **commit** — not the push — is what
updates the docs: a fleet repo's post-commit hook reships [pilot](https://github.com/deepwa7er/pilot)'s
docs from the local working tree, so a push failure (offline, remote behind) is
returned as a `warning`, not an error.

**Editing a service's top-level docs** on `docs.intern.deepwa7er.net/<name>`
means editing that repo's `README.md` here — pilot harvests the README as the
service's "Architecture" prose, and regenerates on the commit.

## Security model

`bind` is the dev box's **Tailscale IP** (not loopback), because breakwater runs
on the VPS and reaches this service across the tailnet. So any tailnet device can
reach it directly — reads **and writes**. This matches the fleet's model (the
tailnet is the boundary; lighthouse/harbor/drydock all mutate state with no app
auth). If write exposure ever needs tightening, the options are a bearer token on
`PUT` or a loopback-only write path; today it's deliberately tailnet-gated only.

## Run

### Local development

```sh
# Backend on loopback (the dev proxy in vite.config.ts points /api here):
printf 'bind = "127.0.0.1"\nport = 7879\nfleet = "~/code/tugboat/fleet.toml"\nweb_dir = "~/code/source/web/dist"\n' > source.dev.toml
cargo run -- serve --config source.dev.toml

# Frontend with hot reload:
cd web && bun install && bun run dev
```

### Production (dev box launchd agent)

```sh
cargo install --path .                       # → ~/.cargo/bin/source
cd web && bun install && bun run build       # → web/dist

# Install the agent (replace __HOME__ with $HOME):
sed "s|__HOME__|$HOME|g" deploy/com.deepwa7er.source.plist \
  > ~/Library/LaunchAgents/com.deepwa7er.source.plist
launchctl load -w ~/Library/LaunchAgents/com.deepwa7er.source.plist
```

`source.toml` holds the production settings (Tailscale-IP bind, port 7879, the
fleet manifest, and the `web/dist` path). Then add the breakwater route:

```toml
[[routes]]
label = "source"
upstream = "100.111.100.87:7879"   # the dev box's Tailscale IP
```

and `tugboat deploy breakwater`. Because the upstream is the dev box, the page is
served only while the dev box is awake — inherent, since that's where the source
lives.
