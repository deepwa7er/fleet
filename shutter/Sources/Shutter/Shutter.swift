import AppKit

/// The entry point is an `@main` type rather than a `main.swift`, because
/// top-level code is not main-actor isolated and every AppKit call here is.
@main
enum Shutter {
    @MainActor
    static func main() {
        let app = NSApplication.shared
        // `NSApplication.delegate` is an unowned reference; this local holds the
        // only strong one, and outlives `run()`.
        let delegate = AppDelegate()
        app.delegate = delegate
        // Accessory: a status-bar item and no Dock tile.
        app.setActivationPolicy(.accessory)
        app.run()
    }
}
