# driftword

Generates pronounceable fake words — words that aren't real but sound real.
A single Rust binary with two modes: a **web service** (UI + JSON API) for the
`deepwa7er` tailnet fleet, and a **CLI**. Replaces the original `wordgen.py`.

## Engines

- **markov** (default) — a character n-gram trained on an embedded ~234k-word
  corpus. Mirrors real English letter statistics, so output looks authentically
  word-like (`prelation`, `mantion`, `undescate`). `--order` controls how close
  to real words it gets (3 is the sweet spot; 4 is closer).
- **phono** — assembles words from English onset/nucleus/coda inventories by
  rule. Cleaner, more brandable (`vondar`, `plimsy`, `henbaddi`).

Both filter out any actual real word, so every result is novel. `--seed` makes
output reproducible across runs.

### Letter preference

Favor a set of letters (works in both engines):

```
driftword gen --prefer aoeinum                 # soft bias toward those letters
driftword gen --prefer aoeinum --only          # strict: use ONLY those letters
driftword gen --prefer aoeinum --prefer-strength 8
```

## CLI

```
driftword gen [--mode markov|phono] [-n COUNT] [--min N] [--max N]
              [--order N] [--syllables N]
              [--prefer LETTERS] [--prefer-strength F] [--only] [--seed N]
```

## Web service

```
driftword serve --config /etc/driftword/config.toml
```

- `GET /`              — the UI (US-Graphics style; see `~/code/design-guide.md`)
- `GET /api/generate`  — JSON. Query params mirror the CLI flags
  (`mode, count, min, max, order, syllables, prefer, strength, only, seed`).
  Validation errors return `422` with `{ "error": "…" }`.
- `GET /healthz`       — `ok`

The corpus and all web assets are embedded in the binary (`include_str!`), so the
one file is the whole deploy — no `/usr/share/dict/words` or runtime deps.

`config.toml` is just the bind address and port:

```toml
bind = "100.64.0.1"   # auto-filled to the Tailscale IP by provision.sh
port = 8091
```

On the VPS it binds the Tailscale IP, so it's reachable only from the tailnet —
no public exposure, no auth (the tailnet is the security boundary, same model as
harbor and lighthouse).

## Deploy

One-time infra (service user, config, systemd unit, tailnet-IP bind):

```
deploy/provision.sh            # DRIFTWORD_HOST defaults to the `deepwa7er` ssh alias
```

Routine deploys (build → ship → atomic swap → restart → health-check → rollback,
then enroll in `lighthouse.target`):

```
tugboat --dry-run              # preview
tugboat                        # deploy
```

`deploy.toml` cross-compiles a static `x86_64-unknown-linux-musl` binary locally;
nothing is built on the VPS.

## Access

- ferry:  `b drift` / `b words`
- harbor: the "Apps" card on the new-tab page

## Develop

```
cargo test                                     # generator unit tests
cargo run -- gen --mode markov -n 20 --seed 7
printf 'bind = "127.0.0.1"\nport = 8099\n' > /tmp/dw.toml
cargo run -- serve --config /tmp/dw.toml       # open http://127.0.0.1:8099/
```

The word corpus lives at `assets/words.txt` (cleaned from `/usr/share/dict/words`:
lowercase ASCII a–z, length 2–24, deduped, sorted) and is committed for
reproducible builds.
