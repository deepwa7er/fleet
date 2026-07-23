import AppKit

/// Global input tap that drives the carousel, gated by pointer location:
/// events only reach the ring while the pointer is on its display, so the
/// same gestures and keys behave normally everywhere else.
///
/// - Filmstrip displays: ⌥+scroll spins the ring 1:1 and snaps to the
///   nearest window when the gesture ends.
/// - Stepper displays: ⌘1–9 switches straight to the numbered window and
///   ⌥⌘1–9 assigns the front window a number; ⌥+scroll passes through.
final class EventTap {
    /// Wheel notches per window slot. Trackpads instead use raw points
    /// against `Carousel.pointsPerSlot`.
    var wheelNotchesPerSlot: Double = 2

    private let carousel: Carousel
    private var tap: CFMachPort?

    var isTapActive: Bool { tap != nil }

    init(carousel: Carousel) {
        self.carousel = carousel
    }

    func start() {
        let mask: CGEventMask = (1 << CGEventType.scrollWheel.rawValue)
            | (1 << CGEventType.keyDown.rawValue)
        guard let tap = CGEvent.tapCreate(
            tap: .cgSessionEventTap,
            place: .headInsertEventTap,
            options: .defaultTap,
            eventsOfInterest: mask,
            callback: { _, type, event, refcon in
                let me = Unmanaged<EventTap>.fromOpaque(refcon!).takeUnretainedValue()
                return me.handle(type: type, event: event)
            },
            userInfo: Unmanaged.passUnretained(self).toOpaque()
        ) else {
            FileHandle.standardError.write(Data(
                "Carousel: failed to create event tap — is Accessibility granted?\n".utf8))
            return
        }
        self.tap = tap
        let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)
    }

    private func handle(type: CGEventType, event: CGEvent) -> Unmanaged<CGEvent>? {
        switch type {
        case .tapDisabledByTimeout, .tapDisabledByUserInput:
            if let tap { CGEvent.tapEnable(tap: tap, enable: true) }
        case .scrollWheel:
            return handleScroll(event)
        case .keyDown:
            return handleKey(event)
        default:
            break
        }
        return Unmanaged.passUnretained(event)
    }

    // MARK: ⌥+scroll (filmstrip displays)

    private func handleScroll(_ event: CGEvent) -> Unmanaged<CGEvent>? {
        guard event.flags.contains(.maskAlternate) else {
            return Unmanaged.passUnretained(event)
        }
        // Only spin when the pointer is on the ring's display and that
        // display switches by scrolling; everywhere else the event belongs
        // to the apps. (Scroll locations are Quartz global coordinates,
        // the same space as the display frame.)
        guard carousel.switchStyle() == .scroll,
              let stage = carousel.displayFrame(), stage.contains(event.location) else {
            return Unmanaged.passUnretained(event)
        }
        // Fingers lifted (trackpad): snap immediately rather than waiting for
        // the idle timer. 4 = scroll phase "ended".
        if event.getIntegerValueField(.scrollWheelEventScrollPhase) == 4 {
            carousel.endScroll()
            return nil
        }
        // Coasting after a flick: swallow it so it can't zoom/scroll the app
        // underneath, but don't drive the ring with it.
        guard event.getIntegerValueField(.scrollWheelEventMomentumPhase) == 0 else { return nil }

        // Dominant axis wins, so horizontal swipes drive the ring too.
        // Sign convention: swipe up or swipe left brings the next window in
        // from the right (natural-scrolling deltas).
        let delta: Double
        if event.getIntegerValueField(.scrollWheelEventIsContinuous) != 0 {
            let dx = event.getDoubleValueField(.scrollWheelEventPointDeltaAxis2)
            let dy = event.getDoubleValueField(.scrollWheelEventPointDeltaAxis1)
            delta = abs(dx) > abs(dy) ? -dx : dy
        } else {
            let dx = event.getDoubleValueField(.scrollWheelEventDeltaAxis2)
            let dy = event.getDoubleValueField(.scrollWheelEventDeltaAxis1)
            delta = (abs(dx) > abs(dy) ? -dx : dy) * carousel.pointsPerSlot / wheelNotchesPerSlot
        }
        carousel.scroll(byPoints: CGFloat(delta))
        return nil // consume so ⌥+scroll never reaches the app underneath
    }

    // MARK: ⌘1–9 (stepper displays)

    /// ANSI-layout virtual keycodes for the digit row.
    private static let digitKeycodes: [Int64: Int] = [
        18: 1, 19: 2, 20: 3, 21: 4, 23: 5, 22: 6, 26: 7, 28: 8, 25: 9,
    ]

    private func handleKey(_ event: CGEvent) -> Unmanaged<CGEvent>? {
        let pass = Unmanaged.passUnretained(event)
        let flags = event.flags
        guard flags.contains(.maskCommand),
              !flags.contains(.maskControl), !flags.contains(.maskShift),
              let digit = Self.digitKeycodes[event.getIntegerValueField(.keyboardEventKeycode)]
        else { return pass }
        // Keystrokes carry no useful location; gate on where the pointer
        // is, the same rule the scroll gesture uses.
        guard carousel.switchStyle() == .hotkeys,
              let stage = carousel.displayFrame(),
              let pointer = CGEvent(source: nil)?.location,
              stage.contains(pointer)
        else { return pass }
        // A held key auto-repeats; one switch per press is plenty.
        guard event.getIntegerValueField(.keyboardEventAutorepeat) == 0 else { return nil }
        if flags.contains(.maskAlternate) {
            return carousel.assignFrontWindow(number: digit) ? nil : pass
        }
        return carousel.switchToWindow(number: digit) ? nil : pass
    }
}
