# git-autocommit

A small, reusable mechanism that keeps a service's **runtime config/state** in
git automatically: watch a tracked file, and on change commit + push it to its
remote. So a service can edit its own config (e.g. via a web UI) on the box and
have every edit land in GitHub — durable and portable — with no app changes.

This is the fleet's answer to "a default config plus settings I add at runtime,
that I don't want to lose on a disk loss or VPS migration." Defaults live in the
service's code repo (shipped by tugboat); the *live, edited* config is a git
checkout kept current by this. First user: [ferry](https://github.com/deepwa7er/ferry)
via the `ferry-config` repo.

## Pieces

- `git-autocommit` (→ `/usr/local/bin/`): commit + push the repo containing a
  given path. Repo-agnostic; auth + identity come from the checkout's own config.
- `git-autocommit@.path` / `@.service`: systemd templates. The `.path` watches a
  file and triggers the `.service` to commit it. One instance per watched file.

## Install (once per host)

```sh
./provision.sh
```

## Enable a watch (per repo)

The config lives in a git checkout on the box (e.g. `/var/lib/ferry`, which *is*
a clone of `ferry-config`). Then:

```sh
# 1. A write deploy key for the config repo, on the box:
sudo install -m600 -o root -g root /path/to/key /etc/git-autocommit/<repo>.key

# 2. Point the checkout's remote at that key, and set a committer identity:
sudo git -C /var/lib/ferry config core.sshCommand \
  'ssh -i /etc/git-autocommit/<repo>.key -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new'

# 3. Watch the tracked file (systemd-escape turns the path into the instance):
inst="$(systemd-escape -p /var/lib/ferry/ferry.toml)"
sudo systemctl enable --now "git-autocommit@${inst}.path"
```

From then on, every change to the file is committed and pushed within seconds.

## Notes

- Watch the **file**, not its directory — watching the dir would re-fire on the
  commit's own `.git` writes and loop.
- The deploy key is **never** in any repo. Scope it to the single config repo,
  with write access.
- Runs as root over a service-user-owned checkout (`safe.directory` is handled),
  so it works regardless of which user owns the files.
