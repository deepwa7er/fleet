# tidepool iOS

Native iOS client for tidepool. Target #1 (this commit) is the main-app
skeleton: SwiftUI, iOS 17+, `URLSession` against `/clip.json`, App Group
container for cache state that future extensions (share, widget,
keyboard) will share.

Xcode project lives at `ios/Tidepool/Tidepool.xcodeproj/`. Swift sources
are under `ios/Tidepool/Tidepool/Sources/`. Every subsequent target
(share extension, widget, keyboard) will be added to this same project.

**Building from the command line:**

```sh
cd ios/Tidepool
xcodebuild -scheme Tidepool -destination 'platform=iOS Simulator,name=iPhone 17' build
```

## One-time Xcode setup

### 1. Create the Xcode project

Xcode → File → New → Project → **iOS** → **App**.

| Field | Value |
| --- | --- |
| Product Name | `Tidepool` |
| Team | (your Apple ID team) |
| Organization Identifier | `com.deepwa7er` |
| Bundle Identifier | (auto: `com.deepwa7er.Tidepool` — see note) |
| Interface | **SwiftUI** |
| Language | **Swift** |
| Storage | **None** |
| Include Tests | up to you |

Save into `tidepool/ios/`. Xcode will create `Tidepool.xcodeproj` and a
`Tidepool/` folder inside `ios/` with template files.

**Bundle ID note.** Xcode title-cases the product name into the bundle
ID by default (`com.deepwa7er.Tidepool`). Change it to lowercase
`com.deepwa7er.tidepool` in the target's General → Identity to match
the App Group naming convention. The product (display) name stays
"Tidepool".

### 2. Set deployment target to iOS 17.0

Target → General → Minimum Deployments → iOS **17.0**.

### 3. Add the App Group capability

Target → Signing & Capabilities → **+ Capability** → **App Groups** →
click **+** under the App Groups list and add:

```
group.com.deepwa7er.tidepool
```

Make sure the checkbox next to it is ticked. Xcode writes this into
`Tidepool.entitlements` for you. Without this step the app launches but
the cache file and shared settings will surface a clear error.

You'll need to register this group in the Apple Developer portal (or
let Xcode do it automatically when signing) the first time.

### 4. Remove Xcode's template files

Inside the `Tidepool/` group in the project navigator, **move to trash**:

- `TidepoolApp.swift` (Xcode's template — we'll replace it with ours)
- `ContentView.swift` (same)

Keep `Assets.xcassets`, `Preview Content/`, and `Tidepool.entitlements`.

### 5. Add the pre-written source files to the target

In Finder, drag the **`Sources` folder** (the whole folder from
`ios/Sources/`) into the `Tidepool` group in the Xcode project
navigator. In the dialog:

- Destination: **leave "Copy items if needed" unchecked** — the sources
  stay at their on-disk path in the repo.
- Added folders: **Create groups** (the default).
- Add to targets: **Tidepool** ✓

Build (`Cmd+B`). Should compile cleanly.

### 6. Run

Select an iOS 17+ simulator (e.g. iPhone 15) or your physical device,
then `Cmd+R`. On first launch:

- The app shows "No clip yet" until you tap **Pull to refresh** or push
  a clip from another device.
- Tap the gear icon to change the server URL if your tailnet hostname
  differs from `https://tidepool.tailcfab97.ts.net`.

For physical device installs, you'll need a free Apple ID developer
account and to trust the developer profile under Settings → General →
VPN & Device Management on the phone. Tailscale iOS must be running
and connected for the app to reach the server.

## Source layout

```
ios/
├── README.md                              # this file
├── .gitignore                             # Xcode artifacts
└── Tidepool/
    ├── Tidepool.xcodeproj/                # project file
    └── Tidepool/                          # main-app target folder
        ├── Assets.xcassets
        ├── Tidepool.entitlements          # contains App Group entitlement
        └── Sources/
            ├── TidepoolApp.swift          # @main, scene root
            ├── ClipStore.swift            # observable app state
            ├── Models/
            │   └── Clip.swift             # /clip.json decode model
            ├── Networking/
            │   ├── JSON.swift             # shared encoder/decoder, RFC3339 handling
            │   └── TidepoolClient.swift   # URLSession-based client
            ├── Storage/
            │   ├── SharedStorage.swift    # App Group container + Settings
            │   └── ClipCache.swift        # cache.json read/write
            └── Views/
                ├── ContentView.swift      # main screen
                └── SettingsView.swift     # server URL editor
```

`Sources/` is grouped by concern, not by target. When we add the share
extension, widget, and keyboard later, each will live as a sibling
target folder (e.g. `Tidepool/ShareExtension/`) and reuse
`Sources/Models/`, `Sources/Networking/`, and `Sources/Storage/` via
multi-target membership on the relevant files.

## Roadmap

See [`../IOS_APP_PLAN.md`](../IOS_APP_PLAN.md) for the full plan. After
target #1 lands, next up is the Share Extension.
