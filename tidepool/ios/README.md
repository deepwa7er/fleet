# tidepool iOS

Native iOS client for tidepool. iOS 17+, SwiftUI for app + widget,
UIKit for extensions. Xcode project lives at
`ios/Tidepool/Tidepool.xcodeproj/`.

## Targets

| Target | Status | Folder |
| --- | --- | --- |
| `Tidepool` (main app) | shipped | `Tidepool/Tidepool/` |
| `TidepoolShare` (share extension) | pending Xcode setup, sources staged | `_target2_share_staging/` → `Tidepool/TidepoolShare/` |
| `TidepoolWidget` (widget) | not started | — |
| `TidepoolKeyboard` (keyboard) | not started | — |

## Build from the command line

```sh
cd ios/Tidepool
xcodebuild -scheme Tidepool -destination 'platform=iOS Simulator,name=iPhone 17' build
```

## Source layout

```
ios/
├── README.md                                     # this file
├── .gitignore                                    # Xcode artifacts
├── _target2_share_staging/                       # see "Adding target #2" below
└── Tidepool/
    ├── Tidepool.xcodeproj/
    └── Tidepool/                                 # main-app target
        ├── Assets.xcassets/
        ├── Tidepool.entitlements                 # App Group entitlement
        └── Sources/
            ├── TidepoolApp.swift                 # @main
            ├── ClipStore.swift                   # observable app state
            ├── Models/Clip.swift                 # /clip.json model
            ├── Networking/
            │   ├── JSON.swift                    # encoder + RFC3339 decoder
            │   └── TidepoolClient.swift          # URLSession-based client
            ├── Storage/
            │   ├── SharedStorage.swift           # App Group + Settings
            │   └── ClipCache.swift               # cache.json read/write
            └── Views/
                ├── ContentView.swift             # main screen
                └── SettingsView.swift            # server URL editor
```

`Sources/` is grouped by concern, not by target. Extension targets reuse
the same files via multi-target membership — they don't get their own
copies.

## Adding target #2 — Share Extension

The share-extension sources are pre-written and staged under
`ios/_target2_share_staging/`. Follow these steps in Xcode to create the
target and wire them in.

### 1. Create the target

Xcode → File → New → **Target** → iOS tab → **Share Extension**.

| Field | Value |
| --- | --- |
| Product Name | `TidepoolShare` |
| Team | (your Apple ID team) |
| Bundle Identifier | `com.deepwa7er.tidepool.share` |
| Language | **Swift** |
| Embed in Application | **Tidepool** |

Xcode creates `ios/Tidepool/TidepoolShare/` with template files
(`ShareViewController.swift`, `MainInterface.storyboard`, `Info.plist`,
`TidepoolShare.entitlements`).

### 2. Set minimum deployment

`TidepoolShare` target → General → Minimum Deployments → iOS **17.0**.

### 3. Drop Xcode's templates

In the project navigator, **move to trash** all four template files
under the `TidepoolShare` group:

- `ShareViewController.swift`
- `MainInterface.storyboard`
- `Info.plist`
- `TidepoolShare.entitlements`

### 4. Move the staged files into the target folder

In Finder, move all three files from `ios/_target2_share_staging/` into
`ios/Tidepool/TidepoolShare/`, then delete the now-empty staging folder.

```sh
mv ios/_target2_share_staging/* ios/Tidepool/TidepoolShare/
rmdir ios/_target2_share_staging
```

### 5. Add the files to the target

Right-click the `TidepoolShare` group in Xcode → **Add Files to
"Tidepool"…** → select:

- `ShareViewController.swift`
- `Info.plist`
- `TidepoolShare.entitlements`

In the dialog:

- **Copy items if needed:** unchecked (files stay at their repo path).
- **Added folders:** Create groups.
- **Add to targets:** **TidepoolShare** ✓ (only — NOT the main app).

### 6. Wire up Info.plist and entitlements references

`TidepoolShare` target → Build Settings:

- Search "Info.plist File" → set to `TidepoolShare/Info.plist`.
- Search "Code Signing Entitlements" → set to
  `TidepoolShare/TidepoolShare.entitlements`.

### 7. Add the App Group capability

`TidepoolShare` target → Signing & Capabilities → **+ Capability** →
**App Groups** → tick `group.com.deepwa7er.tidepool` (the same group the
main app uses).

### 8. Multi-target membership on shared sources

The share extension reuses model, networking, and storage code from the
main app. Select each of these files in the project navigator, then in
the File Inspector (right pane) under **Target Membership**, tick
**TidepoolShare** in addition to the existing **Tidepool**:

- `Sources/Models/Clip.swift`
- `Sources/Networking/JSON.swift`
- `Sources/Networking/TidepoolClient.swift`
- `Sources/Storage/SharedStorage.swift`
- `Sources/Storage/ClipCache.swift`

Do **not** add membership for `TidepoolApp.swift`, `ClipStore.swift`, or
the `Views/` files — those import SwiftUI/UIKit in app-specific ways and
shouldn't be linked into the extension.

### 9. Build

```sh
xcodebuild -scheme Tidepool -destination 'platform=iOS Simulator,name=iPhone 17' build
```

Both targets should compile. The share extension links into the main
app bundle at `Tidepool.app/PlugIns/TidepoolShare.appex/`.

### 10. Test the share extension

Run the main app on a simulator or device, then leave it. From Safari
(or Notes, or any text-selection context), invoke the iOS share sheet.
"Tidepool" should appear as a share destination. Tap it → compose sheet
appears pre-populated with the URL or selected text → tap **Post** →
extension dismisses → the next time you open the main app, the shared
text is shown as the current clip.

If the POST fails (Tailscale off, server down), an alert offers Cancel
or Retry; cancelling dismisses the extension with the error.

## Running the main app

Open `Tidepool.xcodeproj` and `Cmd+R` to a simulator or device. Tap the
gear icon to change the server URL.

For physical devices, register the App Group and bundle IDs in the
Apple Developer portal (or let Xcode do it during automatic signing),
trust the dev profile under Settings → General → VPN & Device
Management, and make sure Tailscale iOS is connected.

## Roadmap

See [`../IOS_APP_PLAN.md`](../IOS_APP_PLAN.md) for the full plan. After
the share extension lands, next up is the WidgetKit target.
