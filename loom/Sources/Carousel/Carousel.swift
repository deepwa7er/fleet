import AppKit

/// The donut: windows ordered around a ring, a rotation angle, and membership upkeep.
///
/// Every enrolled window is resized exactly once — into the solo tile — when
/// it joins. From then on the ring only ever moves windows, so spins are
/// position-only AX writes and apps never re-layout mid-animation.
final class Carousel {
    private var slots: [ManagedWindow] = []
    private var rotation: CGFloat = 0   // rendered angle
    private var target: CGFloat = 0     // angle we're easing toward
    private var animator: Animator?
    private var reconcileTimer: Timer?

    /// What the ring is doing between frames. All rendering happens on the
    /// animator's display-link tick — never in the scroll event tap, whose
    /// callback must return fast (AX writes are blocking IPC into the target
    /// app and would stall the whole event pipeline).
    private enum Motion {
        case idle
        /// Rotation follows the live gesture 1:1, rendered next frame.
        case tracking
        /// Timed glide from `from` toward `target`, begun at `start`.
        case snapping(from: CGFloat, start: CFTimeInterval)
        /// Snap landed and `front` was raised; waiting for the WindowServer
        /// to report it frontmost before stacking the rest behind it.
        /// Collapsing earlier would flash the previously-front window over
        /// the stage. `deadline` bounds the wait if an app ignores the raise.
        case settling(front: ManagedWindow, deadline: CFTimeInterval)
    }
    private var motion: Motion = .idle

    /// True while the ring is collapsed into the at-rest stack.
    private var resting = false

    /// How long `settling` waits for a raise to land before collapsing anyway.
    private let raiseTimeout: CFTimeInterval = 0.25

