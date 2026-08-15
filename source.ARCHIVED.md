# Source — archived 2026-08-15

Source (browse/search fleet source at https://source.intern.deepwa7er.net) is archived and no longer built, deployed, or routed.

- **Why:** not in use — dev-box source viewer (reads ~/code working trees directly, exposed tailnet write path `PUT /api/file`) duplicated by the fleet IDE (`fleet/ide`) and warehouse MCP. The fleet's source now lives in the monorepo itself and the IDE provides the viewer with better auth. See card https://fizzy.intern.deepwa7er.net/1/cards/70
- **Resurrection pointer:**
  - Git tag: `archive/source-2026-08-15` (annotated, pushed to origin)
  - Branch: `archive/source` (points at same commit as tag, pushed)
  - To resurrect full code: `git checkout archive/source -- source/`
  - To restore manifests: `git checkout archive/source -- Cargo.toml Cargo.lock breakwater/breakwater.toml README.md`
  - Then `cargo run -p tugboat -- fleet gen` to verify (source had no `deploy.toml`, so no generated entries — the route was hand-written) and `bun run build` in `source/web` + `cargo test --workspace` to restore Cargo.lock entry
- **What was removed in this commit:** `source/` directory (Cargo.toml, source.toml, README.md, deploy/com.deepwa7er.source.plist, .gitignore, src/main.rs, src/config.rs, src/fleet.rs, src/repo.rs, src/search.rs, src/web.rs, src/edit.rs, web/package.json, web/vite.config.ts, web/tsconfig.json, web/index.html, web/src/App.tsx, web/src/api.ts, web/src/components/FileView.tsx, web/src/components/RepoTree.tsx, web/src/components/SearchPanel.tsx, web/src/lib/highlight.ts, web/src/lib/tree.ts, web/src/lib/useTheme.ts, web/src/main.tsx, web/src/styles.css, web/bun.lock, web/dist — 27 files), `"source"` from `Cargo.toml` workspace members (and `source` from `Cargo.lock`), hand-written `[[routes]] label="source"` block (`fedora.tailcfab97.ts.net:7879`) and `source` references from `breakwater/breakwater.toml` comments, and `| **source** | ... |` row from `README.md`.
- **VPS state after merge:** source never ran on the VPS — it was a dev-box launchd/systemd service. Existing dev-box state at `~/.cargo/bin/source` + `~/Library/LaunchAgents/com.deepwa7er.source.plist` (macOS) or `systemd --user` unit (Fedora, `~/.config/systemd/user/source.service` if present) + `~/code/fleet/source/web/dist` remains until manual `launchctl unload -w ~/Library/LaunchAgents/com.deepwa7er.source.plist` / `systemctl --user disable --now source` + `rm -rf` + `cargo install` cleanup. Breakwater on next deploy stops proxying `source.intern.deepwa7er.net` (now 404). No DB snapshot (stateless file viewer).

History is intact — no code lost.
