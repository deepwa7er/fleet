# Depot — archived 2026-08-15

Depot (fleet data warehouse at https://depot.intern.deepwa7er.net) is archived and no longer built, deployed, or routed.

- **Why:** replaced by dev-warehouse — the local warehouse for `~/code` + shell + git (SQLite + Parquet, hourly ingest, R2 backup, MCP query) now moving into the fleet. Depot's journal+deploy historian had no consumers beyond ad-hoc API; dev-warehouse is the single warehouse going forward. See card https://fizzy.intern.deepwa7er.net/1/cards/67
- **Resurrection pointer:**
  - Git tag: `archive/depot-2026-08-15` (annotated, pushed to origin)
  - Branch: `archive/depot` (points at same commit as tag, pushed)
  - To resurrect full code: `git checkout archive/depot -- depot/`
  - To restore manifests: `git checkout archive/depot -- Cargo.toml Cargo.lock breakwater/breakwater.toml fleet-backup/state.sh README.md`
  - Then `cargo run -p tugboat -- fleet gen` to regenerate routes/backup (depot had a `deploy.toml`, so generated entries exist) and `cargo test --workspace` to restore Cargo.lock entry
- **What was removed in this commit:** `depot/` directory (Cargo.toml, deploy.toml, README.md, deploy/depot.service, deploy/provision.sh, src/main.rs, src/schema.rs, src/store.rs, src/ingest.rs, src/server.rs — 10 files), `"depot"` from `Cargo.toml` workspace members (and `depot` from `Cargo.lock`), generated `depot` route `127.0.0.1:8100` from `breakwater/breakwater.toml` and `depot/depot.db` from `fleet-backup/state.sh`, `| **depot** | ... |` row + diagram + layout reference from `README.md`, and `depot` mentions from `mirror/Cargo.toml` + `mirror/deploy.toml` comments.
- **VPS state after merge:** next `tugboat fleet deploy` stops shipping `depot` binary and routing `depot.intern.deepwa7er.net`; existing unit/db at `/var/lib/depot/depot.db` + `/var/lib/depot/breakwater.cursor` + `systemd depot.service` remain until manual `ssh vps systemctl disable --now depot` + cleanup (and `fleet-backup` will no longer snapshot the DB). `tugboat/src/events.rs` emitter still appends locally to `~/.local/share/tugboat/deploys.jsonl` and best-effort POSTs to `https://depot.intern.deepwa7er.net/api/events/deploy` — after archive it will log `deploy event not forwarded to depot` until `DEPOT_URL=""` or the push is removed in the dev-warehouse follow-up.

History is intact — no code lost.
