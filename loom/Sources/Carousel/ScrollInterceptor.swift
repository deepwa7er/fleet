import AppKit

/// Drives the carousel ring from ⌥+scroll events: the ring tracks the
/// gesture 1:1, and the carousel snaps to the nearest window when it ends.
final class ScrollInterceptor {
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
        guard let tap = CGEvent.tapCreate(
            tap: .cgSessionEventTap,
            place: .headInsertEventTap,
            options: .defaultTap,
            eventsOfInterest: mask,
            callback: { _, type, event, refcon in
                let me = Unmanaged<ScrollInterceptor>.fromOpaque(refcon!).takeUnretainedValue()
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
        default:
            break
        }
        return Unmanaged.passUnretained(event)
    }

    private func handleScroll(_ event: CGEvent) -> Unmanaged<CGEvent>? {
        guard event.flags.contains(.maskAlternate) else {
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
}
