# blog

A small Rails blog, styled to [DW-001](../deepwater-style-guide.md) — cream
paper, warm ink, one Bavarian blue, no dividing lines anywhere.

It is the first service the fleet deploys that the **public internet can read**,
and most of what is unusual about it follows from that one fact.

```
   internet ──▶ nginx (VPS, :443, certbot)     Host pinned to blog.deepwa7er.com
                     │                                    │
                     └──────────▶ 127.0.0.1:8102 ◀────────┘
                                       ▲          one Rails process
   tailnet  ──▶ breakwater (VPS)  ─────┘          Host forwarded verbatim
                                                  blog.intern.deepwa7er.net
```

## Two front doors, one process

The public site is read-only. The admin — writing, editing, publishing — is
reachable only over the tailnet. Both are served by the same container on
`127.0.0.1:8102`, and **the hostname is what separates them**.

The `/admin` routes are drawn inside a host constraint (`config/routes.rb`), so
for a request arriving on the public hostname they do not exist at all: the
router 404s before any controller runs. That constraint reads `request.host`,
which a client controls — so it is only as strong as the proxy in front of it.
Three things make it hold, and they fail independently:

| Layer | Where | What it does |
|---|---|---|
| Host pinning | `deploy/nginx-blog.conf` | Sets `proxy_set_header Host` to a **literal**, so a forged `Host:` on the public edge is rewritten before the app sees it |
| Route constraint | `config/routes.rb` | Admin routes are not drawn for any other host |
| Controller check | `Admin::BaseController` | Re-asserts the same fact, so a routing refactor that drops the constraint cannot silently publish the admin |

`test/integration/host_boundary_test.rb` is the regression net. If one of those
tests fails, the admin is on the public internet — treat it that way.

There is deliberately **no password**. The tailnet is the security boundary,
the same trade every other fleet service makes. The difference worth
remembering is that this app is *also* publicly served, so that trade rests
entirely on the host separation above rather than on the network alone.

## Writing

Posts are **Action Text rich text, edited with [Lexxy](https://lexxy.dev)** —
the same editor the notes app uses — written at
`https://blog.intern.deepwa7er.net/admin`. Markdown shortcuts still work as you
type; they are the editor's input method, not the storage format.

Two consequences worth knowing:

- **The editor's 933 KB of JavaScript never reaches a reader.** Lexxy defines
  the editor element and nothing that touches rendered content, so the layout
  picks between two importmap entry points by hostname (`admin.js` /
  `application.js`) and both editor pins are `preload: false`. The public site
  gets `lexxy-content.css` only. `test/integration/admin_editor_test.rb` holds
  that split in place — it cannot be checked by hand in development, where both
  hostnames are `localhost` and every page looks like the admin.
- **Lexxy widens Action Text's sanitizer allowlist** (video/audio/table tags,
  plus a `style` attribute). That is a broader surface than the markdown
  pipeline it replaced, on a page served to the public. Acceptable because the
  only writer reaches the admin over the tailnet, but it is a real widening.

How posts behave:

- **Slugs** are generated from the title on create and then left alone —
  retitling must not break links that already exist in the wild.
- **Publishing is its own action**, not a checkbox on the edit form, so making
  something public is never a side effect of saving.
- **`published_at` in the future means scheduled**, not live. The `published`
  scope compares against the clock, so a date set from the console does not
  publish early.
- **Rendered HTML is sanitized** on every read by Action Text. The author is
  trusted; the row in the database is a separate question, and this page is
  served to strangers.

The feed is Atom, at `/feed`. Every URL and id in it is built from the public
origin, never the requesting host — see `ApplicationHelper#atom_tag`.

## Running it

```bash
bin/rails db:prepare && bin/rails db:seed
bin/rails server
```

In development both hostnames default to `localhost`, so the admin is reachable
at `/admin` — there is nothing to separate on a laptop. The test environment
uses two distinct hosts on purpose (`config/environments/test.rb`); a boundary
that collapses to one host in the environment that tests it cannot be tested.

```bash
bin/rails test      # 51 tests
bin/rubocop
bin/brakeman -q
```

## Deploying

Containerised, shipped by tugboat, same shape as `readout`:

```bash
tugboat deploy
```

First time on a new host, in order:

1. `scp config/master.key vps:/opt/blog/master.key`
2. `scp deploy/provision.sh vps:/tmp/ && ssh vps 'bash /tmp/provision.sh'`
3. `tugboat deploy`
4. Install `deploy/nginx-blog.conf` and issue the cert — see that file's header
5. Add `deploy/breakwater-route.toml` to `breakwater.toml`, then deploy breakwater

### Where things live on the VPS

| Path | What | Backed up |
|---|---|---|
| `/opt/blog/blog-image.tar` | Deploy artifact, replaced every deploy | no |
| `/opt/blog/master.key` | Credentials key, `0600` | no |
| `/opt/blog/storage/production.sqlite3` | **The posts** | **not yet** |

Standard layout, matching `readout`. `fleet-backup` only snapshots
`/var/lib/<service>/`, so nothing here is in the encrypted offsite backup set —
a known gap that `readout` shares and that is being solved for both together.
See [`deploy/fleet-backup.md`](deploy/fleet-backup.md), which also has the
one-command manual snapshot to use until then.

## Notes

- No breakwater route for the *public* name, by design. breakwater binds only
  the Tailscale IP and fronts `harness`; giving it a public listener is not a
  trade worth making for a blog. nginx already terminates public TLS on that box
  for `deepwa7er.com`, `music`, `netfps` and `discovery`.
- `libvips` is in the image because Action Text attachments are Active Storage
  attachments and their variants need it. Uploads land in the same
  `/opt/blog/storage` volume as the database, so they share its backup gap.
- kramdown is still in the Gemfile, but nothing renders markdown at runtime.
  Its only remaining caller is the migration that converted the pre-Lexxy
  bodies; it can go once that migration is squashed away.
- Memory: measured at ~160 MB resident, capped at 350 MB. The VPS has ~815 MB
  available and no swap.
