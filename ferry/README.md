# ferry

A local redirect server that turns your browser's address bar into a command
line. Type `b mail` and land in your mail app; type `b gh axum` and land on a
GitHub search. Unknown input falls through to a normal web search.

It works by registering ferry as a custom search engine in your browser: the
keyword `b` sends the rest of your input to `http://localhost:7777/?q=%s`, and
ferry answers with a `302` redirect.

## Setup

```sh
cargo install --path .
ferry
```

By default ferry reads `~/.config/ferry/ferry.toml`, creating it from the
bundled starter config (the repo's `ferry.toml`) on first run. Pass a path as
the first argument to use a different file — an explicit path is never
auto-created. Config edits take effect on the next request (no restart
needed); changing `port` requires a restart.

### Register the keyword in your browser

The search-engine URL is `http://localhost:7777/?q=%s` in all cases.

- **Chrome / Brave / Edge**: Settings → Search engine → Manage search engines
  → Add. Name `ferry`, shortcut `b`.
- **Firefox**: Settings → Search → Add (or bookmark
  `http://localhost:7777/?q=%s` and give the bookmark the keyword `b`).
- **Safari**: no native keyword support; use an extension such as
  xSearch/Keyword Search and point it at the same URL.

Then type <kbd>b</kbd> <kbd>space</kbd> `mail` in the address bar.

### Autocomplete suggestions

Ferry speaks the OpenSearch suggestions protocol: as you type, the browser
queries `/suggest` and offers matching command names in the dropdown.

- **Firefox**: visit `http://localhost:7777/commands` — the page advertises
  `/opensearch.xml`, so ferry appears under Settings → Search → Add (or via
  the address-bar page actions). Engines added this way get suggestions;
  assign the keyword `b` to it in the search settings.
- **Chrome / Brave / Edge**: manually added search engines have no
  suggestions-URL field, so the keyword works but the dropdown stays plain.
  Chromium may pick up the descriptor from `/commands` as an inactive
  shortcut; suggestion support there varies by version.

The endpoint itself is `GET /suggest?q=<partial>` returning
`["<input>", [completions], [descriptions]]`. Only the command word is
completed — once the input contains a space you're typing arguments and the
list goes quiet.

## Commands

Defined in the `[commands]` table of the config:

- A plain URL is a static shortcut: `b mail` → `https://mail.google.com`.
- A URL containing `{query}` is parameterized: `b gh axum` substitutes the
  percent-encoded argument → `https://github.com/search?q=axum`.
- A URL with positional placeholders `{1}`, `{2}`, … substitutes each
  whitespace-separated argument independently, missing ones becoming empty.
  E.g. `svc = "https://dash/services/{1}?action={2}"` turns `b svc nginx restart`
  into `https://dash/services/nginx?action=restart`, and `b svc nginx` into
  `https://dash/services/nginx?action=`.

Command URLs must be absolute (`scheme://…`); a relative value would be
resolved against ferry's own origin instead of sending the browser onward, so
it is rejected both at config load and when added through the UI.

Anything that doesn't match — an unknown command, or an argument given to a
static command — is sent to the `fallback` template as a search.

`b list` (or visiting `http://localhost:7777/commands`) shows every configured
command. A config entry named `list` shadows the built-in.

### Managing commands from the page

The `/commands` page lets you manage shortcuts without editing the file by hand.
All writes preserve your config's existing comments and formatting, are atomic
(a crash never truncates the file), and take effect on the next request.

- **Add**: enter a URL and one or more names. Space-separate the names to create
  several aliases for the same URL in one go — e.g. names `mail m gmail` all map
  to `https://mail.google.com`. Include `{query}` in the URL to make it
  parameterized. An existing name is overwritten.
- **Edit**: each row is editable in place. Change the name and/or URL and press
  *Save*. Renaming onto a *different* existing command is refused so you can't
  silently clobber another shortcut. (Renaming drops any TOML comment that was
  attached to that specific line; comments elsewhere are untouched.)
- **Delete**: the *Delete* button on a row removes it.

Note that anyone who can reach the page can change commands. When ferry is
shared over a tailnet (see below) that means every device on your tailnet —
the intended audience for a personal tool, but worth being aware of, since
there is no authentication.

## Run at login (macOS)

Save as `~/Library/LaunchAgents/com.ferry.plist`, adjusting the binary path
(`which ferry`), then run `launchctl load ~/Library/LaunchAgents/com.ferry.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.ferry</string>
  <key>ProgramArguments</key>
  <array>
    <string>/Users/YOU/.cargo/bin/ferry</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
</dict>
</plist>
```

## Share across your devices (Tailscale)

To use one set of shortcuts from every device on your tailnet — including your
phone — expose the loopback server through `tailscale serve`. Ferry stays bound
to localhost; Tailscale terminates TLS with a real cert for your machine's
MagicDNS name and only tailnet members can reach it.

On the machine running ferry:

```sh
tailscale serve --bg --https=443 http://127.0.0.1:7777
```

`tailscale serve status` prints the public-on-your-tailnet URL, e.g.
`https://<machine>.<tailnet>.ts.net/`. Use that origin everywhere you'd
otherwise use `http://localhost:7777`:

- search engine URL: `https://<machine>.<tailnet>.ts.net/?q=%s`
- suggestions URL: `https://<machine>.<tailnet>.ts.net/suggest?q=%s`

Ferry detects it's behind the proxy from the `X-Forwarded-Proto` header
Tailscale sets, so the `/opensearch.xml` descriptor advertises the `https`
tailnet URLs automatically — Firefox discovery still works when you visit
`https://<machine>.<tailnet>.ts.net/commands`.

Do **not** use `tailscale funnel`: that would publish ferry (and your shortcut
list) to the public internet. `serve` keeps it tailnet-only, which is what you
want.

Keep the loopback launchd agent above running too — `tailscale serve` only
proxies to ferry; it doesn't start it.

## Run on a Linux VPS (systemd + Tailscale)

For an always-on home — so the shortcuts work even when your laptop is asleep —
run ferry on a Linux box under systemd. The binary is statically linked, so it
has no runtime dependencies.

**1. Build a static Linux binary** (from a macOS or Linux dev machine):

```sh
rustup target add x86_64-unknown-linux-musl
# macOS also needs a musl linker: brew install FiloSottile/musl-cross/musl-cross
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc \
  cargo build --release --target x86_64-unknown-linux-musl
```

**2. Install on the VPS** as a dedicated, unprivileged user with its config in a
state directory the service can write (the add/edit/delete UI needs that):

```sh
scp target/x86_64-unknown-linux-musl/release/ferry vps:/tmp/ferry
ssh vps 'sudo install -m755 /tmp/ferry /usr/local/bin/ferry
  sudo useradd --system --no-create-home --shell /usr/sbin/nologin ferry
  sudo install -d -o ferry -g ferry -m750 /var/lib/ferry
  sudo -u ferry tee /var/lib/ferry/ferry.toml < /dev/null'   # then add config
```

The config path is passed explicitly, so it is never auto-created — seed
`/var/lib/ferry/ferry.toml` before first start (e.g. copy your existing one).

**3. systemd unit** at `/etc/systemd/system/ferry.service`:

```ini
[Unit]
Description=ferry address-bar shortcut server
After=network-online.target
Wants=network-online.target

[Service]
User=ferry
Group=ferry
ExecStart=/usr/local/bin/ferry /var/lib/ferry/ferry.toml
Restart=on-failure
NoNewPrivileges=yes
ProtectSystem=strict
ReadWritePaths=/var/lib/ferry
ProtectHome=yes
PrivateTmp=yes

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl enable --now ferry
```

**4. Expose over the tailnet.** If the box already serves something on 443 (a
reverse proxy, another app), give ferry its own HTTPS port so the two never
collide:

```sh
sudo tailscale serve --bg --https=8443 http://127.0.0.1:7777
```

ferry reads the forwarded `Host`/`X-Forwarded-Proto` from the proxy, so the
OpenSearch descriptor advertises the right `https://…:8443` URLs on its own.
Register `https://<machine>.<tailnet>.ts.net:8443/?q=%s` as the search engine
on each device.

To tear it all down: `sudo tailscale serve --https=8443 off` and
`sudo systemctl disable --now ferry`.

### Redeploying

Once the steps above are done, ship a new build with
[tugboat](https://github.com/deepwa7er/tugboat):

```sh
tugboat            # reads ./deploy.toml
```

It rebuilds the static musl binary, uploads it, swaps it in atomically,
restarts the service, health-checks it on loopback, and rolls the binary back
if the new one fails to come up — then re-asserts ferry's enrollment in
`lighthouse.target`. It leaves the config, systemd unit, and tailscale serve
mapping alone. The deploy is described by `deploy.toml`; the tailnet verify URL
lives in the untracked `deploy.local.toml`.
