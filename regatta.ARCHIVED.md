# Regatta — archived 2026-08-15

Regatta (sequence-voting party game at https://regatta.intern.deepwa7er.net) is archived and no longer built, deployed, or routed.

- **Why:** not in use — no consumers, per 2026-08-15 decision. See card https://fizzy.intern.deepwa7er.net/1/cards/66
- **Resurrection pointer:**
  - Git tag: `archive/regatta-2026-08-15` (annotated, pushed to origin)
  - Branch: `archive/regatta` (points at same commit as tag, pushed)
  - To resurrect full code: `git checkout archive/regatta -- regatta/`
  - To restore manifests: `git checkout archive/regatta -- Cargo.toml Cargo.lock breakwater/breakwater.toml fleet-backup/state.sh`
  - Then `cargo run -p tugboat -- fleet gen` to regenerate routes/backup
- **What was removed in this commit:** `regatta/` directory (Cargo.toml, deploy.toml, service, provision, migrations, src, web), `"regatta"` from `Cargo.toml` workspace members (and `regatta` from `Cargo.lock`), generated `regatta` route `127.0.0.1:8096` from `breakwater/breakwater.toml` and `regatta/regatta.db` from `fleet-backup/state.sh`.
- **VPS state after merge:** next `tugboat fleet deploy` stops shipping `regatta` binary/web and routing `regatta.intern.deepwa7er.net`; existing unit/db at `/var/lib/regatta/regatta.db` + `/opt/regatta/web` + `systemd regatta.service` remain until manual `ssh vps systemctl disable --now regatta` + cleanup (and `fleet-backup` will no longer snapshot the DB).

History is intact — no code lost.
