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
    /// Set when membership changes mid-animation; re-aligns once it settles.
    private var pendingRelayout = false

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

    /// Enroll every movable on-screen window, front-to-back, and snap the ring into place.
    private func capture() {
        slots = Windows.snapshot().compactMap(enroll)
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

    /// Arrivals join at the back, the dead are dropped, everyone else keeps their place.
    private func reconcile() {
        let snapshot = Windows.snapshot()
        let onScreen = Set(snapshot.map(\.id))
        let before = Set(slots.map(\.id))
        slots.removeAll { !onScreen.contains($0.id) || !Windows.isAlive($0.axWindow) }
        let known = Set(slots.map(\.id))
        for info in snapshot where !known.contains(info.id) {
            if let managed = enroll(info) { slots.append(managed) }
        }
        // Realign only when membership changed; never fight the user otherwise.
        guard Set(slots.map(\.id)) != before else { return }
        // The slot angle changed with N, so the ring is off-grid: glide back
        // to alignment (deferred if a spin is mid-flight).
        if animator?.isAnimating == true {
            pendingRelayout = true
        } else {
            endScroll()
        }
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
        motion = .snapping(from: rotation, start: CACurrentMediaTime())
        animator?.start()
    }

    private var slotAngle: CGFloat { 2 * .pi / CGFloat(slots.count) }

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
            if pendingRelayout {
                pendingRelayout = false
                endScroll() // may re-enter .snapping
            }
            if case .idle = motion { return false }
            return true
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

    /// Slide every window to its spot on the ring.
    ///
    /// Off-strip windows hide dead-center behind the front window (true
    /// off-screen parking is impossible; the WindowServer clamps AX positions
    /// to keep ~40pt visible, which used to leave slivers pinned at the
    /// screen edges mid-gesture). That only works if the strip stays above
    /// the hidden stack, so a window joining the strip is raised first —
    /// while it's still at the clamped edge, where the raise can't flash
    /// anything onto the stage.
    private func render() {
        guard let screen = screenFrame(), !slots.isEmpty else { return }
        let tile = CarouselLayout.soloTile(screen: screen)
        let placements = slots.indices.map { i in
            CarouselLayout.placement(atTheta: rotation + CGFloat(i) * slotAngle,
                                     slotAngle: slotAngle, width: tile.width)
        }
        // Stamp strip joiners first so the off-stage rank comparisons below
        // see up-to-date ranks. The actual raise happens after the joiner's
        // position write: a joiner may be coming from the hidden center
        // stack, and raising it while still centered would flash it over the
        // front window until its move lands.
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
            switch placements[i] {
            case .strip(let offset):
                Windows.setPosition(slot, CGPoint(x: tile.minX + offset, y: tile.minY))
                // AX requests to one app process in order, so by the time
                // the raise lands the window is already off-center.
                if joiners.contains(i) { Windows.raise(slot.axWindow) }
            case .offStage(let edgeOffset):
                slot.onStrip = false
                let mayCoverStage = slot === confirmedFront || slot === pendingFront
                    || front == nil || slot.raiseGen > front!.raiseGen
                if mayCoverStage {
                    // Centering this window could cover the stage — it's the
                    // (possibly still) active window, or it outranks the
                    // current front (gesture reversal). Hold it at the
                    // clamped edge; the next settle confirms z-order and
                    // collapses it.
                    Windows.setPosition(slot, CGPoint(x: tile.minX + edgeOffset, y: tile.minY))
                } else {
                    Windows.setPosition(slot, tile.origin)
                }
            }
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

    /// Screen geometry in Quartz (top-left origin) coordinates to match AX.
    private func screenFrame() -> CGRect? {
        guard let screen = NSScreen.screens.first else { return nil }
        let f = screen.visibleFrame
        return CGRect(x: f.minX, y: screen.frame.height - f.maxY, width: f.width, height: f.height)
    }
}
