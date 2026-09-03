# Clothes — archived 2026-08-15

Clothes (wardrobe organizer at https://clothes.intern.deepwa7er.net) is archived and no longer built, deployed, or routed.

- **Why:** not in use — no consumers, per 2026-08-15 decision. See card https://fizzy.intern.deepwa7er.net/1/cards/63
- **Resurrection pointer:**
  - Git tag: `archive/clothes-2026-08-15` (annotated, pushed to origin)
  - Branch: `archive/clothes` (points at same commit as tag, pushed)
  - To resurrect full code: `git checkout archive/clothes -- clothes/`
  - To restore manifests: `git checkout archive/clothes -- Cargo.toml Cargo.lock breakwater/breakwater.toml fleet-backup/state.sh`
  - Then `cargo run -p tugboat -- fleet gen` to regenerate routes/backup
- **What was removed in this commit:** `clothes/` directory (Cargo.toml, deploy.toml, service, provision, migrations, src, web), `"clothes"` from `Cargo.toml` workspace members (and `clothes` from `Cargo.lock`), generated `clothes` route `127.0.0.1:8099` from `breakwater/breakwater.toml` and `clothes/clothes.db` from `fleet-backup/state.sh`.
- **VPS state after merge:** next `tugboat fleet deploy` stops shipping `clothes` binary/web and routing `clothes.intern.deepwa7er.net`; existing unit/db at `/var/lib/clothes/clothes.db` + `/opt/clothes/web` + `systemd clothes.service` remain until manual `ssh vps systemctl disable --now clothes` + cleanup (and `fleet-backup` will no longer snapshot the DB).

History is intact — no code lost.
