# iOS app plan

Handoff doc for picking up tidepool iOS work in a fresh Claude Code session
(e.g. when switching to a Mac for Xcode). Self-contained — covers project
state, decisions already made, the iOS scope, and the first concrete step.

## TL;DR

tidepool is deployed and shuttling files + clipboard between a Fedora
laptop and a MacBook over Tailscale. iPhone has a PWA for files and iOS
Shortcuts hitting `/clip.json` and `/clip` for clipboard. Next: a native
iOS app to close the iPhone gap — Share Extension, Custom Keyboard,
Widget. APNs push later.

## Where we are right now

- **Server** (`main.go` + `internal/server/`, `internal/store/`): single
  Go binary, embeds templates + static. Joins tailnet via `tsnet`. SQLite
  on local disk. Routes: `GET /`, `GET /clip`, `GET /clip.json`,
  `GET /clip/stream` (SSE), `POST /clip`, `POST/GET/DELETE /files`.
- **Deployment**: DigitalOcean droplet (Ubuntu 24.04). Binary at
  `/usr/local/bin/tidepool`, unit at `/etc/systemd/system/tidepool.service`,
  `TS_AUTHKEY` in `/etc/tidepool.env` (mode 600), data at `/opt/tidepool/`.
  Reachable at `https://tidepool.tailcfab97.ts.net` from any tailnet
  device.
- **Desktop daemon** (`cmd/tidepool-clipd/`): pure-Go binary. On Fedora
  via `systemctl --user` user unit; on macOS via LaunchAgent at
  `~/Library/LaunchAgents/com.deepwa7er.tidepool-clipd.plist`. Both
  watch local clipboard, POST to `/clip`, subscribe to `/clip/stream`,
  write incoming events back. SHA-256 dedup breaks the echo loop.
- **iPhone today**: PWA for files (added to Home Screen via Safari), and
  two iOS Shortcuts ("Push to tidepool" and "Pull from tidepool") wired
  to Back Tap. Functional but manual.
- **Repo**: `git@github.com:deepwa7er/tidepool.git`, branch `main`.

## What we're building

A native iOS app that brings iPhone to rough parity with the desktop
daemons, despite Apple's hard rule that no app can read/write the
clipboard in the background.

### Targets, in order of value

1. **Main app + HTTP client** — bare SwiftUI scaffold. Settings screen
   for tidepool URL (default `https://tidepool.tailcfab97.ts.net`).
   Fetches and shows current `/clip.json`. Local cache of last N clips
   in the App Group container.
2. **Share Extension** — registers tidepool in iOS share sheet for text
   and URL content types. POSTs payload to `/clip`. This alone makes
   "push from anywhere" a 2-tap operation from Safari, Notes, Mail, etc.
