# fleet-backup

Offsite, encrypted backup of the `deepwa7er` VPS's per-service state — the bits
that **aren't** reproducible from a tugboat deploy. Most important: lagoon's
notes DB. Also ferry's UI-edited config, tugboat's deploy ledgers/transcripts,
and breakwater's ACME cache.

[restic](https://restic.net) → **Cloudflare R2** (S3-compatible object storage),
on a daily systemd timer. State is tiny (~250 KB), so it's effectively free.

## Why this exists

Each service keeps its runtime state in `/var/lib/<service>/`. Deploys ship only
code, so that state lives only on the box — a disk loss or VPS migration would
lose it (your notes included). This backs it up off-box so it survives either.

## How it works

- `fleet-backup` (→ `/usr/local/bin/`): snapshots lagoon's SQLite with the
  online `.backup` API (consistent even during writes — never a raw copy of a
  live DB), then `restic backup`s that snapshot plus the other state dirs, and
  prunes to a daily/weekly/monthly window.
- `fleet-backup.service` + `.timer`: run it daily (root; reads the
  `restic.env` credentials via `EnvironmentFile`).
- `restic.env` (`/etc/fleet-backup/`, mode 600): the repo URL, encryption
  passphrase, and R2 credentials. **Never committed** — see `restic.env.example`.

## First-time setup

1. **Create the R2 bucket + token** in Cloudflare (Object Read & Write). Note the
   account id, bucket name, access key id, and secret.
2. **Install the machinery:** `./provision.sh` (installs restic/sqlite3, the
   script, and the units).
3. **Write the credentials** to `/etc/fleet-backup/restic.env` (see
   `restic.env.example`). Generate a strong `RESTIC_PASSWORD` and **save it in
   your password manager** — if it's lost, the backups are unrecoverable.
4. **Initialise the repo + enable the timer:**
   ```sh
   ssh deepwa7er 'set -a; . /etc/fleet-backup/restic.env; set +a; restic init'
   ssh deepwa7er 'sudo systemctl enable --now fleet-backup.timer'
   ```
5. **Run once now + verify:**
   ```sh
   ssh deepwa7er 'sudo systemctl start fleet-backup.service && sudo journalctl -u fleet-backup -n 20 --no-pager'
   ssh deepwa7er 'set -a; . /etc/fleet-backup/restic.env; set +a; restic snapshots'
   ```

## Restore (e.g. onto a new VPS)

With `restic.env` in place (repo URL, password, R2 creds):

```sh
set -a; . /etc/fleet-backup/restic.env; set +a
restic snapshots                       # find the snapshot to restore
restic restore latest --target /restore
# lagoon's notes DB:
sudo install -o lagoon -g lagoon -m644 \
  /restore/var/lib/fleet-backup/stage/lagoon/lagoon.sqlite /var/lib/lagoon/lagoon.sqlite
# ferry config, etc. live at their original paths under /restore.
sudo cp -a /restore/var/lib/ferry/.      /var/lib/ferry/
```

(Restoring needs only the bucket + the `RESTIC_PASSWORD` — which is why that
passphrase must live somewhere off the VPS.)
