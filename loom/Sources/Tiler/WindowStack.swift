import AppKit

/// The stack: every window on one display sharing a single frame, one of them
/// in front, and ⌘-digits naming the rest.
///
/// Each enrolled window is resized exactly once — into the solo tile — when it
/// joins. Nothing is moved after that: switching windows is a raise and a focus
/// change, so the frames stay put and apps never re-layout.
final class WindowStack {
    private var windows: [ManagedWindow] = []
    private var reconcileTimer: Timer?

    func start() {
        capture()
        reconcileTimer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            self?.reconcile()
        }
    }

    // MARK: Membership

    /// Enroll every movable window on the stack's display, front-to-back.
    /// Windows on other displays are left alone.
    private func capture() {
        guard let display = displayFrame() else { return }
        windows = Windows.snapshot(on: display).compactMap(enroll)
        assignNumbers()
        FileHandle.standardError.write(Data("Tiler: enrolled \(windows.count) windows\n".utf8))
        // The snapshot is front-to-back, so the first entry already holds the
        // stage; focusing it just makes our idea of the front explicit.
        if let front = windows.first { Windows.focus(front) }
    }

    /// Adopt one window and tile it — the only resize it will ever get from us.
    private func enroll(_ info: Windows.Info) -> ManagedWindow? {
        guard let screen = screenFrame(), let managed = Windows.manage(info) else { return nil }
        Windows.setFrame(managed, StageLayout.tile(screen: screen))
        return managed
    }

    /// Arrivals join at the back, the dead are dropped, everyone else keeps
    /// their place.
    private func reconcile() {
        // A locked screen is not a membership change. The WindowServer stops
        // reporting the session's windows as on-screen while the lock is up, so
        // a pass taken now would evict the whole stack and re-enroll it from
        // scratch on unlock — losing every window's original frame and its
        // place in the order.
        guard !Session.screenIsLocked, let display = displayFrame() else { return }
        let snapshot = Windows.snapshot(on: display)
        let onScreen = Set(snapshot.map(\.id))
        let before = Set(windows.map(\.id))
        windows.removeAll { !onScreen.contains($0.id) || !Windows.isAlive($0.axWindow) }
        let known = Set(windows.map(\.id))
        let saved = Assignments.load()
        for info in snapshot where !known.contains(info.id) {
            if let managed = enroll(info) {
                managed.number = number(forArrival: managed, saved: saved)
                windows.append(managed)
            }
        }
        guard Set(windows.map(\.id)) != before else { return }
        persistNumbers()
    }

    // MARK: Switching

    /// Bring the window assigned this ⌘-digit to the front. Returns false when
    /// no window holds the number, so the keystroke can pass through to the app.
    func switchToWindow(number: Int) -> Bool {
        guard let window = windows.first(where: { $0.number == number }) else { return false }
        Windows.focus(window)
        return true
    }

    /// Menu- and switcher-driven switch to a specific window.
    func switchToWindow(id: CGWindowID) {
        guard let window = windows.first(where: { $0.id == id }) else { return }
        Windows.focus(window)
    }

    /// The stack member the WindowServer currently has in front, if any.
    private func front() -> ManagedWindow? {
        guard let id = Windows.frontmost(among: Set(windows.map(\.id))) else { return nil }
        return windows.first { $0.id == id }
    }

    // MARK: Numbering

    /// ⌥⌘-digit: give the front window this number.
    func assignFrontWindow(number: Int) -> Bool {
        guard let front = front() else { return false }
        assignWindow(id: front.id, number: number)
        return true
    }

    /// Give a window a ⌘-digit — a window already holding it takes the old
    /// number in exchange — or clear its digit (nil), which also drops the
    /// digit's persisted reservation.
    func assignWindow(id: CGWindowID, number: Int?) {
        guard let window = windows.first(where: { $0.id == id }) else { return }
        if let number {
            guard (1...9).contains(number), number != window.number else { return }
            if let holder = windows.first(where: { $0.number == number }) {
                holder.number = window.number
            }
            window.number = number
            StateLog.append("assigned \(number) to \(Windows.title(of: window))")
        } else {
            guard let old = window.number else { return }
            window.number = nil
            var map = Assignments.load()
            map[old] = nil
            Assignments.save(map)
            StateLog.append("cleared \(old) from \(Windows.title(of: window))")
        }
        persistNumbers()
    }

    /// Number the freshly captured stack: saved app assignments first (each
    /// digit goes to the first unnumbered window of its app), then the
    /// remaining windows take digits that are neither in use nor reserved for
    /// an absent app. The result is persisted, so numbering stays stable
    /// across restarts instead of following capture order.
    private func assignNumbers() {
        let saved = Assignments.load()
        for window in windows { window.number = nil }
        for (number, appID) in saved.sorted(by: { $0.key < $1.key }) {
            if let window = windows.first(where: { $0.appID == appID && $0.number == nil }) {
                window.number = number
            }
        }
        for window in windows where window.number == nil {
            window.number = freeNumber(saved: saved)
        }
        persistNumbers()
    }

    /// Digit for a window arriving mid-session: its app's lowest unused
    /// reservation first, else the lowest unreserved free digit.
    private func number(forArrival window: ManagedWindow, saved: [Int: String]) -> Int? {
        let used = Set(windows.compactMap(\.number))
        if let app = window.appID,
           let reserved = saved.filter({ $0.value == app && !used.contains($0.key) })
               .keys.min() {
            return reserved
        }
        return freeNumber(saved: saved)
    }

    /// The lowest digit neither held by a managed window nor reserved for an
    /// app that isn't on the stack right now.
    private func freeNumber(saved: [Int: String]) -> Int? {
        let used = Set(windows.compactMap(\.number))
        return (1...9).first { !used.contains($0) && saved[$0] == nil }
    }

    /// Write the stack's numbering over the saved map. Current windows win
    /// their digits; reservations of absent apps are kept, so quitting an app
    /// doesn't forfeit its number.
    private func persistNumbers() {
        var map = Assignments.load()
        for window in windows {
            if let number = window.number, let app = window.appID { map[number] = app }
        }
        Assignments.save(map)
    }

    // MARK: Reporting

    /// One managed window as the status menu and the switcher panel see it.
    struct WindowEntry {
        let id: CGWindowID
        /// The ⌘-digit this window holds, if any.
        let number: Int?
        let title: String
        /// Owning process — the UI resolves the app's name and icon from it,
        /// so the stack itself never touches AppKit imagery.
        let pid: pid_t
        /// Whether this window currently holds the stage.
        let isFront: Bool
    }

    /// Membership for the status menu and the panels, ordered by ⌘-digit.
    ///
    /// Digit order rather than stack order, so the list reads top to bottom as
    /// ⌘1, ⌘2, ⌘3 — the order is the numbering. Windows holding no digit sort
    /// last, keeping their relative stack order; the enumeration index is the
    /// tiebreaker because Swift's sort is not stable, so equal keys would
    /// otherwise shuffle between calls and make the list jitter.
    func windowList() -> [WindowEntry] {
        let front = front()
        return windows.enumerated()
            .sorted { a, b in
                (a.element.number ?? Int.max, a.offset) < (b.element.number ?? Int.max, b.offset)
            }
            .map {
                WindowEntry(id: $0.element.id, number: $0.element.number,
                            title: Windows.title(of: $0.element),
                            pid: $0.element.pid, isFront: $0.element === front)
            }
    }

    /// Renumber the stack from an explicit top-to-bottom order: the first
    /// window becomes ⌘1, the second ⌘2, and so on. Past the ninth, windows
    /// hold no digit — there are only nine keys.
    ///
    /// This is the drag-and-drop path, and it is a wholesale renumbering rather
    /// than the swap `assignWindow` performs: the user is stating what the
    /// order *is*, so every digit is rewritten to match rather than two windows
    /// trading places.
    func renumber(order ids: [CGWindowID]) {
        for window in windows { window.number = nil }
        var claimed: Set<Int> = []
        for (index, id) in ids.prefix(9).enumerated() {
            guard let window = windows.first(where: { $0.id == id }) else { continue }
            window.number = index + 1
            claimed.insert(index + 1)
        }

        var map = Assignments.load()
        // Reservations for apps that aren't here are kept — quitting an app
        // still shouldn't forfeit its digit. But a reservation naming an app
        // that *is* on the stack, at a digit the new order didn't give it, is
        // now stale and would fight the order the user just set by hand.
        let present = Set(windows.compactMap(\.appID))
        for (digit, app) in map where present.contains(app) && !claimed.contains(digit) {
            map[digit] = nil
        }
        for window in windows {
            if let number = window.number, let app = window.appID { map[number] = app }
        }
        Assignments.save(map)
        StateLog.append("renumbered \(min(ids.count, 9)) windows from drag order")
    }

    /// Put every enrolled window back where we found it.
    func restoreAll() {
        for window in windows { Windows.setFrame(window, window.originalFrame) }
    }

    // MARK: Display

    /// UUID of the chosen display; nil means "the primary display". Loaded once
    /// at startup and changed only through `select(displayUUID:)`.
    private(set) var selectedUUID: String? = Displays.savedSelection()

    /// Choose the stack's display, persist the choice, and rebuild there: give
    /// every window back its original frame, then capture from the new display.
    func select(displayUUID uuid: String?) {
        selectedUUID = uuid
        Displays.persistSelection(uuid)
        restoreAll()
        windows = []
        capture()
    }

    /// Display geometry changed (resolution, arrangement, connect or
    /// disconnect): re-tile every window for the current screen. Membership
    /// drift — e.g. windows macOS evacuated from a vanished display — is picked
    /// up by the next reconcile pass. Re-tiling is not a window switch, so the
    /// front window keeps both the stage and focus.
    func displayConfigurationChanged() {
        guard let screen = screenFrame() else { return }
        let tile = StageLayout.tile(screen: screen)
        for window in windows { Windows.setFrame(window, tile) }
    }

    /// Stage geometry (visible frame of the stack's display) in Quartz
    /// coordinates to match AX.
    private func screenFrame() -> CGRect? {
        Displays.screen(matching: selectedUUID).map(Displays.visibleFrame(of:))
    }

    /// Full frame of the stack's display, for window membership tests and the
    /// hotkey pointer gate.
    func displayFrame() -> CGRect? {
        Displays.screen(matching: selectedUUID).map(Displays.frame(of:))
    }
}
