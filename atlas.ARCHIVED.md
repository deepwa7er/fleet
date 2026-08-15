# Atlas — archived 2026-08-14

Atlas (map and trace the fleet's Rust code at https://atlas.intern.deepwa7er.net) is archived and no longer built, deployed, or routed.

- **Why:** not in use — dev-box-only service (rust-analyzer SCIP → SQLite → axum + React) with no consumers, per 2026-08-14 decision. See card https://fizzy.intern.deepwa7er.net/1/cards/61
- **Resurrection pointer:**
  - Git tag: `archive/atlas-2026-08-14` (annotated, pushed to origin)
  - Branch: `archive/atlas` (points at same commit as tag, pushed)
  - To resurrect full code: `git checkout archive/atlas -- atlas/`
  - To restore manifests: `git checkout archive/atlas -- Cargo.toml Cargo.lock breakwater/breakwater.toml`
  - Then `cargo run -p tugboat -- fleet gen` to verify generated registries (atlas had no `deploy.toml`, so no generated entries)
- **What was removed in this commit:** `atlas/` directory (8 Rust files, `atlas.toml`, `Cargo.toml`, `migrations/`, `web/` with built `dist`, `deploy/com.deepwa7er.atlas.plist`), `"atlas"` from `Cargo.toml` workspace members (and `atlas` + `scip`/`protobuf` from `Cargo.lock`), hand-written `[[routes]] label="atlas"` block from `breakwater/breakwater.toml` and `" and atlas"` from the harness comment.
- **VPS state after merge:** next `tugboat fleet deploy` stops routing `atlas.intern.deepwa7er.net` (breakwater no longer proxies `fedora.tailcfab97.ts.net:7880`). Atlas never shipped via tugboat (dev-box service), so no VPS unit/db to stop — existing dev-box state at `~/.local/share/atlas/atlas.db` + `~/Library/LaunchAgents/com.deepwa7er.atlas.plist` (or systemd user unit on Fedora) + `cargo install` binary remains until manual `launchctl unload` / `systemctl --user disable --now atlas` + cleanup.

History is intact — no code lost.
