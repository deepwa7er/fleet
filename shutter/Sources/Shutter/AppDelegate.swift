import AppKit

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private var hotkeys: HotkeyManager?
    private var capture: CaptureController?
    private var editors: [EditorWindowController] = []
    private var statusItem: NSStatusItem?
    private var stateItem: NSMenuItem?
    private var loginItem: NSMenuItem?
    private var takeoverItem: NSMenuItem?
    private var saveLocationItem: NSMenuItem?
    private var permissionTimer: Timer?
    /// True once the relaunch prompt has been shown, so the poll below does not
    /// stack alerts every second.
    private var askedToRelaunch = false

    func applicationDidFinishLaunching(_ notification: Notification) {
        setUpStatusItem() // visible even while ungranted, so the app never looks dead

        guard Permissions.hasScreenRecording else {
            Permissions.requestScreenRecording()
            waitForScreenRecording()
            return
        }
        boot()
    }

    private func boot() {
        capture = CaptureController(
            onCapture: { [weak self] capture in self?.openEditor(for: capture) },
            onFailure: { [weak self] error in self?.report(error) })

        // Displace the system's own screenshot shortcuts before registering
        // ours: while the window server still claims ⌘⇧3/4/5, a Carbon
        // registration for them succeeds and then never fires.
        SystemScreenshotShortcuts.takeOver(CaptureShortcut.all.map(\.symbolicHotKey))

        let manager = HotkeyManager { [weak self] shortcut in
            // Carbon delivers on the main run loop, but the compiler cannot see
            // that from a C callback.
            Task { @MainActor in self?.trigger(shortcut) }
        }
        manager.register(CaptureShortcut.all)
        hotkeys = manager

        updateStateItem()
        Log.info("running")
    }

    private func updateStateItem() {
        guard let hotkeys else { return }
        if !SystemScreenshotShortcuts.isAvailable {
            stateItem?.title = "Shutter — turn off Screenshots shortcuts in System Settings"
        } else if let taken = hotkeys.failed.first {
            stateItem?.title = "Shutter — \(taken.label) is claimed by another app"
        } else if SystemScreenshotShortcuts.isTakenOver {
            stateItem?.title = "Shutter — \(CaptureShortcut.region.label) to capture"
        } else {
            stateItem?.title = "Shutter — the system owns ⌘⇧3/4/5"
        }
    }

    private func trigger(_ shortcut: CaptureShortcut) {
        switch shortcut.action {
        case .selection: capture?.arm()
        case .display: capture?.captureDisplayUnderCursor()
        }
    }

    /// Hand ⌘⇧3/4/5 back. This does not run on a crash, which is why
    /// `takeOver` also repairs an abandoned takeover at launch.
    func applicationWillTerminate(_ notification: Notification) {
        SystemScreenshotShortcuts.restore()
    }

    /// The Screen Recording grant only takes effect for a newly launched
    /// process, so noticing it is not enough — the app has to restart to be
    /// able to capture anything.
    private func waitForScreenRecording() {
        stateItem?.title = "Shutter — grant Screen Recording in System Settings…"
        Log.info("waiting for the Screen Recording grant")
        permissionTimer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] timer in
            guard Permissions.hasScreenRecording else { return }
            timer.invalidate()
            Task { @MainActor in
                self?.permissionTimer = nil
                self?.offerRelaunch()
            }
        }
    }

    @MainActor
    private func offerRelaunch() {
        guard !askedToRelaunch else { return }
        askedToRelaunch = true
        stateItem?.title = "Shutter — relaunch to finish granting access"

        NSApp.activate(ignoringOtherApps: true)
        let alert = NSAlert()
        alert.messageText = "Shutter needs to relaunch"
        alert.informativeText = "Screen Recording access was granted. macOS only applies it to a "
            + "newly launched process, so Shutter has to restart before it can capture anything."
        alert.addButton(withTitle: "Relaunch")
        alert.addButton(withTitle: "Later")
        guard alert.runModal() == .alertFirstButtonReturn else { return }

        let configuration = NSWorkspace.OpenConfiguration()
        configuration.createsNewApplicationInstance = true
        NSWorkspace.shared.openApplication(at: Bundle.main.bundleURL,
                                           configuration: configuration) { _, error in
            if let error {
                Log.error("relaunch failed: \(error)")
                return
            }
            Task { @MainActor in NSApp.terminate(nil) }
        }
    }

    // MARK: Captures

    private func openEditor(for capture: CapturedImage) {
        let controller = EditorWindowController(capture: capture) { [weak self] finished in
            self?.editors.removeAll { $0 === finished }
        }
        editors.append(controller)
        controller.present()
    }

    private func report(_ error: Error) {
        Log.error("\(error)")
        NSApp.activate(ignoringOtherApps: true)
        NSAlert(error: error).runModal()
    }

    // MARK: Menu bar

    private func setUpStatusItem() {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.button?.image = NSImage(systemSymbolName: "camera.viewfinder",
                                     accessibilityDescription: "Shutter")

        let menu = NSMenu()
        menu.delegate = self // the login-item state can change in System Settings

        let state = NSMenuItem(title: "Shutter", action: nil, keyEquivalent: "")
        state.isEnabled = false
        menu.addItem(state)
        stateItem = state

        menu.addItem(.separator())
        menu.addItem(NSMenuItem(title: "Capture…", action: #selector(startCapture),
                                keyEquivalent: ""))

        let saveLocation = NSMenuItem(title: "Save Location…",
                                      action: #selector(chooseSaveLocation), keyEquivalent: "")
        menu.addItem(saveLocation)
        saveLocationItem = saveLocation
        menu.addItem(NSMenuItem(title: "Open Save Folder",
                                action: #selector(openSaveFolder), keyEquivalent: ""))

        menu.addItem(.separator())
        let takeover = NSMenuItem(title: "Use Shutter for ⌘⇧3/4/5",
                                  action: #selector(toggleTakeover), keyEquivalent: "")
        menu.addItem(takeover)
        takeoverItem = takeover

        let login = NSMenuItem(title: "Open at Login",
                               action: #selector(toggleLoginItem), keyEquivalent: "")
        menu.addItem(login)
        loginItem = login

        menu.addItem(.separator())
        let quit = NSMenuItem(title: "Quit Shutter", action: #selector(NSApplication.terminate(_:)),
                              keyEquivalent: "q")
        quit.target = NSApp
        menu.addItem(quit)

        // Without an explicit target, menu validation walks a responder chain
        // that an accessory app's status menu does not have, and every item
        // draws greyed out.
        for menuItem in menu.items where menuItem.action != nil && menuItem.target == nil {
            menuItem.target = self
        }
        item.menu = menu
        statusItem = item
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        loginItem?.state = LoginItem.isEnabled ? .on : .off
        takeoverItem?.state = SystemScreenshotShortcuts.isTakenOver ? .on : .off
        takeoverItem?.isEnabled = SystemScreenshotShortcuts.isAvailable
        saveLocationItem?.title = "Save Location: \(Destinations.saveDirectory.lastPathComponent)"
    }

    @objc private func startCapture() {
        capture?.arm()
    }

    @objc private func toggleTakeover() {
        if SystemScreenshotShortcuts.isTakenOver {
            SystemScreenshotShortcuts.restore()
        } else {
            SystemScreenshotShortcuts.takeOver(CaptureShortcut.all.map(\.symbolicHotKey))
        }
        updateStateItem()
    }

    @objc private func chooseSaveLocation() {
        NSApp.activate(ignoringOtherApps: true)
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.canCreateDirectories = true
        panel.directoryURL = Destinations.saveDirectory
        panel.prompt = "Use Folder"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Destinations.saveDirectory = url
    }

    @objc private func openSaveFolder() {
        NSWorkspace.shared.open(Destinations.saveDirectory)
    }

    @objc private func toggleLoginItem() {
        do {
            try LoginItem.setEnabled(!LoginItem.isEnabled)
        } catch {
            Log.error("could not change the login item: \(error)")
            LoginItem.openSystemSettings()
        }
    }
}
