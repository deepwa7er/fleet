# timeline

The rendered half of the public record (DW-003, `docs/public-record.md` §4):
a zero-dependency static generator over the record repository
(`deepwa7er/record`, checkout `~/code/record`), serving on the tailnet at
`https://record.intern.deepwa7er.net`.

- `build.mjs` — record entries → `dist/`: `index.html` (newest first) plus
  one page per entry, the annotated change — parsed git diffs with the
  agent's annotations at their (path, side, line) anchors, DW-001's six
  rules in miniature (whitespace hierarchy, instrumentation metadata, the
  one blue on links only, `--good`/`--danger` washes on diff lines — the
  diff precedent, never a second color set). Every interpolated string is
  escaped; the privacy boundary itself lives upstream in
  `crates/change/src/record.rs` (`build_public_change`).
- `publish.sh` — pull the record, no-op if HEAD matches the last-shipped
  stamp, rebuild, rsync + staged swap to `vps:/opt/record/web` (the
  `[docs]` pipeline's shape; breakwater serves the directory via a
  hand-written `serve_dir` route, so there is no service to restart and no
  VPS provisioning beyond the route).
- `deploy/record-timeline.{service,timer}` — hourly on the desktop, the
  warehouse-timer pattern:

```bash
cp timeline/deploy/record-timeline.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now record-timeline.timer
systemctl --user start record-timeline.service   # first ship now
```

Tests: `node --test timeline/test/` (a fixture entry rendered end to end).
Run them before any PR touching this directory — no other gate covers it.

The serving choice is deliberately the simplest thing that works and is
cheap to change: moving the timeline elsewhere later means pointing
`TIMELINE_HOST`/`TIMELINE_DEST` (or a different route) at the new home;
`build.mjs` neither knows nor cares where its output serves.
