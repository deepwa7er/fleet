# warehouse

Local data warehouse for **all of `~/code` + shell + git** on Fedora, embedded `SQLite` (rusqlite bundled, pure Rust/C) + `Parquet`, hourly ingest, R2 disaster recovery, agent query via MCP. Library-first, hand-rolled, heuristic-only integrations.

## Quick start

```bash
cp .env.example .env  # warehouse/.env — not fleet root
# edit warehouse/.env - leave R2_* empty to run ingest without backup
cargo build -p warehouse --release
./target/release/warehouse-ingest  # or fleet workspace: cargo build --release -p warehouse --dry-run
./target/release/warehouse-ingest
./target/release/warehouse-backup --dry-run  # no-op if R2 not set, clear message
```

## .env (placeholder already)

`R2_ENDPOINT`, `R2_BUCKET`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY` are all placeholder until you create R2 bucket. Ingest works without them; backup prints `R2 not configured` and exits 0.

## Host on Fedora (hourly)

```bash
mkdir -p ~/.config/systemd/user
cp warehouse/systemd/warehouse-ingest.{service,timer} ~/.config/systemd/user/
cp warehouse/systemd/warehouse-backup.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now warehouse-ingest.timer warehouse-backup.timer
systemctl --user start warehouse-ingest.service  # one-shot now
journalctl --user -u warehouse-ingest -f
journalctl --user -u warehouse-backup -f
```

## Layout

* `WAREHOUSE_DIR=~/data/warehouse` -> `warehouse.sqlite` (WAL mode, + Parquet `raw/dt=YYYY-MM-DD/*.parquet`)
* Ingest: `crawler` (all dirs in `CODE_ROOT`) + `git_extract` (libgit2) + `shell_extract` (bash_history) + `heuristic` integrations
* Views agents use: `repo_profile`, `integration_graph`, `tool_preferences`

## Agent query (MCP)

Stdio MCP server for code agents:

```bash
./target/release/warehouse-mcp
# tools:
# - get_repo_context(repo: "skiff") -> full context for "add feature to X"
# - search_build_knowledge(query: "rust", limit: 20)
# - query_warehouse(sql: "SELECT * FROM repo_profile WHERE build_system='cargo'")
# - list_repos()
```

Add to `~/.config/claude/mcp.json` or your agent's MCP config:

```json
{
  "mcpServers": {
    "warehouse": {
      "command": "/home/deepwater/code/fleet/warehouse/target/release/warehouse-mcp",
      "env": {}
    }
  }
}
```

## Disaster recovery (R2)

Backup is incremental sync of `warehouse.sqlite` + `raw/` to `s3://$R2_BUCKET/$R2_PREFIX`. Restore:

```bash
rclone sync r2:warehouse/warehouse ~/data/warehouse --checksum
# or aws s3 sync s3://warehouse/warehouse ~/data/warehouse --endpoint-url $R2_ENDPOINT
```

Heuristic-only: `fact_integration.type` is always `heuristic_*` with `confidence 0.4-0.65` and `evidence=file:line`. No manual edges in v0.