3. **Widget (WidgetKit)** — Lock Screen + Home Screen widgets showing
   current clip preview. Timeline refresh every ~15 min (Apple-enforced
   minimum). Tap opens app on a deep link that auto-copies (one tap to
   paste once you're back in your previous app).
4. **Custom Keyboard Extension** — shows recent clips as tappable rows.
   Tap inserts at cursor in whatever text field you're in. Requires
   "Allow Full Access" because we need to make HTTP requests; surface
   this in onboarding.
5. **APNs push** (server + iOS) — server registers device tokens via a
   new endpoint, sends a silent push on `/clip` POST. iOS app wakes
   briefly to refresh local cache so widget and keyboard always show
   fresh data. Optionally a user-visible notification with a "Copy"
   action button.

Each step ships value on its own.

## Decisions already made

- **Min iOS target**: **iOS 17.0**. Universal install base by now, smaller
  swift-availability noise, lets us use Live Activities and the modern
  Widget APIs without `if #available` everywhere. iOS 16.4 is only
  needed if we add Web Push to the PWA later — not relevant for the
  native app.
- **UI**: **SwiftUI** for the main app and Widget. **UIKit** for the
  Share Extension (`SLComposeServiceViewController`) and Keyboard
  (`UIInputViewController`) since those base classes are UIKit-rooted.
- **Networking**: Standard library `URLSession`. No third-party HTTP
  client. SSE consumed via `URLSession.bytes(for:)` line-by-line, same
  shape as the Go daemon's parser.
- **Persistence**: App Group `group.com.deepwa7er.tidepool` holding a
  shared `cache.json` (latest clip + last 50 clips). Each target reads
  the same file. Writes are debounced and atomic (write-tmp + rename).
- **Bundle IDs**: `com.deepwa7er.tidepool` (main), `.share`, `.keyboard`,
  `.widget` (extensions).
- **Auth**: none, tailnet is the perimeter. Tailscale iOS app must be
  running with VPN engaged for the app to reach the server. Surface a
  clear "Can't reach tidepool — is Tailscale connected?" error.
- **Codebase location**: keep iOS in the **same repo** under `ios/`.
  Mixed Go + Swift in one repo is fine; .gitignore the Xcode build
  artifacts (`DerivedData/`, `xcuserdata/`, `*.xcuserstate`).
- **No CI/notarization for v1**: install via dev provisioning to your
  own devices only. Worry about TestFlight/App Store later if ever.

## Open questions to settle on Mac

- Bundle ID prefix `com.deepwa7er.*` vs `me.joemafrici.*` — pick one
  before creating App IDs in the Apple Developer portal.
- App icon and name in springboard — placeholder ("Tidepool" + a wave
  glyph) or invest time in real branding?
- For the Keyboard Extension: include a "recent clips" history view, or
  start with just a "current clip → paste" single button? History is
  more useful but more UI to build.
- Should the Share Extension *also* set the iOS clipboard, or only POST?
  (Setting the clipboard means the desktop daemons echo it back via
  SSE, harmless. Pro: matches user intuition. Con: triggers the
  iOS 14+ "App pasted from X" banner on next paste.)

## API reference (server)

| Endpoint | Method | Body | Returns |
|---|---|---|---|
| `/clip.json` | GET | — | `{"text","updated_at","updated_by"}` |
| `/clip` | POST | form `text=<value>` | HTML (ignore) + broadcasts to `/clip/stream` |
| `/clip/stream` | GET | — | `text/event-stream`; events named `clip` with the same JSON payload as `/clip.json` |
| `/files` | POST | multipart `file` | HTML row of the new file |
| `/files/{id}` | GET | — | blob with original `Content-Type` |
| `/files/{id}` | DELETE | — | 200 OK |

Device attribution (`updated_by`) is set server-side via `tsnet.WhoIs`
on the caller's tailnet IP — the iOS app does not and should not send
its own device name.

## Operational facts

- **Tailnet**: `tailcfab97.ts.net`
- **Server URL**: `https://tidepool.tailcfab97.ts.net`
- **Droplet**: `tidepool-do` in `~/.ssh/config` → root@147.182.250.13
  (Ubuntu 24.04). Other services live there too (Rails apps,
  navidrome, slskd); ufw allows only 22/80/443 publicly.
- **Server logs**: `ssh tidepool-do 'journalctl -u tidepool -f'`
- **Server update**: cross-compile linux/amd64, `scp` to
  `/usr/local/bin/tidepool.new`, `mv` + `systemctl restart tidepool`
  on the droplet (can't overwrite a running ELF, hence the temp file).
- **Desktop daemon update**: rebuild, replace `~/.local/bin/tidepool-clipd`
  on each device, restart via `systemctl --user restart tidepool-clipd`
  (Fedora) or `launchctl kickstart -k gui/$(id -u)/com.deepwa7er.tidepool-clipd`
  (Mac).

## First step on Mac

```sh
git pull
cd ios/    # create this directory if it doesn't exist yet
```

Then in Claude Code: "Let's start the iOS app. Read IOS_APP_PLAN.md.
Create the Xcode project for target #1 (main app skeleton with
HTTP client and current-clip view), bundle ID `com.deepwa7er.tidepool`,
SwiftUI, iOS 17 minimum. App Group `group.com.deepwa7er.tidepool`."

I'll write the Swift; you drive Xcode (create the project, add the App
Group capability, run on simulator or your device). For each subsequent
target (Share Extension, Widget, Keyboard) we'll add a new Xcode target
together when we get there.
