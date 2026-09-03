# Tide — archived 2026-08-15

Tide (fleet-wide settings at https://tide.intern.deepwa7er.net) is archived and no longer built, deployed, or routed.

- **Why:** not in use as a single global dark/light theme — per-device/system-preference or local theme is more useful, and tide's shared cookie + polling model fights that. Depot already archived (5d6b12a) and warehouse (dev-warehouse → fleet/warehouse) is the active warehouse; tide's tiny JSON state has no consumers beyond theme. See card https://fizzy.intern.deepwa7er.net/1/cards/69
- **Resurrection pointer:**
  - Git tag: `archive/tide-2026-08-15` (annotated, pushed to origin)
  - Branch: `archive/tide` (points at same commit as tag, pushed)
  - To resurrect full code: `git checkout archive/tide -- tide/`
  - To restore manifests: `git checkout archive/tide -- Cargo.toml Cargo.lock breakwater/breakwater.toml README.md`
  - Then `cargo run -p tugboat -- fleet gen` to regenerate routes (tide had a `deploy.toml`, so generated tide route 127.0.0.1:8094 was present) and `cargo test --workspace` to restore Cargo.lock entry
- **What was removed in this commit:** `tide/` directory (Cargo.toml, deploy.toml, README.md, deploy/tide.service, deploy/tide.toml, deploy/provision.sh, src/main.rs, src/config.rs, src/store.rs, src/web.rs, .gitignore — 11 files), `"tide"` from `Cargo.toml` workspace members (and `tide` from `Cargo.lock`), generated `tide` route `127.0.0.1:8094` from `breakwater/breakwater.toml`, `| **tide** | ... |` row from `README.md`. `ferry/ferry.toml` `dark`/`light` commands and `web/fleet-ui/theme.js` + `harbor/extension/theme.js` polling `https://tide.../theme` are kept as best-effort (they will 404/poll-fail but keep current cookie/default dark); cleanup deferred to replacement.
- **VPS state after merge:** next `tugboat fleet deploy` stops shipping `tide` binary and routing `tide.intern.deepwa7er.net`; existing unit/state at `/var/lib/tide/settings.json` + `systemd tide.service` remain until manual `ssh vps systemctl disable --now tide` + cleanup. No DB snapshot (plain JSON, not snapshotted by `fleet-backup`).

History is intact — no code lost.
