import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var carousel: Carousel?
    private var interceptor: ScrollInterceptor?
    private var statusItem: NSStatusItem?
    private var stateMenuItem: NSMenuItem?
    private var permissionTimer: Timer?
    /// Keeps the process out of App Nap: Finder-launched agents get their
    /// timers throttled to nothing, which freezes the spin animation.
    private var activity: NSObjectProtocol?

    func applicationDidFinishLaunching(_ notification: Notification) {
        activity = ProcessInfo.processInfo.beginActivity(
            options: [.userInitiatedAllowingIdleSystemSleep, .latencyCritical],
            reason: "Animating the window ring")
        setUpStatusItem() // visible even while ungranted, so the app never looks dead
        let trusted = Permissions.ensureAccessibility(prompt: true)
        FileHandle.standardError.write(Data(
            "Carousel: launch, accessibility trusted=\(trusted)\n".utf8))
        if trusted {
            boot()
        } else {
            StateLog.write("waiting for Accessibility grant")
            stateMenuItem?.title = "Carousel — grant Accessibility access in System Settings…"
            // Poll until the grant lands, then start without a relaunch.
            permissionTimer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] timer in
                guard Permissions.isTrusted else { return }
                timer.invalidate()
                self?.permissionTimer = nil
                self?.boot()
            }
        }
    }

    private func boot() {
        let listen = CGPreflightListenEventAccess()
        stateMenuItem?.title = "Carousel — hold ⌥ and scroll to spin the ring"

        let carousel = Carousel()
        carousel.start()
        self.carousel = carousel

        let interceptor = ScrollInterceptor(carousel: carousel)
        interceptor.start()
        self.interceptor = interceptor
        StateLog.write("running, accessibility granted, listenEvent=\(listen), tap=\(interceptor.isTapActive)")
    }

    func applicationWillTerminate(_ notification: Notification) {
        carousel?.restoreAll()
    }

    // MARK: Menu bar

    private func setUpStatusItem() {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.button?.title = "◎"

        let menu = NSMenu()
        let state = NSMenuItem(title: "Carousel", action: nil, keyEquivalent: "")
        menu.addItem(state)
        stateMenuItem = state

        let restore = NSMenuItem(title: "Restore Window Frames",
                                 action: #selector(restoreFrames), keyEquivalent: "r")
        restore.target = self
        menu.addItem(restore)
        menu.addItem(.separator())
        menu.addItem(NSMenuItem(title: "Quit", action: #selector(NSApplication.terminate(_:)),
                                keyEquivalent: "q"))
        item.menu = menu
        statusItem = item
    }

    @objc private func restoreFrames() {
        carousel?.restoreAll()
    }
}
