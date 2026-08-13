# Drydock — archived 2026-08-13

Drydock (ticket queue for autonomous fleet work) is archived and no longer
built, deployed, or routed.

- **Why:** infra incompatible with current environment, no open tickets, only for autonomous loop — per 2026-08-13 decision. See card https://fizzy.intern.deepwa7er.net/1/cards/43
- **Resurrection pointer:**
  - Git tag: `archive/drydock-2026-08-13` (annotated, pushed to origin)
  - Branch: `archive/drydock` (points at same commit as tag, pushed)
  - To resurrect full code: `git checkout archive/drydock -- drydock/`
  - To restore manifests: `git checkout archive/drydock -- Cargo.toml fleet.toml breakwater/breakwater.toml fleet-backup/state.sh .agents/skills/fleet/SKILL.md`
  - Then `cargo run -p tugboat -- fleet gen` to regenerate routes/backup
- **What was removed in this commit:** `drydock/` directory, `"drydock"` from `Cargo.toml` workspace members, `[[docs.guidance]] Drydock worker-task` from `fleet.toml`, generated `drydock` entries from `breakwater/breakwater.toml` + `fleet-backup/state.sh`, and fleet skill drydock paragraphs (kept as archived note).
- **VPS state after merge:** next `tugboat fleet deploy` stops shipping `drydock` binary/web; existing unit/db at `/var/lib/drydock/drydock.db` + `/opt/drydock/web` + `systemd drydock.service` remain until manual `ssh vps systemctl disable --now drydock` + cleanup.

History is intact — no code lost.
