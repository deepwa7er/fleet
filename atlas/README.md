# atlas

Map and trace the fleet's Rust code from one tailnet page:
https://atlas.intern.deepwa7er.net.

atlas runs `rust-analyzer scip` over each configured cargo workspace, derives a
symbol graph from the index, stores it in SQLite, and serves a web UI with
three views:

- **Module explorer** — every crate and module, with a dense item table
  (kinds, signatures, doc-comments, definition sites).
- **Symbol page** — one symbol's signature, docs, callers, callees, trait
  implementations, and dispatch counterparts.
- **Trace view** — the call graph from any function, drawn as a layered
  left-to-right DAG: pick `main` (or any handler) and follow the flow. Calls
  into std/deps are hidden by default and one checkbox away.

## How the graph is derived

SCIP records *occurrences*, not calls. rust-analyzer emits `enclosing_range`
(the full body extent) on every definition, so ingest attributes each
reference to the innermost enclosing function body: those are the `call` and
`use` edges (`src/ingest.rs`). Trait linkage comes from the symbol grammar
itself — `impl#[Type][Trait]method()` names both sides — since rust-analyzer
emits no SCIP relationships. Dynamic dispatch is not resolvable statically;
the symbol page lists a trait method's implementations instead of guessing.

## Running

Like source, atlas is a **dev-box** service: it needs the working trees and a
Rust toolchain (rust-analyzer on PATH). `atlas.toml` holds the production
config (tailnet bind, the workspaces to index); breakwater fronts it from the
VPS via the hand-written route in `breakwater/breakwater.toml`.

```sh
atlas index --config atlas.toml            # index every configured workspace
atlas index --config atlas.toml --project fleet
atlas serve --config atlas.toml            # serve the UI + API
```

Indexing is roughly a `cargo check` of the workspace (seconds warm, minutes
cold); the UI's **re-index** button runs the same pipeline in the background
and swaps the project's graph atomically.

### Dev loop

Run the server against a loopback copy of the config, then `cd web && bun run
dev` — vite proxies `/api` to port 7880. `cd web && bun run build` produces
the `web/dist` the binary serves in production.

### Install (launchd)

```sh
cargo install --path .                     # or: cargo build --release + copy
cd web && bun install && bun run build
sed "s|__HOME__|$HOME|g" deploy/com.deepwa7er.atlas.plist \
  > ~/Library/LaunchAgents/com.deepwa7er.atlas.plist
launchctl load -w ~/Library/LaunchAgents/com.deepwa7er.atlas.plist
```

Then deploy breakwater so the `atlas` route exists on the VPS.
