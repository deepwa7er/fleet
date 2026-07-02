# tide

Fleet-wide settings for the `deepwa7er` VPS fleet. Today it holds one thing: the
shared **dark/light theme** every fleet web UI honors — one global setting, the
single source of truth, flipped from anywhere.

## How it works

```
b dark  ──ferry 302──►  tide /set?theme=dark
                          ├─ persist /var/lib/tide/settings.json   (source of truth)
                          ├─ Set-Cookie fleet_theme=dark; Domain=.intern.deepwa7er.net
                          └─ confirmation page
each fleet UI:  read the cookie for instant first-paint, then fetch /theme and
                poll it (~5s) so open tabs flip live and every device agrees.
```

- `GET /theme` → `{"theme":"dark"|"light"}`. CORS-open for GET, so each UI (on its
  own subdomain) reads it cross-origin.
- `GET /set?theme=dark|light` → persists, sets the shared-domain cookie, shows a
  confirmation page. [ferry](https://github.com/deepwa7er/ferry)'s `b dark` /
  `b light` redirect here.
- `GET /healthz` → liveness.

`bind` is loopback; [breakwater](https://github.com/deepwa7er/breakwater) is the
tailnet front door at `https://tide.intern.deepwa7er.net` (the tailnet is the
security boundary — no auth). The cookie is only a per-browser first-paint cache;
`settings.json` on the VPS is authoritative across devices.

## Deploy

```sh
deploy/provision.sh     # once: service user, config, systemd unit
tugboat deploy          # build (musl) + ship the binary
```

Add the breakwater route (`tide.intern.deepwa7er.net → 127.0.0.1:8094`) and the
ferry `dark`/`light` commands; see the fleet docs.