    func start() {
        capture()
        animator = Animator { [weak self] now in self?.stepAnimation(now: now) ?? false }
        reconcileTimer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            self?.reconcile()
        }
    }

    // MARK: Membership

    /// Enroll every movable window on the ring's display, front-to-back, and
    /// snap the ring into place. Windows on other displays are left alone.
    private func capture() {
        guard let display = displayFrame() else { return }
        slots = Windows.snapshot(on: display).compactMap(enroll)
        assignNumbers()
        FileHandle.standardError.write(Data("Carousel: enrolled \(slots.count) windows\n".utf8))
        // Slot 0 is the OS's frontmost window, so it already covers the
        // stack — no raise race to wait out here.
        renderResting()
        if let front = frontSlot() {
            markRaised(front)
            confirmedFront = front
            Windows.focus(front)
        }
    }

    /// Adopt one window and tile it — the only resize it will ever get from us.
    private func enroll(_ info: Windows.Info) -> ManagedWindow? {
        guard let screen = screenFrame(), let managed = Windows.manage(info) else { return nil }
        Windows.setFrame(managed, CarouselLayout.soloTile(screen: screen))
        return managed
    }

    /// Arrivals join at the back, the dead are dropped, everyone else keeps
    /// their place. Runs only while the ring is at rest: mid-gesture, ring
    /// windows are deliberately away from their resting spot (sliding,
    /// edge-held, or hidden), and judging membership by position then would
    /// wrongly drop them.
    private func reconcile() {
        // A locked screen is not a membership change. The WindowServer stops
        // reporting the session's windows as on-screen while the lock is up, so
        // a pass taken now would evict the entire ring and re-enroll it from
        // scratch on unlock — losing every window's original frame and
        // re-deriving the ring's order from whatever z-order the unlock left.
        guard !Session.screenIsLocked else { return }
        guard case .idle = motion, let display = displayFrame() else { return }
        let snapshot = Windows.snapshot(on: display)
        let onScreen = Set(snapshot.map(\.id))
        let before = Set(slots.map(\.id))
        slots.removeAll { !onScreen.contains($0.id) || !Windows.isAlive($0.axWindow) }
        let known = Set(slots.map(\.id))
        let saved = Assignments.load()
        for info in snapshot where !known.contains(info.id) {
            if let managed = enroll(info) {
                managed.number = number(forArrival: managed, saved: saved)
                slots.append(managed)
            }
        }
        // Realign only when membership changed; never fight the user otherwise.
        guard Set(slots.map(\.id)) != before else { return }
        persistNumbers()
        // The slot angle changed with N, so the ring is off-grid: put it back
        // on-grid around whichever window is already in front.
        realign()
    }

    // MARK: Rotation

    /// Scroll points of travel per window slot — bigger = slower ring.
    var pointsPerSlot: CGFloat = 75

    /// How long the snap glide to the nearest slot takes.
    var snapDuration: CFTimeInterval = 0.18

    private var snapTimer: Timer?

    /// Drive the ring directly with the scroll gesture: 1:1 tracking, no
    /// quantization. The snap timer glides to the nearest window once the
    /// gesture goes quiet.
    func scroll(byPoints points: CGFloat) {
        guard slots.count > 1 else { return }
        target -= points * slotAngle / pointsPerSlot
        motion = .tracking
        animator?.start()
        snapTimer?.invalidate()
        snapTimer = Timer.scheduledTimer(withTimeInterval: 0.15, repeats: false) { [weak self] _ in
            self?.endScroll()
        }
    }

    /// Ease to the nearest slot; the window that lands in front takes focus.
    func endScroll() {
        guard !slots.isEmpty else { return }
        snapTimer?.invalidate()
        snapTimer = nil
        let slot = (target / slotAngle).rounded()
        FileHandle.standardError.write(Data("Carousel: snap to slot \(Int(slot))\n".utf8))
        StateLog.append("snap to slot \(Int(slot))")
        target = slot * slotAngle
        beginSnap()
    }

    private func beginSnap() {
        if stepperMode() { rotation = target } // nothing animates; land now
        motion = .snapping(from: rotation, start: CACurrentMediaTime())
        animator?.start()
    }

    /// Put the ring back on-grid after its membership or its display geometry
    /// changed, without touching focus.
    ///
    /// Rotation is an angle over slot *indices*, so a window joining or leaving
    /// renumbers every slot: the same angle now names a different window. The
    /// ring therefore re-derives its angle from the window the WindowServer
    /// already has in front, which keeps the stage with the window the user is
    /// looking at. Nothing is raised or focused — an arrival or a departure is
    /// not a request to switch windows.
    ///
    /// `endScroll` is the counterpart for the user's own gesture: that one does
    /// land on a new window, and does move focus to it.
    private func realign() {
        snapTimer?.invalidate()
        snapTimer = nil
        motion = .idle
        pendingFront = nil
        guard let anchor = frontmostSlot() ?? slots.first,
              let index = slots.firstIndex(where: { $0 === anchor })
        else {
            rotation = 0
            target = 0
            confirmedFront = nil
            return
        }
        target = -CGFloat(index) * slotAngle
        rotation = target
        confirmedFront = anchor
        renderResting()
    }

    /// The ring member the WindowServer currently has in front, if any.
    private func frontmostSlot() -> ManagedWindow? {
        guard let id = Windows.frontmost(among: Set(slots.map(\.id))) else { return nil }
        return slots.first { $0.id == id }
    }

    private var slotAngle: CGFloat { 2 * .pi / CGFloat(slots.count) }

    // MARK: Switching

    /// How windows are switched on the ring's display. Filmstrip displays
    /// spin with ⌥+scroll; stepper displays (a side blocked by an adjacent
    /// display — nothing can slide) switch instantly with ⌘1–9 instead.
    enum SwitchStyle {
        case scroll
        case hotkeys
    }

    func switchStyle() -> SwitchStyle {
        stepperMode() ? .hotkeys : .scroll
    }

    /// Bring the window assigned this ⌘-digit to the front. Returns false
    /// when no window holds the number, so the keystroke can pass through.
    func switchToWindow(number: Int) -> Bool {
        guard let index = slots.firstIndex(where: { $0.number == number }) else { return false }
        switchTo(index: index)
        return true
    }

    /// Menu-driven switch to a specific window.
    func switchToWindow(id: CGWindowID) {
        guard let index = slots.firstIndex(where: { $0.id == id }) else { return }
        switchTo(index: index)
    }

    /// ⌥⌘-digit: give the front window this number.
    func assignFrontWindow(number: Int) -> Bool {
        guard let front = frontSlot() else { return false }
        assignWindow(id: front.id, number: number)
        return true
    }

    /// Give a window a ⌘-digit — a window already holding it takes the old
    /// number in exchange — or clear its digit (nil), which also drops the
    /// digit's persisted reservation.
    func assignWindow(id: CGWindowID, number: Int?) {
        guard let slot = slots.first(where: { $0.id == id }) else { return }
        if let number {
            guard (1...9).contains(number), number != slot.number else { return }
            if let holder = slots.first(where: { $0.number == number }) {
                holder.number = slot.number
            }
            slot.number = number
            StateLog.append("assigned \(number) to \(Windows.title(of: slot))")
        } else {
            guard let old = slot.number else { return }
            slot.number = nil
            var map = Assignments.load()
            map[old] = nil
            Assignments.save(map)
            StateLog.append("cleared \(old) from \(Windows.title(of: slot))")
        }
        persistNumbers()
    }

    /// Ring membership for the status menu, in slot order.
    func windowList() -> [(number: Int?, title: String, id: CGWindowID)] {
        slots.map { ($0.number, Windows.title(of: $0), $0.id) }
    }

    /// Rotate the ring so `index`'s window lands in front, via the shortest
    /// path. Snaps instantly on stepper displays, animates on filmstrips.
    private func switchTo(index: Int) {
        snapTimer?.invalidate()
        snapTimer = nil
        let desired = -CGFloat(index) * slotAngle
        var delta = (desired - target).truncatingRemainder(dividingBy: 2 * .pi)
        if delta > .pi { delta -= 2 * .pi } else if delta <= -.pi { delta += 2 * .pi }
        target += delta
        beginSnap()
    }

    /// Number the freshly captured ring: saved app assignments first (each
    /// digit goes to the first unnumbered window of its app), then the
    /// remaining windows take digits that are neither in use nor reserved
    /// for an absent app. The result is persisted, so numbering stays
    /// stable across restarts instead of following capture order.
    private func assignNumbers() {
        let saved = Assignments.load()
        for slot in slots { slot.number = nil }
        for (number, appID) in saved.sorted(by: { $0.key < $1.key }) {
            if let slot = slots.first(where: { $0.appID == appID && $0.number == nil }) {
                slot.number = number
            }
        }
        for slot in slots where slot.number == nil {
            slot.number = freeNumber(saved: saved)
        }
        persistNumbers()
    }

    /// Digit for a window arriving mid-session: its app's lowest unused
    /// reservation first, else the lowest unreserved free digit.
    private func number(forArrival slot: ManagedWindow, saved: [Int: String]) -> Int? {
        let used = Set(slots.compactMap(\.number))
        if let app = slot.appID,
           let reserved = saved.filter({ $0.value == app && !used.contains($0.key) })
               .keys.min() {
            return reserved
        }
        return freeNumber(saved: saved)
    }

    /// The lowest digit neither held by a ring window nor reserved for an
    /// app that isn't on the ring right now.
    private func freeNumber(saved: [Int: String]) -> Int? {
        let used = Set(slots.compactMap(\.number))
        return (1...9).first { !used.contains($0) && saved[$0] == nil }
    }

    /// Write the ring's numbering over the saved map. Current windows win
    /// their digits; reservations of absent apps are kept, so quitting an
    /// app doesn't forfeit its number.
    private func persistNumbers() {
        var map = Assignments.load()
        for slot in slots {
            if let number = slot.number, let app = slot.appID { map[number] = app }
        }
        Assignments.save(map)
    }

    /// One animation frame. Returns false once the ring has settled.
    private func stepAnimation(now: CFTimeInterval) -> Bool {
        switch motion {
        case .idle:
            return false
        case .tracking:
            rotation = target
            render()
            return true
        case .snapping(let from, let start):
            let progress = min(1, max(0, (now - start) / snapDuration))
            let done = progress >= 1 || abs(target - from) < 0.0001
            rotation = done ? target : from + (target - from) * easeOutCubic(CGFloat(progress))
            guard done else {
                render()
                return true
            }
            // Land the final strip frame — the new front alone covers the
            // stage — raise it, then hold this layout until the raise lands.
            if !resting { render() }
            guard let front = frontSlot() else {
                motion = .idle
                return false
            }
            markRaised(front)
            pendingFront = front
            Windows.focus(front)
            motion = .settling(front: front, deadline: now + raiseTimeout)
            return true
        case .settling(let front, let deadline):
            guard now >= deadline
                || Windows.isFrontmost(front.id, among: Set(slots.map(\.id))) else { return true }
            confirmedFront = front
            pendingFront = nil
            renderResting()
            motion = .idle
            return false
        }
    }

    /// Fast start, gentle landing — and unlike a proportional ease, it
    /// actually finishes instead of crawling through the last few pixels.
    private func easeOutCubic(_ t: CGFloat) -> CGFloat {
        1 - pow(1 - t, 3)
    }

    // MARK: Layout

    /// Monotonic stamp source for `ManagedWindow.raiseGen`.
    private var raiseGeneration = 0

    /// The window the OS currently considers active (`confirmedFront`) and
    /// the one we've asked to take over but haven't confirmed (`pendingFront`).
    /// Neither may hide at stage center: the WindowServer keeps the active
    /// app's window above AX-raised background windows, so raise stamps
    /// can't vouch for it — only the settle confirmation can.
    private var confirmedFront: ManagedWindow?
    private var pendingFront: ManagedWindow?

    private func markRaised(_ slot: ManagedWindow) {
        raiseGeneration += 1
        slot.raiseGen = raiseGeneration
    }

    /// A display where the filmstrip can't run: an adjacent display blocks
    /// at least one side, so sliding windows out would parade them across
    /// the neighbor screen. Such displays switch instantly instead — the
    /// windows never move; focus and z-order flips are the whole show.
    private func stepperMode() -> Bool {
        guard let screen = Displays.screen(matching: selectedUUID) else { return false }
        let tile = CarouselLayout.soloTile(screen: Displays.visibleFrame(of: screen))
        let open = Displays.openSides(of: screen,
                                      stride: CarouselLayout.stride(width: tile.width))
        return !(open.left && open.right)
    }

    /// Move every window to its spot on the ring (filmstrip displays only;
    /// stepper displays never move windows — see `stepperMode`).
    ///
    /// Off-strip windows hide dead-center behind the front window — true
    /// off-screen parking is impossible; the WindowServer clamps AX
    /// positions to keep ~40pt visible. That only works if whatever is on
    /// the strip stays above the hidden stack, so strip joiners are raised
    /// (position first, then raise: AX requests to one app process in
    /// order, so the raise lands only after the window has left center).
    private func render() {
        guard let screen = Displays.screen(matching: selectedUUID), !slots.isEmpty else { return }
        guard !stepperMode() else {
            renderResting() // everyone stays stacked; stepFocus does the rest
            return
        }
        let tile = CarouselLayout.soloTile(screen: Displays.visibleFrame(of: screen))
        let stride = CarouselLayout.stride(width: tile.width)
        let placements = slots.indices.map { i in
            CarouselLayout.placement(atTheta: rotation + CGFloat(i) * slotAngle,
                                     slotAngle: slotAngle, width: tile.width)
        }
        // Stamp strip joiners first so the off-stage rank comparisons below
        // see up-to-date ranks. The actual raise happens after the joiner's
        // position write, so a joiner leaving the hidden center stack can't
        // flash over the front window on its way out.
        var joiners = Set<Int>()
        for (i, slot) in slots.enumerated() {
            if case .strip = placements[i], !slot.onStrip {
                slot.onStrip = true
                markRaised(slot)
                joiners.insert(i)
            }
        }
        // The visual front: the on-strip window nearest stage center.
        var front: ManagedWindow?
        var frontDistance = CGFloat.infinity
        for (i, placement) in placements.enumerated() {
            if case .strip(let offset) = placement, abs(offset) < frontDistance {
                frontDistance = abs(offset)
                front = slots[i]
            }
        }
        for (i, slot) in slots.enumerated() {
            let x: CGFloat
            switch placements[i] {
            case .strip(let offset):
                x = tile.minX + offset
            case .offStage(let side):
                slot.onStrip = false
                let mayCoverStage = slot === confirmedFront || slot === pendingFront
                    || front == nil || slot.raiseGen > front!.raiseGen
                if mayCoverStage {
                    // Centering this window could cover the stage — it's the
                    // (possibly still) active window, or it outranks the
                    // current front (gesture reversal). Hold it at the
                    // clamped screen edge until the next settle confirms
                    // z-order and collapses it.
                    x = tile.minX + side * stride
                } else {
                    x = tile.minX
                }
            }
            Windows.setPosition(slot, CGPoint(x: x, y: tile.minY))
            if joiners.contains(i) { Windows.raise(slot.axWindow) }
        }
        resting = false
    }

    /// The at-rest layout: every window sits exactly on the tile, stacked
    /// behind the front one. Parking off-screen can never fully hide a
    /// window — the WindowServer clamps AX positions so ~40pt always stays
    /// on-screen, leaving a sliver peeking into the stage — but the windows
    /// all share the tile frame, so dead-center behind the front window
    /// they're covered completely. They fan back out to the strip on the
    /// first frame of the next gesture, while the front window still covers
    /// the stage.
    private func renderResting() {
        guard let screen = screenFrame(), !slots.isEmpty else { return }
        let origin = CarouselLayout.soloTile(screen: screen).origin
        for slot in slots {
            slot.onStrip = false
            Windows.setPosition(slot, origin)
        }
        resting = true
    }

    private func frontSlot() -> ManagedWindow? {
        guard let screen = screenFrame() else { return slots.first }
        return slotsByDepth(screen: screen).last
    }

    /// Slots sorted back-to-front (ascending depth) at the current rotation.
    private func slotsByDepth(screen: CGRect) -> [ManagedWindow] {
        slots.enumerated()
            .sorted { CarouselLayout.depth(atTheta: rotation + CGFloat($0.offset) * slotAngle)
                    < CarouselLayout.depth(atTheta: rotation + CGFloat($1.offset) * slotAngle) }
            .map(\.element)
    }

    /// Put every enrolled window back where we found it.
    func restoreAll() {
        for slot in slots { Windows.setFrame(slot, slot.originalFrame) }
    }

    // MARK: Display

    /// UUID of the chosen display; nil means "the primary display". Loaded
    /// once at startup and changed only through `select(displayUUID:)`.
    private(set) var selectedUUID: String? = Displays.savedSelection()

    /// Choose the ring's display, persist the choice, and rebuild there.
    func select(displayUUID uuid: String?) {
        selectedUUID = uuid
        Displays.persistSelection(uuid)
        retarget()
    }

    /// Move the ring to the newly selected display: give every window back
    /// its original frame, then rebuild membership from that display.
    private func retarget() {
        restoreAll()
        snapTimer?.invalidate()
        snapTimer = nil
        motion = .idle
        confirmedFront = nil
        pendingFront = nil
        rotation = 0
        target = 0
        slots = []
        capture()
    }

    /// Display geometry changed (resolution, arrangement, connect or
    /// disconnect): re-tile every slot for the current screen and settle.
    /// Membership drift — e.g. windows macOS evacuated from a vanished
    /// display — is picked up by the next reconcile pass. Re-tiling is not a
    /// window switch, so the front window keeps both the stage and focus.
    func displayConfigurationChanged() {
        guard let screen = screenFrame() else { return }
        let tile = CarouselLayout.soloTile(screen: screen)
        for slot in slots { Windows.setFrame(slot, tile) }
        realign()
    }

    /// Stage geometry (visible frame of the ring's display) in Quartz
    /// coordinates to match AX.
    private func screenFrame() -> CGRect? {
        Displays.screen(matching: selectedUUID).map(Displays.visibleFrame(of:))
    }

    /// Full frame of the ring's display, for window membership tests and
    /// the scroll interceptor's pointer gate.
    func displayFrame() -> CGRect? {
        Displays.screen(matching: selectedUUID).map(Displays.frame(of:))
    }
}
