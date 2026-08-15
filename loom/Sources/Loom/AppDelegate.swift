import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private var stack: WindowStack?
    private var eventTap: EventTap?
    private var switcher: SwitcherPanel?
    private var command: CommandPanel?
    private var statusItem: NSStatusItem?
    private var stateMenuItem: NSMenuItem?
    private var loginItemMenuItem: NSMenuItem?
    private let displayMenu = NSMenu(title: "Display")
    private let windowsMenu = NSMenu(title: "Windows")
    private var permissionTimer: Timer?
    /// Keeps the process out of App Nap: a Finder-launched background app gets
    /// its timers throttled to nothing, which would stall the reconcile pass
    /// and leave the stack's membership stale for minutes at a time.
    private var activity: NSObjectProtocol?

    func applicationDidFinishLaunching(_ notification: Notification) {
        MigrationLog.startSession()
        activity = ProcessInfo.processInfo.beginActivity(
            options: .userInitiatedAllowingIdleSystemSleep,
            reason: "Tracking window membership")
        setUpStatusItem() // visible even while ungranted, so the app never looks dead
        let trusted = Permissions.ensureAccessibility(prompt: true)
        FileHandle.standardError.write(Data(
            "Loom: launch, accessibility trusted=\(trusted)\n".utf8))
        if trusted {
            boot()
        } else {
            StateLog.write("waiting for Accessibility grant")
            stateMenuItem?.title = "Loom — grant Accessibility access in System Settings…"
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
        stateMenuItem?.title = "Loom — ⌘1–9 switches windows, 🎤 opens the panel"

        let stack = WindowStack()
        stack.start()
        self.stack = stack

        let switcher = SwitcherPanel(stack: stack)
        self.switcher = switcher

        let command = CommandPanel(stack: stack)
        self.command = command

        let eventTap = EventTap(stack: stack, switcher: switcher, command: command)
        eventTap.start()
        self.eventTap = eventTap

        NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification,
            object: nil, queue: .main
        ) { [weak self] _ in
            self?.stack?.displayConfigurationChanged()
        }
        StateLog.write("running, accessibility granted, listenEvent=\(listen), tap=\(eventTap.isTapActive)")
    }

    func applicationWillTerminate(_ notification: Notification) {
        stack?.restoreAll()
    }

    // MARK: Menu bar

    private func setUpStatusItem() {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.button?.title = "◎"

        let menu = NSMenu()
        menu.delegate = self // the login-item state can change in System Settings
        let state = NSMenuItem(title: "Loom", action: nil, keyEquivalent: "")
        menu.addItem(state)
        stateMenuItem = state

        let switcher = NSMenuItem(title: "Window Switcher  ⌥Space",
                                  action: #selector(showSwitcher), keyEquivalent: "")
        switcher.target = self
        menu.addItem(switcher)

        let panel = NSMenuItem(title: "Command Panel  🎤",
                               action: #selector(showCommandPanel), keyEquivalent: "")
        panel.target = self
        menu.addItem(panel)

        let display = NSMenuItem(title: "Display", action: nil, keyEquivalent: "")
        displayMenu.delegate = self // rebuilt on every open, so new monitors show up
        display.submenu = displayMenu
        menu.addItem(display)

        let windows = NSMenuItem(title: "Windows", action: nil, keyEquivalent: "")
        windowsMenu.delegate = self // rebuilt on every open with live membership
        windows.submenu = windowsMenu
        menu.addItem(windows)
        menu.addItem(.separator())

        let login = NSMenuItem(title: "Start at Login",
                               action: #selector(toggleStartAtLogin), keyEquivalent: "")
        login.target = self
        menu.addItem(login)
        loginItemMenuItem = login

        menu.addItem(.separator())
        menu.addItem(NSMenuItem(title: "Quit", action: #selector(NSApplication.terminate(_:)),
                                keyEquivalent: "q"))
        item.menu = menu
        statusItem = item
    }

    @objc private func showSwitcher() {
        switcher?.show()
    }

    @objc private func showCommandPanel() {
        command?.show()
    }

    // MARK: Menu updates

    func menuNeedsUpdate(_ menu: NSMenu) {
        if menu === windowsMenu { return rebuildWindowsMenu() }
        if menu === displayMenu { return rebuildDisplayMenu() }
        if menu === statusItem?.menu { return refreshLoginItem() }
    }

    // MARK: Start at login

    private func refreshLoginItem() {
        guard let item = loginItemMenuItem else { return }
        // `.requiresApproval` means the registration exists but the user
        // switched it off; say so rather than showing a checkbox that won't move.
        let needsApproval = LoginItem.status == .requiresApproval
        item.title = needsApproval ? "Start at Login — approve in System Settings…"
                                   : "Start at Login"
        item.state = LoginItem.isEnabled ? .on : .off
    }

    @objc private func toggleStartAtLogin() {
        guard LoginItem.status != .requiresApproval else {
            LoginItem.openSystemSettings() // only the user can clear that state
            return
        }
        let enabling = !LoginItem.isEnabled
        do {
            try LoginItem.setEnabled(enabling)
            StateLog.append("start at login -> \(enabling)")
        } catch {
            StateLog.append("start at login \(enabling ? "register" : "unregister") failed: \(error)")
            reportLoginItemFailure(error, enabling: enabling)
        }
    }

    private func reportLoginItemFailure(_ error: Error, enabling: Bool) {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = enabling
            ? "Loom couldn’t add itself to your login items."
            : "Loom couldn’t remove itself from your login items."
        alert.informativeText = error.localizedDescription
        alert.addButton(withTitle: "OK")
        alert.addButton(withTitle: "Open Login Items…")
        NSApp.activate(ignoringOtherApps: true) // an accessory app has no window to own the sheet
        if alert.runModal() == .alertSecondButtonReturn { LoginItem.openSystemSettings() }
    }

    // MARK: Display picker

    private func rebuildDisplayMenu() {
        let menu = displayMenu
        menu.removeAllItems()
        // Resolve through the fallback so the checkmark shows the display
        // actually in use, not a disconnected saved selection.
        let selection = stack?.selectedUUID ?? Displays.savedSelection()
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
        stack?.select(displayUUID: uuid)
        StateLog.append("display -> \(sender.title)")
    }

    // MARK: Window list

    private func rebuildWindowsMenu() {
        windowsMenu.removeAllItems()
        guard let stack else { return }
        for entry in stack.windowList() {
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

    private func windowSubmenu(for entry: WindowStack.WindowEntry) -> NSMenu {
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
        stack?.switchToWindow(id: id)
    }

    @objc private func assignNumber(_ sender: NSMenuItem) {
        guard let (id, number) = sender.representedObject as? (CGWindowID, Int?) else { return }
        stack?.assignWindow(id: id, number: number)
    }
}
