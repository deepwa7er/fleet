import AppKit

/// Global key tap driving the stack:
///
/// - ⌘1–9 brings the numbered window to the front, ⌥⌘1–9 gives the front
///   window a number. Both are gated on pointer location, so they only bind
///   while the pointer is on the stack's display and reach the app normally
///   everywhere else.
/// - ⌥Space toggles the switcher panel, from anywhere.
final class EventTap {
    private let stack: WindowStack
    private let switcher: SwitcherPanel
    private var tap: CFMachPort?

    var isTapActive: Bool { tap != nil }

    init(stack: WindowStack, switcher: SwitcherPanel) {
        self.stack = stack
        self.switcher = switcher
    }

    func start() {
        let mask: CGEventMask = 1 << CGEventType.keyDown.rawValue
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
                "Tiler: failed to create event tap — is Accessibility granted?\n".utf8))
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
        case .keyDown:
            return handleKey(event)
        default:
            break
        }
        return Unmanaged.passUnretained(event)
    }

    /// ANSI-layout virtual keycodes for the digit row.
    private static let digitKeycodes: [Int64: Int] = [
        18: 1, 19: 2, 20: 3, 21: 4, 23: 5, 22: 6, 26: 7, 28: 8, 25: 9,
    ]

    /// Virtual keycodes for the switcher's own keys.
    private static let spaceKeycode: Int64 = 49
    private static let escapeKeycode: Int64 = 53

    private func handleKey(_ event: CGEvent) -> Unmanaged<CGEvent>? {
        let pass = Unmanaged.passUnretained(event)
        let flags = event.flags
        let keycode = event.getIntegerValueField(.keyboardEventKeycode)

        // ⌥Space summons the switcher from anywhere. Unlike the digit keys it
        // is not aimed at a particular display, so it is deliberately not gated
        // on the pointer: the panel is how you reach the stack when the pointer
        // isn't on it.
        if keycode == Self.spaceKeycode, flags.contains(.maskAlternate),
           !flags.contains(.maskCommand), !flags.contains(.maskControl),
           !flags.contains(.maskShift) {
            guard event.getIntegerValueField(.keyboardEventAutorepeat) == 0 else { return nil }
            switcher.toggle()
            return nil
        }
        // Escape is only ours to swallow while the panel is actually up.
        if keycode == Self.escapeKeycode, switcher.isVisible {
            switcher.dismiss()
            return nil
        }

        guard flags.contains(.maskCommand),
              !flags.contains(.maskControl), !flags.contains(.maskShift),
              let digit = Self.digitKeycodes[keycode]
        else { return pass }
        // Keystrokes carry no useful location, so gate on where the pointer is:
        // off the stack's display, ⌘1–9 belongs to whatever app is there.
        guard let stage = stack.displayFrame(),
              let pointer = CGEvent(source: nil)?.location,
              stage.contains(pointer)
        else { return pass }
        // A held key auto-repeats; one switch per press is plenty.
        guard event.getIntegerValueField(.keyboardEventAutorepeat) == 0 else { return nil }
        if flags.contains(.maskAlternate) {
            return stack.assignFrontWindow(number: digit) ? nil : pass
        }
        return stack.switchToWindow(number: digit) ? nil : pass
    }
}
