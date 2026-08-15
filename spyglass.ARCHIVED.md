# Spyglass — archived 2026-08-15

Spyglass (federated fleet search at https://spyglass.intern.deepwa7er.net) is archived and no longer built, deployed, or routed.

- **Why:** not in use — stateless federated search (fans queries to `source` + `lagoon`, embedded DG-001 UI) with no consumers, per 2026-08-15 decision. See card https://fizzy.intern.deepwa7er.net/1/cards/65
- **Resurrection pointer:**
  - Git tag: `archive/spyglass-2026-08-15` (annotated, pushed to origin)
  - Branch: `archive/spyglass` (points at same commit as tag, pushed)
  - To resurrect full code: `git checkout archive/spyglass -- spyglass/`
  - To restore manifests: `git checkout archive/spyglass -- Cargo.toml Cargo.lock breakwater/breakwater.toml`
  - Then `cargo run -p tugboat -- fleet gen` to regenerate routes (no backup state — spyglass is stateless)
- **What was removed in this commit:** `spyglass/` directory (Cargo.toml, deploy.toml, service, provision, config, src, assets), `"spyglass"` from `Cargo.toml` workspace members (and `spyglass` from `Cargo.lock`), generated `spyglass` route `127.0.0.1:8095` from `breakwater/breakwater.toml`.
- **VPS state after merge:** next `tugboat fleet deploy` stops shipping `spyglass` binary/assets and routing `spyglass.intern.deepwa7er.net`; existing unit at `/etc/spyglass/config.toml` + `/usr/local/bin/spyglass` + `systemd spyglass.service` remains until manual `ssh vps systemctl disable --now spyglass` + cleanup. No DB to snapshot (stateless).

History is intact — no code lost.
