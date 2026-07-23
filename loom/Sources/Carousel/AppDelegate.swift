import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private var carousel: Carousel?
    private var eventTap: EventTap?
    private var statusItem: NSStatusItem?
    private var stateMenuItem: NSMenuItem?
    private let displayMenu = NSMenu(title: "Display")
    private let windowsMenu = NSMenu(title: "Windows")
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

        let eventTap = EventTap(carousel: carousel)
        eventTap.start()
        self.eventTap = eventTap

        NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification,
            object: nil, queue: .main
        ) { [weak self] _ in
            self?.carousel?.displayConfigurationChanged()
        }
        StateLog.write("running, accessibility granted, listenEvent=\(listen), tap=\(eventTap.isTapActive)")
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

        let display = NSMenuItem(title: "Display", action: nil, keyEquivalent: "")
        displayMenu.delegate = self // rebuilt on every open, so new monitors show up
        display.submenu = displayMenu
        menu.addItem(display)

        let windows = NSMenuItem(title: "Windows", action: nil, keyEquivalent: "")
        windowsMenu.delegate = self // rebuilt on every open with live membership
        windows.submenu = windowsMenu
        menu.addItem(windows)
        menu.addItem(.separator())
        menu.addItem(NSMenuItem(title: "Quit", action: #selector(NSApplication.terminate(_:)),
                                keyEquivalent: "q"))
        item.menu = menu
        statusItem = item
    }

    @objc private func restoreFrames() {
        carousel?.restoreAll()
    }

    // MARK: Display picker

    func menuNeedsUpdate(_ menu: NSMenu) {
        if menu === windowsMenu { return rebuildWindowsMenu() }
        guard menu === displayMenu else { return }
        menu.removeAllItems()
        // Resolve through the fallback so the checkmark shows the display
        // actually in use, not a disconnected saved selection.
        let selection = carousel?.selectedUUID ?? Displays.savedSelection()
        let selected = Displays.screen(matching: selection).flatMap(Displays.uuid(of:))
        for screen in NSScreen.screens {
            guard let uuid = Displays.uuid(of: screen) else { continue }
            let item = NSMenuItem(title: screen.localizedName,
                                  action: #selector(selectDisplay(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = uuid
            item.state = uuid == selected ? .on : .off
            menu.addItem(item)
        }
    }

    @objc private func selectDisplay(_ sender: NSMenuItem) {
        guard let uuid = sender.representedObject as? String else { return }
        carousel?.select(displayUUID: uuid)
        StateLog.append("display -> \(sender.title)")
    }

    // MARK: Window list

    private func rebuildWindowsMenu() {
        windowsMenu.removeAllItems()
        guard let carousel else { return }
        for entry in carousel.windowList() {
            let label = entry.number.map { "⌘\($0)  \(entry.title)" } ?? "—  \(entry.title)"
            let item = NSMenuItem(title: label, action: nil, keyEquivalent: "")
            item.submenu = windowSubmenu(for: entry)
            windowsMenu.addItem(item)
        }
        windowsMenu.addItem(.separator())
        let hint = NSMenuItem(title: "⌥⌘1–9 gives the front window that number",
                              action: nil, keyEquivalent: "")
        hint.isEnabled = false
        windowsMenu.addItem(hint)
    }

    private func windowSubmenu(for entry: (number: Int?, title: String, id: CGWindowID)) -> NSMenu {
        let submenu = NSMenu(title: entry.title)
        let bringFront = NSMenuItem(title: "Bring to Front",
                                    action: #selector(switchToWindow(_:)), keyEquivalent: "")
        bringFront.target = self
        bringFront.representedObject = entry.id
        submenu.addItem(bringFront)
        submenu.addItem(.separator())
        for digit in 1...9 {
            let item = NSMenuItem(title: "⌘\(digit)",
                                  action: #selector(assignNumber(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = (entry.id, digit) as (CGWindowID, Int?)
            item.state = entry.number == digit ? .on : .off
            submenu.addItem(item)
        }
        submenu.addItem(.separator())
        let none = NSMenuItem(title: "No Number",
                              action: #selector(assignNumber(_:)), keyEquivalent: "")
        none.target = self
        none.representedObject = (entry.id, nil) as (CGWindowID, Int?)
        none.state = entry.number == nil ? .on : .off
        submenu.addItem(none)
        return submenu
    }

    @objc private func switchToWindow(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? CGWindowID else { return }
        carousel?.switchToWindow(id: id)
    }

    @objc private func assignNumber(_ sender: NSMenuItem) {
        guard let (id, number) = sender.representedObject as? (CGWindowID, Int?) else { return }
        carousel?.assignWindow(id: id, number: number)
    }
}
